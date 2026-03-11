pub mod memory;

use std::fmt;

use async_trait::async_trait;

use crate::gcra::{RateLimitInfo, RateLimited};
use crate::quota::{Nanos, Quota};

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
/// The outer `Result` represents storage-level errors (e.g., Redis down).
/// The inner `Result` represents the GCRA decision (allowed vs rate limited).
#[async_trait]
pub trait Storage: Send + Sync + 'static {
    /// Check rate limit and update state atomically.
    ///
    /// - `Ok(Ok(info))` — request allowed
    /// - `Ok(Err(limited))` — request rate limited
    /// - `Err(StorageError)` — storage backend failure
    async fn check_and_update(
        &self,
        key: &str,
        quota: &Quota,
        cost: u32,
        now: Nanos,
    ) -> Result<Result<RateLimitInfo, RateLimited>, StorageError>;
}
