use std::sync::Arc;

use http::{HeaderMap, Response};
use tower_layer::Layer;

use crate::gcra::RateLimited;
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
/// Callback invoked when a request is rate limited.
///
/// Receives `(user_id, tier_name, rate_limited_info)`.
pub type OnLimitedFn = dyn Fn(&str, &str, &RateLimited) + Send + Sync;

/// Custom response builder for rate-limited requests.
///
/// Receives `(user_id, tier_name, rate_limited_info)` and returns a `Response<String>`.
pub type RateLimitedResponseFn = dyn Fn(&str, &str, &RateLimited) -> Response<String> + Send + Sync;

#[derive(Clone)]
pub struct TierLimitLayer {
    pub(crate) rate_tier: Arc<RateTier>,
    pub(crate) identifier: Arc<dyn TierIdentifier>,
    pub(crate) on_storage_error: OnStorageError,
    pub(crate) on_limited: Option<Arc<OnLimitedFn>>,
    pub(crate) rate_limited_response: Option<Arc<RateLimitedResponseFn>>,
}

/// Default identifier that returns `None` for all requests.
struct NoopIdentifier;

impl TierIdentifier for NoopIdentifier {
    fn identify(
        &self,
        _headers: &HeaderMap,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<TierIdentity>> + Send + '_>>
    {
        Box::pin(std::future::ready(None))
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
            on_limited: None,
            rate_limited_response: None,
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

    /// Set a callback invoked every time a request is rate limited.
    ///
    /// The callback receives `(user_id, tier_name, &RateLimited)` and must be
    /// non-blocking (sync). Useful for incrementing metrics counters or logging.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tower_rate_tier::{RateTier, Quota, TierLimitLayer};
    /// # let rate_tier = RateTier::builder().tier("free", Quota::per_hour(100)).build();
    /// let layer = TierLimitLayer::new(rate_tier)
    ///     .on_limited(|user_id, tier, limited| {
    ///         eprintln!("rate limited: user={user_id} tier={tier} retry_after={:?}", limited.retry_after);
    ///     });
    /// ```
    pub fn on_limited(
        mut self,
        f: impl Fn(&str, &str, &RateLimited) + Send + Sync + 'static,
    ) -> Self {
        self.on_limited = Some(Arc::new(f));
        self
    }

    /// Set a custom response builder for rate-limited requests.
    ///
    /// When set, this replaces the default 429 JSON response. The closure
    /// receives `(user_id, tier_name, &RateLimited)` and must return a
    /// `Response<String>`.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tower_rate_tier::{RateTier, Quota, TierLimitLayer};
    /// # use http::{Response, StatusCode};
    /// # let rate_tier = RateTier::builder().tier("free", Quota::per_hour(100)).build();
    /// let layer = TierLimitLayer::new(rate_tier)
    ///     .rate_limited_response(|_user_id, tier, limited| {
    ///         Response::builder()
    ///             .status(StatusCode::TOO_MANY_REQUESTS)
    ///             .header("Content-Type", "application/problem+json")
    ///             .header("Retry-After", limited.retry_after.as_secs())
    ///             .body(format!(r#"{{"type":"rate_limit","tier":"{}"}}"#, tier))
    ///             .unwrap()
    ///     });
    /// ```
    pub fn rate_limited_response(
        mut self,
        f: impl Fn(&str, &str, &RateLimited) -> Response<String> + Send + Sync + 'static,
    ) -> Self {
        self.rate_limited_response = Some(Arc::new(f));
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
            on_limited: self.on_limited,
            rate_limited_response: self.rate_limited_response,
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
            on_limited: self.on_limited.clone(),
            rate_limited_response: self.rate_limited_response.clone(),
        }
    }
}
