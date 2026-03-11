use std::time::Duration;
use tower_rate_tier::gcra::check_gcra;
use tower_rate_tier::Quota;

fn quota_params(q: &Quota) -> (u64, u64) {
    (q.emission_interval_nanos(), q.burst_offset_nanos())
}

#[test]
fn single_request_allowed() {
    let q = Quota::per_hour(100);
    let (ei, bo) = quota_params(&q);
    let result = check_gcra(None, 0, ei, bo, 1);
    assert!(result.is_ok());
    let (_, info) = result.unwrap();
    assert_eq!(info.limit, 100);
    assert_eq!(info.remaining, 99);
}

#[test]
fn burst_fills_quota_exactly() {
    let q = Quota::per_second(5);
    let (ei, bo) = quota_params(&q);
    let now = 0;
    let mut tat = None;

    for i in 0..5 {
        let result = check_gcra(tat, now, ei, bo, 1);
        assert!(result.is_ok(), "request {} should be allowed", i);
        let (new_tat, info) = result.unwrap();
        tat = Some(new_tat);
        assert_eq!(info.remaining, 4 - i as u32);
    }
}

#[test]
fn over_quota_denied() {
    let q = Quota::per_second(5);
    let (ei, bo) = quota_params(&q);
    let now = 0;
    let mut tat = None;

    // Fill quota
    for _ in 0..5 {
        let (new_tat, _) = check_gcra(tat, now, ei, bo, 1).unwrap();
        tat = Some(new_tat);
    }

    // 6th request should be denied
    let result = check_gcra(tat, now, ei, bo, 1);
    assert!(result.is_err());
    let limited = result.unwrap_err();
    assert_eq!(limited.limit, 5);
    assert!(limited.retry_after > Duration::ZERO);
}

#[test]
fn recovery_after_time() {
    let q = Quota::per_second(1);
    let (ei, bo) = quota_params(&q);
    let now = 0;

    // First request: allowed
    let (tat, _) = check_gcra(None, now, ei, bo, 1).unwrap();

    // Second request immediately: denied
    assert!(check_gcra(Some(tat), now, ei, bo, 1).is_err());

    // After 1 second: allowed again
    let later = Duration::from_secs(1).as_nanos() as u64;
    let result = check_gcra(Some(tat), later, ei, bo, 1);
    assert!(result.is_ok());
}

#[test]
fn cost_greater_than_one() {
    let q = Quota::per_second(10);
    let (ei, bo) = quota_params(&q);
    let now = 0;

    // Cost 5: consumes 5 of 10
    let (tat, info) = check_gcra(None, now, ei, bo, 5).unwrap();
    assert_eq!(info.remaining, 5);

    // Cost 5 again: consumes remaining 5
    let (_, info) = check_gcra(Some(tat), now, ei, bo, 5).unwrap();
    assert_eq!(info.remaining, 0);
}

#[test]
fn cost_exceeding_remaining_denied() {
    let q = Quota::per_second(10);
    let (ei, bo) = quota_params(&q);
    let now = 0;

    // Consume 8 of 10
    let (tat, _) = check_gcra(None, now, ei, bo, 8).unwrap();

    // Cost 5 with only 2 remaining: denied
    let result = check_gcra(Some(tat), now, ei, bo, 5);
    assert!(result.is_err());
}

#[test]
fn remaining_accuracy_progressive() {
    let q = Quota::per_second(10);
    let (ei, bo) = quota_params(&q);
    let now = 0;
    let mut tat = None;

    for i in 0..10 {
        let (new_tat, info) = check_gcra(tat, now, ei, bo, 1).unwrap();
        tat = Some(new_tat);
        assert_eq!(info.remaining, 9 - i as u32, "at request {}", i);
    }
}

#[test]
fn retry_after_is_positive_when_denied() {
    let q = Quota::per_second(1);
    let (ei, bo) = quota_params(&q);
    let now = 0;

    let (tat, _) = check_gcra(None, now, ei, bo, 1).unwrap();
    let err = check_gcra(Some(tat), now, ei, bo, 1).unwrap_err();
    assert!(err.retry_after > Duration::ZERO);
    assert!(err.retry_after <= Duration::from_secs(1));
}

#[test]
fn first_request_with_none_state() {
    let q = Quota::per_minute(60);
    let (ei, bo) = quota_params(&q);
    let now = 1_000_000_000; // 1 second in

    let result = check_gcra(None, now, ei, bo, 1);
    assert!(result.is_ok());
    let (_, info) = result.unwrap();
    assert_eq!(info.limit, 60);
    assert_eq!(info.remaining, 59);
}

#[test]
fn stale_tat_resets_to_now() {
    let q = Quota::per_second(5);
    let (ei, bo) = quota_params(&q);

    // Old TAT from long ago
    let old_tat = 0;
    let now = 10_000_000_000; // 10 seconds later

    let result = check_gcra(Some(old_tat), now, ei, bo, 1);
    assert!(result.is_ok());
    let (_, info) = result.unwrap();
    // Should be like a fresh start since tat < now
    assert_eq!(info.remaining, 4);
}

#[test]
fn cost_zero_allowed_without_consuming() {
    let q = Quota::per_second(5);
    let (ei, bo) = quota_params(&q);
    let now = 0;

    let (_, info) = check_gcra(None, now, ei, bo, 0).unwrap();
    assert_eq!(info.remaining, 5);
}
