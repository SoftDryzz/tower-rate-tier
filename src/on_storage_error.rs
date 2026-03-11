/// Behavior when the storage backend fails (e.g., Redis is down).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OnStorageError {
    /// Fail open: let the request through without rate limiting.
    #[default]
    Allow,
    /// Fail closed: return 503 Service Unavailable.
    Deny,
}
