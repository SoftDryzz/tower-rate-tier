use std::sync::Arc;
use std::time::Duration;

use tower_rate_tier::clock::{Clock, FakeClock};
use tower_rate_tier::gc::GcHandle;
use tower_rate_tier::storage::memory::MemoryStorage;
use tower_rate_tier::storage::Storage;
use tower_rate_tier::Quota;

#[tokio::test]
async fn basic_allow_and_deny() {
    let storage = MemoryStorage::new();
    let q = Quota::per_second(3);
    let now = 1_000_000_000;

    assert!(storage.check_and_update("u1", &q, 1, now).await.is_ok());
    assert!(storage.check_and_update("u1", &q, 1, now).await.is_ok());
    assert!(storage.check_and_update("u1", &q, 1, now).await.is_ok());
    assert!(storage.check_and_update("u1", &q, 1, now).await.is_err());
}

#[tokio::test]
async fn independent_keys() {
    let storage = MemoryStorage::new();
    let q = Quota::per_second(1);
    let now = 1_000_000_000;

    assert!(storage.check_and_update("u1", &q, 1, now).await.is_ok());
    assert!(storage.check_and_update("u2", &q, 1, now).await.is_ok());

    // u1 is exhausted, u2 is exhausted, but they don't interfere
    assert!(storage.check_and_update("u1", &q, 1, now).await.is_err());
    assert!(storage.check_and_update("u2", &q, 1, now).await.is_err());
}

#[tokio::test]
async fn recovery_after_time() {
    let storage = MemoryStorage::new();
    let q = Quota::per_second(1);
    let now = 1_000_000_000;

    assert!(storage.check_and_update("u1", &q, 1, now).await.is_ok());
    assert!(storage.check_and_update("u1", &q, 1, now).await.is_err());

    let later = now + Duration::from_secs(1).as_nanos() as u64;
    assert!(storage.check_and_update("u1", &q, 1, later).await.is_ok());
}

#[tokio::test]
async fn cost_consumes_multiple() {
    let storage = MemoryStorage::new();
    let q = Quota::per_second(10);
    let now = 1_000_000_000;

    let info = storage.check_and_update("u1", &q, 7, now).await.unwrap();
    assert_eq!(info.remaining, 3);

    assert!(storage.check_and_update("u1", &q, 5, now).await.is_err());
    assert!(storage.check_and_update("u1", &q, 3, now).await.is_ok());
}

#[tokio::test]
async fn remaining_accuracy() {
    let storage = MemoryStorage::new();
    let q = Quota::per_second(5);
    let now = 1_000_000_000;

    for expected_remaining in (0..5).rev() {
        let info = storage.check_and_update("u1", &q, 1, now).await.unwrap();
        assert_eq!(info.remaining, expected_remaining);
    }
}

#[tokio::test]
async fn concurrent_access() {
    let storage = Arc::new(MemoryStorage::new());
    let q = Quota::per_second(100);
    let now = 1_000_000_000;

    let mut handles = Vec::new();
    for _ in 0..200 {
        let s = storage.clone();
        let quota = q;
        handles.push(tokio::spawn(async move {
            s.check_and_update("shared", &quota, 1, now).await.is_ok()
        }));
    }

    let mut allowed = 0;
    for h in handles {
        if h.await.unwrap() {
            allowed += 1;
        }
    }

    assert_eq!(allowed, 100, "exactly 100 of 200 requests should be allowed");
}

#[tokio::test]
async fn len_and_is_empty() {
    let storage = MemoryStorage::new();
    let q = Quota::per_second(5);
    let now = 1_000_000_000;

    assert!(storage.is_empty());
    assert_eq!(storage.len(), 0);

    storage.check_and_update("u1", &q, 1, now).await.ok();
    storage.check_and_update("u2", &q, 1, now).await.ok();

    assert_eq!(storage.len(), 2);
    assert!(!storage.is_empty());
}

#[tokio::test]
async fn retain_active_removes_expired() {
    let storage = MemoryStorage::new();
    let q = Quota::per_second(1);
    let now = 1_000_000_000;

    storage.check_and_update("u1", &q, 1, now).await.ok();
    storage.check_and_update("u2", &q, 1, now).await.ok();
    assert_eq!(storage.len(), 2);

    // Advance well past expiry
    let far_future = now + Duration::from_secs(10).as_nanos() as u64;
    storage.retain_active(far_future);
    assert_eq!(storage.len(), 0);
}

#[tokio::test]
async fn retain_active_preserves_active() {
    let storage = MemoryStorage::new();
    let q = Quota::per_second(1);
    let now = 1_000_000_000;

    storage.check_and_update("u1", &q, 1, now).await.ok();
    assert_eq!(storage.len(), 1);

    // Retain at current time — entry TAT is in the future, should be kept
    storage.retain_active(now);
    assert_eq!(storage.len(), 1);
}

#[tokio::test]
async fn gc_cleans_expired_entries() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let storage = Arc::new(MemoryStorage::new());
    let q = Quota::per_second(1);

    storage
        .check_and_update("u1", &q, 1, clock.now())
        .await
        .ok();
    assert_eq!(storage.len(), 1);

    // Spawn GC with short interval
    let _gc = GcHandle::spawn(
        storage.clone(),
        Arc::new(clock.clone()),
        Duration::from_millis(50),
    );

    // Advance clock past expiry
    clock.advance(Duration::from_secs(10));

    // Wait for GC to run
    tokio::time::sleep(Duration::from_millis(150)).await;

    assert_eq!(storage.len(), 0);
}

#[tokio::test]
async fn gc_handle_aborts_on_drop() {
    let clock = FakeClock::new();
    let storage = Arc::new(MemoryStorage::new());

    {
        let _gc = GcHandle::spawn(
            storage.clone(),
            Arc::new(clock.clone()),
            Duration::from_millis(10),
        );
        // _gc dropped here
    }

    // If the task wasn't aborted, this would panic or misbehave
    tokio::time::sleep(Duration::from_millis(50)).await;
}
