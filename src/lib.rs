//! Tier-based rate limiting middleware for [`tower`] services.
//!
//! Assign users to named tiers (e.g. "free", "pro") with distinct quotas,
//! and let the middleware enforce limits automatically via the GCRA algorithm.

/// Clock abstractions for real and deterministic (test) time sources.
pub mod clock;
/// Cost extraction: decide how many tokens each request consumes.
pub mod cost;
/// Background garbage collection for expired rate-limit entries.
pub mod gc;
/// Generic Cell Rate Algorithm (GCRA) implementation.
pub mod gcra;
/// User identity and tier extraction from incoming requests.
pub mod identifier;
/// Tower `Layer` implementation that wraps services with rate limiting.
pub mod layer;
/// Policy for handling requests whose identity cannot be resolved.
pub mod on_missing;
/// Policy for handling storage backend errors.
pub mod on_storage_error;
/// Quota definitions (rate, burst, period).
pub mod quota;
/// HTTP response helpers for rate-limit headers and 429 replies.
pub mod response;
/// Tower `Service` implementation that enforces per-tier rate limits.
pub mod service;
/// Storage backends for persisting rate-limit state.
pub mod storage;
/// Tier configuration and the `RateTier` builder.
pub mod tier;

#[cfg(feature = "buffered-body")]
/// Buffered-body variants that allow identifier access to the request body.
pub mod buffered;

pub use cost::{tier_cost, TierCost};
pub use gcra::{RateLimitInfo, RateLimited};
pub use identifier::{TierIdentifier, TierIdentity};
pub use layer::TierLimitLayer;
pub use on_missing::OnMissing;
pub use on_storage_error::OnStorageError;
pub use quota::{Nanos, Quota};
pub use storage::StorageError;
pub use tier::{CheckError, RateTier};

#[cfg(feature = "buffered-body")]
pub use buffered::{BufferedTierLimitLayer, BufferedTierLimitService};
