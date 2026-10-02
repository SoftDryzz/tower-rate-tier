//! Shared rate-limit check logic used by both `TierLimitService` and
//! `BufferedTierLimitService`.

#![allow(clippy::result_large_err)]

use std::time::SystemTime;

use http::Response;

use crate::event::LimitEvent;
use crate::gcra::RateLimitInfo;
use crate::identifier::TierIdentity;
use crate::layer::Settings;
use crate::on_missing::OnMissing;
use crate::on_storage_error::OnStorageError;
use crate::quota::Quota;
use crate::response;
use crate::tier::RateTier;

/// The outcome of a rate-limit check.
pub(crate) enum CheckOutcome {
    /// Request is allowed. Contains rate limit info for response headers.
    Allow(RateLimitInfo),
    /// Request should pass through without any rate limiting (no headers).
    PassThrough,
    /// Request is denied. Contains the pre-built response.
    Deny(Response<String>),
}

/// Resolve the identity result into a `(user_id, tier_name)` pair,
/// or return a `CheckOutcome` if the request should be short-circuited.
pub(crate) fn resolve_identity(
    identity: Option<TierIdentity>,
    rate_tier: &RateTier,
) -> Result<(String, String), CheckOutcome> {
    match identity {
        Some(id) => Ok((id.user_id, id.tier)),
        None => match rate_tier.on_missing() {
            OnMissing::Allow => Err(CheckOutcome::PassThrough),
            OnMissing::Deny(status) => Err(CheckOutcome::Deny(response::deny_response(status))),
            OnMissing::UseDefault => {
                if let Some(default) = rate_tier.default_tier() {
                    Ok(("__anonymous__".to_string(), default.to_string()))
                } else {
                    Err(CheckOutcome::PassThrough)
                }
            }
        },
    }
}

/// Check whether the tier exists and is not unlimited.
/// Returns the quota if rate limiting should proceed, or a `CheckOutcome` to short-circuit.
pub(crate) fn resolve_quota<'a>(
    rate_tier: &'a RateTier,
    tier_name: &str,
) -> Result<&'a Quota, CheckOutcome> {
    match rate_tier.get_quota(tier_name) {
        Some(q) if q.is_unlimited() => Err(CheckOutcome::PassThrough),
        Some(q) => Ok(q),
        None => Err(CheckOutcome::PassThrough),
    }
}

/// Reject a request whose cost is above the tier's maximum burst.
///
/// Such a request can never be allowed, so it is answered with 403 before
/// storage is touched. Returns `None` when the request may proceed.
pub(crate) fn reject_cost_over_limit(
    cost: u32,
    quota: &Quota,
    user_id: &str,
    tier_name: &str,
    settings: &Settings,
) -> Option<Response<String>> {
    let limit = quota.max_burst();
    if cost <= limit {
        return None;
    }
    if let Some(cb) = &settings.on_event {
        cb(&LimitEvent::CostExceedsLimit {
            user_id,
            tier: tier_name,
            cost,
            limit,
        });
    }
    Some(response::cost_exceeds_limit_response(
        tier_name, cost, limit,
    ))
}

/// Process the storage result into a `CheckOutcome`.
pub(crate) fn process_result(
    result: Result<Result<RateLimitInfo, crate::gcra::RateLimited>, crate::storage::StorageError>,
    user_id: &str,
    tier_name: &str,
    settings: &Settings,
    now: SystemTime,
) -> CheckOutcome {
    match result {
        Ok(Ok(info)) => CheckOutcome::Allow(info),
        Ok(Err(limited)) => {
            if let Some(cb) = &settings.on_limited {
                cb(user_id, tier_name, &limited);
            }
            let resp = if let Some(builder) = &settings.rate_limited_response {
                builder(user_id, tier_name, &limited)
            } else {
                response::rate_limited_response(&limited, tier_name, now)
            };
            CheckOutcome::Deny(resp)
        }
        Err(error) => {
            if let Some(cb) = &settings.on_event {
                cb(&LimitEvent::StorageError {
                    user_id,
                    tier: tier_name,
                    error: &error,
                });
            }
            match settings.on_storage_error {
                OnStorageError::Allow => CheckOutcome::PassThrough,
                OnStorageError::Deny => CheckOutcome::Deny(response::storage_error_response()),
            }
        }
    }
}
