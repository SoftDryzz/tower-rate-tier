use http::Request;
use tower_layer::Layer;
use tower_rate_tier::TierCost;
use tower_service::Service;

/// A simple service that extracts TierCost from request extensions.
#[derive(Clone)]
struct ExtractCostService;

impl Service<Request<()>> for ExtractCostService {
    type Response = Option<TierCost>;
    type Error = std::convert::Infallible;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<()>) -> Self::Future {
        let cost = req.extensions().get::<TierCost>().copied();
        std::future::ready(Ok(cost))
    }
}

#[tokio::test]
async fn tier_cost_inserts_extension() {
    let layer = tower_rate_tier::tier_cost(5);
    let mut svc = layer.layer(ExtractCostService);

    let req = Request::new(());
    let cost = svc.call(req).await.unwrap();
    assert_eq!(cost.unwrap().0, 5);
}

#[tokio::test]
async fn no_tier_cost_gives_none() {
    let mut svc = ExtractCostService;

    let req = Request::new(());
    let cost = svc.call(req).await.unwrap();
    assert!(cost.is_none());
}

#[tokio::test]
async fn different_costs_on_different_routes() {
    let layer_search = tower_rate_tier::tier_cost(5);
    let layer_export = tower_rate_tier::tier_cost(20);

    let mut svc_search = layer_search.layer(ExtractCostService);
    let mut svc_export = layer_export.layer(ExtractCostService);

    let cost_search = svc_search.call(Request::new(())).await.unwrap();
    let cost_export = svc_export.call(Request::new(())).await.unwrap();

    assert_eq!(cost_search.unwrap().0, 5);
    assert_eq!(cost_export.unwrap().0, 20);
}
