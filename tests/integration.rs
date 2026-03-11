use std::time::Duration;

use http::{Request, Response, StatusCode};
use tower_layer::Layer;
use tower_rate_tier::clock::FakeClock;
use tower_rate_tier::on_missing::OnMissing;
use tower_rate_tier::{Quota, RateTier, TierIdentity, TierLimitLayer};
use tower_service::Service;

/// Simple echo service that returns 200 OK with a string body.
#[derive(Clone)]
struct OkService;

impl Service<Request<String>> for OkService {
    type Response = Response<String>;
    type Error = std::convert::Infallible;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: Request<String>) -> Self::Future {
        std::future::ready(Ok(Response::new("ok".to_string())))
    }
}

fn build_request(api_key: Option<&str>) -> Request<String> {
    let mut builder = Request::builder().uri("/api/test");
    if let Some(key) = api_key {
        builder = builder.header("x-api-key", key);
    }
    builder.body(String::new()).unwrap()
}

fn build_request_with_cost(api_key: &str, cost: u32) -> Request<String> {
    let mut req = build_request(Some(api_key));
    req.extensions_mut()
        .insert(tower_rate_tier::TierCost(cost));
    req
}

fn make_layer(clock: FakeClock) -> TierLimitLayer {
    let rate_tier = RateTier::builder()
        .tier("free", Quota::per_second(2))
        .tier("pro", Quota::per_second(10))
        .tier("enterprise", Quota::unlimited())
        .default_tier("free")
        .clock(clock)
        .build();

    TierLimitLayer::new(rate_tier).identifier_fn(|headers| {
        let key = headers.get("x-api-key")?.to_str().ok()?;
        let tier = headers
            .get("x-tier")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("free");
        Some(TierIdentity::new(key, tier))
    })
}

#[tokio::test]
async fn allows_request_within_quota() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let layer = make_layer(clock);
    let mut svc = layer.layer(OkService);

    let resp = svc.call(build_request(Some("user1"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().contains_key("x-ratelimit-limit"));
    assert!(resp.headers().contains_key("x-ratelimit-remaining"));
    assert!(resp.headers().contains_key("x-ratelimit-reset"));
}

#[tokio::test]
async fn returns_429_when_over_quota() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let layer = make_layer(clock);
    let mut svc = layer.layer(OkService);

    // Exhaust free tier (2 requests/second)
    svc.call(build_request(Some("user1"))).await.unwrap();
    svc.call(build_request(Some("user1"))).await.unwrap();

    let resp = svc.call(build_request(Some("user1"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(resp.headers().contains_key("retry-after"));

    let body = resp.into_body();
    assert!(body.contains("rate limit exceeded"));
    assert!(body.contains("free"));
}

#[tokio::test]
async fn rate_limit_headers_on_success() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let layer = make_layer(clock);
    let mut svc = layer.layer(OkService);

    let resp = svc.call(build_request(Some("user1"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let remaining: u32 = resp
        .headers()
        .get("x-ratelimit-remaining")
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(remaining, 1); // 2 allowed, used 1, remaining 1
}

#[tokio::test]
async fn different_tiers_different_limits() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let layer = make_layer(clock);
    let mut svc = layer.layer(OkService);

    // Free user (2/sec) — exhausts after 2
    svc.call(build_request(Some("free-user"))).await.unwrap();
    svc.call(build_request(Some("free-user"))).await.unwrap();
    let resp = svc.call(build_request(Some("free-user"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

    // Pro user (10/sec) — build request with tier header
    let req = Request::builder()
        .header("x-api-key", "pro-user")
        .header("x-tier", "pro")
        .body(String::new())
        .unwrap();
    let resp = svc.call(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn unlimited_tier_always_passes() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let layer = make_layer(clock);
    let mut svc = layer.layer(OkService);

    for _ in 0..100 {
        let req = Request::builder()
            .header("x-api-key", "enterprise-user")
            .header("x-tier", "enterprise")
            .body(String::new())
            .unwrap();
        let resp = svc.call(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // Unlimited tiers should NOT have rate limit headers
        assert!(!resp.headers().contains_key("x-ratelimit-limit"));
    }
}

#[tokio::test]
async fn on_missing_use_default_applies_default_tier() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let layer = make_layer(clock);
    let mut svc = layer.layer(OkService);

    // No x-api-key header → identifier returns None → UseDefault → "free" tier
    let resp = svc.call(build_request(None)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // Second anonymous request still within free quota
    let resp = svc.call(build_request(None)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // Third anonymous request exceeds free quota (2/sec)
    let resp = svc.call(build_request(None)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn on_missing_allow_passes_through() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let rate_tier = RateTier::builder()
        .tier("free", Quota::per_second(1))
        .on_missing(OnMissing::Allow)
        .clock(clock)
        .build();

    let layer = TierLimitLayer::new(rate_tier).identifier_fn(|_headers| None);
    let mut svc = layer.layer(OkService);

    // All requests pass through without rate limiting
    for _ in 0..10 {
        let resp = svc.call(build_request(None)).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn on_missing_deny_returns_status() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let rate_tier = RateTier::builder()
        .tier("free", Quota::per_second(1))
        .on_missing(OnMissing::Deny(StatusCode::FORBIDDEN))
        .clock(clock)
        .build();

    let layer = TierLimitLayer::new(rate_tier).identifier_fn(|_headers| None);
    let mut svc = layer.layer(OkService);

    let resp = svc.call(build_request(None)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn cost_extension_affects_rate_limit() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let layer = make_layer(clock);
    let mut svc = layer.layer(OkService);

    // Free tier has 2/sec. Use cost=2 to exhaust in one request.
    let resp = svc
        .call(build_request_with_cost("user1", 2))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Next request should be denied
    let resp = svc.call(build_request(Some("user1"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn recovery_after_time_elapses() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let layer = make_layer(clock.clone());
    let mut svc = layer.layer(OkService);

    // Exhaust free tier
    svc.call(build_request(Some("user1"))).await.unwrap();
    svc.call(build_request(Some("user1"))).await.unwrap();
    let resp = svc.call(build_request(Some("user1"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);

    // Advance time by 1 second
    clock.advance(Duration::from_secs(1));

    let resp = svc.call(build_request(Some("user1"))).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
