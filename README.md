# tower-rate-tier

**Tier-based rate limiting middleware for Tower.**

[![Crates.io](https://img.shields.io/crates/v/tower-rate-tier.svg)](https://crates.io/crates/tower-rate-tier)
[![Documentation](https://docs.rs/tower-rate-tier/badge.svg)](https://docs.rs/tower-rate-tier)
[![CI](https://github.com/SoftDryzz/tower-rate-tier/actions/workflows/ci.yml/badge.svg)](https://github.com/SoftDryzz/tower-rate-tier/actions/workflows/ci.yml)
[![License](https://img.shields.io/crates/l/tower-rate-tier.svg)](LICENSE-MIT)

Every SaaS API needs rate limiting by user plan (free/pro/enterprise). `tower-rate-tier` eliminates the 200-400 lines of custom middleware you'd otherwise write.

**Walkthrough (in Spanish):** [Limitar una API por plan en Rust: GCRA, Redis y tower-rate-tier](https://softdryzz.com/blog/posts/tower-rate-tier-limitar-por-plan) explains why GCRA instead of a fixed window, requests that cost more, limits shared across instances with Redis and the safe defaults, with an example service and real output.

## Features

- **Named tiers** — Define `free`, `pro`, `enterprise` (or any names) with distinct quotas
- **Request cost/weight** — Expensive endpoints consume more quota (`/export` = 20, `/search` = 5)
- **Async identifier** — Extract `(user_id, tier)` from headers, JWT, API keys, or request body
- **GCRA algorithm** — Smooth rate enforcement, with no burst at window boundaries
- **Shared limits across instances** — Redis backend (feature `redis`) with an atomic GCRA script and Redis's own clock
- **Pluggable storage** — In-memory (DashMap) with automatic GC, Redis, or your own via the `Storage` trait
- **Safe defaults** — Unknown tiers and unidentified requests never bypass the limit unless you opt in
- **Testable clock** — Deterministic time control in tests with `FakeClock`
- **Standard headers** — `X-RateLimit-Limit`, `Remaining`, `Reset` (Unix timestamp), `Retry-After`
- **Callbacks** — `on_limited` and `on_event` for metrics/logging, custom 429 response builder
- **Tower-native** — Works with Axum, Hyper, or any Tower service whose response body can be built from a `String` (Tonic support is planned)

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
tower-rate-tier = "0.3"
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
use tower_rate_tier::{TierLimitLayer, TierIdentity};

let layer = TierLimitLayer::new(tier)
    .identifier_fn(|headers| {
        let api_key = headers.get("X-Api-Key")?.to_str().ok()?.to_owned();
        Some(TierIdentity::new(api_key, "free"))
    });
```

### Apply to Routes

Give expensive endpoints a higher cost with `cost_fn`. It runs inside the
middleware, so it works when the limiter is added with `Router::layer`:

```rust
use axum::{Router, extract::MatchedPath, routing::{get, post}};

let layer = layer.cost_fn(|req| {
    match req.extensions.get::<MatchedPath>().map(MatchedPath::as_str) {
        Some("/api/search") => 5,  // cost: 5
        Some("/api/export") => 20, // cost: 20
        Some("/health") => 0,      // free (no quota consumed)
        _ => 1,                    // cost: 1 (default)
    }
});

let app = Router::new()
    .route("/api/users", get(list_users))
    .route("/api/search", post(search))
    .route("/api/export", post(export))
    .route("/health", get(health))
    .layer(layer);
```

The `tier_cost(n)` layer also sets a cost, but it must wrap the limiter. A
route's own `.layer(tier_cost(n))` runs *inside* a limiter added with
`Router::layer`, so its cost arrives too late and is ignored.

### Rate Limit Response

When a user exceeds their quota, the middleware returns:

```http
HTTP/1.1 429 Too Many Requests
X-RateLimit-Limit: 100
X-RateLimit-Remaining: 0
X-RateLimit-Reset: 1710432000
Retry-After: 2450
Content-Type: application/json

{"error":"rate limit exceeded","tier":"free","retry_after":2450}
```

### Custom 429 Response

```rust
let layer = TierLimitLayer::new(tier)
    .identifier_fn(|headers| { /* ... */ None })
    .rate_limited_response(|_user_id, tier, limited| {
        Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .header("Content-Type", "application/problem+json")
            .header("Retry-After", limited.retry_after_secs())
            .body(format!(r#"{{"type":"rate_limit","tier":"{}"}}"#, tier))
            .unwrap()
    });
```

### Metrics / Logging

```rust
let layer = TierLimitLayer::new(tier)
    .identifier_fn(|headers| { /* ... */ None })
    .on_limited(|user_id, tier, limited| {
        eprintln!("rate limited: user={user_id} tier={tier} retry_after={:?}", limited.retry_after);
    })
    .on_event(|event| match event {
        LimitEvent::StorageError { error, .. } => eprintln!("rate limit storage failed: {error}"),
        LimitEvent::UnknownTier { user_id, tier, .. } => eprintln!("unknown tier {tier} for {user_id}"),
        LimitEvent::CostExceedsLimit { cost, limit, .. } => eprintln!("cost {cost} > limit {limit}"),
        _ => {}
    });
```

A request whose cost is above its tier's limit can never succeed, so it is
answered with `403 Forbidden` and no `Retry-After`, without touching storage.

## Optional Features

```toml
# Body-based identification (opt-in, buffers request body)
tower-rate-tier = { version = "0.3", features = ["buffered-body"] }

# Limits shared by every instance through Redis
tower-rate-tier = { version = "0.3", features = ["redis"] }
```

## Redis: Limits Shared by Every Instance

With the `redis` feature, `RedisStorage` keeps the rate-limit state in Redis,
so every instance of a service enforces the same limits:

```rust
use std::sync::Arc;
use tower_rate_tier::{Quota, RateTier, RedisStorage};

let conn = redis::Client::open("redis://127.0.0.1:6379")?
    .get_connection_manager()
    .await?;

let tier = RateTier::builder()
    .tier("free", Quota::per_hour(100))
    .storage(Arc::new(RedisStorage::new(conn)))
    .build();
```

- Each check is one atomic Lua script (`EVALSHA`, reloaded after `NOSCRIPT`).
  Time comes from Redis's `TIME`, so instances never disagree about the clock.
- Keys are `trt:<tier>:<sha1(user_id)>` and expire when the bucket is full
  again, so nothing needs cleaning up. For ids with little entropy (IP
  addresses, emails), add `.key_secret(secret)` so they cannot be recovered
  from Redis.
- A check that takes longer than 100 ms (`.timeout(..)`) counts as a storage
  error and follows the [storage error policy](#storage-error-behavior).
- Works with `ConnectionManager`, multiplexed and cluster connections.

See [`examples/axum_api_key.rs`](examples/axum_api_key.rs) for a complete
service that also looks up each API key's tier in Redis.

## Custom Storage Backend

Implement the `Storage` trait for any other backend. It receives a
`StorageKey { user_id, tier }`; encode it so that distinct pairs never share
state (for example, length-prefix the parts instead of joining them with `:`).

```rust
let custom_storage: Arc<dyn Storage> = Arc::new(MyStorage::new());

let tier = RateTier::builder()
    .tier("free", Quota::per_hour(100))
    .storage(custom_storage) // GC disabled automatically for custom backends
    .build();
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

```rust
let tier = RateTier::builder()
    .on_missing(OnMissing::UseDefault)           // Use default tier (403 if none is set)
    // .on_missing(OnMissing::Allow)              // No rate limiting
    // .on_missing(OnMissing::Deny(StatusCode::FORBIDDEN)) // Block
    .build();
```

## Handling Unknown Tiers

If the identifier returns a tier that is not configured (a typo, a plan the
limiter does not know yet, or a value a client can influence), the request is
**not** let through unlimited. By default it gets the default tier's quota in
the user's own bucket, or `403 Forbidden` when no default tier is set:

```rust
let tier = RateTier::builder()
    .on_unknown_tier(OnUnknownTier::UseDefault)              // Default tier's quota (default)
    // .on_unknown_tier(OnUnknownTier::Deny(StatusCode::FORBIDDEN)) // Block
    // .on_unknown_tier(OnUnknownTier::Allow)                  // No rate limiting (opt-in)
    .build();
```

## Storage Error Behavior

```rust
let layer = TierLimitLayer::new(tier)
    .on_storage_error(OnStorageError::Allow);  // Fail open (default)
    // .on_storage_error(OnStorageError::Deny); // Fail closed (503)
```

## Minimum Supported Rust Version

Rust 1.75 for the default features and `buffered-body`, and Rust 1.88 with
`redis` (required by the `redis` crate). The MSRV is only raised in minor
releases, and every raise is noted in the [changelog](CHANGELOG.md).

## Upgrading from 0.2

0.3 has breaking changes; the [changelog](CHANGELOG.md) lists them all. The
ones that need code changes:

- `RateLimitInfo` and `RateLimited`: `reset_at` is now `reset_after: Duration`.
  Use `RateLimited::retry_after_secs()` for a `Retry-After` header.
- Custom `Storage` backends: `check_and_update` receives a
  `StorageKey { user_id, tier }` instead of a joined `&str`.
- `match` on `OnMissing`, `OnStorageError` or `CheckError` needs a `_ =>` arm.
- Per-route costs under axum's `Router::layer`: use `cost_fn` instead of a
  route's own `.layer(tier_cost(n))`, which was silently ignored.

Behavior that changed on purpose:

- An unknown tier gets the default tier's quota instead of no limit.
- `OnMissing::UseDefault` without a default tier answers 403 instead of no
  limit; use `OnMissing::Allow` to keep the old behavior.
- A request costing more than the tier's limit gets 403 instead of a 429
  that could never succeed.

## Comparison

Checked against each crate's source in October 2026:

| Feature | tower_governor 0.8 | tokio-rate-limit 0.10 | axum_gcra 0.1 | **tower-rate-tier 0.3** |
|---------|--------------------|-----------------------|---------------|-------------------------|
| Named tiers (per-plan quotas) | No | No | No | **Yes** |
| Limits shared across instances | No | No | No | **Yes (Redis)** |
| Request cost/weight | No | Yes | No | **Yes** |
| Custom storage backend | No | No | No | **Yes** |
| Injectable clock for tests | No | Paused Tokio time | Explicit `now` argument | **`FakeClock`** |
| Frameworks | Tower layer | Axum, Tonic | Axum | **Tower layer (Axum, Hyper)** |
| Algorithm | GCRA | Token / leaky bucket | GCRA | **GCRA** |

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT License ([LICENSE-MIT](LICENSE-MIT) or <http://opensource.org/licenses/MIT>)

at your option.
