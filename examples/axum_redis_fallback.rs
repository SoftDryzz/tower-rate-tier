//! Limits shared through Redis that keep working while Redis is down.
//!
//! - Rate-limit state lives in Redis (`RedisStorage`), shared by every
//!   instance.
//! - `CircuitBreaker` answers from local memory when Redis fails, and after
//!   3 failures in a row stops calling Redis for 5 seconds at a time.
//! - Tiers come from a map in the code, so identifying a request never
//!   needs Redis.
//!
//! - The Redis connection is lazy, so the service also starts while Redis
//!   is down.
//!
//! Run with: `cargo run --example axum_redis_fallback --features redis`
//! (Redis at `REDIS_URL`, default `redis://127.0.0.1:6379`).
//!
//! Test, then stop Redis and test again: the limit still applies.
//!   curl -i -H "x-api-key: key_free" http://localhost:3000/api/data
//!   curl -i -H "x-api-key: unknown" http://localhost:3000/api/data   # 401

use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use http::StatusCode;
use redis::aio::ConnectionManagerConfig;
use tower_rate_tier::storage::memory::MemoryStorage;
use tower_rate_tier::{
    BreakerEvent, CircuitBreaker, OnMissing, Quota, RateTier, RedisStorage, TierIdentity,
    TierLimitLayer,
};

/// The tier of each API key. A real service would load this at startup or
/// cache it, so that identification does not depend on Redis either.
fn tier_of(api_key: &str) -> Option<&'static str> {
    match api_key {
        "key_free" => Some("free"),
        "key_pro" => Some("pro"),
        _ => None,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
    // Lazy: connects on the first command instead of waiting here, so a
    // restart during an outage still serves requests (from the fallback).
    let conn =
        redis::Client::open(url)?.get_connection_manager_lazy(ConnectionManagerConfig::new())?;

    let storage = CircuitBreaker::new(RedisStorage::new(conn), MemoryStorage::new())
        .on_state_change(|event| match event {
            BreakerEvent::Opened { error, .. } => {
                eprintln!("rate limiter: redis down, limiting locally: {error}");
            }
            BreakerEvent::Closed { .. } => eprintln!("rate limiter: redis back"),
            _ => {}
        });

    let rate_tier = RateTier::builder()
        .tier("free", Quota::per_minute(5))
        .tier("pro", Quota::per_minute(100))
        .on_missing(OnMissing::Deny(StatusCode::UNAUTHORIZED))
        .storage(Arc::new(storage))
        .build();

    let layer = TierLimitLayer::new(rate_tier).identifier_fn(|headers| {
        let api_key = headers.get("x-api-key")?.to_str().ok()?;
        Some(TierIdentity::new(api_key, tier_of(api_key)?))
    });

    let app = Router::new()
        .route("/api/data", get(|| async { "data" }))
        .layer(layer);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    println!("Listening on http://localhost:3000");
    axum::serve(listener, app).await?;
    Ok(())
}
