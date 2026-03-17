# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/).

## [0.2.0] - 2026-03-17

### Breaking Changes

- `RateTier::check()` returns `Err(CheckError::UnknownTier)` instead of panicking (#8)
- `X-RateLimit-Reset` header now contains Unix timestamps (#9)
- Storage key is now `user_id:tier_name` — existing in-memory state is invalidated (#10)
- `OnMissing` implements `Copy` and `PartialEq`; `on_missing()` returns by value (#14)
- `async-trait` removed; `Storage` and `TierIdentifier` use `Pin<Box<dyn Future>>` (#16)
- `inject_headers()` and `rate_limited_response()` require `unix_offset_nanos` parameter (#9)
- `serde` and `serde_json` moved to dev-dependencies (#15)
- MSRV set to 1.75 (#17)

### Added

- `CheckError` enum with `UnknownTier` and `Storage` variants (#8)
- `Clock::unix_offset_nanos()` for Unix timestamp conversion (#9)
- `Quota::per_day()` and `Quota::with_window()` constructors (#18)
- `RateTierBuilder::storage(Arc<dyn Storage>)` for custom backends (#25)
- `RateTierBuilder::disable_gc()` to skip garbage collection (#26)
- `TierLimitLayer::on_limited()` callback for metrics/logging (#27)
- `TierLimitLayer::rate_limited_response()` custom 429 response builder (#28)
- `StorageFuture` type alias (#16)
- GitHub Actions CI (#20)
- `CHANGELOG.md` (#21)
- `criterion` benchmarks (#24)
- Shared `check` module to deduplicate service/buffered logic (#22)

### Fixed

- Silent overflow in `burst_offset_nanos` — uses `saturating_mul` (#11)
- Fragile first-request TAT — uses explicit `None` (#12)
- Tier name not escaped in 429 JSON body (#13)
- Hand-rolled base64 replaced with `base64` crate in example (#23)

## [0.1.1] - 2026-03-11

### Fixed

- Added missing doc comments for 100% documentation coverage
- Fixed inaccurate Redis claims in README
- Fixed LICENSE badge link pointing to nonexistent file
- Marked completed v0.1.0 items in roadmap and design doc

### Added

- `CONTRIBUTING.md` with guidelines for issues, PRs, and contributions

## [0.1.0] - 2026-03-11

### Added

- Core types: `RateTier`, `RateTierBuilder`, `Quota`, `TierIdentity`
- GCRA algorithm implementation with request cost/weight support
- `TierLimitLayer` / `TierLimitService` (Tower Layer + Service)
- `TierIdentifier` trait + closure adapter (`identifier_fn`)
- `tier_cost()` layer for per-endpoint request weighting
- `OnMissing` behavior: `UseDefault`, `Allow`, `Deny(StatusCode)`
- `OnStorageError` behavior: `Allow` (fail open) / `Deny` (fail closed)
- `StorageError` type for distinguishing backend failures from rate limits
- In-memory storage with `DashMap` + automatic GC of expired keys
- Standard rate limit headers (`X-RateLimit-Limit`, `Remaining`, `Reset`, `Retry-After`)
- 429 JSON response body with tier name and retry_after
- `FakeClock` for deterministic testing
- Body-based identification (feature-gated: `buffered-body`)
- Examples: `axum_basic`, `axum_jwt`
- README with usage guide
- Dual-licensed under MIT OR Apache-2.0
