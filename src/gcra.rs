use std::time::Duration;

use crate::quota::Nanos;

/// Information about the current rate limit state after a successful check.
///
/// Times are relative to the moment of the check, so they do not depend on
/// which clock the storage backend used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitInfo {
    /// Maximum number of requests allowed in the window.
    pub limit: u32,
    /// Remaining requests before rate limiting kicks in.
    pub remaining: u32,
    /// Time until the quota fully replenishes.
    pub reset_after: Duration,
}

/// Returned when a request is denied due to rate limiting.
///
/// Times are relative to the moment of the check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimited {
    /// Maximum number of requests allowed in the window.
    pub limit: u32,
    /// How long the caller should wait before retrying.
    pub retry_after: Duration,
    /// Time until the quota fully replenishes. The rejected request is not
    /// counted, since it consumed nothing.
    pub reset_after: Duration,
}

/// Perform a GCRA (Generic Cell Rate Algorithm) check.
///
/// # Arguments
///
/// * `tat` - Previous Theoretical Arrival Time for this key, or `None` for first request.
/// * `now` - Current time in nanoseconds.
/// * `emission_interval` - Time between allowed cells (window / max_burst).
/// * `burst_offset` - Maximum burst window (emission_interval * max_burst).
/// * `cost` - Number of cells this request consumes. A cost of `0` means the
///   request is free (no quota consumed) and is always allowed.
///
/// # Returns
///
/// * `Ok((new_tat, info))` - Request is allowed. `new_tat` should be stored.
/// * `Err(limited)` - Request is denied.
///
/// # Panics
///
/// Panics if `emission_interval` is 0. Every [`Quota`](crate::Quota) built by
/// its constructors (other than [`Quota::unlimited`](crate::Quota::unlimited),
/// which is never checked) has a non-zero interval.
pub fn check_gcra(
    tat: Option<Nanos>,
    now: Nanos,
    emission_interval: Nanos,
    burst_offset: Nanos,
    cost: u32,
) -> Result<(Nanos, RateLimitInfo), RateLimited> {
    let limit = (burst_offset / emission_interval) as u32;
    // An expired TAT behaves like no TAT at all.
    let current_tat = tat.unwrap_or(now).max(now);
    let increment = emission_interval.saturating_mul(cost as Nanos);
    let new_tat = current_tat.saturating_add(increment);
    let allow_at = new_tat.saturating_sub(burst_offset);

    if allow_at > now {
        return Err(RateLimited {
            limit,
            retry_after: Duration::from_nanos(allow_at - now),
            reset_after: Duration::from_nanos(current_tat - now),
        });
    }

    let diff = burst_offset.saturating_sub(new_tat - now);
    let remaining = (diff / emission_interval) as u32;

    Ok((
        new_tat,
        RateLimitInfo {
            limit,
            remaining,
            reset_after: Duration::from_nanos(new_tat - now),
        },
    ))
}
