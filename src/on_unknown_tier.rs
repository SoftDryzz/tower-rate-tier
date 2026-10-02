use http::StatusCode;

/// Behavior when the identifier returns a tier that is not configured.
///
/// This covers typos, plans added to a database before the rate limiter
/// knows them, and tier names a client can influence (a header or an
/// unverified token claim). None of these remove the limit unless
/// [`OnUnknownTier::Allow`] is chosen explicitly, or the default tier that
/// [`OnUnknownTier::UseDefault`] falls back to is itself unlimited.
///
/// Applies to the middleware. [`RateTier::check()`](crate::RateTier::check)
/// returns [`CheckError::UnknownTier`](crate::CheckError::UnknownTier) instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum OnUnknownTier {
    /// Limit the user with the default tier's quota, in the user's own
    /// bucket. If the default tier is unlimited, so is the request. Without a
    /// default tier, the request is denied with 403.
    #[default]
    UseDefault,
    /// Deny the request with the given status code.
    Deny(StatusCode),
    /// Let the request through without rate limiting.
    Allow,
}
