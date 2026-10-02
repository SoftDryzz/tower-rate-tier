# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

### Added

- `Debug` implementations for all public types (`MemoryStorage` shows only its entry count, never user keys)
- `RateLimited::retry_after_secs()` rounds the wait up to whole seconds for a `Retry-After` header; the custom 429 examples use it
- `TierLimitLayer::cost_fn()` computes each request's cost inside the middleware from its method, URI, headers and extensions (including axum's `MatchedPath`)
- `TierLimitLayer::on_event()` callback and `LimitEvent` enum, starting with `LimitEvent::StorageError`, so storage failures are visible even when the request fails open
- `OnUnknownTier` policy (`RateTierBuilder::on_unknown_tier()`) and `LimitEvent::UnknownTier` for tiers that are not configured
- `CheckError::CostExceedsLimit` and `LimitEvent::CostExceedsLimit` for requests that cost more than the tier's maximum burst
- `TierLimitLayer::new()` also accepts an `Arc<RateTier>`, so the middleware and programmatic `RateTier::check()` calls can share one set of limits

### Changed

- **Breaking:** `OnMissing`, `OnStorageError` and `CheckError` are `#[non_exhaustive]`, so new variants can be added without another breaking release. The new `LimitEvent` variants and `CheckError::CostExceedsLimit` are `#[non_exhaustive]` too, so their fields must be matched with `..`
- **Breaking:** `RateLimitInfo` and `RateLimited` report `reset_after: Duration` (time until the quota fully replenishes) instead of an absolute `reset_at: Nanos`, so results no longer depend on the storage backend's clock. Both now derive `PartialEq` and `Eq`
- **Breaking:** `Storage::check_and_update()` takes a `StorageKey { user_id, tier }` instead of a pre-joined `&str`, and the key, quota and returned future share one lifetime so async backends can borrow them
- **Breaking:** `Clock::unix_offset_nanos()` is removed and `SystemClock` is purely monotonic. `response::inject_headers()` and `response::rate_limited_response()` take the wall-clock `now: SystemTime` instead of a Unix offset

### Removed

- Unused `tower` and `pin-project-lite` dependencies

### Fixed

- The README and the `axum_basic` example set per-route costs with a route's own `.layer(tier_cost(n))` under a limiter added with `Router::layer`. That layer runs after the limiter, so the cost was silently ignored and every request cost 1. They now use `cost_fn`, and the `tier_cost` docs explain the order it needs
- A request costing more than the tier's maximum burst was answered with 429 and a `Retry-After` that never helped. It is now rejected without touching storage: the middleware answers 403 Forbidden with no `Retry-After`, and `RateTier::check()` returns `CheckError::CostExceedsLimit`
- A user in one tier could share a bucket with another user in another tier when the names contained `:` (user `a:b` in tier `c` and user `a` in tier `b:c` were both stored as `a:b:c`)
- `X-RateLimit-Reset` on a 429 counted the rejected request, so it reported the reset one emission interval (times the request cost) too late
- `Retry-After` was rounded down, so a client that waited exactly that long was rejected again. It is now rounded up, and the JSON body's `retry_after` always matches the header
- `MemoryStorage` let concurrent requests for the same key exceed the quota, because the check and the update were not atomic
- GCRA overflow with very large costs: it panicked in debug builds, and in release builds it allowed the request and reset the user's state
- `max_body_size` buffered the whole request body before checking its size; it now rejects bodies whose declared length is over the limit without reading them, and stops reading at the first chunk that crosses the limit
- `RateTierBuilder::build()` panicked outside a Tokio runtime. The garbage collector still starts at build time inside a runtime; otherwise it starts on the first use of the storage inside one
- A zero GC interval silently killed the garbage collector task; `gc_interval(Duration::ZERO)` and `GcHandle::spawn` now panic instead
- `RateTierBuilder::storage()` docs claimed `gc_interval()` re-enables garbage collection for custom backends
- Quotas faster than one request per nanosecond (e.g. `Quota::per_second(2_000_000_000)`) now panic when built instead of panicking with a division by zero on every check
- docs.rs now builds with all features, so `buffered-body` items are documented
- `TierLimitLayer` docs and example were attached to the `OnLimitedFn` alias
- `identify_with_body` docs referenced a nonexistent `buffer_body(true)` signature

### Security

- A tier name the middleware did not know (a typo, a new plan, or a value a client could influence, such as a header) let the request through with no rate limit at all. By default it now gets the default tier's quota in the user's own bucket, or 403 Forbidden when no default tier is set; `OnUnknownTier` can choose `Deny` or an explicit `Allow`

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
