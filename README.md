# tower-rate-tier

**Tier-based rate limiting middleware for Tower.**

[![Crates.io](https://img.shields.io/crates/v/tower-rate-tier.svg)](https://crates.io/crates/tower-rate-tier)
[![Documentation](https://docs.rs/tower-rate-tier/badge.svg)](https://docs.rs/tower-rate-tier)
[![License](https://img.shields.io/crates/l/tower-rate-tier.svg)](LICENSE)

Every SaaS API needs rate limiting by user plan (free/pro/enterprise). `tower-rate-tier` eliminates the 200-400 lines of custom middleware you'd otherwise write.

## Features

- **Named tiers** — Define `free`, `pro`, `enterprise` (or any names) with distinct quotas
- **Request cost/weight** — Expensive endpoints consume more quota (`/export` = 20, `/search` = 5)
- **Async identifier** — Extract `(user_id, tier)` from headers, JWT, API keys, or request body
- **GCRA algorithm** — Smooth rate enforcement, no burst-at-boundary issues (used by Stripe, GitHub, Shopify)
- **Pluggable storage** — In-memory (DashMap) or Redis for distributed setups
- **Testable clock** — Deterministic time control in tests with `FakeClock`
- **Standard headers** — `X-RateLimit-Limit`, `X-RateLimit-Remaining`, `X-RateLimit-Reset`, `Retry-After`
- **Tower-native** — Works with Axum, Tonic, Hyper, or any Tower-based framework

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
tower-rate-tier = "0.1"
```

### Define Tiers

```rust
use tower_rate_tier::{RateTier, Quota};

let tier = RateTier::builder()
    .tier("free", Quota::per_hour(100))
    .tier("pro", Quota::per_hour(5_000))
    .tier("enterprise", Quota::unlimited())
    .default_tier("free")
    .build();
```

### Identify Users

**With a closure** (simple cases):

```rust
use tower_rate_tier::TierIdentity;

let layer = TierLimitLayer::new(tier)
    .identifier_fn(|headers: &HeaderMap| {
        let api_key = headers.get("X-Api-Key")?.to_str().ok()?.to_owned();
        Some(TierIdentity::new(api_key, "free"))
    });
```

**With a trait** (async lookups):

```rust
use tower_rate_tier::{TierIdentifier, TierIdentity};

struct ApiKeyIdentifier { db: Pool }

impl TierIdentifier for ApiKeyIdentifier {
    async fn identify(&self, req: &HeaderMap) -> Option<TierIdentity> {
        let key = req.get("X-Api-Key")?.to_str().ok()?;
        let tier = self.db.get_tier(key).await?;
        Some(TierIdentity::new(key, tier))
    }
}

let layer = TierLimitLayer::new(tier)
    .identifier(ApiKeyIdentifier { db });
```

### Apply to Routes

```rust
use axum::{Router, routing::{get, post}};
use tower_rate_tier::tier_cost;

let app = Router::new()
    .route("/api/users", get(list_users))                   // cost: 1 (default)
    .route("/api/search", post(search).layer(tier_cost(5)))  // cost: 5
    .route("/api/export", post(export).layer(tier_cost(20))) // cost: 20
    .layer(layer);
```

### Rate Limit Response

When a user exceeds their quota, the middleware returns:

```http
HTTP/1.1 429 Too Many Requests
X-RateLimit-Limit: 100
X-RateLimit-Remaining: 0
X-RateLimit-Reset: 1710432000
Retry-After: 2450
Content-Type: application/json

{"error": "rate limit exceeded", "tier": "free", "retry_after": 2450}
```

## Optional Features

```toml
# Redis support for distributed rate limiting
tower-rate-tier = { version = "0.1", features = ["redis"] }

# Body-based identification (opt-in, buffers request body)
tower-rate-tier = { version = "0.1", features = ["buffered-body"] }
```

## Testing

Use `FakeClock` for deterministic rate limit tests:

```rust
use tower_rate_tier::clock::FakeClock;

#[tokio::test]
async fn test_rate_limit_expiry() {
    let clock = FakeClock::new();
    let limiter = RateTier::builder()
        .clock(clock.clone())
        .tier("free", Quota::per_hour(1))
        .build();

    assert!(limiter.check("user1", "free", 1).await.unwrap().is_ok());
    assert!(limiter.check("user1", "free", 1).await.unwrap().is_err());

    clock.advance(Duration::from_secs(3600));
    assert!(limiter.check("user1", "free", 1).await.unwrap().is_ok());
}
```

## Handling Unidentified Requests

Configure behavior when the identifier returns `None`:

```rust
let tier = RateTier::builder()
    .on_missing(OnMissing::UseDefault)           // Use default tier
    // .on_missing(OnMissing::Allow)              // No rate limiting
    // .on_missing(OnMissing::Deny(StatusCode::FORBIDDEN)) // Block
    .build();
```

## Storage Error Behavior

When Redis is unavailable:

```rust
let layer = TierLimitLayer::new(tier)
    .on_storage_error(OnStorageError::Allow);  // Fail open (default)
    // .on_storage_error(OnStorageError::Deny); // Fail closed (503)
```

## Comparison

| Feature | tower-governor | tokio-rate-limit | axum_gcra | **tower-rate-tier** |
|---------|---------------|-----------------|-----------|-------------------|
| Named tiers | No | No | No | **Yes** |
| Request cost/weight | No | No | No | **Yes** |
| Async identifier | No | Partial | No | **Yes** |
| Body-based identification | No | No | No | **Yes** |
| Distributed (Redis) | No | No | No | **Yes** |
| Testable clock | No | Yes | No | **Yes** |
| Tower-compatible | Yes | Axum only | Axum only | **Yes** |
| Algorithm | GCRA | Token bucket | GCRA | **GCRA** |

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT License ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.
