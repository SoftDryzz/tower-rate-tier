/// Behavior when the storage backend fails (e.g., Redis is down).
///
/// To keep limiting during an outage instead of choosing between these two,
/// wrap the backend in a [`CircuitBreaker`](crate::CircuitBreaker): its local
/// fallback answers every request, so this policy is never reached.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum OnStorageError {
    /// Fail open: let the request through without rate limiting.
    #[default]
    Allow,
    /// Fail closed: return 503 Service Unavailable.
    Deny,
}
