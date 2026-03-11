use async_trait::async_trait;
use dashmap::DashMap;

use crate::gcra::{check_gcra, RateLimitInfo, RateLimited};
use crate::quota::{Nanos, Quota};
use crate::storage::{Storage, StorageError};

/// In-memory rate limit storage backed by `DashMap`.
///
/// Thread-safe with per-shard locking. Suitable for single-server deployments.
pub struct MemoryStorage {
    /// Maps "user_key" -> TAT (Theoretical Arrival Time in nanos)
    state: DashMap<String, Nanos>,
}

impl MemoryStorage {
    /// Creates a new empty `MemoryStorage`.
    pub fn new() -> Self {
        Self {
            state: DashMap::new(),
        }
    }

    /// Returns the number of tracked keys (useful for testing GC).
    pub fn len(&self) -> usize {
        self.state.len()
    }

    /// Returns `true` if no keys are tracked.
    pub fn is_empty(&self) -> bool {
        self.state.is_empty()
    }

    /// Remove all entries where the TAT has expired (TAT < now).
    ///
    /// Called by the garbage collector.
    pub fn retain_active(&self, now: Nanos) {
        self.state.retain(|_, tat| *tat >= now);
    }
}

impl Default for MemoryStorage {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Storage for MemoryStorage {
    async fn check_and_update(
        &self,
        key: &str,
        quota: &Quota,
        cost: u32,
        now: Nanos,
    ) -> Result<Result<RateLimitInfo, RateLimited>, StorageError> {
        let ei = quota.emission_interval_nanos();
        let bo = quota.burst_offset_nanos();

        let mut entry = self.state.entry(key.to_owned()).or_insert(now);
        let current_tat = *entry.value();

        Ok(match check_gcra(Some(current_tat), now, ei, bo, cost) {
            Ok((new_tat, info)) => {
                *entry.value_mut() = new_tat;
                Ok(info)
            }
            Err(limited) => Err(limited),
        })
    }
}
