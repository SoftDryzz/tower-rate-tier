use std::fmt;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use crate::quota::{Nanos, Quota};
use crate::storage::memory::MemoryStorage;
use crate::storage::{Storage, StorageError, StorageFuture, StorageKey};

use self::transition::{after_call, Outcome, Policy, Route, State};

mod transition;

/// Consecutive failures that open the breaker unless
/// [`failure_threshold`](CircuitBreaker::failure_threshold) says otherwise.
const DEFAULT_FAILURE_THRESHOLD: u32 = 3;

/// How long the breaker stays open unless
/// [`open_for`](CircuitBreaker::open_for) says otherwise.
const DEFAULT_OPEN_FOR: Duration = Duration::from_secs(5);

type Callback = Box<dyn Fn(&BreakerEvent<'_>) + Send + Sync>;

/// A [`Storage`] that keeps rate limiting with a local [`MemoryStorage`]
/// while its primary storage (usually Redis) fails.
///
/// A call that fails on the primary is answered by the fallback. After
/// `failure_threshold` consecutive failures the breaker opens: requests go
/// straight to the fallback, without waiting for the primary, for
/// `open_for`. Then one request probes the primary; if it answers, requests
/// go back to it and the fallback is emptied.
pub struct CircuitBreaker<P> {
    primary: P,
    fallback: MemoryStorage,
    policy: Policy,
    state: Mutex<State>,
    on_state_change: Option<Callback>,
}

/// Something that happened to a [`CircuitBreaker`], delivered to the
/// callback set with [`on_state_change`](CircuitBreaker::on_state_change).
///
/// New variants may be added in minor releases.
#[derive(Debug)]
#[non_exhaustive]
pub enum BreakerEvent<'a> {
    /// A call to the primary storage failed, including a failed probe. The
    /// request was answered by the fallback.
    #[non_exhaustive]
    PrimaryFailed {
        /// The error returned by the primary storage.
        error: &'a StorageError,
    },
    /// The breaker opened: requests stop going to the primary storage.
    /// Reported once per outage; a failed probe only reports
    /// [`PrimaryFailed`](Self::PrimaryFailed).
    #[non_exhaustive]
    Opened {
        /// The failure that opened the breaker.
        error: &'a StorageError,
    },
    /// A probe succeeded: requests go back to the primary storage.
    #[non_exhaustive]
    Closed,
}

impl<P: Storage> CircuitBreaker<P> {
    /// Wraps `primary`, answering from `fallback` while it fails.
    ///
    /// The breaker opens after 3 consecutive failures and stays open for
    /// 5 seconds before probing.
    pub fn new(primary: P, fallback: MemoryStorage) -> Self {
        Self {
            primary,
            fallback,
            policy: Policy {
                failure_threshold: DEFAULT_FAILURE_THRESHOLD,
                open_for: duration_nanos(DEFAULT_OPEN_FOR),
            },
            state: Mutex::new(State::Closed { failures: 0 }),
            on_state_change: None,
        }
    }

    /// Sets how many consecutive failures open the breaker (default 3).
    ///
    /// # Panics
    ///
    /// Panics if `threshold` is 0.
    pub fn failure_threshold(mut self, threshold: u32) -> Self {
        assert!(threshold > 0, "failure threshold must be greater than 0");
        self.policy.failure_threshold = threshold;
        self
    }

    /// Sets how long the breaker stays open before a probe (default 5 s).
    ///
    /// # Panics
    ///
    /// Panics if `duration` is zero.
    pub fn open_for(mut self, duration: Duration) -> Self {
        assert!(!duration.is_zero(), "open_for must be non-zero");
        self.policy.open_for = duration_nanos(duration);
        self
    }

