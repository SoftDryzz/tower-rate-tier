use std::fmt;

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;

use crate::gcra::check_gcra;
use crate::quota::{Nanos, Quota};
use crate::storage::{Storage, StorageFuture, StorageKey};

/// In-memory rate limit storage backed by `DashMap`.
///
/// Thread-safe with per-shard locking. Suitable for single-server deployments.
pub struct MemoryStorage {
    /// Maps (user_id, tier) -> TAT (Theoretical Arrival Time in nanos)
    state: DashMap<(String, String), Nanos>,
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

impl fmt::Debug for MemoryStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Keys are user identifiers (often API keys), so only the count is shown.
        f.debug_struct("MemoryStorage")
            .field("entries", &self.state.len())
            .finish()
    }
}

impl Storage for MemoryStorage {
    fn check_and_update<'a>(
        &'a self,
        key: StorageKey<'a>,
        quota: &'a Quota,
        cost: u32,
        now: Nanos,
    ) -> StorageFuture<'a> {
        let ei = quota.emission_interval_nanos();
        let bo = quota.burst_offset_nanos();

        // The entry holds the shard's write lock from the read to the write, so
        // concurrent requests for the same key cannot both see the old TAT.
        // A vacant entry is the first request (None), never Some(now).
        let entry = (key.user_id.to_owned(), key.tier.to_owned());
        let result = match self.state.entry(entry) {
            Entry::Occupied(mut slot) => {
                check_gcra(Some(*slot.get()), now, ei, bo, cost).map(|(new_tat, info)| {
                    slot.insert(new_tat);
                    info
                })
            }
            Entry::Vacant(slot) => check_gcra(None, now, ei, bo, cost).map(|(new_tat, info)| {
                slot.insert(new_tat);
                info
            }),
        };

        Box::pin(std::future::ready(Ok(result)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_shows_entry_count_but_not_keys() {
        let storage = MemoryStorage::new();
        storage
            .state
            .insert(("secret-api-key".to_owned(), "free".to_owned()), 1);

        let out = format!("{:?}", storage);

        assert!(out.contains("entries: 1"), "{out}");
        assert!(!out.contains("secret-api-key"), "{out}");
    }
}
