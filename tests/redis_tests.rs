#![cfg(feature = "redis")]
//! Integration tests against a real Redis.
//!
//! They connect to `REDIS_URL` (default `redis://127.0.0.1:6379`), for example
//! `docker run --rm -p 6379:6379 redis:7`. Without a server they are skipped,
//! unless `TRT_REQUIRE_REDIS=1` (set in CI) turns that into a failure.
//!
//! The `use_client_clock()` tests drive time with explicit values, so they can
//! compare the Lua script with the Rust reference `check_gcra` step by step.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use http::{Request, Response, StatusCode};
use redis::aio::ConnectionManager;
use tower_layer::Layer;
use tower_rate_tier::storage::memory::MemoryStorage;
use tower_rate_tier::storage::Storage;
use tower_rate_tier::{
    Nanos, Quota, RateTier, RedisStorage, StorageKey, TierIdentity, TierLimitLayer,
};
use tower_service::Service;

async fn connect() -> Option<ConnectionManager> {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
    let required = std::env::var("TRT_REQUIRE_REDIS").is_ok_and(|v| v == "1");
    let client = redis::Client::open(url.as_str()).expect("REDIS_URL must be a valid URL");

    let error =
        match tokio::time::timeout(Duration::from_secs(2), client.get_connection_manager()).await {
            Ok(Ok(conn)) => return Some(conn),
            Ok(Err(err)) => err.to_string(),
            Err(_) => "timed out".to_owned(),
        };
    assert!(
        !required,
        "Redis is required but unreachable at {url}: {error}"
    );
    eprintln!("skipping: no Redis at {url} ({error})");
    None
}

/// A key prefix no other test or earlier run shares.
fn unique_prefix(test: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("trt-test:{test}:{nanos}:")
}

fn millis(ms: u64) -> Nanos {
    ms * 1_000_000
}

#[tokio::test]
async fn results_match_the_in_memory_gcra() {
    let Some(conn) = connect().await else { return };
    let redis = RedisStorage::new(conn)
        .key_prefix(unique_prefix("parity"))
        .use_client_clock();
    let memory = MemoryStorage::new();
    // 250 ms per request: every value stays a whole number of microseconds.
    let quota = Quota::per_second(4);
    let key = StorageKey::new("alice", "free");

    let steps: &[(u64, u32)] = &[
        (0, 1),
        (0, 1),
        (0, 1),
        (0, 1),
        (0, 1), // over the burst
        (100, 1),
        (250, 2),
        (300, 0),
        (1_000, 4),
        (1_000, 0),
        (5_000, 3),
        (5_000, 2), // over what is left
    ];
    for (i, &(at_ms, cost)) in steps.iter().enumerate() {
        let now = millis(at_ms);
        let expected = memory
            .check_and_update(key, &quota, cost, now)
            .await
            .unwrap();
        let actual = redis
            .check_and_update(key, &quota, cost, now)
            .await
            .unwrap_or_else(|err| panic!("step {i}: {err}"));
        assert_eq!(actual, expected, "step {i}: t={at_ms}ms cost={cost}");
    }
}

#[tokio::test]
async fn stores_the_tat_as_an_exact_integer() {
    let Some(mut conn) = connect().await else {
        return;
    };
    let redis = RedisStorage::new(conn.clone())
        .key_prefix(unique_prefix("precision"))
        .use_client_clock();
    let key = StorageKey::new("alice", "free");
    // 2026 in microseconds: 16 digits, past what "%.14g" keeps.
    let now_us: u64 = 1_790_000_000_123_456;

    redis
        .check_and_update(key, &Quota::per_second(1), 1, now_us * 1_000)
        .await
        .unwrap()
        .expect("the first request is allowed");

    let stored: Option<String> = redis::cmd("GET")
        .arg(redis.redis_key(key))
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(stored.as_deref(), Some("1790000001123456"));
}

#[tokio::test]
async fn expires_the_key_when_the_bucket_is_full_again() {
    let Some(mut conn) = connect().await else {
        return;
    };
    let redis = RedisStorage::new(conn.clone())
        .key_prefix(unique_prefix("ttl"))
        .use_client_clock();
    let key = StorageKey::new("alice", "free");

    let info = redis
        .check_and_update(key, &Quota::per_second(4), 1, 0)
        .await
        .unwrap()
        .expect("the first request is allowed");
    assert_eq!(info.reset_after, Duration::from_millis(250));

    let ttl_ms: i64 = redis::cmd("PTTL")
        .arg(redis.redis_key(key))
        .query_async(&mut conn)
        .await
        .unwrap();
    assert!((1..=250).contains(&ttl_ms), "PTTL = {ttl_ms}");
}

