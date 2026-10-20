//! Helpers shared by the tests that need a real Redis.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use redis::aio::ConnectionManager;

/// Connects to `REDIS_URL` (default `redis://127.0.0.1:6379`).
///
/// Returns `None`, so the test is skipped, when no server answers, unless
/// `TRT_REQUIRE_REDIS=1` (set in CI) turns that into a failure.
pub async fn connect() -> Option<ConnectionManager> {
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
pub fn unique_prefix(test: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("trt-test:{test}:{nanos}:")
}
