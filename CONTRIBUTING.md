# Contributing to tower-rate-tier

Thanks for your interest in contributing! This guide explains how to report bugs, suggest features, and submit code.

## Reporting Bugs

Open an [issue](https://github.com/SoftDryzz/tower-rate-tier/issues) with:

- **Title**: Short description of the problem
- **Rust version**: Output of `rustc --version`
- **Crate version**: The version of `tower-rate-tier` you're using
- **Minimal reproduction**: The smallest code that triggers the bug
- **Expected vs actual behavior**: What you expected and what happened
- **Logs/errors**: Any relevant error messages or panics

## Suggesting Features

Open an issue with the label `enhancement` and include:

- **Use case**: What problem does this solve?
- **Proposed API**: How would the feature look from the user's perspective?
- **Alternatives considered**: Other approaches you've thought about

## Submitting Code

### Setup

```bash
git clone https://github.com/SoftDryzz/tower-rate-tier.git
cd tower-rate-tier
cargo test
cargo test --features buffered-body
```

### Branch Naming

Create your branch from `main` using this convention:

| Type | Pattern | Example |
|------|---------|---------|
| Feature | `feat/<username>/<short-description>` | `feat/alice/redis-storage` |
| Bug fix | `fix/<username>/<short-description>` | `fix/bob/gcra-overflow` |
| Docs | `docs/<username>/<short-description>` | `docs/carol/quota-examples` |

### Commit Messages

We use [Conventional Commits](https://www.conventionalcommits.org/) in English:

```
feat: add Redis storage backend
fix: prevent GCRA overflow on large costs
docs: add Tonic/gRPC usage example
test: add integration tests for OnMissing::Deny
refactor: simplify storage trait bounds
```

### Pull Requests

1. Fork the repo and create your branch from `main`
2. Write or update tests for your changes
3. Run all checks before submitting:
   ```bash
   cargo fmt --check
   cargo clippy -- -D warnings
   cargo test
   cargo test --features buffered-body
   ```
4. Open a PR against `main` with a clear description of what and why
5. Link any related issues

### Code Style

- Run `cargo fmt` before committing
- No Clippy warnings (`cargo clippy -- -D warnings`)
- Add doc comments (`///`) for all public items
- Keep unsafe code out — this crate is 100% safe Rust
- Prefer simple, readable code over clever abstractions

### Tests

- Unit tests go in the same file as the code (`#[cfg(test)]` module)
- Integration tests go in `tests/`
- Use `FakeClock` for any time-dependent tests — never use `tokio::time::sleep` in tests
- Feature-gated code needs tests behind the same feature flag

## Architecture Overview

If you want to understand the codebase before contributing:

- `src/tier.rs` — `RateTier` and builder, the main entry point
- `src/gcra.rs` — GCRA algorithm implementation
- `src/storage/` — `Storage` trait and `MemoryStorage` (DashMap-based)
- `src/layer.rs` + `src/service.rs` — Tower Layer/Service integration
- `src/identifier.rs` — `TierIdentifier` trait for user identification
- `src/cost.rs` — Per-endpoint request cost/weight
- `src/buffered.rs` — Body-based identification (feature: `buffered-body`)

## Questions?

Open a [discussion](https://github.com/SoftDryzz/tower-rate-tier/discussions) or an issue. No question is too small.

## License

By contributing, you agree that your contributions will be licensed under MIT OR Apache-2.0, the same as the project.
