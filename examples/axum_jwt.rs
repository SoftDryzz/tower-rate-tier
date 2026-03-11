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

use async_trait::async_trait;
use axum::{routing::get, Router};
use http::HeaderMap;
use tower_rate_tier::{Quota, RateTier, TierIdentifier, TierIdentity, TierLimitLayer};

/// Identifier that extracts user_id and tier from JWT claims.
///
/// This is a simplified example — in production you would verify
/// the JWT signature and expiration.
struct JwtIdentifier;

#[async_trait]
impl TierIdentifier for JwtIdentifier {
    async fn identify(&self, headers: &HeaderMap) -> Option<TierIdentity> {
        let auth = headers.get("authorization")?.to_str().ok()?;
        let token = auth.strip_prefix("Bearer ")?;

        // JWT format: header.payload.signature
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return None;
        }

        // Decode the payload (base64url → JSON)
        let payload = base64_decode(parts[1])?;
        let claims: serde_json::Value = serde_json::from_slice(&payload).ok()?;

        let user_id = claims.get("user_id")?.as_str()?;
        let tier = claims.get("tier").and_then(|v| v.as_str()).unwrap_or("free");

        Some(TierIdentity::new(user_id, tier))
    }
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    // Minimal base64url decoder (no padding)
    let input = input.replace('-', "+").replace('_', "/");
    let padded = match input.len() % 4 {
        2 => format!("{}==", input),
        3 => format!("{}=", input),
        _ => input,
    };

    // Simple base64 decode using a lookup table
    let table: Vec<u8> = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/".to_vec();

    let mut output = Vec::new();
    let bytes: Vec<u8> = padded.bytes().collect();

    for chunk in bytes.chunks(4) {
        if chunk.len() != 4 {
            return None;
        }
        let vals: Vec<Option<usize>> = chunk
            .iter()
            .map(|&b| {
                if b == b'=' {
                    Some(0)
                } else {
                    table.iter().position(|&t| t == b)
                }
            })
            .collect();

        if vals.iter().any(|v| v.is_none()) {
            return None;
        }
        let vals: Vec<usize> = vals.into_iter().map(|v| v.unwrap()).collect();

        let n = (vals[0] << 18) | (vals[1] << 12) | (vals[2] << 6) | vals[3];
        output.push((n >> 16) as u8);
        if chunk[2] != b'=' {
            output.push((n >> 8 & 0xFF) as u8);
        }
        if chunk[3] != b'=' {
            output.push((n & 0xFF) as u8);
        }
    }

    Some(output)
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
