# tower-rate-tier — Roadmap

> Tier-based rate limiting middleware for Tower

## v0.1.0 — MVP (Current)

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
- [ ] Published to crates.io

## v0.2.0 — Distributed & Observability

Redis storage and monitoring hooks.

- [ ] Redis storage backend (feature-gated: `redis`)
- [ ] Atomic GCRA via Lua script (race-condition-free)
- [ ] `on_limited` callback for metrics/events
- [ ] Custom 429 response body builder
- [ ] Example: `axum_api_key` with Redis tier lookup

## v0.3.0 — Dynamic & gRPC

Runtime tier management and gRPC support.

- [ ] Dynamic tier updates at runtime (add/remove/modify tiers without restart)
- [ ] Dashboard-ready metrics export (Prometheus-compatible)
- [ ] Tonic/gRPC example and documentation
- [ ] Rate limit scoping (per-endpoint + per-user combined quotas)

## Future Ideas

- Sliding window log algorithm as alternative to GCRA
- Rate limit sharing across tier groups
- WebSocket rate limiting support
- OpenTelemetry integration
- `tower-rate-tier-admin` companion crate for management API
