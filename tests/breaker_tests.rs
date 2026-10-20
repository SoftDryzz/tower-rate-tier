//! The circuit breaker behind the middleware, through a storage outage.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use http::{Request, Response, StatusCode};
use tower_layer::Layer;
use tower_rate_tier::clock::FakeClock;
use tower_rate_tier::storage::memory::MemoryStorage;
use tower_rate_tier::storage::{Storage, StorageFuture};
use tower_rate_tier::{
    BreakerEvent, CircuitBreaker, LimitEvent, Nanos, Quota, RateTier, StorageError, StorageKey,
    TierIdentity, TierLimitLayer,
};
use tower_service::Service;

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

/// Stands in for Redis: a real `MemoryStorage` that can be switched off.
#[derive(Clone, Default)]
struct Switchable(Arc<SwitchableInner>);

#[derive(Default)]
struct SwitchableInner {
    storage: MemoryStorage,
    down: AtomicBool,
    calls: AtomicUsize,
}

impl Switchable {
    fn set_down(&self, down: bool) {
        self.0.down.store(down, Ordering::SeqCst);
    }

    fn calls(&self) -> usize {
        self.0.calls.load(Ordering::SeqCst)
    }
}

impl Storage for Switchable {
    fn check_and_update<'a>(
        &'a self,
        key: StorageKey<'a>,
        quota: &'a Quota,
        cost: u32,
        now: Nanos,
    ) -> StorageFuture<'a> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        if self.0.down.load(Ordering::SeqCst) {
            let error = StorageError(Box::from("connection refused"));
            return Box::pin(std::future::ready(Err(error)));
        }
        self.0.storage.check_and_update(key, quota, cost, now)
    }
}

fn request() -> Request<String> {
    Request::builder().uri("/api").body(String::new()).unwrap()
}

fn breaker_over(
    redis: &Switchable,
    events: &Arc<Mutex<Vec<&'static str>>>,
) -> CircuitBreaker<Switchable> {
    let sink = Arc::clone(events);
    CircuitBreaker::new(redis.clone(), MemoryStorage::new()).on_state_change(move |event| {
        sink.lock().unwrap().push(match event {
            BreakerEvent::PrimaryFailed { .. } => "PrimaryFailed",
            BreakerEvent::Opened { .. } => "Opened",
            BreakerEvent::Closed { .. } => "Closed",
            _ => "other",
        });
    })
}

#[tokio::test]
async fn limits_through_an_outage_and_returns_to_the_primary() {
    let clock = FakeClock::new();
    let redis = Switchable::default();
    let breaker_events = Arc::new(Mutex::new(Vec::new()));
    let storage_errors = Arc::new(AtomicUsize::new(0));

    let rate_tier = RateTier::builder()
        .tier("free", Quota::per_minute(3))
        .clock(clock.clone())
        .storage(Arc::new(breaker_over(&redis, &breaker_events)))
        .build();
    let errors = Arc::clone(&storage_errors);
    let layer = TierLimitLayer::new(rate_tier)
        .identifier_fn(|_| Some(TierIdentity::new("u1", "free")))
        .on_event(move |event| {
            if let LimitEvent::StorageError { .. } = event {
                errors.fetch_add(1, Ordering::SeqCst);
            }
        });
    let mut svc = layer.layer(OkService);

    // Redis is up: it enforces the quota.
    for _ in 0..3 {
        let resp = svc.call(request()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["x-ratelimit-limit"], "3");
    }
    let resp = svc.call(request()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(redis.calls(), 4);

    // Redis goes down a minute later: the fallback enforces the same quota.
    clock.advance(Duration::from_secs(60));
    redis.set_down(true);
    for _ in 0..3 {
        let resp = svc.call(request()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()["x-ratelimit-limit"], "3");
    }
    let resp = svc.call(request()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(resp.headers().contains_key("retry-after"));
    // Three failures opened the breaker; the fourth request skipped Redis.
    assert_eq!(redis.calls(), 7);
    assert_eq!(
        *breaker_events.lock().unwrap(),
        ["PrimaryFailed", "PrimaryFailed", "PrimaryFailed", "Opened"]
    );
    assert_eq!(storage_errors.load(Ordering::SeqCst), 0);

    // Redis is back: after open_for (5 s) the next request probes it.
    redis.set_down(false);
    clock.advance(Duration::from_secs(5));
    let resp = svc.call(request()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(redis.calls(), 8);
    assert_eq!(breaker_events.lock().unwrap().last(), Some(&"Closed"));
}

#[tokio::test]
async fn programmatic_check_uses_the_fallback() {
    let redis = Switchable::default();
    redis.set_down(true);
    let events = Arc::new(Mutex::new(Vec::new()));
    let rate_tier = RateTier::builder()
        .tier("free", Quota::per_minute(3))
        .clock(FakeClock::new())
        .storage(Arc::new(breaker_over(&redis, &events)))
        .build();

    let decision = rate_tier.check("u1", "free", 1).await;

    assert!(matches!(decision, Ok(Ok(_))), "{decision:?}");
    assert_eq!(redis.calls(), 1);
}
