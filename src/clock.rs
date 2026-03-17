use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::quota::Nanos;

/// Abstraction over time for testability.
///
/// Implementations must be thread-safe (`Send + Sync`).
pub trait Clock: Send + Sync + 'static {
    /// Returns the current time in nanoseconds since an arbitrary epoch.
    fn now(&self) -> Nanos;

    /// Returns the Unix timestamp offset in nanoseconds.
    ///
    /// This offset, when added to a value from [`now()`](Clock::now), produces
    /// a nanosecond-precision Unix timestamp. Used by the response layer to emit
    /// `X-RateLimit-Reset` as a standard Unix timestamp.
    ///
    /// Default returns `0`, which is suitable for testing with [`FakeClock`].
    fn unix_offset_nanos(&self) -> u64 {
        0
    }
}

/// Real clock backed by `tokio::time::Instant`.
///
/// Uses a fixed epoch (created at construction time) and measures elapsed
/// nanoseconds from that point. The Unix offset is captured at construction
/// so that internal timestamps can be converted to Unix timestamps for
/// HTTP headers.
pub struct SystemClock {
    epoch: tokio::time::Instant,
    unix_offset: u64,
}

impl SystemClock {
    /// Creates a new `SystemClock` with the current instant as its epoch.
    ///
    /// Captures the current Unix time so that elapsed values can be converted
    /// to Unix timestamps via [`unix_offset_nanos()`](Clock::unix_offset_nanos).
    pub fn new() -> Self {
        let unix_nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time before Unix epoch")
            .as_nanos() as u64;
        Self {
            epoch: tokio::time::Instant::now(),
            unix_offset: unix_nanos,
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

    fn unix_offset_nanos(&self) -> u64 {
        self.unix_offset
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
#[derive(Clone)]
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
        let _ = self.nanos.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
            Some(current.saturating_add(delta))
        });
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
            .build()
            .unwrap();
        rt.block_on(async {
            let clock = SystemClock::new();
            let t0 = clock.now();
            tokio::time::sleep(Duration::from_millis(10)).await;
            let t1 = clock.now();
            assert!(t1 > t0);
        });
    }
}
