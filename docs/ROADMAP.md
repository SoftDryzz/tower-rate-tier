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

## v0.2.0 — Correctness, Performance & Extensibility

API-breaking fixes, dependency cleanup, infrastructure, and storage abstraction.

- [x] `check()` returns `CheckError` instead of panicking on unknown tier (#8)
- [x] `X-RateLimit-Reset` header uses real Unix timestamps (#9)
- [x] Storage key includes tier name (`user_id:tier`) (#10)
- [x] Overflow protection with `saturating_mul` in `burst_offset_nanos` (#11)
- [x] Explicit `None` path for first-request TAT (#12)
- [x] Escape tier name in 429 JSON body (#13)
- [x] `OnMissing` derives `Copy` (#14)
- [x] Move `serde` / `serde_json` to `[dev-dependencies]` (#15)
- [x] Remove `async-trait`: use `Pin<Box<dyn Future>>` (#16)
- [x] Declare `rust-version = "1.75"` (MSRV) (#17)
- [x] Add `Quota::per_day()` and `Quota::with_window()` constructors (#18)
- [x] Document `cost = 0` behavior (#19)
- [x] GitHub Actions CI: test, clippy, fmt, doc, MSRV (#20)
- [x] `CHANGELOG.md` (#21)
- [x] Extract shared rate-limit logic into `check` module (#22)
- [x] Replace hand-rolled base64 in `axum_jwt.rs` (#23)
- [x] Add `criterion` benchmarks (#24)
- [x] `RateTierBuilder::storage()` accepts `Arc<dyn Storage>` (#25)
- [x] GC conditional: only for `MemoryStorage` (#26)
- [x] `on_limited` callback for metrics/events (#27)
- [x] Custom 429 response body builder (#28)
- [x] Published to crates.io

## v0.3.0 — Redis & Distributed

Production-ready distributed rate limiting.

- [ ] Redis storage backend (feature-gated: `redis`) (#29)
- [ ] Atomic GCRA via Lua script (race-condition-free) (#29)
- [ ] Example: `axum_api_key` with Redis tier lookup (#30)
- [ ] Dynamic tier updates at runtime (#31)
- [ ] Dashboard-ready metrics export (Prometheus-compatible) (#32)
- [ ] Tonic/gRPC example and documentation (#33)

## Future Ideas

- Sliding window log algorithm as alternative to GCRA
- Rate limit sharing across tier groups
- WebSocket rate limiting support
- OpenTelemetry integration
- `tower-rate-tier-admin` companion crate for management API
