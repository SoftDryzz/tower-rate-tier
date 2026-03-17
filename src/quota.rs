use std::time::Duration;

/// Nanoseconds since an arbitrary epoch. Used internally for GCRA calculations.
pub type Nanos = u64;

/// A rate limit quota defining the maximum number of requests allowed within a time window.
///
/// # Examples
///
/// ```
/// use tower_rate_tier::Quota;
///
/// let free_tier = Quota::per_hour(100);
/// let pro_tier = Quota::per_minute(50);
/// let unlimited = Quota::unlimited();
///
/// assert!(!free_tier.is_unlimited());
/// assert!(unlimited.is_unlimited());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quota {
    max_burst: u32,
    window: Duration,
}

impl Quota {
    /// Create a quota allowing `count` requests per second.
    pub fn per_second(count: u32) -> Self {
        assert!(count > 0, "quota count must be greater than 0");
        Self {
            max_burst: count,
            window: Duration::from_secs(1),
        }
    }

    /// Create a quota allowing `count` requests per minute.
    pub fn per_minute(count: u32) -> Self {
        assert!(count > 0, "quota count must be greater than 0");
        Self {
            max_burst: count,
            window: Duration::from_secs(60),
        }
    }

    /// Create a quota allowing `count` requests per hour.
    pub fn per_hour(count: u32) -> Self {
        assert!(count > 0, "quota count must be greater than 0");
        Self {
            max_burst: count,
            window: Duration::from_secs(3600),
        }
    }

    /// Create an unlimited quota that bypasses rate limiting entirely.
    pub fn unlimited() -> Self {
        Self {
            max_burst: 0,
            window: Duration::ZERO,
        }
    }

    /// Returns `true` if this quota is unlimited.
    pub fn is_unlimited(&self) -> bool {
        self.max_burst == 0
    }

    /// The maximum number of requests allowed in the window.
    pub fn max_burst(&self) -> u32 {
        self.max_burst
    }

    /// The time interval between each allowed request (window / max_burst).
    ///
    /// Returns `Duration::ZERO` for unlimited quotas.
    pub fn replenish_interval(&self) -> Duration {
        if self.is_unlimited() {
            return Duration::ZERO;
        }
        self.window / self.max_burst
    }

    /// The total window duration.
    pub fn window(&self) -> Duration {
        self.window
    }

    /// The emission interval in nanoseconds (for GCRA calculations).
    ///
    /// Saturates to `u64::MAX` if the interval exceeds ~584 years.
    pub fn emission_interval_nanos(&self) -> Nanos {
        let nanos_u128 = self.replenish_interval().as_nanos();
        if nanos_u128 > u64::MAX as u128 {
            u64::MAX
        } else {
            nanos_u128 as Nanos
        }
    }

    /// The burst offset in nanoseconds: emission_interval * max_burst.
    ///
    /// Uses saturating multiplication to prevent overflow on large quotas.
    pub fn burst_offset_nanos(&self) -> Nanos {
        self.emission_interval_nanos().saturating_mul(self.max_burst as Nanos)
    }
}
