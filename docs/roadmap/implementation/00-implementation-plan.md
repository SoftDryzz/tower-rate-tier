# tower-rate-tier — Implementation Plan

**Date:** 2026-03-11
**Reference:** [Design Spec](../design/2026-03-11-tower-rate-tier-design.md)

---

## Overview

This document defines the step-by-step implementation order for `tower-rate-tier` v0.1.0. Each phase builds on the previous one and is independently testable.

## Phase 1: Foundation (Core Types)

**Goal:** Define the data types that everything else depends on.

### Files to create:
- `Cargo.toml` — Project manifest with dependencies
- `src/lib.rs` — Public re-exports
- `src/quota.rs` — `Quota` type with `per_second`, `per_minute`, `per_hour`, `unlimited`
- `src/clock.rs` — `Clock` trait, `SystemClock`, `FakeClock`

### Details:

**`Quota`** encapsulates a rate limit as `(max_burst, replenish_interval)`:
- `Quota::per_hour(100)` → 100 requests per hour
- `Quota::per_minute(10)` → 10 requests per minute
- `Quota::per_second(5)` → 5 requests per second
- `Quota::unlimited()` → sentinel value, bypasses GCRA entirely

**`Clock`** abstracts time for testability:
```rust
pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> Instant;
}
```
- `SystemClock` — wraps `tokio::time::Instant::now()`
- `FakeClock` — `Arc<AtomicU64>` with `advance(Duration)`

### Tests:
- `tests/quota_tests.rs` — Quota construction and interval calculations
- Unit tests in `clock.rs` — FakeClock advance behavior

---

## Phase 2: GCRA Algorithm

**Goal:** Implement the core rate limiting logic, independent of Tower.

### Files to create:
- `src/gcra.rs` — GCRA `check` function

### Details:

The GCRA function signature:
```rust
pub fn check_gcra(
    state: Option<Nanos>,  // previous TAT (theoretical arrival time)
    now: Nanos,
    emission_interval: Nanos,
    burst_offset: Nanos,
    cost: u32,
) -> Result<(Nanos, RateLimitInfo), RateLimited>
```

Where:
- `emission_interval` = window_duration / max_requests
- `burst_offset` = emission_interval * max_burst
- `new_tat = max(old_tat, now) + emission_interval * cost`
- Allowed if `new_tat - now <= burst_offset`

`RateLimitInfo` carries `{ remaining, limit, reset_at }` for both allowed and denied cases.

### Tests:
- `tests/gcra_tests.rs` — Single request, burst, over-limit, recovery after time, cost > 1

---

## Phase 3: Storage

**Goal:** Persist GCRA state per user key.

### Files to create:
- `src/storage/mod.rs` — `Storage` trait
- `src/storage/memory.rs` — `MemoryStorage` with `DashMap`
- `src/gc.rs` — Background garbage collection task

### Details:

**`Storage` trait:**
```rust
#[async_trait]
pub trait Storage: Send + Sync + 'static {
    async fn check_and_update(
        &self,
        key: &str,
        quota: &Quota,
        cost: u32,
        clock: &dyn Clock,
    ) -> Result<RateLimitInfo, RateLimited>;
}
```

**`MemoryStorage`:**
- Uses `DashMap<String, Nanos>` to store TAT per key
- Calls `check_gcra()` internally
- Thread-safe, lock-free per-shard

**Garbage Collection:**
- Spawns `tokio::spawn` task on construction
- Runs every `gc_interval` (default: 60 seconds)
- Removes entries where `TAT < now` (expired keys)
- Uses `DashMap::retain()` for efficient in-place filtering

### Tests:
- `tests/storage_tests.rs` — Concurrent access, key expiry, GC cleanup

---

## Phase 4: Tier Configuration

**Goal:** Builder API for defining named tiers.

### Files to create:
- `src/tier.rs` — `RateTier`, `RateTierBuilder`
- `src/on_missing.rs` — `OnMissing` enum
- `src/on_storage_error.rs` — `OnStorageError` enum

### Details:

**`RateTierBuilder`:**
```rust
RateTier::builder()
    .tier("free", Quota::per_hour(100))
    .tier("pro", Quota::per_hour(5_000))
    .default_tier("free")
    .on_missing(OnMissing::UseDefault)
    .clock(clock)
    .build() -> RateTier
```

`RateTier` stores:
- `HashMap<String, Quota>` — tier name → quota
- `default_tier: Option<String>`
- `on_missing: OnMissing`
- `storage: Arc<dyn Storage>`
- `clock: Arc<dyn Clock>`

**Validation on `build()`:**
- Panics if `default_tier` references a non-existent tier
- Panics if no tiers defined

The `RateTier` also exposes `check(user_id, tier_name, cost)` for programmatic use (non-HTTP).

### Tests:
- `tests/tier_tests.rs` — Builder validation, default tier, on_missing behavior, check() API

