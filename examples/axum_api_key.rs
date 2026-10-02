//! API-key rate limiting shared by every instance through Redis.
//!
//! - Each request's `x-api-key` is resolved to a tier with a Redis lookup.
//! - Rate-limit state lives in Redis (`RedisStorage`), so all instances of
//!   the service share the same limits.
//! - Expensive endpoints cost more (`cost_fn`).
//!
//! Run with: `cargo run --example axum_api_key --features redis`
//!
//! Setup (a Redis reachable at `REDIS_URL`, default `redis://127.0.0.1:6379`):
//!   redis-cli SET tier:key_abc123 pro
//!   redis-cli SET tier:key_xyz789 free
//!
//! Optional: set `RATE_LIMIT_KEY_SECRET` so the API keys in rate-limit keys
//! are hashed with a secret (see `RedisStorage::key_secret`).
//!
//! Test:
//!   curl -i -H "x-api-key: key_abc123" http://localhost:3000/api/data
//!   curl -i -H "x-api-key: key_xyz789" -X POST http://localhost:3000/api/export
//!   curl -i -H "x-api-key: unknown" http://localhost:3000/api/data   # 401

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use axum::routing::{get, post};
use axum::Router;
use http::{HeaderMap, StatusCode};
use redis::aio::ConnectionManager;
use redis::AsyncTypedCommands;
use tower_rate_tier::{
    OnMissing, Quota, RateTier, RedisStorage, TierIdentifier, TierIdentity, TierLimitLayer,
};

/// Resolves an API key to its tier with a Redis lookup (`tier:<api key>`).
struct RedisTierLookup {
    conn: ConnectionManager,
}

impl TierIdentifier for RedisTierLookup {
    fn identify(
        &self,
        headers: &HeaderMap,
    ) -> Pin<Box<dyn Future<Output = Option<TierIdentity>> + Send + '_>> {
        let api_key = headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut conn = self.conn.clone();

        Box::pin(async move {
            let api_key = api_key?;
            // A slow lookup must not hang the request; treat it as unidentified.
            let lookup = conn.get(format!("tier:{api_key}"));
            let tier = tokio::time::timeout(Duration::from_millis(100), lookup)
                .await
                .ok()?
                .ok()??;
            Some(TierIdentity::new(api_key, tier))
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
    let conn = redis::Client::open(url)?.get_connection_manager().await?;

    let mut storage = RedisStorage::new(conn.clone());
    if let Ok(secret) = std::env::var("RATE_LIMIT_KEY_SECRET") {
        storage = storage.key_secret(secret);
    }

    let rate_tier = RateTier::builder()
        .tier("free", Quota::per_minute(10))
        .tier("pro", Quota::per_minute(100))
        .tier("enterprise", Quota::unlimited())
        .default_tier("free")
        // Unknown or missing API keys are rejected, never given a tier.
        .on_missing(OnMissing::Deny(StatusCode::UNAUTHORIZED))
        .storage(Arc::new(storage))
        .build();

    let layer = TierLimitLayer::new(rate_tier)
        .identifier(RedisTierLookup { conn })
        .cost_fn(|req| match req.uri.path() {
            "/api/export" => 5,
            _ => 1,
        });

    let app = Router::new()
        .route("/api/data", get(|| async { "data" }))
        .route("/api/export", post(|| async { "export started" }))
        .layer(layer);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await?;
    println!("Listening on http://localhost:3000");
    axum::serve(listener, app).await?;
    Ok(())
}
