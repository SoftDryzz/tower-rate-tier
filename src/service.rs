use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::SystemTime;

use http::{Request, Response};
use tower_service::Service;

use crate::check::{self, CheckOutcome};
use crate::layer::Settings;
use crate::response;
use crate::storage::StorageKey;
use crate::tier::RateTier;

/// Tower service that enforces tier-based rate limiting.
///
/// Created by [`TierLimitLayer`](crate::layer::TierLimitLayer).
/// This service intercepts requests, identifies the user/tier,
/// checks the rate limit, and either forwards the request or returns 429.
#[derive(Clone, Debug)]
pub struct TierLimitService<S> {
    pub(crate) inner: S,
    pub(crate) rate_tier: Arc<RateTier>,
    pub(crate) settings: Arc<Settings>,
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
        let settings = self.settings.clone();
        let mut inner = self.inner.clone();
        // Swap to preserve readiness: the clone gets future calls, self keeps the ready one.
        std::mem::swap(&mut self.inner, &mut inner);

        Box::pin(async move {
            let identity = settings.identifier.identify(req.headers()).await;

            let (user_id, tier_name) = match check::resolve_identity(identity, &rate_tier) {
                Ok(pair) => pair,
                Err(CheckOutcome::PassThrough) => return inner.call(req).await,
                Err(CheckOutcome::Deny(resp)) => return Ok(resp.map(Into::into)),
                Err(CheckOutcome::Allow(_)) => unreachable!(),
            };

            let resolved = check::resolve_quota(&rate_tier, &user_id, tier_name, &settings);
            let (tier_name, quota) = match resolved {
                Ok(resolved) => resolved,
                Err(CheckOutcome::PassThrough) => return inner.call(req).await,
                Err(CheckOutcome::Deny(resp)) => return Ok(resp.map(Into::into)),
                Err(CheckOutcome::Allow(_)) => unreachable!(),
            };

            let (parts, body) = req.into_parts();
            let cost = check::request_cost(&parts, &settings);
            let req = Request::from_parts(parts, body);
            if let Some(resp) =
                check::reject_cost_over_limit(cost, quota, &user_id, &tier_name, &settings)
            {
                return Ok(resp.map(Into::into));
            }
            let now = rate_tier.clock().now();
            let key = StorageKey::new(&user_id, &tier_name);
            let result = rate_tier
                .storage()
                .check_and_update(key, quota, cost, now)
                .await;

            // Wall-clock time of the check; the durations in the result are
            // relative to it.
            let checked_at = SystemTime::now();

            match check::process_result(result, &user_id, &tier_name, &settings, checked_at) {
                CheckOutcome::Allow(info) => {
                    let mut resp = inner.call(req).await?;
                    response::inject_headers(&mut resp, &info, checked_at);
                    Ok(resp)
                }
                CheckOutcome::Deny(resp) => Ok(resp.map(Into::into)),
                CheckOutcome::PassThrough => inner.call(req).await,
            }
        })
    }
}
