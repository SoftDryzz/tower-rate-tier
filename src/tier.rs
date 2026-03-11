use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::clock::{Clock, SystemClock};
use crate::gc::GcHandle;
use crate::gcra::{RateLimitInfo, RateLimited};
use crate::on_missing::OnMissing;
use crate::quota::Quota;
use crate::storage::memory::MemoryStorage;
use crate::storage::Storage;

/// Tier-based rate limiter configuration.
///
/// Maps tier names to quotas and provides a programmatic `check()` API.
///
/// # Examples
///
/// ```
/// # #[tokio::main]
/// # async fn main() {
/// use tower_rate_tier::{RateTier, Quota};
///
/// let limiter = RateTier::builder()
///     .tier("free", Quota::per_hour(100))
///     .tier("pro", Quota::per_hour(5_000))
///     .tier("enterprise", Quota::unlimited())
///     .default_tier("free")
///     .build();
/// # }
/// ```
pub struct RateTier {
    tiers: HashMap<String, Quota>,
    default_tier: Option<String>,
    on_missing: OnMissing,
    storage: Arc<dyn Storage>,
    clock: Arc<dyn Clock>,
    _gc: Option<GcHandle>,
}

impl RateTier {
    pub fn builder() -> RateTierBuilder {
        RateTierBuilder::default()
    }

    /// Look up the quota for a tier name.
    pub fn get_quota(&self, tier_name: &str) -> Option<&Quota> {
        self.tiers.get(tier_name)
    }

    /// Get the on_missing policy.
    pub fn on_missing(&self) -> &OnMissing {
        &self.on_missing
    }

    /// Get the default tier name, if set.
    pub fn default_tier(&self) -> Option<&str> {
        self.default_tier.as_deref()
    }

    /// Get a reference to the clock.
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    /// Programmatic rate limit check (non-HTTP).
    ///
    /// Returns `Ok(info)` if allowed, `Err(limited)` if denied.
    /// Unlimited tiers always return `Ok` without touching storage.
    pub async fn check(
        &self,
        user_id: &str,
        tier_name: &str,
        cost: u32,
    ) -> Result<RateLimitInfo, RateLimited> {
        let quota = self
            .tiers
            .get(tier_name)
            .unwrap_or_else(|| panic!("unknown tier: {}", tier_name));

        if quota.is_unlimited() {
            return Ok(RateLimitInfo {
                limit: 0,
                remaining: 0,
                reset_at: 0,
            });
        }

        let now = self.clock.now();
        self.storage.check_and_update(user_id, quota, cost, now).await
    }
}

/// Builder for `RateTier`.
pub struct RateTierBuilder {
    tiers: HashMap<String, Quota>,
    default_tier: Option<String>,
    on_missing: OnMissing,
    clock: Option<Arc<dyn Clock>>,
    storage: Option<Arc<MemoryStorage>>,
    gc_interval: Duration,
}

impl Default for RateTierBuilder {
    fn default() -> Self {
        Self {
            tiers: HashMap::new(),
            default_tier: None,
            on_missing: OnMissing::default(),
            clock: None,
            storage: None,
            gc_interval: Duration::from_secs(60),
        }
    }
}

impl RateTierBuilder {
    /// Define a tier with the given name and quota.
    pub fn tier(mut self, name: impl Into<String>, quota: Quota) -> Self {
        self.tiers.insert(name.into(), quota);
        self
    }

    /// Set the default tier name (used when `OnMissing::UseDefault`).
    pub fn default_tier(mut self, name: impl Into<String>) -> Self {
        self.default_tier = Some(name.into());
        self
    }

    /// Set the behavior when the identifier returns `None`.
    pub fn on_missing(mut self, policy: OnMissing) -> Self {
        self.on_missing = policy;
        self
    }

    /// Set a custom clock (useful for testing with `FakeClock`).
    pub fn clock(mut self, clock: impl Clock) -> Self {
        self.clock = Some(Arc::new(clock));
        self
    }

    /// Set the garbage collection interval for expired entries.
    ///
    /// Default: 60 seconds.
    pub fn gc_interval(mut self, interval: Duration) -> Self {
        self.gc_interval = interval;
        self
    }

    /// Build the `RateTier` configuration.
    ///
    /// # Panics
    ///
    /// - If no tiers are defined.
    /// - If `default_tier` references a non-existent tier.
    pub fn build(self) -> RateTier {
        assert!(!self.tiers.is_empty(), "at least one tier must be defined");

        if let Some(ref default) = self.default_tier {
            assert!(
                self.tiers.contains_key(default),
                "default tier '{}' does not exist in defined tiers",
                default
            );
        }

        let clock: Arc<dyn Clock> = self.clock.unwrap_or_else(|| Arc::new(SystemClock::new()));
        let storage = self
            .storage
            .unwrap_or_else(|| Arc::new(MemoryStorage::new()));

        let gc = GcHandle::spawn(storage.clone(), clock.clone(), self.gc_interval);

        RateTier {
            tiers: self.tiers,
            default_tier: self.default_tier,
            on_missing: self.on_missing,
            storage,
            clock,
            _gc: Some(gc),
        }
    }
}