---

## Phase 5: Identifier

**Goal:** Extract user identity from requests.

### Files to create:
- `src/identifier.rs` — `TierIdentifier` trait, `TierIdentity` struct, `ClosureIdentifier`

### Details:

```rust
pub struct TierIdentity {
    pub user_id: String,
    pub tier: String,
}

#[async_trait]
pub trait TierIdentifier: Send + Sync + 'static {
    async fn identify(&self, headers: &HeaderMap) -> Option<TierIdentity>;

    /// Optional: override for body-based identification
    async fn identify_with_body(
        &self,
        headers: &HeaderMap,
        body: &Bytes,
    ) -> Option<TierIdentity> {
        self.identify(headers).await  // default: ignore body
    }
}
```

**`ClosureIdentifier`** wraps `Fn(&HeaderMap) -> Option<TierIdentity> + Send + Sync` for the `identifier_fn` API.

### Tests:
- Unit tests — Closure adapter, trait implementation mock

---

## Phase 6: Tower Integration

**Goal:** Implement the Tower Layer and Service.

### Files to create:
- `src/layer.rs` — `TierLimitLayer`
- `src/service.rs` — `TierLimitService`
- `src/cost.rs` — `tier_cost()` layer, `TierCost` extension
- `src/response.rs` — 429 response builder, header injection

### Details:

**`TierLimitLayer`:**
```rust
pub struct TierLimitLayer {
    rate_tier: Arc<RateTier>,
    identifier: Arc<dyn TierIdentifier>,
    on_storage_error: OnStorageError,
    buffer_body: bool,
}

impl<S> Layer<S> for TierLimitLayer {
    type Service = TierLimitService<S>;
}
```

**`TierLimitService<S>`:**
- Implements `tower::Service<Request<B>>`
- Flow: extract headers → identify → lookup quota → storage.check → inject headers or return 429
- Uses `pin-project-lite` for the response future

**`tier_cost()`:**
- Returns a simple Layer that inserts `TierCost(n)` into request extensions
- `TierLimitService` reads it: `req.extensions().get::<TierCost>().map(|c| c.0).unwrap_or(1)`

**Response building:**
- `inject_headers(response, info)` — adds `X-RateLimit-*` headers to successful responses
- `rate_limited_response(info)` — builds 429 response with JSON body and headers

### Tests:
- `tests/integration.rs` — Full Axum app with multiple tiers, cost, 429 behavior
- `tests/cost_tests.rs` — Default cost, custom cost, multiple costs on different routes

---

## Phase 7: Body-Based Identification (Feature-Gated)

**Goal:** Support identifying users from the request body.

### Files to modify:
- `src/service.rs` — Buffer body when `buffer_body` is enabled
- `src/identifier.rs` — Already has `identify_with_body` from Phase 5

### Details:
- When `buffer_body(true)`:
  1. Read entire body into `Bytes` (up to `max_body_size`, default 64KB)
  2. If body exceeds limit → return 413 immediately
  3. Call `identifier.identify_with_body(headers, &body)`
  4. Reconstruct body as `Full<Bytes>` for downstream service
- Feature-gated under `buffered-body` because it changes the `Service` generic bounds

### Tests:
- Integration test with body-based identifier

---

## Phase 8: Examples & Documentation

**Goal:** Runnable examples and polished documentation.

### Files to create:
- `examples/axum_basic.rs` — Minimal setup with closure identifier
- `examples/axum_jwt.rs` — JWT-based tier identification
- Doc comments on all public types and methods
- `README.md` finalization

### Quality checklist:
- All examples compile with `cargo build --examples`
- `cargo doc --no-deps` produces clean documentation
- `cargo clippy` passes with no warnings
- `cargo test` passes all tests

---

## Dependency Graph

```
Phase 1 (Foundation)
    │
    ▼
Phase 2 (GCRA)
    │
    ▼
Phase 3 (Storage)
    │
    ▼
Phase 4 (Tier Config) ◄── Phase 5 (Identifier)
    │                           │
    └───────────┬───────────────┘
                ▼
         Phase 6 (Tower Integration)
                │
                ▼
         Phase 7 (Body-Based ID)
                │
                ▼
         Phase 8 (Examples & Docs)
```

## Estimated Deliverables per Phase

| Phase | Files | Tests | Feature |
|-------|-------|-------|---------|
| 1 - Foundation | 4 | quota, clock | Core types |
| 2 - GCRA | 1 | gcra | Algorithm |
| 3 - Storage | 3 | storage, gc | Persistence |
| 4 - Tier Config | 3 | tier, builder | Configuration |
| 5 - Identifier | 1 | identifier | User extraction |
| 6 - Tower | 4 | integration, cost | Middleware |
| 7 - Body ID | 0 (modify) | body integration | `buffered-body` |
| 8 - Examples | 2+ | compile check | Documentation |
