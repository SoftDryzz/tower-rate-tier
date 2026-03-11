use std::time::Duration;

use crate::quota::Nanos;

/// Information about the current rate limit state after a successful check.
#[derive(Debug, Clone, Copy)]
pub struct RateLimitInfo {
    /// Maximum number of requests allowed in the window.
    pub limit: u32,
    /// Remaining requests before rate limiting kicks in.
    pub remaining: u32,
    /// Absolute time (in nanos) when the quota fully replenishes.
    pub reset_at: Nanos,
}

/// Returned when a request is denied due to rate limiting.
#[derive(Debug, Clone, Copy)]
pub struct RateLimited {
    /// Maximum number of requests allowed in the window.
    pub limit: u32,
    /// How long the caller should wait before retrying.
    pub retry_after: Duration,
    /// Absolute time (in nanos) when the quota fully replenishes.
    pub reset_at: Nanos,
}

/// Perform a GCRA (Generic Cell Rate Algorithm) check.
///
/// # Arguments
///
/// * `tat` - Previous Theoretical Arrival Time for this key, or `None` for first request.
/// * `now` - Current time in nanoseconds.
/// * `emission_interval` - Time between allowed cells (window / max_burst).
/// * `burst_offset` - Maximum burst window (emission_interval * max_burst).
/// * `cost` - Number of cells this request consumes.
///
/// # Returns
///
/// * `Ok((new_tat, info))` - Request is allowed. `new_tat` should be stored.
/// * `Err(limited)` - Request is denied.
pub fn check_gcra(
    tat: Option<Nanos>,
    now: Nanos,
    emission_interval: Nanos,
    burst_offset: Nanos,
    cost: u32,
) -> Result<(Nanos, RateLimitInfo), RateLimited> {
    let limit = (burst_offset / emission_interval) as u32;
    let tat = tat.unwrap_or(now);
    let increment = emission_interval.saturating_mul(cost as Nanos);
    let new_tat = tat.max(now) + increment;
    let allow_at = new_tat.saturating_sub(burst_offset);

    if allow_at > now {
        let retry_after_nanos = allow_at - now;
        return Err(RateLimited {
            limit,
            retry_after: Duration::from_nanos(retry_after_nanos),
            reset_at: new_tat,
        });
    }

    let diff = burst_offset.saturating_sub(new_tat.saturating_sub(now));
    let remaining = (diff / emission_interval) as u32;

    Ok((
        new_tat,
        RateLimitInfo {
            limit,
            remaining,
            reset_at: new_tat,
        },
    ))
}
