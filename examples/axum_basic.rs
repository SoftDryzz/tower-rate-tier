//! Basic Axum example with closure-based tier identification.
//!
//! Run with: `cargo run --example axum_basic`
//!
//! Test:
//!   curl -H "x-api-key: user1" http://localhost:3000/api/users
//!   curl -H "x-api-key: user1" http://localhost:3000/api/search

use axum::{routing::get, Router};
use tower_rate_tier::{tier_cost, Quota, RateTier, TierIdentity, TierLimitLayer};

#[tokio::main]
async fn main() {
    // 1. Define tiers with quotas
    let rate_tier = RateTier::builder()
        .tier("free", Quota::per_minute(10))
        .tier("pro", Quota::per_minute(100))
        .tier("enterprise", Quota::unlimited())
        .default_tier("free")
        .build();

    // 2. Create the rate limit layer with a closure identifier
    let layer = TierLimitLayer::new(rate_tier).identifier_fn(|headers| {
        let api_key = headers.get("x-api-key")?.to_str().ok()?;
        // In production: look up tier from database/cache
        let tier = match api_key {
            "pro-user" => "pro",
            "enterprise-user" => "enterprise",
            _ => "free",
        };
        Some(TierIdentity::new(api_key, tier))
    });

    // 3. Build routes with per-endpoint costs
    let app = Router::new()
        .route("/api/users", get(list_users))
        .route(
            "/api/search",
            get(search).layer(tier_cost(5)), // costs 5 units
        )
        .layer(layer);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Listening on http://localhost:3000");
    axum::serve(listener, app).await.unwrap();
}

async fn list_users() -> &'static str {
    "users list"
}

async fn search() -> &'static str {
    "search results"
}
