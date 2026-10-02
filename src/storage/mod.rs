/// In-memory storage backend using `DashMap`.
pub mod memory;

use std::fmt;
use std::future::Future;
use std::pin::Pin;

use crate::gcra::{RateLimitInfo, RateLimited};
use crate::quota::{Nanos, Quota};

/// The future type returned by [`Storage::check_and_update`].
pub type StorageFuture<'a> = Pin<
    Box<dyn Future<Output = Result<Result<RateLimitInfo, RateLimited>, StorageError>> + Send + 'a>,
>;

/// Identifies one rate-limit bucket: a user within a tier.
///
/// The parts stay separate so each backend can choose an encoding in which
/// distinct pairs stay distinct, even when a user id or tier name contains
/// the backend's separator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StorageKey<'a> {
    /// The user identifier returned by the identifier.
    pub user_id: &'a str,
    /// The tier whose quota applies to this bucket.
    pub tier: &'a str,
}

impl<'a> StorageKey<'a> {
    /// Creates the key for `user_id` within `tier`.
    pub fn new(user_id: &'a str, tier: &'a str) -> Self {
        Self { user_id, tier }
    }
}

/// Error returned when the storage backend fails (e.g., Redis connection lost).
#[derive(Debug)]
pub struct StorageError(pub Box<dyn std::error::Error + Send + Sync>);

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "storage error: {}", self.0)
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

/// Trait for rate limit state persistence backends.
///
/// Implementations must atomically check the current state and update it.
///
/// Each distinct [`StorageKey`] must map to its own state. A backend that
/// joins the parts into one string must make that encoding injective, for
/// example by length-prefixing or escaping the parts: a plain
/// `format!("{user}:{tier}")` makes user `a:b` in tier `c` share state with
/// user `a` in tier `b:c`.
///
/// The outer `Result` represents storage-level errors (e.g., Redis down).
/// The inner `Result` represents the GCRA decision (allowed vs rate limited).
pub trait Storage: Send + Sync + 'static {
    /// Check rate limit and update state atomically.
    ///
    /// `now` comes from the rate limiter's [`Clock`](crate::clock::Clock),
    /// whose epoch is local to one process. A backend shared between
    /// processes should use its own time source instead.
    ///
    /// - `Ok(Ok(info))` — request allowed
    /// - `Ok(Err(limited))` — request rate limited
    /// - `Err(StorageError)` — storage backend failure
    fn check_and_update<'a>(
        &'a self,
        key: StorageKey<'a>,
        quota: &'a Quota,
        cost: u32,
        now: Nanos,
    ) -> StorageFuture<'a>;
}
