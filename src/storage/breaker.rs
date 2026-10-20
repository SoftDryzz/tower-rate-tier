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

        Box::pin(async move {
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

/// Converts `duration` to nanoseconds, saturating at `u64::MAX` (~584 years).
fn duration_nanos(duration: Duration) -> Nanos {
    Nanos::try_from(duration.as_nanos()).unwrap_or(Nanos::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

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
    }

    /// A primary storage that answers from a script and counts its calls.
    #[derive(Debug)]
    struct Scripted {
        calls: AtomicUsize,
        script: Mutex<VecDeque<Step>>,
        then: Step,
    }

    impl Scripted {
        /// Answers `steps` in order, then `then` forever.
        fn new(steps: &[Step], then: Step) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                script: Mutex::new(steps.iter().copied().collect()),
                then,
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
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
            self.calls.fetch_add(1, Ordering::SeqCst);
            let step = self.script.lock().unwrap().pop_front().unwrap_or(self.then);
            let result = match step {
                Step::Allow => Ok(Ok(RateLimitInfo {
                    limit: 3,
                    remaining: 2,
                    reset_after: Duration::from_secs(20),
                })),
                Step::Limit => Ok(Err(RateLimited {
                    limit: 3,
                    retry_after: Duration::from_secs(20),
                    reset_after: Duration::from_secs(60),
                })),
                Step::Fail(message) => Err(StorageError(Box::from(message))),
            };
            Box::pin(std::future::ready(result))
        }
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
}
