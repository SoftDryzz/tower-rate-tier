use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use http::{Request, Response};
use tower_service::Service;

use crate::cost::TierCost;
use crate::identifier::TierIdentifier;
use crate::on_missing::OnMissing;
use crate::on_storage_error::OnStorageError;
use crate::response;
use crate::tier::RateTier;

/// Tower service that enforces tier-based rate limiting.
///
/// Created by [`TierLimitLayer`](crate::layer::TierLimitLayer).
/// This service intercepts requests, identifies the user/tier,
/// checks the rate limit, and either forwards the request or returns 429.
pub struct TierLimitService<S> {
    pub(crate) inner: S,
    pub(crate) rate_tier: Arc<RateTier>,
    pub(crate) identifier: Arc<dyn TierIdentifier>,
    pub(crate) on_storage_error: OnStorageError,
}

impl<S: Clone> Clone for TierLimitService<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            rate_tier: self.rate_tier.clone(),
            identifier: self.identifier.clone(),
            on_storage_error: self.on_storage_error,
        }
    }
}

impl<S, B, ResBody> Service<Request<B>> for TierLimitService<S>
where
    S: Service<Request<B>, Response = Response<ResBody>> + Clone + Send + 'static,
    S::Future: Send,
    S::Error: Send,
    B: Send + 'static,
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
        let mut inner = self.inner.clone();
        // Swap to preserve readiness: the clone gets future calls, self keeps the ready one.
        std::mem::swap(&mut self.inner, &mut inner);

        Box::pin(async move {
            let headers = req.headers();
            let identity = identifier.identify(headers).await;

            let (user_id, tier_name) = match identity {
                Some(id) => (id.user_id, id.tier),
                None => match rate_tier.on_missing() {
                    OnMissing::Allow => return inner.call(req).await,
                    OnMissing::Deny(status) => {
                        return Ok(response::deny_response(*status).map(Into::into));
                    }
                    OnMissing::UseDefault => {
                        if let Some(default) = rate_tier.default_tier() {
                            ("__anonymous__".to_string(), default.to_string())
                        } else {
                            return inner.call(req).await;
                        }
                    }
                },
            };

            // Look up the quota for this tier
            let quota = match rate_tier.get_quota(&tier_name) {
                Some(q) => q,
                None => {
                    // Unknown tier — treat as unidentified
                    return inner.call(req).await;
                }
            };

            // Unlimited tiers bypass rate limiting entirely
            if quota.is_unlimited() {
                return inner.call(req).await;
            }

            // Read cost from extensions (set by tier_cost layer), default 1
            let cost = req
                .extensions()
                .get::<TierCost>()
                .map(|c| c.0)
                .unwrap_or(1);

            // Perform the rate limit check
            let now = rate_tier.clock().now();
            let result = rate_tier
                .storage()
                .check_and_update(&user_id, quota, cost, now)
                .await;

            let unix_offset = rate_tier.clock().unix_offset_nanos();

            match result {
                Ok(Ok(info)) => {
                    let mut resp = inner.call(req).await?;
                    response::inject_headers(&mut resp, &info, unix_offset);
                    Ok(resp)
                }
                Ok(Err(limited)) => {
                    Ok(response::rate_limited_response(&limited, &tier_name, unix_offset).map(Into::into))
                }
                Err(_storage_err) => match on_storage_error {
                    OnStorageError::Allow => inner.call(req).await,
                    OnStorageError::Deny => {
                        Ok(response::storage_error_response().map(Into::into))
                    }
                },
            }
        })
    }
}
