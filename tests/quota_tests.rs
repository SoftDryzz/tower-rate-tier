use std::time::Duration;
use tower_rate_tier::Quota;

#[test]
fn per_hour_interval() {
    let q = Quota::per_hour(100);
    assert_eq!(q.max_burst(), 100);
    assert_eq!(q.replenish_interval(), Duration::from_secs(36));
    assert_eq!(q.window(), Duration::from_secs(3600));
}

#[test]
fn per_minute_interval() {
    let q = Quota::per_minute(60);
    assert_eq!(q.max_burst(), 60);
    assert_eq!(q.replenish_interval(), Duration::from_secs(1));
}

#[test]
fn per_second_interval() {
    let q = Quota::per_second(10);
    assert_eq!(q.max_burst(), 10);
    assert_eq!(q.replenish_interval(), Duration::from_millis(100));
}

#[test]
fn unlimited_is_unlimited() {
    let q = Quota::unlimited();
    assert!(q.is_unlimited());
    assert_eq!(q.max_burst(), 0);
    assert_eq!(q.replenish_interval(), Duration::ZERO);
}

#[test]
fn non_unlimited_is_not_unlimited() {
    assert!(!Quota::per_hour(100).is_unlimited());
    assert!(!Quota::per_minute(10).is_unlimited());
    assert!(!Quota::per_second(5).is_unlimited());
}

#[test]
fn emission_interval_nanos() {
    let q = Quota::per_second(10);
    // 1s / 10 = 100ms = 100_000_000 nanos
    assert_eq!(q.emission_interval_nanos(), 100_000_000);
}

#[test]
fn burst_offset_nanos() {
    let q = Quota::per_second(10);
    // emission_interval * max_burst = 100_000_000 * 10 = 1_000_000_000
    assert_eq!(q.burst_offset_nanos(), 1_000_000_000);
}

#[test]
fn per_hour_single_request() {
    let q = Quota::per_hour(1);
    assert_eq!(q.max_burst(), 1);
    assert_eq!(q.replenish_interval(), Duration::from_secs(3600));
}

#[test]
#[should_panic(expected = "quota count must be greater than 0")]
fn per_hour_zero_panics() {
    Quota::per_hour(0);
}

#[test]
#[should_panic(expected = "quota count must be greater than 0")]
fn per_minute_zero_panics() {
    Quota::per_minute(0);
}

#[test]
#[should_panic(expected = "quota count must be greater than 0")]
fn per_second_zero_panics() {
    Quota::per_second(0);
}

#[test]
fn quota_equality() {
    let a = Quota::per_hour(100);
    let b = Quota::per_hour(100);
    assert_eq!(a, b);
}

#[test]
fn quota_inequality() {
    let a = Quota::per_hour(100);
    let b = Quota::per_hour(200);
    assert_ne!(a, b);
}

#[test]
fn quota_clone() {
    let a = Quota::per_hour(100);
    let b = a;
    assert_eq!(a, b);
}
