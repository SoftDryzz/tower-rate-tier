use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::clock::{Clock, SystemClock};
use crate::gc::GcHandle;
use crate::gcra::{RateLimitInfo, RateLimited};
use crate::on_missing::OnMissing;
use crate::quota::{Nanos, Quota};
use crate::storage::memory::MemoryStorage;
use crate::storage::{Storage, StorageError, StorageFuture};

/// Error returned by [`RateTier::check()`].
///
/// Distinguishes between an unknown tier name (a configuration/logic error)
/// and a storage backend failure (e.g., Redis connection lost).
#[derive(Debug)]
pub enum CheckError {
    /// The tier name passed to `check()` does not exist in the configured tiers.
    UnknownTier(String),
    /// The storage backend failed during the rate limit check.
    Storage(StorageError),
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CheckError::UnknownTier(name) => write!(f, "unknown tier: {}", name),
            CheckError::Storage(err) => write!(f, "{}", err),
        }
    }
}

impl std::error::Error for CheckError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CheckError::UnknownTier(_) => None,
            CheckError::Storage(err) => Some(err),
        }
    }
}

impl From<StorageError> for CheckError {
    fn from(err: StorageError) -> Self {
        CheckError::Storage(err)
    }
}

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
    gc: Option<LazyGc>,
}

/// Garbage collector for the built-in `MemoryStorage`.
///
/// The task is spawned by the first check rather than by `build()`, so a
/// `RateTier` can be built outside a Tokio runtime.
struct LazyGc {
    storage: Arc<MemoryStorage>,
    interval: Duration,
    handle: OnceLock<GcHandle>,
}

impl fmt::Debug for RateTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RateTier")
            .field("tiers", &self.tiers)
            .field("default_tier", &self.default_tier)
            .field("on_missing", &self.on_missing)
            .field("gc_enabled", &self.gc.is_some())
            .finish_non_exhaustive()
    }
}

impl RateTier {
    /// Returns a new [`RateTierBuilder`] for configuring tiers and quotas.
    pub fn builder() -> RateTierBuilder {
        RateTierBuilder::default()
    }

    /// Look up the quota for a tier name.
    pub fn get_quota(&self, tier_name: &str) -> Option<&Quota> {
        self.tiers.get(tier_name)
    }

    /// Get the on_missing policy.
    pub fn on_missing(&self) -> OnMissing {
        self.on_missing
    }

    /// Get the default tier name, if set.
    pub fn default_tier(&self) -> Option<&str> {
        self.default_tier.as_deref()
    }

    /// Get a reference to the clock.
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    /// Get a reference to the storage backend.
    pub fn storage(&self) -> &dyn Storage {
        self.storage.as_ref()
    }

    /// Programmatic rate limit check (non-HTTP).
    ///
    /// Returns `Ok(Ok(info))` if allowed, `Ok(Err(limited))` if denied,
    /// or `Err(CheckError)` if the tier is unknown or the storage backend fails.
    /// Unlimited tiers always return `Ok(Ok(...))` without touching storage.
    ///
    /// # Errors
    ///
    /// - [`CheckError::UnknownTier`] — the tier name does not exist in the configured tiers.
    /// - [`CheckError::Storage`] — the storage backend failed (e.g., Redis connection lost).
    pub async fn check(
        &self,
        user_id: &str,
        tier_name: &str,
        cost: u32,
    ) -> Result<Result<RateLimitInfo, RateLimited>, CheckError> {
        let quota = self
            .tiers
            .get(tier_name)
            .ok_or_else(|| CheckError::UnknownTier(tier_name.to_string()))?;

        if quota.is_unlimited() {
            return Ok(Ok(RateLimitInfo {
                limit: 0,
                remaining: 0,
                reset_at: 0,
            }));
        }

        let now = self.clock.now();
        let storage_key = format!("{}:{}", user_id, tier_name);
        Ok(self.check_storage(&storage_key, quota, cost, now).await?)
    }

