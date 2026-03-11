use http::StatusCode;

/// Behavior when the identifier cannot determine the user/tier.
#[derive(Debug, Clone, Default)]
pub enum OnMissing {
    /// Use the default tier's quota.
    #[default]
    UseDefault,
    /// Allow the request through without rate limiting.
    Allow,
    /// Deny the request with the given status code.
    Deny(StatusCode),
}
