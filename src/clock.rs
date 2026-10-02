use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::quota::Nanos;

/// Abstraction over time for testability.
///
/// Implementations must be thread-safe (`Send + Sync`).
pub trait Clock: Send + Sync + 'static {
    /// Returns the current time in nanoseconds since an arbitrary epoch.
    ///
    /// Only differences between values matter. The epoch is local to the
    /// clock, so storage shared between processes must not compare values
    /// from different clocks; use the backend's own time instead.
    fn now(&self) -> Nanos;
}

/// Real clock backed by `tokio::time::Instant`.
///
/// Monotonic: measures nanoseconds elapsed since the clock was created, so it
/// never jumps when the system's wall clock is adjusted.
#[derive(Debug)]
pub struct SystemClock {
    epoch: tokio::time::Instant,
}

impl SystemClock {
    /// Creates a new `SystemClock` with the current instant as its epoch.
    pub fn new() -> Self {
        Self {
            epoch: tokio::time::Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Nanos {
        self.epoch.elapsed().as_nanos() as Nanos
    }
}

/// Fake clock for deterministic testing.
///
/// Starts at time 0. Use [`advance`](FakeClock::advance) to move time forward.
///
/// # Examples
///
/// ```
/// use tower_rate_tier::clock::FakeClock;
/// use tower_rate_tier::clock::Clock;
/// use std::time::Duration;
///
/// let clock = FakeClock::new();
/// assert_eq!(clock.now(), 0);
///
/// clock.advance(Duration::from_secs(60));
/// assert_eq!(clock.now(), 60_000_000_000);
/// ```
#[derive(Clone, Debug)]
pub struct FakeClock {
    nanos: Arc<AtomicU64>,
}

impl FakeClock {
    /// Creates a new `FakeClock` starting at time zero.
    pub fn new() -> Self {
        Self {
            nanos: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Advance the clock by the given duration.
    ///
    /// Saturates at `u64::MAX` if the total would overflow.
    pub fn advance(&self, duration: Duration) {
        let delta = duration.as_nanos().min(u64::MAX as u128) as u64;
        // A compare-exchange loop: `fetch_update` is deprecated since Rust
        // 1.99 and its replacement does not exist on the 1.75 MSRV.
        let mut current = self.nanos.load(Ordering::SeqCst);
        loop {
            let next = current.saturating_add(delta);
            match self.nanos.compare_exchange_weak(
                current,
                next,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    /// Set the clock to an absolute nanosecond value.
    pub fn set(&self, nanos: Nanos) {
        self.nanos.store(nanos, Ordering::SeqCst);
    }
}

impl Default for FakeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Nanos {
        self.nanos.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_clock_starts_at_zero() {
        let clock = FakeClock::new();
        assert_eq!(clock.now(), 0);
    }

    #[test]
    fn fake_clock_advance() {
        let clock = FakeClock::new();
        clock.advance(Duration::from_secs(60));
        assert_eq!(clock.now(), 60_000_000_000);
    }

    #[test]
    fn fake_clock_advance_accumulates() {
        let clock = FakeClock::new();
        clock.advance(Duration::from_secs(30));
        clock.advance(Duration::from_secs(30));
        assert_eq!(clock.now(), 60_000_000_000);
    }

    #[test]
    fn fake_clock_set() {
        let clock = FakeClock::new();
        clock.set(123_456_789);
        assert_eq!(clock.now(), 123_456_789);
    }

    #[test]
    fn fake_clock_clone_shares_state() {
        let clock1 = FakeClock::new();
        let clock2 = clock1.clone();
        clock1.advance(Duration::from_secs(10));
        assert_eq!(clock2.now(), 10_000_000_000);
    }

    #[test]
    fn system_clock_monotonic() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap();
        rt.block_on(async {
            let clock = SystemClock::new();
            let t0 = clock.now();
            tokio::time::advance(Duration::from_millis(10)).await;
            let t1 = clock.now();
            assert_eq!(t1 - t0, 10_000_000);
        });
    }
}
