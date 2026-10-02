//! Per-route costs when the rate limiter is added with axum's `Router::layer`.

use std::convert::Infallible;

use axum::body::Body;
use axum::extract::MatchedPath;
use axum::routing::{get, post};
use axum::Router;
use http::{Method, Request, StatusCode};
use tower::ServiceExt;
use tower_rate_tier::clock::FakeClock;
use tower_rate_tier::{tier_cost, Quota, RateTier, TierIdentity, TierLimitLayer};

fn limiter() -> TierLimitLayer {
    let rate_tier = RateTier::builder()
        .tier("free", Quota::per_hour(100))
        .default_tier("free")
        .clock(FakeClock::new())
        .build();
    TierLimitLayer::new(rate_tier).identifier_fn(|_| Some(TierIdentity::new("u1", "free")))
}

fn request(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

async fn remaining_after(app: &Router, req: Request<Body>) -> String {
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    resp.headers()["x-ratelimit-remaining"]
        .to_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn cost_fn_sets_per_route_costs_behind_router_layer() {
    let app = Router::new()
        .route("/api/users/{id}", get(|| async { "user" }))
        .route("/api/export", post(|| async { "export" }))
        .route("/health", get(|| async { "ok" }))
        .layer(limiter().cost_fn(|req| {
            match req.extensions.get::<MatchedPath>().map(MatchedPath::as_str) {
                Some("/api/export") => 20,
                Some("/health") => 0,
                _ => 1,
            }
        }));

    assert_eq!(
        remaining_after(&app, request(Method::POST, "/api/export")).await,
        "80"
    );
    assert_eq!(
        remaining_after(&app, request(Method::GET, "/health")).await,
        "80"
    );
    assert_eq!(
        remaining_after(&app, request(Method::GET, "/api/users/7")).await,
        "79"
    );
}

#[tokio::test]
async fn tier_cost_wrapping_the_limiter_takes_precedence_over_cost_fn() {
    // On a method router the last `.layer()` runs first, so tier_cost wraps
    // the limiter here and its cost reaches it.
    let app: Router = Router::new().route(
        "/api/export",
        post(|| async { "export" })
            .layer::<_, Infallible>(limiter().cost_fn(|_| 1))
            .layer::<_, Infallible>(tier_cost(20)),
    );

    assert_eq!(
        remaining_after(&app, request(Method::POST, "/api/export")).await,
        "80"
    );
}
