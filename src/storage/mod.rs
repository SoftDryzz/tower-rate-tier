pub mod memory;

use async_trait::async_trait;

use crate::gcra::{RateLimitInfo, RateLimited};
use crate::quota::{Nanos, Quota};

/// Trait for rate limit state persistence backends.
///
/// Implementations must atomically check the current state and update it.
#[async_trait]
pub trait Storage: Send + Sync + 'static {
    /// Check rate limit and update state atomically.
    ///
    /// Returns `Ok(info)` if the request is allowed, `Err(limited)` if denied.
    async fn check_and_update(
        &self,
        key: &str,
        quota: &Quota,
        cost: u32,
        now: Nanos,
    ) -> Result<RateLimitInfo, RateLimited>;
}
