//! JWT-based tier identification example.
//!
//! Run with: `cargo run --example axum_jwt`
//!
//! This example demonstrates using a `TierIdentifier` trait implementation
//! for more complex identification logic (parsing JWT claims).
//!
//! Test:
//!   # Simulated JWT with base64-encoded payload
//!   curl -H "Authorization: Bearer header.eyJ1c2VyX2lkIjoiYWxpY2UiLCJ0aWVyIjoicHJvIn0.sig" \
//!        http://localhost:3000/api/data

use std::future::Future;
use std::pin::Pin;

use axum::{routing::get, Router};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use http::HeaderMap;
use tower_rate_tier::{Quota, RateTier, TierIdentifier, TierIdentity, TierLimitLayer};

/// Identifier that extracts user_id and tier from JWT claims.
///
/// This is a simplified example — in production you would verify
/// the JWT signature and expiration.
struct JwtIdentifier;

impl TierIdentifier for JwtIdentifier {
    fn identify(&self, headers: &HeaderMap) -> Pin<Box<dyn Future<Output = Option<TierIdentity>> + Send + '_>> {
        let result = (|| {
            let auth = headers.get("authorization")?.to_str().ok()?;
            let token = auth.strip_prefix("Bearer ")?;

            // JWT format: header.payload.signature
            let parts: Vec<&str> = token.split('.').collect();
            if parts.len() != 3 {
                return None;
            }

            // Decode the payload (base64url → JSON)
            let payload = URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
            let claims: serde_json::Value = serde_json::from_slice(&payload).ok()?;

            let user_id = claims.get("user_id")?.as_str()?;
            let tier = claims.get("tier").and_then(|v| v.as_str()).unwrap_or("free");

            Some(TierIdentity::new(user_id, tier))
        })();
        Box::pin(std::future::ready(result))
    }
}

#[tokio::main]
async fn main() {
    let rate_tier = RateTier::builder()
        .tier("free", Quota::per_minute(10))
        .tier("pro", Quota::per_minute(100))
        .tier("enterprise", Quota::unlimited())
        .default_tier("free")
        .build();

    let layer = TierLimitLayer::new(rate_tier).identifier(JwtIdentifier);

    let app = Router::new()
        .route("/api/data", get(get_data))
        .layer(layer);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Listening on http://localhost:3000");
    println!("Try: curl -H 'Authorization: Bearer header.eyJ1c2VyX2lkIjoiYWxpY2UiLCJ0aWVyIjoicHJvIn0.sig' http://localhost:3000/api/data");
    axum::serve(listener, app).await.unwrap();
}

async fn get_data() -> &'static str {
    "data response"
}