    /// Calls `callback` for every [`BreakerEvent`].
    ///
    /// The callback runs on the request's task, after the breaker's lock is
    /// released, so it should be quick (log or count, don't block).
    pub fn on_state_change<F>(mut self, callback: F) -> Self
    where
        F: Fn(&BreakerEvent<'_>) + Send + Sync + 'static,
    {
        self.on_state_change = Some(Box::new(callback));
        self
    }

    /// Decides where a request goes: `Some(route)` to the primary, `None` to
    /// the fallback. The first request after the open period becomes the
    /// probe.
    fn admit(&self, now: Nanos) -> Option<Route> {
        let mut state = self.lock_state();
        match *state {
            State::Closed { .. } => Some(Route::Primary),
            State::Open { until } if now >= until => {
                *state = State::HalfOpen;
                Some(Route::Probe)
            }
            State::Open { .. } | State::HalfOpen => None,
        }
    }

    fn emit(&self, event: &BreakerEvent<'_>) {
        if let Some(callback) = &self.on_state_change {
            callback(event);
        }
    }
}

impl<P> CircuitBreaker<P> {
    /// A panic elsewhere cannot leave `State` half-written, so a poisoned
    /// lock is still safe to use.
    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<P: Storage> Storage for CircuitBreaker<P> {
    fn check_and_update<'a>(
        &'a self,
        key: StorageKey<'a>,
        quota: &'a Quota,
        cost: u32,
        now: Nanos,
    ) -> StorageFuture<'a> {
        let Some(route) = self.admit(now) else {
            return self.fallback.check_and_update(key, quota, cost, now);
        };
        // Created before the future, so dropping the future even unpolled
        // releases the probe.
        let probe_guard = ProbeGuard {
            state: &self.state,
            admitted_at: now,
            armed: route == Route::Probe,
        };

        Box::pin(async move {
            // Binding the whole guard moves it into the future; a bare
            // `probe_guard.armed` would capture only the bool and drop the
            // guard as soon as this function returns.
            let mut probe_guard = probe_guard;
            if route == Route::Probe {
                self.fallback.retain_active(now);
            }

            let result = self.primary.check_and_update(key, quota, cost, now).await;
            let outcome = match result {
                Ok(_) => Outcome::Success,
                Err(_) => Outcome::Failure,
            };
            let (old, new) = {
                let mut state = self.lock_state();
                let old = *state;
                *state = after_call(old, route, outcome, now, &self.policy);
                (old, *state)
            };
            probe_guard.armed = false;

            match result {
                Ok(decision) => {
                    if old == State::HalfOpen && matches!(new, State::Closed { .. }) {
                        self.fallback.clear();
                        self.emit(&BreakerEvent::Closed);
                    }
                    Ok(decision)
                }
                Err(error) => {
                    self.emit(&BreakerEvent::PrimaryFailed { error: &error });
                    if matches!(old, State::Closed { .. }) && matches!(new, State::Open { .. }) {
                        self.fallback.retain_active(now);
                        self.emit(&BreakerEvent::Opened { error: &error });
                    }
                    self.fallback.check_and_update(key, quota, cost, now).await
                }
            }
        })
    }
}

impl<P: fmt::Debug> fmt::Debug for CircuitBreaker<P> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CircuitBreaker")
            .field("primary", &self.primary)
            .field("fallback", &self.fallback)
            .field("state", &*self.lock_state())
            .field("failure_threshold", &self.policy.failure_threshold)
            .field("open_for", &Duration::from_nanos(self.policy.open_for))
            .field("on_state_change", &self.on_state_change.is_some())
            .finish()
    }
}

/// Releases a probe whose future was dropped before it finished (the client
/// went away, or a timeout cancelled it).
///
/// The breaker goes back to open with an `until` that has already passed, so
/// the next request probes again. Without this it would stay half-open, and
/// every request would use the fallback forever.
struct ProbeGuard<'a> {
    state: &'a Mutex<State>,
    admitted_at: Nanos,
    armed: bool,
}

impl Drop for ProbeGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if *state == State::HalfOpen {
            *state = State::Open {
                until: self.admitted_at,
            };
        }
    }
}

