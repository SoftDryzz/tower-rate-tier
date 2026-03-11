use std::task::{Context, Poll};

use http::Request;
use tower_layer::Layer;
use tower_service::Service;

/// Extension type inserted into request extensions by [`tier_cost`].
///
/// `TierLimitService` reads this to determine the cost of the current request.
/// If absent, the default cost of 1 is used.
#[derive(Debug, Clone, Copy)]
pub struct TierCost(pub u32);

/// Create a Tower layer that sets the rate limit cost for requests passing through it.
///
/// # Examples
///
/// ```rust,no_run
/// use axum::{Router, routing::post};
/// use tower_rate_tier::tier_cost;
///
/// let app: Router = Router::new()
///     .route("/search", post(|| async { "ok" }).layer(tier_cost(5)))
///     .route("/export", post(|| async { "ok" }).layer(tier_cost(20)));
/// ```
pub fn tier_cost(cost: u32) -> TierCostLayer {
    TierCostLayer(cost)
}

/// Layer that inserts [`TierCost`] into request extensions.
#[derive(Debug, Clone, Copy)]
pub struct TierCostLayer(u32);

impl<S> Layer<S> for TierCostLayer {
    type Service = TierCostService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TierCostService {
            inner,
            cost: self.0,
        }
    }
}

/// Service that inserts [`TierCost`] into request extensions before forwarding.
#[derive(Debug, Clone)]
pub struct TierCostService<S> {
    inner: S,
    cost: u32,
}

impl<S, B> Service<Request<B>> for TierCostService<S>
where
    S: Service<Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        req.extensions_mut().insert(TierCost(self.cost));
        self.inner.call(req)
    }
}
