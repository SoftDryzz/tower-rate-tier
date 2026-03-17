use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body::Body;
use http_body_util::{BodyExt, Full};
use tower_layer::Layer;
use tower_service::Service;

use crate::cost::TierCost;
use crate::identifier::TierIdentifier;
use crate::on_missing::OnMissing;
use crate::on_storage_error::OnStorageError;
use crate::response;
use crate::tier::RateTier;

/// Tower layer for tier-based rate limiting with body-based identification.
///
/// Created by calling [`buffer_body()`](crate::TierLimitLayer::buffer_body)
/// on a [`TierLimitLayer`](crate::TierLimitLayer).
///
/// This layer buffers the request body before identification, enabling
/// [`TierIdentifier::identify_with_body`] to inspect the body contents.
/// The body is then reconstructed as `Full<Bytes>` for the downstream service.
///
/// # Body size limit
///
/// Requests exceeding [`max_body_size`](Self::max_body_size) (default: 64KB)
/// are immediately rejected with 413 Payload Too Large.
#[derive(Clone)]
pub struct BufferedTierLimitLayer {
    pub(crate) rate_tier: Arc<RateTier>,
    pub(crate) identifier: Arc<dyn TierIdentifier>,
    pub(crate) on_storage_error: OnStorageError,
    pub(crate) max_body_size: usize,
}

impl BufferedTierLimitLayer {
    /// Set the maximum allowed body size in bytes.
    ///
    /// Requests with bodies larger than this are rejected with 413.
    /// Default: 64KB.
    pub fn max_body_size(mut self, size: usize) -> Self {
        self.max_body_size = size;
        self
    }
}

impl<S> Layer<S> for BufferedTierLimitLayer {
    type Service = BufferedTierLimitService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        BufferedTierLimitService {
            inner,
            rate_tier: self.rate_tier.clone(),
            identifier: self.identifier.clone(),
            on_storage_error: self.on_storage_error,
            max_body_size: self.max_body_size,
        }
    }
}

/// Tower service that buffers the request body for identification.
///
/// Created by [`BufferedTierLimitLayer`].
pub struct BufferedTierLimitService<S> {
    inner: S,
    rate_tier: Arc<RateTier>,
    identifier: Arc<dyn TierIdentifier>,
    on_storage_error: OnStorageError,
    max_body_size: usize,
}

impl<S: Clone> Clone for BufferedTierLimitService<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            rate_tier: self.rate_tier.clone(),
            identifier: self.identifier.clone(),
            on_storage_error: self.on_storage_error,
            max_body_size: self.max_body_size,
        }
    }
}

impl<S, B, ResBody> Service<Request<B>> for BufferedTierLimitService<S>
where
    S: Service<Request<Full<Bytes>>, Response = Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send,
    S::Error: Send,
    B: Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
    ResBody: From<String> + Send,
{
    type Response = Response<ResBody>;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<B>) -> Self::Future {
        let rate_tier = self.rate_tier.clone();
        let identifier = self.identifier.clone();
        let on_storage_error = self.on_storage_error;
        let max_body_size = self.max_body_size;
        let mut inner = self.inner.clone();
        std::mem::swap(&mut self.inner, &mut inner);

        Box::pin(async move {
            // Split request to buffer body separately
            let (parts, body) = req.into_parts();

            // Collect the body
            let body_bytes = match body.collect().await {
                Ok(collected) => {
                    let bytes = collected.to_bytes();
                    if bytes.len() > max_body_size {
                        return Ok(payload_too_large_response().map(Into::into));
                    }
                    bytes
                }
                Err(_) => {
                    return Ok(response::bad_request_response().map(Into::into));
                }
            };

            // Identify using headers + body
            let identity = identifier
                .identify_with_body(&parts.headers, &body_bytes)
                .await;

            let (user_id, tier_name) = match identity {
                Some(id) => (id.user_id, id.tier),
                None => match rate_tier.on_missing() {
                    OnMissing::Allow => {
                        let req = Request::from_parts(parts, Full::new(body_bytes));
                        return inner.call(req).await;
                    }
                    OnMissing::Deny(status) => {
                        return Ok(response::deny_response(*status).map(Into::into));
                    }
                    OnMissing::UseDefault => {
                        if let Some(default) = rate_tier.default_tier() {
                            ("__anonymous__".to_string(), default.to_string())
                        } else {
                            let req = Request::from_parts(parts, Full::new(body_bytes));
                            return inner.call(req).await;
                        }
                    }
                },
            };

            // Look up quota
            let quota = match rate_tier.get_quota(&tier_name) {
                Some(q) => q,
                None => {
                    let req = Request::from_parts(parts, Full::new(body_bytes));
                    return inner.call(req).await;
                }
            };

            // Unlimited bypass
            if quota.is_unlimited() {
                let req = Request::from_parts(parts, Full::new(body_bytes));
                return inner.call(req).await;
            }

            // Read cost from extensions
            let cost = parts
                .extensions
                .get::<TierCost>()
                .map(|c| c.0)
                .unwrap_or(1);

            // Rate limit check
            let now = rate_tier.clock().now();
            let storage_key = format!("{}:{}", user_id, tier_name);
            let result = rate_tier
                .storage()
                .check_and_update(&storage_key, quota, cost, now)
                .await;

            let unix_offset = rate_tier.clock().unix_offset_nanos();

            match result {
                Ok(Ok(info)) => {
                    let req = Request::from_parts(parts, Full::new(body_bytes));
                    let mut resp = inner.call(req).await?;
                    response::inject_headers(&mut resp, &info, unix_offset);
                    Ok(resp)
                }
                Ok(Err(limited)) => {
                    Ok(response::rate_limited_response(&limited, &tier_name, unix_offset).map(Into::into))
                }
                Err(_storage_err) => match on_storage_error {
                    OnStorageError::Allow => {
                        let req = Request::from_parts(parts, Full::new(body_bytes));
                        inner.call(req).await
                    }
                    OnStorageError::Deny => {
                        Ok(response::storage_error_response().map(Into::into))
                    }
                },
            }
        })
    }
}

fn payload_too_large_response() -> Response<String> {
    Response::builder()
        .status(StatusCode::PAYLOAD_TOO_LARGE)
        .header("Content-Type", "application/json")
        .body(r#"{"error":"payload too large"}"#.to_string())
        .unwrap()
}
