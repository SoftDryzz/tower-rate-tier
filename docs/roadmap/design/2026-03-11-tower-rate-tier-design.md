# tower-rate-tier — Design Spec

**Date:** 2026-03-11
**Author:** SoftDryzz (OpenCode)
**Status:** Draft
**Tagline:** Tier-based rate limiting middleware for Tower

---

## 1. Problem Statement

Every SaaS API needs rate limiting by user plan (free/pro/enterprise). In Rust, the current options are:

- **tower-governor** — Global or per-key rate limiting, but no concept of tiers. Open issues (#59, #37, #35, #50) for features like multi-rate-per-key, distributed storage, body access in extractors, and testable clocks remain unresolved.
- **tokio-rate-limit** — Per-key with pluggable algorithms, but no tier abstraction. Devs must wire tier logic manually.
- **axum_gcra** — Per-route quotas, Axum-only, no tier concept.

**Result:** Every team writes 200-400 lines of custom middleware to implement tier-based rate limiting. This crate eliminates that.

## 2. Solution

`tower-rate-tier` is a Tower middleware library that provides declarative, tier-based rate limiting with:

- Configurable tiers with named quotas
- Async user/tier identification from requests
- Per-endpoint request cost/weight
- Pluggable storage (in-memory, Redis)
- Testable clock support
- Standard rate limit headers (RFC 6585)

## 3. Target Users

- Rust developers building SaaS APIs with Axum, Tonic, Hyper, or any Tower-based framework
- Teams that need different rate limits per subscription plan
- Developers migrating from Express/Django who expect tier-based rate limiting out of the box

## 4. API Design

### 4.1 Defining Tiers

```rust
use tower_rate_tier::{RateTier, Quota};

let tier = RateTier::builder()
    .tier("free", Quota::per_hour(100))
    .tier("pro", Quota::per_hour(5_000))
    .tier("enterprise", Quota::unlimited())
    .default_tier("free")
    .on_missing(OnMissing::UseDefault) // | Allow | Deny(StatusCode)
    .build();
```

### 4.2 Identifier

The developer provides an identifier to extract `(user_id, tier_name)` from each request. Two approaches are supported:

**Approach A: Trait implementation** (most flexible, recommended for complex logic)

```rust
use tower_rate_tier::{TierIdentifier, TierIdentity};

struct ApiKeyIdentifier {
    redis: RedisPool,
}

impl TierIdentifier for ApiKeyIdentifier {
    async fn identify(&self, req: &HeaderMap) -> Option<TierIdentity> {
        let api_key = req.get("X-Api-Key")?.to_str().ok()?;
        let tier = self.redis.get_tier(api_key).await?;
        Some(TierIdentity::new(api_key, tier))
    }
}

let layer = TierLimitLayer::new(tier)
    .identifier(ApiKeyIdentifier { redis });
```

**Approach B: Closure** (simple cases — returns boxed future internally)

```rust
let layer = TierLimitLayer::new(tier)
    .identifier_fn(|headers: &HeaderMap| {
        let api_key = headers.get("X-Api-Key")?.to_str().ok()?.to_owned();
        let tier = "free".to_string(); // sync-only in closures
        Some(TierIdentity::new(api_key, tier))
    });
```

The identifier receives `&HeaderMap` (not the full request) to avoid body ownership issues. For body-based identification, see section 4.7.

**Note on async trait:** The `TierIdentifier` trait uses `#[async_trait]` from the `async-trait` crate to support `dyn TierIdentifier + Send + Sync`. The middleware stores the identifier as `Arc<dyn TierIdentifier + Send + Sync>`. Closures are sync-only for ergonomics; use the trait for async lookups.

### 4.3 Applying the Middleware

```rust
use axum::{Router, routing::get};
use tower_rate_tier::tier_cost;

let app = Router::new()
    .route("/api/users", get(list_users))                      // cost: 1 (default)
    .route("/api/search", post(search).layer(tier_cost(5)))     // cost: 5
    .route("/api/export", post(export).layer(tier_cost(20)))    // cost: 20
    .layer(layer);
```

### 4.4 Response on Rate Limit Exceeded

Automatic `429 Too Many Requests` with standard headers:

```
HTTP/1.1 429 Too Many Requests
X-RateLimit-Limit: 100
X-RateLimit-Remaining: 0
X-RateLimit-Reset: 1710432000
Retry-After: 2450
Content-Type: application/json

{"error": "rate limit exceeded", "tier": "free", "retry_after": 2450}
```

All successful responses also include rate limit headers:

```
X-RateLimit-Limit: 5000
X-RateLimit-Remaining: 4832
X-RateLimit-Reset: 1710432000
```

### 4.5 Storage Backends

```toml
# In-memory only (default, single server)
[dependencies]
tower-rate-tier = "0.1"

# With Redis support (distributed, multi-server)
[dependencies]
tower-rate-tier = { version = "0.1", features = ["redis"] }
```

In-memory storage uses `DashMap` for lock-free concurrent access with automatic garbage collection of expired keys.

Redis storage enables distributed rate limiting across multiple server instances.

### 4.6 Testable Clock

`RateTier::build()` returns a `RateLimiter` handle that exposes a programmatic `check()` API in addition to the Tower middleware. This is useful for testing and for non-HTTP use cases.

```rust
#[cfg(test)]
use tower_rate_tier::clock::FakeClock;

#[tokio::test]
async fn test_rate_limit_expiry() {
    let clock = FakeClock::new();
    let limiter = RateTier::builder()
        .clock(clock.clone())
        .tier("free", Quota::per_hour(1))
        .build();

    // First request: allowed
    assert!(limiter.check("user1", "free", 1).is_ok());

    // Second request: denied (cost=1, quota=1/hour exhausted)
    assert!(limiter.check("user1", "free", 1).is_err());

    // Advance time 1 hour
    clock.advance(Duration::from_secs(3600));

    // Now allowed again
    assert!(limiter.check("user1", "free", 1).is_ok());
}
```

The `check(user_id, tier_name, cost)` method returns `Result<RateLimitInfo, RateLimited>` where both variants carry the current rate limit state (remaining, reset, limit).

### 4.7 Body-Based Identification

Body access requires buffering the request body, which has memory and type implications. This is opt-in via the `buffered_body` feature:

```toml
tower-rate-tier = { version = "0.1", features = ["buffered-body"] }
```

When enabled, the middleware buffers the request body (up to a configurable `max_body_size`, default 64KB) before calling the identifier. The body is then reconstructed for the downstream service.

```rust
let layer = TierLimitLayer::new(tier)
    .identifier(BodyIdentifier { max_body_size: 8192 })
    .buffer_body(true);  // opt-in

// The TierIdentifier trait gains an optional method:
impl TierIdentifier for LoginIdentifier {
    async fn identify(&self, headers: &HeaderMap) -> Option<TierIdentity> {
        None // fallback: not identified by headers alone
    }

    async fn identify_with_body(&self, headers: &HeaderMap, body: &Bytes) -> Option<TierIdentity> {
        let login: LoginReq = serde_json::from_slice(body).ok()?;
        Some(TierIdentity::new(login.username, "default"))
    }
}
```

**Constraints:**
- Max body size is enforced (default 64KB) to prevent DoS
- If body exceeds max size, returns `413 Payload Too Large` immediately (does NOT fall back to header identification, to prevent rate-limit bypass)
- When `buffer_body(true)`, the service's body type becomes `Full<Bytes>` instead of the original `B`
- This feature is gated because it changes the Tower Service generic constraints
- **Security note:** Endpoints using body-based identification should set `on_missing(OnMissing::Deny(StatusCode::FORBIDDEN))` to prevent unauthenticated bypass

## 5. Architecture

### 5.1 Crate Structure

```
tower-rate-tier/
├── src/
│   ├── lib.rs              # Public re-exports
│   ├── quota.rs            # Quota type, time windows (per_second, per_minute, per_hour, unlimited)
│   ├── tier.rs             # RateTier, RateTierBuilder
│   ├── layer.rs            # TierLimitLayer (implements tower::Layer)
│   ├── service.rs          # TierLimitService (implements tower::Service)
│   ├── identifier.rs       # TierIdentifier trait + closure adapter
│   ├── cost.rs             # tier_cost() layer for per-endpoint weighting
│   ├── response.rs         # 429 response builder, rate limit headers
│   ├── on_missing.rs       # OnMissing enum (UseDefault, Allow, Deny(StatusCode))
│   ├── on_storage_error.rs # OnStorageError enum (Allow, Deny)
│   ├── clock.rs            # Clock trait, SystemClock, FakeClock
│   ├── gcra.rs             # GCRA algorithm implementation
│   ├── gc.rs               # Garbage collection for expired entries
│   └── storage/
│       ├── mod.rs          # Storage trait
│       ├── memory.rs       # DashMap-based in-memory storage
│       └── redis.rs        # Redis storage (feature = "redis")
├── tests/
│   ├── integration.rs      # Full middleware integration tests
│   ├── gcra_tests.rs       # Algorithm correctness tests
│   ├── tier_tests.rs       # Tier configuration tests
│   ├── storage_tests.rs    # Storage backend tests
│   └── cost_tests.rs       # Request cost tests
├── examples/
│   ├── axum_basic.rs       # Basic Axum setup
│   ├── axum_jwt.rs         # JWT-based tier identification
│   ├── axum_api_key.rs     # API key with Redis tier lookup
│   └── tonic_grpc.rs       # gRPC with Tonic
├── Cargo.toml
├── README.md
└── LICENSE                 # MIT OR Apache-2.0
```

### 5.2 Core Flow

```
Request arrives
    │
    ▼
TierLimitService receives request
    │
    ▼
Calls identifier(req, body?) ──► Returns Option<(user_id, tier_name)>
    │                                    │
    │                            None ◄──┘
    │                              │
    │                     OnMissing::UseDefault → use default tier
    │                     OnMissing::Allow → pass through (no limiting)
    │                     OnMissing::Deny(status) → return the given status code
    │
    ▼
Lookup Quota for tier_name from RateTier config
    │
    ▼
Read request cost (from tier_cost layer extension, default = 1)
    │
    ▼
Storage.check(user_id, quota, cost) ──► GCRA algorithm
    │
    ├── Allowed → Add X-RateLimit-* headers → Forward to inner service
    │
    └── Denied → Return 429 with Retry-After header
```

### 5.3 Algorithm: GCRA

The Generic Cell Rate Algorithm (GCRA) is used instead of fixed-window counters. Advantages:

- No burst-at-boundary problem (where a user can make 2x requests at the edge of two windows)
- Smooth rate enforcement
- Single timestamp storage per key (memory efficient)
- Industry standard (used by Stripe, GitHub, Shopify)

With request cost, each check consumes N cells instead of 1.

**`Quota::unlimited()` behavior:** Unlimited tiers bypass the GCRA check entirely — no storage lookup, no computation. The middleware still calls the identifier (to log/trace the user) but skips rate limiting. No `X-RateLimit-*` headers are emitted for unlimited tiers.

### 5.4 Redis Atomicity

The Redis storage backend uses a Lua script to perform atomic GCRA operations. The script executes GET + compute + SET in a single `EVAL` call, avoiding race conditions under concurrent requests for the same key. This is the same approach used by Shopify's `redis-gcra` and Stripe's rate limiter.

```lua
-- Simplified: atomic GCRA in Redis
local tat = tonumber(redis.call('GET', KEYS[1])) or 0
local new_tat = math.max(tat, now) + emission_interval * cost
if new_tat - now > limit_period then
    return {0, new_tat - now}  -- denied, retry_after
end
redis.call('SET', KEYS[1], new_tat, 'EX', ttl)
return {1, 0}  -- allowed
```

### 5.5 Error Handling

The Tower `Service::Error` type is the inner service's error type. Rate limiting errors are **never** propagated as `Service::Error`:

- **429 rate limited** → normal HTTP response, not an error
- **Identifier returns `None`** → handled by `OnMissing` policy
- **Storage failure (Redis down)** → configurable via `on_storage_error`:
  - `OnStorageError::Allow` (default) — fail open, let request through
  - `OnStorageError::Deny` — fail closed, return 503 Service Unavailable

This ensures the middleware never breaks the Tower service contract.

### 5.6 tier_cost Ordering

`tier_cost(N)` works by inserting a `TierCost(N)` value into the request's `Extensions` map. The `TierLimitService` reads this extension when processing the request.

**Ordering requirement:** `tier_cost()` must be applied as an inner layer (per-route), while `TierLimitLayer` is applied as an outer layer (on the router). This is the natural pattern in Axum — route-level layers run before router-level layers.

If no `TierCost` extension is found, the default cost of 1 is used. This is intentional and safe — the common case is cost=1.

### 5.7 Dependencies

```toml
[dependencies]
tower = { version = "0.5", features = ["util"] }
tower-layer = "0.3"
tower-service = "0.3"
http = "1"
http-body = "1"
bytes = "1"
dashmap = "6"
tokio = { version = "1", features = ["time"] }
pin-project-lite = "0.2"
async-trait = "0.1"

[dependencies.http-body-util]
version = "0.1"
optional = true

[dev-dependencies]
axum = "0.8"
tokio = { version = "1", features = ["full", "test-util"] }
hyper = "1"
serde_json = "1"

[features]
default = []
redis = ["dep:redis"]
buffered-body = ["dep:http-body-util"]

[dependencies.redis]
version = "0.27"
features = ["tokio-comp"]
optional = true
```

## 6. Competitive Positioning

| Feature | tower-governor | tokio-rate-limit | axum_gcra | **tower-rate-tier** |
|---------|---------------|-----------------|-----------|-------------------|
| Named tiers (free/pro/etc) | No | No | No | **Yes** |
| Request cost/weight | No | No | No | **Yes** |
| Async identifier | No | Partial | No | **Yes** |
| Body access in identifier | No (issue #35) | No | No | **Yes** |
| Distributed (Redis) | No (issue #37) | No | No | **Yes** |
| Testable clock | No (issue #50) | Yes | No | **Yes** |
| Multi-rate per route | No (issue #59) | No | Yes | **Yes (via cost)** |
| Tower-compatible | Yes | Axum only | Axum only | **Yes** |
| GCRA algorithm | Yes | Token bucket | GCRA | **GCRA** |
| GC for expired keys | No | Yes | No | **Yes** |
| On-missing behavior | N/A | N/A | N/A | **Configurable** |

## 7. Success Criteria

### v0.1.0 (MVP)
- [x] Core: RateTier builder, Quota, GCRA
- [x] Tower Layer/Service implementation
- [x] Async identifier with closure support
- [x] In-memory storage with DashMap + GC
- [x] Request cost/weight layer
- [x] OnMissing behavior
- [x] Standard rate limit headers
- [x] 429 response with JSON body
- [x] FakeClock for testing
- [x] Body access in identifier (feature-gated `buffered-body`)
- [x] Examples: axum_basic, axum_jwt
- [x] README with usage guide
- [ ] Published to crates.io

### v0.2.0
- [ ] Redis storage backend
- [ ] Example: axum_api_key with Redis
- [ ] Metrics/events hook (on_limited callback)
- [ ] Custom 429 response body

### v0.3.0
- [ ] Tonic/gRPC example
- [ ] Dynamic tier updates at runtime
- [ ] Dashboard-ready metrics export

## 8. License

Dual-licensed under MIT OR Apache-2.0 (standard for Rust ecosystem libraries, maximizes adoption).

## 9. Marketing Strategy

1. **Respond to open issues** — Link tower-rate-tier as a solution in tower-governor #59, #37, #35, #50 and loco-rs #1181
2. **r/rust post** — "I built tower-rate-tier: tier-based rate limiting for Tower" with clear comparison table
3. **Blog post** — "Why your SaaS API needs tier-based rate limiting (and how to do it in Rust)"
4. **README-driven** — Excellent README with copy-paste examples (this is how most crates get adopted)
5. **Examples that work** — Runnable examples that compile and demonstrate real use cases