#[tokio::test]
async fn a_free_request_on_a_fresh_bucket_stores_nothing() {
    let Some(mut conn) = connect().await else {
        return;
    };
    let redis = RedisStorage::new(conn.clone())
        .key_prefix(unique_prefix("free"))
        .use_client_clock();
    let key = StorageKey::new("alice", "free");

    let info = redis
        .check_and_update(key, &Quota::per_second(4), 0, 0)
        .await
        .unwrap()
        .expect("a free request is always allowed");
    assert_eq!(info.remaining, 4);

    let exists: i64 = redis::cmd("EXISTS")
        .arg(redis.redis_key(key))
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(exists, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_checks_never_exceed_the_burst() {
    let Some(conn) = connect().await else { return };
    let redis = Arc::new(RedisStorage::new(conn).key_prefix(unique_prefix("concurrent")));

    let tasks: Vec<_> = (0..50)
        .map(|_| {
            let redis = redis.clone();
            tokio::spawn(async move {
                let quota = Quota::per_hour(10);
                redis
                    .check_and_update(StorageKey::new("alice", "free"), &quota, 1, 0)
                    .await
                    .unwrap()
                    .is_ok()
            })
        })
        .collect();

    let mut allowed = 0;
    for task in tasks {
        if task.await.unwrap() {
            allowed += 1;
        }
    }
    assert_eq!(allowed, 10);
}

#[tokio::test]
async fn server_time_limits_a_burst() {
    let Some(conn) = connect().await else { return };
    let redis = RedisStorage::new(conn).key_prefix(unique_prefix("server-time"));
    let key = StorageKey::new("alice", "free");
    let quota = Quota::per_hour(2);

    // The `now` argument is ignored: Redis's TIME is the clock.
    for _ in 0..2 {
        assert!(redis
            .check_and_update(key, &quota, 1, 0)
            .await
            .unwrap()
            .is_ok());
    }
    let limited = redis
        .check_and_update(key, &quota, 1, 0)
        .await
        .unwrap()
        .expect_err("the third request is over the burst");
    let half_hour = Duration::from_secs(30 * 60);
    assert!(
        limited.retry_after <= half_hour
            && limited.retry_after > half_hour - Duration::from_secs(5),
        "retry_after = {:?}",
        limited.retry_after
    );
}

#[tokio::test]
async fn reloads_the_script_after_script_flush() {
    let Some(mut conn) = connect().await else {
        return;
    };
    let redis = RedisStorage::new(conn.clone()).key_prefix(unique_prefix("noscript"));
    let key = StorageKey::new("alice", "free");
    let quota = Quota::per_hour(10);

    redis
        .check_and_update(key, &quota, 1, 0)
        .await
        .unwrap()
        .unwrap();
    let _: () = redis::cmd("SCRIPT")
        .arg("FLUSH")
        .query_async(&mut conn)
        .await
        .unwrap();

    // EVALSHA now answers NOSCRIPT; the storage must load the script again.
    let info = redis
        .check_and_update(key, &quota, 1, 0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.remaining, 8);
}

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

#[tokio::test]
async fn middleware_answers_429_from_redis_state() {
    let Some(conn) = connect().await else { return };
    let redis = RedisStorage::new(conn).key_prefix(unique_prefix("middleware"));
    let rate_tier = RateTier::builder()
        .tier("free", Quota::per_hour(2))
        .storage(Arc::new(redis))
        .build();
    let mut svc = TierLimitLayer::new(rate_tier)
        .identifier_fn(|_| Some(TierIdentity::new("alice", "free")))
        .layer(OkService);

    for _ in 0..2 {
        let resp = svc.call(Request::new(String::new())).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
    let resp = svc.call(Request::new(String::new())).await.unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn a_corrupt_value_heals_itself() {
    let Some(mut conn) = connect().await else {
        return;
    };
    let redis = RedisStorage::new(conn.clone())
        .key_prefix(unique_prefix("corrupt"))
        .use_client_clock();
    let key = StorageKey::new("alice", "free");
    let _: () = redis::cmd("SET")
        .arg(redis.redis_key(key))
        .arg("not a number")
        .query_async(&mut conn)
        .await
        .unwrap();

    let info = redis
        .check_and_update(key, &Quota::per_second(4), 1, millis(1_000))
        .await
        .expect("a corrupt value must not fail the request")
        .expect("it counts as a fresh bucket");
    assert_eq!(info.remaining, 3);

    let stored: Option<String> = redis::cmd("GET")
        .arg(redis.redis_key(key))
        .query_async(&mut conn)
        .await
        .unwrap();
    let ttl_ms: i64 = redis::cmd("PTTL")
        .arg(redis.redis_key(key))
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(stored.as_deref(), Some("1250000"));
    assert!(ttl_ms > 0, "the healed key must expire, PTTL = {ttl_ms}");
}
