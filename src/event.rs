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
}
