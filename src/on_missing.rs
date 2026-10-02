use http::StatusCode;

/// Behavior when the identifier cannot determine the user/tier.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum OnMissing {
    /// Use the default tier's quota. All unidentified requests share one
    /// bucket. Without a default tier, the request is denied with 403
    /// Forbidden; use [`OnMissing::Allow`] to let it through instead.
    #[default]
    UseDefault,
    /// Allow the request through without rate limiting.
    Allow,
    /// Deny the request with the given status code.
    Deny(StatusCode),
}
