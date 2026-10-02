use std::fmt;
use std::sync::Arc;

use http::{HeaderMap, Response};
use tower_layer::Layer;

use crate::event::LimitEvent;
use crate::gcra::RateLimited;
use crate::identifier::{ClosureIdentifier, TierIdentifier, TierIdentity};
use crate::on_storage_error::OnStorageError;
use crate::service::TierLimitService;
use crate::tier::RateTier;

/// Callback invoked when a request is rate limited.
///
/// Receives `(user_id, tier_name, rate_limited_info)`.
pub type OnLimitedFn = dyn Fn(&str, &str, &RateLimited) + Send + Sync;

/// Custom response builder for rate-limited requests.
///
/// Receives `(user_id, tier_name, rate_limited_info)` and returns a `Response<String>`.
pub type RateLimitedResponseFn = dyn Fn(&str, &str, &RateLimited) -> Response<String> + Send + Sync;

/// Callback invoked for every [`LimitEvent`].
pub type OnEventFn = dyn for<'a> Fn(&LimitEvent<'a>) + Send + Sync;

/// Computes the cost of a request from its method, URI, headers and extensions.
pub type CostFn = dyn Fn(&http::request::Parts) -> u32 + Send + Sync;

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
#[derive(Clone, Debug)]
pub struct TierLimitLayer {
    pub(crate) rate_tier: Arc<RateTier>,
    pub(crate) settings: Settings,
}

/// Middleware settings shared by a layer and the services it creates.
#[derive(Clone)]
pub(crate) struct Settings {
    pub(crate) identifier: Arc<dyn TierIdentifier>,
    pub(crate) on_storage_error: OnStorageError,
    pub(crate) on_limited: Option<Arc<OnLimitedFn>>,
    pub(crate) rate_limited_response: Option<Arc<RateLimitedResponseFn>>,
    pub(crate) on_event: Option<Arc<OnEventFn>>,
    pub(crate) cost_fn: Option<Arc<CostFn>>,
}

impl fmt::Debug for Settings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Settings")
            .field("on_storage_error", &self.on_storage_error)
            .field("on_limited", &self.on_limited.is_some())
            .field(
                "rate_limited_response",
                &self.rate_limited_response.is_some(),
            )
            .field("on_event", &self.on_event.is_some())
            .field("cost_fn", &self.cost_fn.is_some())
            .finish_non_exhaustive()
    }
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
    /// Accepts a [`RateTier`] or an `Arc<RateTier>`. Pass a shared `Arc` to
    /// count middleware requests and programmatic
    /// [`RateTier::check()`](crate::RateTier::check) calls against the same
    /// limits and storage.
    ///
    /// You must call [`identifier`](Self::identifier) or
    /// [`identifier_fn`](Self::identifier_fn) before using this layer,
    /// otherwise all requests will be treated as unidentified.
    pub fn new(rate_tier: impl Into<Arc<RateTier>>) -> Self {
        Self {
            rate_tier: rate_tier.into(),
            settings: Settings {
                identifier: Arc::new(NoopIdentifier),
                on_storage_error: OnStorageError::default(),
                on_limited: None,
                rate_limited_response: None,
                on_event: None,
                cost_fn: None,
            },
        }
    }

    /// Set the identifier using a [`TierIdentifier`] trait implementation.
    ///
    /// Use this for async identification logic (e.g., database or Redis lookups).
    pub fn identifier(mut self, identifier: impl TierIdentifier) -> Self {
        self.settings.identifier = Arc::new(identifier);
        self
    }

    /// Set the identifier using a sync closure.
    ///
    /// Convenient for simple cases that only need request headers.
    pub fn identifier_fn<F>(mut self, f: F) -> Self
    where
        F: Fn(&HeaderMap) -> Option<TierIdentity> + Send + Sync + 'static,
    {
        self.settings.identifier = Arc::new(ClosureIdentifier(f));
        self
    }

    /// Set the behavior when the storage backend fails.
    ///
    /// Default: [`OnStorageError::Allow`] (fail open).
    pub fn on_storage_error(mut self, policy: OnStorageError) -> Self {
        self.settings.on_storage_error = policy;
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
        self.settings.on_limited = Some(Arc::new(f));
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
        self.settings.rate_limited_response = Some(Arc::new(f));
        self
    }

    /// Compute the cost of each request inside the middleware.
    ///
    /// The closure receives the request's method, URI, headers and
    /// extensions. A [`TierCost`](crate::TierCost) already in the extensions,
    /// from a [`tier_cost`](crate::tier_cost) layer wrapped *around* this one,
    /// takes precedence. Without either, a request costs 1.
    ///
    /// Use this instead of per-route `tier_cost` layers when the rate limiter
    /// is added with axum's `Router::layer`: route layers run *inside* it, too
    /// late to set the cost. Axum's `MatchedPath` extension is available here,
    /// so routes with parameters can be matched by their pattern.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tower_rate_tier::{RateTier, Quota, TierLimitLayer};
    /// # let rate_tier = RateTier::builder().tier("free", Quota::per_hour(100)).build();
    /// let layer = TierLimitLayer::new(rate_tier).cost_fn(|req| match req.uri.path() {
    ///     "/api/search" => 5,
    ///     "/api/export" => 20,
    ///     "/health" => 0,
    ///     _ => 1,
    /// });
    /// ```
    pub fn cost_fn(
        mut self,
        f: impl Fn(&http::request::Parts) -> u32 + Send + Sync + 'static,
    ) -> Self {
        self.settings.cost_fn = Some(Arc::new(f));
        self
    }

    /// Set a callback invoked for every [`LimitEvent`], such as a storage
    /// backend failure.
    ///
    /// The callback must be non-blocking (sync). Use it to log or count
    /// problems that the policies otherwise handle quietly, for example a
    /// fail-open [`OnStorageError::Allow`] while the backend is down.
    ///
    /// # Examples
    ///
    /// ```rust,no_run
    /// # use tower_rate_tier::{LimitEvent, RateTier, Quota, TierLimitLayer};
    /// # let rate_tier = RateTier::builder().tier("free", Quota::per_hour(100)).build();
    /// let layer = TierLimitLayer::new(rate_tier).on_event(|event| match event {
    ///     LimitEvent::StorageError { error, .. } => eprintln!("rate limit storage failed: {error}"),
    ///     _ => {}
    /// });
    /// ```
    pub fn on_event(mut self, f: impl Fn(&LimitEvent<'_>) + Send + Sync + 'static) -> Self {
        self.settings.on_event = Some(Arc::new(f));
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
    /// 64KB. Override with [`BufferedTierLimitLayer::max_body_size`](crate::buffered::BufferedTierLimitLayer::max_body_size).
    #[cfg(feature = "buffered-body")]
    pub fn buffer_body(self) -> crate::buffered::BufferedTierLimitLayer {
        crate::buffered::BufferedTierLimitLayer {
            rate_tier: self.rate_tier,
            settings: self.settings,
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
            settings: Arc::new(self.settings.clone()),
        }
    }
}
