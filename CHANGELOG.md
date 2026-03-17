# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased] — v0.2.0

### Breaking Changes

- `RateTier::check()` now returns `Err(CheckError::UnknownTier)` instead of panicking on unknown tier names (#8)
- `X-RateLimit-Reset` header now contains Unix timestamps instead of process-local epoch seconds (#9)
- `inject_headers()` and `rate_limited_response()` now require a `unix_offset_nanos` parameter (#9)

### Added

- `CheckError` enum with `UnknownTier(String)` and `Storage(StorageError)` variants (#8)
- `Clock::unix_offset_nanos()` method with default `0` for Unix timestamp conversion (#9)
- `SystemClock` captures Unix time offset at construction (#9)

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
