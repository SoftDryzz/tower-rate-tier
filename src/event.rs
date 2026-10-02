use crate::storage::StorageError;

/// Something worth reporting that happened while rate limiting a request.
///
/// Delivered to the callback set with
/// [`TierLimitLayer::on_event`](crate::TierLimitLayer::on_event), for example
/// to log or count it. New variants may be added in minor releases.
#[derive(Debug)]
#[non_exhaustive]
pub enum LimitEvent<'a> {
    /// The storage backend failed. The request was then handled by the
    /// [`OnStorageError`](crate::OnStorageError) policy.
    StorageError {
        /// The identified user.
        user_id: &'a str,
        /// The tier whose quota was being checked.
        tier: &'a str,
        /// The error returned by the backend.
        error: &'a StorageError,
    },
    /// The identifier returned a tier that is not configured. The request was
    /// then handled by the [`OnUnknownTier`](crate::OnUnknownTier) policy.
    UnknownTier {
        /// The identified user.
        user_id: &'a str,
        /// The tier name the identifier returned.
        tier: &'a str,
    },
    /// A request cost more than the tier allows in a whole window, so it was
    /// rejected without touching storage.
    CostExceedsLimit {
        /// The identified user.
        user_id: &'a str,
        /// The tier whose quota applied.
        tier: &'a str,
        /// The cost of the rejected request.
        cost: u32,
        /// The tier's maximum burst.
        limit: u32,
    },
}
