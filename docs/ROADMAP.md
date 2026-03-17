# tower-rate-tier — Roadmap

> Tier-based rate limiting middleware for Tower

## v0.1.0 — MVP

Core library with in-memory storage and full Tower integration.

- [x] Core types: `RateTier`, `RateTierBuilder`, `Quota`, `TierIdentity`
- [x] GCRA algorithm implementation
- [x] `TierLimitLayer` / `TierLimitService` (Tower Layer + Service)
- [x] `TierIdentifier` trait + closure adapter (`identifier_fn`)
- [x] `tier_cost()` layer for per-endpoint request weighting
- [x] `OnMissing` behavior: `UseDefault`, `Allow`, `Deny(StatusCode)`
- [x] `OnStorageError` behavior: `Allow` (fail open) / `Deny` (fail closed)
- [x] `StorageError` type for distinguishing backend failures from rate limits
- [x] In-memory storage with `DashMap` + automatic GC of expired keys
- [x] Standard rate limit headers (`X-RateLimit-Limit`, `Remaining`, `Reset`, `Retry-After`)
- [x] 429 JSON response body
- [x] `FakeClock` for deterministic testing
- [x] Body-based identification (feature-gated: `buffered-body`)
- [x] Examples: `axum_basic`, `axum_jwt`
- [x] README with usage guide
- [x] Published to crates.io

## v0.2.0 — Correctness & Breaking Fixes

API-breaking fixes for correctness and safety. All breaking changes batched into a single release.

- [ ] `check()` returns `Result` instead of panicking on unknown tier
- [ ] `X-RateLimit-Reset` header uses real Unix timestamps instead of process-local epoch
- [ ] Storage key includes tier name (`user_id:tier`) to isolate state per (user, tier) pair
- [ ] Overflow protection with `saturating_mul` in `burst_offset_nanos`
- [ ] Explicit `None` path for first-request TAT instead of `or_insert(now)` sentinel
- [ ] Escape tier name in 429 JSON body to prevent malformed JSON
- [ ] `OnMissing` derives `Copy`

## v0.2.1 — Dependency Cleanup & Performance

Non-breaking improvements. Lighter dependency tree and fewer allocations.

- [ ] Move `serde` / `serde_json` to `[dev-dependencies]`
- [ ] Remove `async-trait`: use native `async fn` in traits (Rust 1.75+)
- [ ] Declare `rust-version = "1.75"` (MSRV) in Cargo.toml
- [ ] Add `Quota::per_day()` and `Quota::with_window(count, Duration)` constructors
- [ ] Document `cost = 0` behavior and decide on enforcement

## v0.2.2 — Infrastructure & Quality

CI, changelog, code deduplication, benchmarks.

- [ ] GitHub Actions CI: test, clippy, fmt, doc, feature matrix
- [ ] `CHANGELOG.md`
- [ ] Extract shared rate-limit logic from `service.rs` and `buffered.rs` into a private helper
- [ ] Replace hand-rolled base64 in `axum_jwt.rs` with `base64` crate
- [ ] Add `criterion` benchmarks for middleware latency

## v0.2.3 — Storage Trait Refactor

Prepare the Storage abstraction for pluggable backends. No Redis yet, but the trait and builder are ready.

- [ ] `RateTierBuilder::storage()` accepts `Arc<dyn Storage>` instead of `Arc<MemoryStorage>`
- [ ] GC is conditional: only spawns for `MemoryStorage`, not for backends with native TTL
- [ ] `on_limited` callback for metrics/events
- [ ] Custom 429 response body builder

## v0.3.0 — Redis & Distributed

Production-ready distributed rate limiting.

- [ ] Redis storage backend (feature-gated: `redis`)
- [ ] Atomic GCRA via Lua script (race-condition-free)
- [ ] Example: `axum_api_key` with Redis tier lookup
- [ ] Dynamic tier updates at runtime (add/remove/modify tiers without restart)
- [ ] Dashboard-ready metrics export (Prometheus-compatible)
- [ ] Tonic/gRPC example and documentation

## Future Ideas

- Sliding window log algorithm as alternative to GCRA
- Rate limit sharing across tier groups
- WebSocket rate limiting support
- OpenTelemetry integration
- `tower-rate-tier-admin` companion crate for management API