    /// Run a storage check, spawning the GC task first if it has not started.
    ///
    /// Must be called from inside a Tokio runtime, like any Tower service.
    pub(crate) fn check_storage(
        &self,
        key: &str,
        quota: &Quota,
        cost: u32,
        now: Nanos,
    ) -> StorageFuture<'_> {
        if let Some(gc) = &self.gc {
            gc.handle.get_or_init(|| {
                GcHandle::spawn(gc.storage.clone(), self.clock.clone(), gc.interval)
            });
        }
        self.storage.check_and_update(key, quota, cost, now)
    }
}

/// Builder for `RateTier`.
pub struct RateTierBuilder {
    tiers: HashMap<String, Quota>,
    default_tier: Option<String>,
    on_missing: OnMissing,
    clock: Option<Arc<dyn Clock>>,
    storage: Option<Arc<dyn Storage>>,
    gc_interval: Duration,
    gc_enabled: bool,
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
            gc_enabled: true,
        }
    }
}

impl fmt::Debug for RateTierBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RateTierBuilder")
            .field("tiers", &self.tiers)
            .field("default_tier", &self.default_tier)
            .field("on_missing", &self.on_missing)
            .field("custom_clock", &self.clock.is_some())
            .field("custom_storage", &self.storage.is_some())
            .field("gc_interval", &self.gc_interval)
            .field("gc_enabled", &self.gc_enabled)
            .finish()
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

    /// Set a custom storage backend.
    ///
    /// When a custom storage is provided, garbage collection is automatically
    /// disabled (custom backends are expected to manage their own expiry,
    /// e.g., Redis TTL). Use [`gc_interval`](Self::gc_interval) to re-enable
    /// GC if your custom backend needs it.
    pub fn storage(mut self, storage: Arc<dyn Storage>) -> Self {
        self.storage = Some(storage);
        self.gc_enabled = false;
        self
    }

    /// Set the garbage collection interval for expired entries.
    ///
    /// Default: 60 seconds. Only applies when using the built-in
    /// `MemoryStorage` backend.
    pub fn gc_interval(mut self, interval: Duration) -> Self {
        self.gc_interval = interval;
        self.gc_enabled = true;
        self
    }

    /// Disable automatic garbage collection of expired entries.
    ///
    /// Useful when using a storage backend that manages its own expiry
    /// (e.g., Redis with TTL).
    pub fn disable_gc(mut self) -> Self {
        self.gc_enabled = false;
        self
    }

    /// Build the `RateTier` configuration.
    ///
    /// Building does not need a Tokio runtime: the garbage collector for the
    /// built-in storage is spawned by the first check, which runs inside one.
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

        // Use provided storage or default to MemoryStorage.
        // GC only runs for the default MemoryStorage and when gc_enabled is true.
        let (storage, gc): (Arc<dyn Storage>, Option<LazyGc>) = match self.storage {
            Some(custom) => (custom, None),
            None => {
                let memory = Arc::new(MemoryStorage::new());
                let gc = self.gc_enabled.then(|| LazyGc {
                    storage: memory.clone(),
                    interval: self.gc_interval,
                    handle: OnceLock::new(),
                });
                (memory, gc)
            }
        };

        RateTier {
            tiers: self.tiers,
            default_tier: self.default_tier,
            on_missing: self.on_missing,
            storage,
            clock,
            gc,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FakeClock;

    #[tokio::test(start_paused = true)]
    async fn gc_starts_on_first_check_and_cleans_expired_entries() {
        let clock = FakeClock::new();
        let limiter = RateTier::builder()
            .tier("free", Quota::per_second(1))
            .clock(clock.clone())
            .gc_interval(Duration::from_secs(1))
            .build();
        let gc = limiter.gc.as_ref().expect("built-in storage enables GC");
        assert!(
            gc.handle.get().is_none(),
            "GC must not start before a check"
        );

        limiter.check("u1", "free", 1).await.unwrap().unwrap();
        assert!(gc.handle.get().is_some(), "first check starts the GC");
        assert_eq!(gc.storage.len(), 1);

        clock.advance(Duration::from_secs(10));
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(gc.storage.len(), 0, "expired entry must be collected");
    }
}
