use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use http::{HeaderMap, Request, Response};
use tower_layer::Layer;
use tower_rate_tier::clock::FakeClock;
use tower_rate_tier::gcra::check_gcra;
use tower_rate_tier::identifier::{TierIdentifier, TierIdentity};
use tower_rate_tier::storage::memory::MemoryStorage;
use tower_rate_tier::storage::Storage;
use tower_rate_tier::{Quota, RateTier, TierLimitLayer};
use tower_service::Service;

// --- Helpers ---

#[derive(Clone)]
struct OkService;

impl Service<Request<String>> for OkService {
    type Response = Response<String>;
    type Error = std::convert::Infallible;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: Request<String>) -> Self::Future {
        std::future::ready(Ok(Response::new("ok".to_string())))
    }
}

struct StaticIdentifier;

impl TierIdentifier for StaticIdentifier {
    fn identify(
        &self,
        _headers: &HeaderMap,
    ) -> Pin<Box<dyn Future<Output = Option<TierIdentity>> + Send + '_>> {
        Box::pin(std::future::ready(Some(TierIdentity::new("user1", "free"))))
    }
}

fn make_request() -> Request<String> {
    Request::builder().body(String::new()).unwrap()
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

// --- Benchmarks ---

fn bench_gcra_check_allowed(c: &mut Criterion) {
    let ei = 600_000_000u64; // 100 req/min → 600ms interval
    let bo = ei * 100; // burst offset
    let now = 1_000_000_000u64;

    c.bench_function("gcra_check_allowed", |b| {
        b.iter(|| {
            let _ = black_box(check_gcra(Some(now), now, ei, bo, 1));
        });
    });
}

fn bench_gcra_check_denied(c: &mut Criterion) {
    let ei = 600_000_000u64;
    let bo = ei * 100;
    let now = 1_000_000_000u64;
    // TAT far in the future → denied
    let tat = now + bo + ei * 10;

    c.bench_function("gcra_check_denied", |b| {
        b.iter(|| {
            let _ = black_box(check_gcra(Some(tat), now, ei, bo, 1));
        });
    });
}

fn bench_storage_single_key(c: &mut Criterion) {
    let runtime = rt();
    let storage = Arc::new(MemoryStorage::new());
    let quota = Quota::per_minute(100);

    c.bench_function("storage_check_single_key", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let _ = black_box(
                    storage
                        .check_and_update("user1:free", &quota, 1, 1_000_000_000)
                        .await,
                );
            });
        });
    });
}

fn bench_storage_many_keys(c: &mut Criterion) {
    let runtime = rt();
    let storage = Arc::new(MemoryStorage::new());
    let quota = Quota::per_minute(1000);

    // Pre-populate 10,000 keys
    runtime.block_on(async {
        for i in 0..10_000 {
            let key = format!("user{}:free", i);
            let _ = storage
                .check_and_update(&key, &quota, 1, 1_000_000_000)
                .await;
        }
    });

    c.bench_function("storage_check_10k_keys", |b| {
        let mut i = 0u64;
        b.iter(|| {
            let key = format!("user{}:free", i % 10_000);
            i += 1;
            runtime.block_on(async {
                let _ = black_box(
                    storage
                        .check_and_update(&key, &quota, 1, 1_000_000_000)
                        .await,
                );
            });
        });
    });
}

fn bench_full_middleware_allowed(c: &mut Criterion) {
    let runtime = rt();
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let rate_tier = RateTier::builder()
        .tier("free", Quota::per_second(1_000_000))
        .default_tier("free")
        .clock(clock)
        .build();

    let layer = TierLimitLayer::new(rate_tier).identifier(StaticIdentifier);
    let mut svc = layer.layer(OkService);

    c.bench_function("full_middleware_allowed", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let resp = svc.call(make_request()).await.unwrap();
                black_box(resp);
            });
        });
    });
}

fn bench_full_middleware_denied(c: &mut Criterion) {
    let runtime = rt();
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let rate_tier = RateTier::builder()
        .tier("free", Quota::per_hour(1))
        .default_tier("free")
        .clock(clock)
        .build();

    let layer = TierLimitLayer::new(rate_tier).identifier(StaticIdentifier);
    let mut svc = layer.layer(OkService);

    // Exhaust quota
    runtime.block_on(async {
        let _ = svc.call(make_request()).await;
    });

    c.bench_function("full_middleware_denied", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let resp = svc.call(make_request()).await.unwrap();
                black_box(resp);
            });
        });
    });
}

criterion_group!(
    benches,
    bench_gcra_check_allowed,
    bench_gcra_check_denied,
    bench_storage_single_key,
    bench_storage_many_keys,
    bench_full_middleware_allowed,
    bench_full_middleware_denied,
);
criterion_main!(benches);
