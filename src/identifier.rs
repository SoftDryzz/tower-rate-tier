use async_trait::async_trait;
use bytes::Bytes;
use http::HeaderMap;

/// The result of identifying a user and their tier from a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierIdentity {
    /// Unique identifier for the user (e.g. API key, account ID).
    pub user_id: String,
    /// Name of the tier this user belongs to (e.g. "free", "pro").
    pub tier: String,
}

impl TierIdentity {
    /// Creates a new `TierIdentity` from a user ID and tier name.
    pub fn new(user_id: impl Into<String>, tier: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            tier: tier.into(),
        }
    }
}

/// Trait for extracting user identity and tier from HTTP requests.
///
/// Implement this trait for async lookups (e.g., database, Redis).
/// For simple sync cases, use [`identifier_fn`](crate::TierLimitLayer::identifier_fn).
#[async_trait]
pub trait TierIdentifier: Send + Sync + 'static {
    /// Identify the user from request headers.
    async fn identify(&self, headers: &HeaderMap) -> Option<TierIdentity>;

    /// Identify the user from request headers and body.
    ///
    /// Only called when `buffer_body(true)` is enabled.
    /// Default implementation delegates to [`identify`](TierIdentifier::identify).
    async fn identify_with_body(
        &self,
        headers: &HeaderMap,
        _body: &Bytes,
    ) -> Option<TierIdentity> {
        self.identify(headers).await
    }
}

/// Wraps a sync closure as a `TierIdentifier`.
#[allow(dead_code)]
pub(crate) struct ClosureIdentifier<F>(pub(crate) F);

#[async_trait]
impl<F> TierIdentifier for ClosureIdentifier<F>
where
    F: Fn(&HeaderMap) -> Option<TierIdentity> + Send + Sync + 'static,
{
    async fn identify(&self, headers: &HeaderMap) -> Option<TierIdentity> {
        (self.0)(headers)
    }
}
