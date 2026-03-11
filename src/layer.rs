use std::sync::Arc;

use http::HeaderMap;
use tower_layer::Layer;

use crate::identifier::{ClosureIdentifier, TierIdentifier, TierIdentity};
use crate::on_storage_error::OnStorageError;
use crate::service::TierLimitService;
use crate::tier::RateTier;

/// Tower layer for tier-based rate limiting.
///
/// Wraps an inner service with [`TierLimitService`] to enforce per-tier rate limits.
///
/// # Examples
///
/// ```rust,no_run
/// use tower_rate_tier::{RateTier, Quota, TierIdentity, TierLimitLayer};
///
/// let rate_tier = RateTier::builder()
///     .tier("free", Quota::per_hour(100))
///     .tier("pro", Quota::per_hour(5_000))
///     .default_tier("free")
///     .build();
///
/// let layer = TierLimitLayer::new(rate_tier)
///     .identifier_fn(|headers| {
///         let key = headers.get("x-api-key")?.to_str().ok()?;
///         Some(TierIdentity::new(key, "free"))
///     });
/// ```
pub struct TierLimitLayer {
    pub(crate) rate_tier: Arc<RateTier>,
    pub(crate) identifier: Arc<dyn TierIdentifier>,
    pub(crate) on_storage_error: OnStorageError,
}

/// Default identifier that returns `None` for all requests.
struct NoopIdentifier;

#[async_trait::async_trait]
impl TierIdentifier for NoopIdentifier {
    async fn identify(&self, _headers: &HeaderMap) -> Option<TierIdentity> {
        None
    }
}

impl TierLimitLayer {
    /// Create a new layer with the given rate tier configuration.
    ///
    /// You must call [`identifier`](Self::identifier) or
    /// [`identifier_fn`](Self::identifier_fn) before using this layer,
    /// otherwise all requests will be treated as unidentified.
    pub fn new(rate_tier: RateTier) -> Self {
        Self {
            rate_tier: Arc::new(rate_tier),
            identifier: Arc::new(NoopIdentifier),
            on_storage_error: OnStorageError::default(),
        }
    }

    /// Set the identifier using a [`TierIdentifier`] trait implementation.
    ///
    /// Use this for async identification logic (e.g., database or Redis lookups).
    pub fn identifier(mut self, identifier: impl TierIdentifier) -> Self {
        self.identifier = Arc::new(identifier);
        self
    }

    /// Set the identifier using a sync closure.
    ///
    /// Convenient for simple cases that only need request headers.
    pub fn identifier_fn<F>(mut self, f: F) -> Self
    where
        F: Fn(&HeaderMap) -> Option<TierIdentity> + Send + Sync + 'static,
    {
        self.identifier = Arc::new(ClosureIdentifier(f));
        self
    }

    /// Set the behavior when the storage backend fails.
    ///
    /// Default: [`OnStorageError::Allow`] (fail open).
    pub fn on_storage_error(mut self, policy: OnStorageError) -> Self {
        self.on_storage_error = policy;
        self
    }

    /// Enable body-based identification.
    ///
    /// When enabled, the middleware buffers the request body before identification,
    /// allowing [`TierIdentifier::identify_with_body`] to inspect body contents.
    /// The body is reconstructed as `Full<Bytes>` for the downstream service.
    ///
    /// Requires the `buffered-body` feature.
    ///
    /// # Default body size limit
    ///
    /// 64KB. Override with [`BufferedTierLimitLayer::max_body_size`].
    #[cfg(feature = "buffered-body")]
    pub fn buffer_body(self) -> crate::buffered::BufferedTierLimitLayer {
        crate::buffered::BufferedTierLimitLayer {
            rate_tier: self.rate_tier,
            identifier: self.identifier,
            on_storage_error: self.on_storage_error,
            max_body_size: 64 * 1024,
        }
    }
}

impl<S> Layer<S> for TierLimitLayer {
    type Service = TierLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TierLimitService {
            inner,
            rate_tier: self.rate_tier.clone(),
            identifier: self.identifier.clone(),
            on_storage_error: self.on_storage_error,
        }
    }
}
