use std::sync::{Arc, Barrier};
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

    assert!(storage
        .check_and_update("u1", &q, 1, now)
        .await
        .unwrap()
        .is_ok());
    assert!(storage
        .check_and_update("u1", &q, 1, now)
        .await
        .unwrap()
        .is_ok());
    assert!(storage
        .check_and_update("u1", &q, 1, now)
        .await
        .unwrap()
        .is_ok());
    assert!(storage
        .check_and_update("u1", &q, 1, now)
        .await
        .unwrap()
        .is_err());
}

#[tokio::test]
async fn independent_keys() {
    let storage = MemoryStorage::new();
    let q = Quota::per_second(1);
    let now = 1_000_000_000;

    assert!(storage
        .check_and_update("u1", &q, 1, now)
        .await
        .unwrap()
        .is_ok());
    assert!(storage
        .check_and_update("u2", &q, 1, now)
        .await
        .unwrap()
        .is_ok());

    // u1 is exhausted, u2 is exhausted, but they don't interfere
    assert!(storage
        .check_and_update("u1", &q, 1, now)
        .await
        .unwrap()
        .is_err());
    assert!(storage
        .check_and_update("u2", &q, 1, now)
        .await
        .unwrap()
        .is_err());
}

#[tokio::test]
async fn recovery_after_time() {
    let storage = MemoryStorage::new();
    let q = Quota::per_second(1);
    let now = 1_000_000_000;

    assert!(storage
        .check_and_update("u1", &q, 1, now)
        .await
        .unwrap()
        .is_ok());
    assert!(storage
        .check_and_update("u1", &q, 1, now)
        .await
        .unwrap()
        .is_err());

    let later = now + Duration::from_secs(1).as_nanos() as u64;
    assert!(storage
        .check_and_update("u1", &q, 1, later)
        .await
        .unwrap()
        .is_ok());
}

#[tokio::test]
async fn cost_consumes_multiple() {
    let storage = MemoryStorage::new();
    let q = Quota::per_second(10);
    let now = 1_000_000_000;

    let info = storage
        .check_and_update("u1", &q, 7, now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.remaining, 3);

    assert!(storage
        .check_and_update("u1", &q, 5, now)
        .await
        .unwrap()
        .is_err());
    assert!(storage
        .check_and_update("u1", &q, 3, now)
        .await
        .unwrap()
        .is_ok());
}

#[tokio::test]
async fn remaining_accuracy() {
    let storage = MemoryStorage::new();
    let q = Quota::per_second(5);
    let now = 1_000_000_000;

    for expected_remaining in (0..5).rev() {
        let info = storage
            .check_and_update("u1", &q, 1, now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(info.remaining, expected_remaining);
    }
}

#[test]
fn concurrent_access() {
    // OS threads released together by a barrier, so the checks really overlap.
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let q = Quota::per_second(100);
    let now = 1_000_000_000;

    for round in 0..20 {
        let storage = Arc::new(MemoryStorage::new());
        let barrier = Arc::new(Barrier::new(200));

        let handles: Vec<_> = (0..200)
            .map(|_| {
                let s = storage.clone();
                let b = barrier.clone();
                let handle = rt.handle().clone();
                std::thread::spawn(move || {
                    b.wait();
                    handle
                        .block_on(s.check_and_update("shared", &q, 1, now))
                        .unwrap()
                        .is_ok()
                })
            })
            .collect();

        let allowed = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|ok| *ok)
            .count();

        assert_eq!(
            allowed, 100,
            "round {round}: exactly 100 of 200 requests should be allowed"
        );
    }
}

#[tokio::test]
async fn len_and_is_empty() {
    let storage = MemoryStorage::new();
    let q = Quota::per_second(5);
    let now = 1_000_000_000;

    assert!(storage.is_empty());
    assert_eq!(storage.len(), 0);

    let _ = storage.check_and_update("u1", &q, 1, now).await;
    let _ = storage.check_and_update("u2", &q, 1, now).await;

    assert_eq!(storage.len(), 2);
    assert!(!storage.is_empty());
}

#[tokio::test]
async fn retain_active_removes_expired() {
    let storage = MemoryStorage::new();
    let q = Quota::per_second(1);
    let now = 1_000_000_000;

    let _ = storage.check_and_update("u1", &q, 1, now).await;
    let _ = storage.check_and_update("u2", &q, 1, now).await;
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

    let _ = storage.check_and_update("u1", &q, 1, now).await;
    assert_eq!(storage.len(), 1);

    // Retain at current time — entry TAT is in the future, should be kept
    storage.retain_active(now);
    assert_eq!(storage.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn gc_cleans_expired_entries() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let storage = Arc::new(MemoryStorage::new());
    let q = Quota::per_second(1);

    let _ = storage.check_and_update("u1", &q, 1, clock.now()).await;
    assert_eq!(storage.len(), 1);

    let _gc = GcHandle::spawn(
        storage.clone(),
        Arc::new(clock.clone()),
        Duration::from_millis(50),
    );

    // The first tick fires at once, while the entry is still live.
    tokio::task::yield_now().await;
    assert_eq!(storage.len(), 1);

    // Expire the entry, then move Tokio's paused clock to the second tick.
    clock.advance(Duration::from_secs(10));
    tokio::time::advance(Duration::from_millis(50)).await;
    tokio::task::yield_now().await;

    assert_eq!(storage.len(), 0);
}

#[tokio::test]
#[should_panic(expected = "gc interval must be non-zero")]
async fn gc_handle_rejects_zero_interval() {
    let _gc = GcHandle::spawn(
        Arc::new(MemoryStorage::new()),
        Arc::new(FakeClock::new()),
        Duration::ZERO,
    );
}

#[tokio::test(start_paused = true)]
async fn gc_handle_aborts_on_drop() {
    let clock = FakeClock::new();
    let storage = Arc::new(MemoryStorage::new());
    let _ = storage
        .check_and_update("u1", &Quota::per_second(1), 1, clock.now())
        .await;

    drop(GcHandle::spawn(
        storage.clone(),
        Arc::new(clock.clone()),
        Duration::from_millis(10),
    ));

    // The entry expires, but an aborted GC task must never collect it.
    clock.advance(Duration::from_secs(10));
    tokio::time::advance(Duration::from_millis(100)).await;
    tokio::task::yield_now().await;

    assert_eq!(storage.len(), 1);
}