/// Converts `duration` to nanoseconds, saturating at `u64::MAX` (~584 years).
fn duration_nanos(duration: Duration) -> Nanos {
    Nanos::try_from(duration.as_nanos()).unwrap_or(Nanos::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, OnceLock, Weak};
    use std::time::Duration;

    use tokio::sync::Semaphore;

    use super::*;
    use crate::gcra::{RateLimitInfo, RateLimited};
    use crate::quota::{Nanos, Quota};
    use crate::storage::memory::MemoryStorage;
    use crate::storage::{Storage, StorageError, StorageFuture, StorageKey};

    const S: Nanos = 1_000_000_000;

    /// One scripted answer of the fake primary storage.
    #[derive(Debug, Clone, Copy)]
    enum Step {
        Allow,
        Limit,
        Fail(&'static str),
        /// Waits for [`Scripted::release`], then allows.
        HeldAllow,
        /// Waits for [`Scripted::release`], then fails.
        HeldFail(&'static str),
        /// Never answers, like a call stuck on a dead connection.
        Never,
    }

    /// A primary storage that answers from a script and counts its calls.
    #[derive(Debug)]
    struct Scripted {
        calls: AtomicUsize,
        script: Mutex<VecDeque<Step>>,
        then: Step,
        gate: Arc<Semaphore>,
    }

    impl Scripted {
        /// Answers `steps` in order, then `then` forever.
        fn new(steps: &[Step], then: Step) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                script: Mutex::new(steps.iter().copied().collect()),
                then,
                gate: Arc::new(Semaphore::new(0)),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        /// Lets `n` held calls answer.
        fn release(&self, n: usize) {
            self.gate.add_permits(n);
        }
    }

    impl Storage for Scripted {
        fn check_and_update<'a>(
            &'a self,
            _key: StorageKey<'a>,
            _quota: &'a Quota,
            _cost: u32,
            _now: Nanos,
        ) -> StorageFuture<'a> {
            // Take the step before counting the call, so a test that waits
            // for `calls() == n` knows the first n steps are already taken.
            let step = self.script.lock().unwrap().pop_front().unwrap_or(self.then);
            self.calls.fetch_add(1, Ordering::SeqCst);
            let gate = Arc::clone(&self.gate);
            Box::pin(async move {
                let step = match step {
                    Step::HeldAllow | Step::HeldFail(_) => {
                        gate.acquire().await.unwrap().forget();
                        match step {
                            Step::HeldFail(message) => Step::Fail(message),
                            _ => Step::Allow,
                        }
                    }
                    Step::Never => std::future::pending().await,
                    other => other,
                };
                match step {
                    Step::Fail(message) => Err(StorageError(Box::from(message))),
                    Step::Limit => Ok(Err(RateLimited {
                        limit: 3,
                        retry_after: Duration::from_secs(20),
                        reset_after: Duration::from_secs(60),
                    })),
                    _ => Ok(Ok(RateLimitInfo {
                        limit: 3,
                        remaining: 2,
                        reset_after: Duration::from_secs(20),
                    })),
                }
            })
        }
    }

    /// Yields until `condition` holds, failing the test after 5 seconds.
    async fn wait_until(condition: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !condition() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("condition not reached within 5 s");
    }

    type Decision = Result<Result<RateLimitInfo, RateLimited>, StorageError>;

    fn quota() -> Quota {
        Quota::per_minute(3)
    }

    async fn call(breaker: &CircuitBreaker<Scripted>, now: Nanos) -> Decision {
        let quota = quota();
        breaker
            .check_and_update(StorageKey::new("u1", "free"), &quota, 1, now)
            .await
    }

    /// Makes a call that must be answered, by the primary or the fallback.
    async fn serve(breaker: &CircuitBreaker<Scripted>, now: Nanos) {
        let decision = call(breaker, now).await;
        assert!(decision.is_ok(), "{decision:?}");
    }

    /// Records every event as a short string, in order.
    fn recorder() -> (
        Arc<Mutex<Vec<String>>>,
        impl Fn(&BreakerEvent<'_>) + Send + Sync + 'static,
    ) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let callback = move |event: &BreakerEvent<'_>| {
            let line = match event {
                BreakerEvent::PrimaryFailed { error, .. } => format!("PrimaryFailed: {error}"),
                BreakerEvent::Opened { error, .. } => format!("Opened: {error}"),
                BreakerEvent::Closed { .. } => "Closed".to_owned(),
            };
            sink.lock().unwrap().push(line);
        };
        (events, callback)
    }

    fn failing(times: usize) -> Vec<Step> {
        vec![Step::Fail("redis down"); times]
    }

    #[tokio::test]
    async fn closed_passes_primary_results_through() {
        let breaker = CircuitBreaker::new(Scripted::new(&[], Step::Limit), MemoryStorage::new());

        let result = call(&breaker, 0).await.unwrap();

        assert_eq!(
            result,
            Err(RateLimited {
                limit: 3,
                retry_after: Duration::from_secs(20),
                reset_after: Duration::from_secs(60),
            })
        );
        assert_eq!(breaker.primary.calls(), 1);
    }

    #[tokio::test]
    async fn primary_failure_is_served_by_fallback() {
        let breaker = CircuitBreaker::new(
            Scripted::new(&failing(1), Step::Allow),
            MemoryStorage::new(),
        );

        let info = call(&breaker, 0).await.unwrap().unwrap();

        // The fallback applied the full quota: 3 per minute, 1 used.
        assert_eq!(info.limit, 3);
        assert_eq!(info.remaining, 2);
        assert_eq!(breaker.primary.calls(), 1);
    }

    #[tokio::test]
    async fn opens_after_threshold_and_skips_primary() {
        let breaker = CircuitBreaker::new(
            Scripted::new(&failing(3), Step::Allow),
            MemoryStorage::new(),
        );
        for _ in 0..3 {
            assert!(call(&breaker, 0).await.unwrap().is_ok());
        }

        let fourth = call(&breaker, 1).await.unwrap();

        assert_eq!(breaker.primary.calls(), 3);
        // The three failed calls were served and counted by the fallback.
        assert!(fourth.is_err(), "{fourth:?}");
    }

    #[tokio::test]
    async fn rate_limited_answers_count_as_success() {
        let (events, callback) = recorder();
        let breaker = CircuitBreaker::new(Scripted::new(&[], Step::Limit), MemoryStorage::new())
            .on_state_change(callback);

        for now in 0..10 {
            assert!(call(&breaker, now).await.unwrap().is_err());
        }

        assert_eq!(breaker.primary.calls(), 10);
        assert!(events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn probe_waits_for_open_for() {
        let breaker = CircuitBreaker::new(
            Scripted::new(&failing(3), Step::Allow),
            MemoryStorage::new(),
        );
        for _ in 0..3 {
            serve(&breaker, 0).await;
        }

        serve(&breaker, 5 * S - 1).await;
        assert_eq!(breaker.primary.calls(), 3);

        serve(&breaker, 5 * S).await;
        assert_eq!(breaker.primary.calls(), 4);
    }

    #[tokio::test]
    async fn failed_probe_is_served_by_fallback_and_reopens() {
        let breaker = CircuitBreaker::new(
            Scripted::new(&failing(4), Step::Allow),
            MemoryStorage::new(),
        );
        for _ in 0..3 {
            serve(&breaker, 0).await;
        }

        assert!(call(&breaker, 5 * S).await.is_ok());
        assert_eq!(breaker.primary.calls(), 4);

        serve(&breaker, 10 * S - 1).await;
        assert_eq!(breaker.primary.calls(), 4);

        serve(&breaker, 10 * S).await;
        assert_eq!(breaker.primary.calls(), 5);
    }

    #[tokio::test]
    async fn successful_probe_closes_and_clears_fallback() {
        let breaker = CircuitBreaker::new(
            Scripted::new(&failing(3), Step::Allow),
            MemoryStorage::new(),
        );
        for _ in 0..3 {
            serve(&breaker, 0).await;
        }
        assert_eq!(breaker.fallback.len(), 1);

        assert!(call(&breaker, 5 * S).await.unwrap().is_ok());

        assert_eq!(breaker.fallback.len(), 0);
        serve(&breaker, 5 * S + 1).await;
        assert_eq!(breaker.primary.calls(), 5);
    }

    #[tokio::test]
    async fn events_in_order() {
        let (events, callback) = recorder();
        let steps = [
            Step::Fail("first"),
            Step::Fail("second"),
            Step::Fail("third"),
            Step::Fail("probe"),
        ];
        let breaker = CircuitBreaker::new(Scripted::new(&steps, Step::Allow), MemoryStorage::new())
            .on_state_change(callback);

        for _ in 0..3 {
            serve(&breaker, 0).await;
        }
        serve(&breaker, 5 * S).await;
        serve(&breaker, 10 * S).await;

        assert_eq!(
            *events.lock().unwrap(),
            [
                "PrimaryFailed: storage error: first",
                "PrimaryFailed: storage error: second",
                "PrimaryFailed: storage error: third",
                "Opened: storage error: third",
                "PrimaryFailed: storage error: probe",
                "Closed",
            ]
        );
    }

    #[test]
    #[should_panic(expected = "failure threshold must be greater than 0")]
    fn failure_threshold_zero_panics() {
        let _ = CircuitBreaker::new(Scripted::new(&[], Step::Allow), MemoryStorage::new())
            .failure_threshold(0);
    }

    #[test]
    #[should_panic(expected = "open_for must be non-zero")]
    fn open_for_zero_panics() {
        let _ = CircuitBreaker::new(Scripted::new(&[], Step::Allow), MemoryStorage::new())
            .open_for(Duration::ZERO);
    }

    #[tokio::test]
    async fn debug_shows_state_but_not_keys() {
        let breaker = CircuitBreaker::new(
            Scripted::new(&failing(1), Step::Allow),
            MemoryStorage::new(),
        );
        serve(&breaker, 0).await;

        let out = format!("{breaker:?}");

        assert!(out.contains("CircuitBreaker"), "{out}");
        assert!(out.contains("Closed"), "{out}");
        assert!(!out.contains("u1"), "{out}");
    }

    /// Opens a breaker whose primary failed 3 times at t=0.
    async fn opened(steps_after_open: &[Step], then: Step) -> Arc<CircuitBreaker<Scripted>> {
        let mut steps = failing(3);
        steps.extend_from_slice(steps_after_open);
        let breaker = Arc::new(CircuitBreaker::new(
            Scripted::new(&steps, then),
            MemoryStorage::new(),
        ));
        for _ in 0..3 {
            serve(&breaker, 0).await;
        }
        breaker
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn only_one_probe_with_concurrent_requests() {
        let breaker = opened(&[Step::HeldAllow], Step::Allow).await;

        let handles: Vec<_> = (0..50)
            .map(|_| {
                let breaker = Arc::clone(&breaker);
                tokio::spawn(async move { call(&breaker, 5 * S).await })
            })
            .collect();
        wait_until(|| handles.iter().filter(|h| h.is_finished()).count() == 49).await;

        // The 49 others were answered by the fallback while the probe waits.
        assert_eq!(breaker.primary.calls(), 4);
        breaker.primary.release(1);
        for handle in handles {
            assert!(handle.await.unwrap().is_ok());
        }
        assert_eq!(breaker.primary.calls(), 4);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stale_success_does_not_close() {
        let breaker = Arc::new(CircuitBreaker::new(
            Scripted::new(&[Step::HeldAllow], Step::Fail("redis down")),
            MemoryStorage::new(),
        ));
        let slow = {
            let breaker = Arc::clone(&breaker);
            tokio::spawn(async move { call(&breaker, 0).await })
        };
        wait_until(|| breaker.primary.calls() == 1).await;
        for _ in 0..3 {
            serve(&breaker, 0).await;
        }

        breaker.primary.release(1);
        assert!(slow.await.unwrap().is_ok());

        // Still open: the late success came from a call admitted before.
        serve(&breaker, 1).await;
        assert_eq!(breaker.primary.calls(), 4);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn in_flight_failures_emit_one_opened() {
        let (events, callback) = recorder();
        let breaker = Arc::new(
            CircuitBreaker::new(
                Scripted::new(&[Step::HeldFail("in flight"); 10], Step::Allow),
                MemoryStorage::new(),
            )
            .on_state_change(callback),
        );
        let handles: Vec<_> = (0..10)
            .map(|_| {
                let breaker = Arc::clone(&breaker);
                tokio::spawn(async move { call(&breaker, 0).await })
            })
            .collect();
        wait_until(|| breaker.primary.calls() == 10).await;

        breaker.primary.release(10);
        for handle in handles {
            assert!(handle.await.unwrap().is_ok());
        }

        let events = events.lock().unwrap().clone();
        let count = |name: &str| events.iter().filter(|e| e.starts_with(name)).count();
        assert_eq!(count("PrimaryFailed"), 10, "{events:?}");
        assert_eq!(count("Opened"), 1, "{events:?}");
        // The late failures did not move the probe time.
        serve(&breaker, 5 * S - 1).await;
        assert_eq!(breaker.primary.calls(), 10);
        serve(&breaker, 5 * S).await;
        assert_eq!(breaker.primary.calls(), 11);
    }

    #[tokio::test]
    async fn cancelled_probe_lets_the_next_request_probe() {
        let breaker = opened(&[Step::Never], Step::Allow).await;

        let cancelled = tokio::time::timeout(Duration::from_millis(1), call(&breaker, 5 * S)).await;
        assert!(cancelled.is_err(), "the probe should still be waiting");
        assert_eq!(breaker.primary.calls(), 4);

        serve(&breaker, 5 * S).await;
        assert_eq!(breaker.primary.calls(), 5);
    }

    #[tokio::test]
    async fn unpolled_probe_lets_the_next_request_probe() {
        let breaker = opened(&[], Step::Allow).await;
        let quota = quota();

        let probe = breaker.check_and_update(StorageKey::new("u1", "free"), &quota, 1, 5 * S);
        drop(probe);
        assert_eq!(breaker.primary.calls(), 3);

        serve(&breaker, 5 * S).await;
        assert_eq!(breaker.primary.calls(), 4);
    }

    #[tokio::test]
    async fn callback_runs_without_the_lock() {
        let cell: Arc<OnceLock<Weak<CircuitBreaker<Scripted>>>> = Arc::new(OnceLock::new());
        let lock_free = Arc::new(Mutex::new(Vec::new()));
        let callback = {
            let cell = Arc::clone(&cell);
            let lock_free = Arc::clone(&lock_free);
            move |_: &BreakerEvent<'_>| {
                let breaker = cell.get().and_then(Weak::upgrade).unwrap();
                let free = breaker.state.try_lock().is_ok();
                lock_free.lock().unwrap().push(free);
            }
        };
        let breaker = Arc::new(
            CircuitBreaker::new(
                Scripted::new(&failing(3), Step::Allow),
                MemoryStorage::new(),
            )
            .on_state_change(callback),
        );
        cell.set(Arc::downgrade(&breaker)).unwrap();

        for _ in 0..3 {
            serve(&breaker, 0).await;
        }
        serve(&breaker, 5 * S).await;

        // PrimaryFailed x3, Opened, Closed: the lock was free every time.
        assert_eq!(*lock_free.lock().unwrap(), [true; 5]);
    }

    #[tokio::test]
    async fn fallback_is_pruned_at_each_probe() {
        let quota = Quota::per_second(10);
        let breaker = CircuitBreaker::new(
            Scripted::new(&failing(4), Step::Allow),
            MemoryStorage::new(),
        );
        let check = |user: String, now: Nanos| {
            let breaker = &breaker;
            async move {
                breaker
                    .check_and_update(StorageKey::new(&user, "free"), &quota, 1, now)
                    .await
                    .unwrap()
            }
        };
        for _ in 0..3 {
            let _ = check("u1".to_owned(), 0).await;
        }
        for i in 0..100 {
            let _ = check(format!("user{i}"), 1).await;
        }
        assert_eq!(breaker.fallback.len(), 101);

        // Every entry expired 100 ms after its request; the failed probe
        // prunes them and then stores only its own.
        let _ = check("u1".to_owned(), 5 * S).await;

        assert_eq!(breaker.primary.calls(), 4);
        assert_eq!(breaker.fallback.len(), 1);
    }
}

#[cfg(all(test, feature = "redis"))]
mod redis_tests {
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use super::*;
    use crate::gcra::{RateLimitInfo, RateLimited};
    use crate::storage::redis::test_conns::{NeverAnswers, Refused};
    use crate::storage::redis::RedisStorage;

    async fn call<P: Storage>(
        breaker: &CircuitBreaker<P>,
        now: Nanos,
    ) -> Result<Result<RateLimitInfo, RateLimited>, StorageError> {
        let quota = Quota::per_minute(3);
        breaker
            .check_and_update(StorageKey::new("u1", "free"), &quota, 1, now)
            .await
    }

    #[tokio::test]
    async fn never_answering_redis_opens_and_skips_the_timeout() {
        let redis = RedisStorage::new(NeverAnswers).timeout(Duration::from_millis(50));
        let breaker = CircuitBreaker::new(redis, MemoryStorage::new());

        for _ in 0..3 {
            let started = Instant::now();
            assert!(call(&breaker, 0).await.is_ok());
            assert!(started.elapsed() >= Duration::from_millis(50));
        }

        let started = Instant::now();
        assert!(call(&breaker, 0).await.is_ok());
        assert!(
            started.elapsed() < Duration::from_millis(10),
            "an open breaker must not wait for Redis: {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn refused_redis_opens_the_breaker() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let breaker = CircuitBreaker::new(RedisStorage::new(Refused), MemoryStorage::new())
            .on_state_change(move |event| {
                sink.lock().unwrap().push(match event {
                    BreakerEvent::PrimaryFailed { .. } => "PrimaryFailed",
                    BreakerEvent::Opened { .. } => "Opened",
                    BreakerEvent::Closed => "Closed",
                });
            });

        for _ in 0..4 {
            assert!(call(&breaker, 0).await.is_ok());
        }

        assert_eq!(
            *events.lock().unwrap(),
            ["PrimaryFailed", "PrimaryFailed", "PrimaryFailed", "Opened"]
        );
    }
}
