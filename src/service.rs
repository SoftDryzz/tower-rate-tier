use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use http::{Request, Response};
use tower_service::Service;

use crate::check::{self, CheckOutcome};
use crate::cost::TierCost;
use crate::identifier::TierIdentifier;
use crate::layer::{OnLimitedFn, RateLimitedResponseFn};
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
    pub(crate) on_limited: Option<Arc<OnLimitedFn>>,
    pub(crate) rate_limited_response: Option<Arc<RateLimitedResponseFn>>,
}

impl<S: Clone> Clone for TierLimitService<S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            rate_tier: self.rate_tier.clone(),
            identifier: self.identifier.clone(),
            on_storage_error: self.on_storage_error,
            on_limited: self.on_limited.clone(),
            rate_limited_response: self.rate_limited_response.clone(),
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
        let on_limited = self.on_limited.clone();
        let rate_limited_response_fn = self.rate_limited_response.clone();
        let mut inner = self.inner.clone();
        // Swap to preserve readiness: the clone gets future calls, self keeps the ready one.
        std::mem::swap(&mut self.inner, &mut inner);

        Box::pin(async move {
            let identity = identifier.identify(req.headers()).await;

            let (user_id, tier_name) = match check::resolve_identity(identity, &rate_tier) {
                Ok(pair) => pair,
                Err(CheckOutcome::PassThrough) => return inner.call(req).await,
                Err(CheckOutcome::Deny(resp)) => return Ok(resp.map(Into::into)),
                Err(CheckOutcome::Allow(_)) => unreachable!(),
            };

            let quota = match check::resolve_quota(&rate_tier, &tier_name) {
                Ok(q) => q,
                Err(CheckOutcome::PassThrough) => return inner.call(req).await,
                Err(_) => unreachable!(),
            };

            let cost = req.extensions().get::<TierCost>().map(|c| c.0).unwrap_or(1);
            let now = rate_tier.clock().now();
            let storage_key = format!("{}:{}", user_id, tier_name);
            let result = rate_tier
                .storage()
                .check_and_update(&storage_key, quota, cost, now)
                .await;

            let unix_offset = rate_tier.clock().unix_offset_nanos();

            match check::process_result(
                result,
                &user_id,
                &tier_name,
                on_storage_error,
                &on_limited,
                &rate_limited_response_fn,
                unix_offset,
            ) {
                CheckOutcome::Allow(info) => {
                    let mut resp = inner.call(req).await?;
                    response::inject_headers(&mut resp, &info, unix_offset);
                    Ok(resp)
                }
                CheckOutcome::Deny(resp) => Ok(resp.map(Into::into)),
                CheckOutcome::PassThrough => inner.call(req).await,
            }
        })
    }
}
