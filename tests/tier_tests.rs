use std::time::Duration;
use tower_rate_tier::clock::FakeClock;
use tower_rate_tier::on_missing::OnMissing;
use tower_rate_tier::{Quota, RateTier};

#[tokio::test]
#[should_panic(expected = "at least one tier must be defined")]
async fn build_with_no_tiers_panics() {
    RateTier::builder().build();
}

#[tokio::test]
#[should_panic(expected = "does not exist in defined tiers")]
async fn build_with_invalid_default_tier_panics() {
    RateTier::builder()
        .tier("free", Quota::per_hour(100))
        .default_tier("nonexistent")
        .build();
}

#[tokio::test]
async fn build_with_valid_config() {
    let _limiter = RateTier::builder()
        .tier("free", Quota::per_hour(100))
        .tier("pro", Quota::per_hour(5_000))
        .default_tier("free")
        .build();
}

#[tokio::test]
async fn get_quota_returns_correct_quota() {
    let limiter = RateTier::builder()
        .tier("free", Quota::per_hour(100))
        .tier("pro", Quota::per_hour(5_000))
        .build();

    assert_eq!(limiter.get_quota("free"), Some(&Quota::per_hour(100)));
    assert_eq!(limiter.get_quota("pro"), Some(&Quota::per_hour(5_000)));
    assert_eq!(limiter.get_quota("nonexistent"), None);
}

#[tokio::test]
async fn default_tier_accessor() {
    let with_default = RateTier::builder()
        .tier("free", Quota::per_hour(100))
        .default_tier("free")
        .build();
    assert_eq!(with_default.default_tier(), Some("free"));

    let without_default = RateTier::builder()
        .tier("free", Quota::per_hour(100))
        .build();
    assert_eq!(without_default.default_tier(), None);
}

#[tokio::test]
async fn on_missing_default_is_use_default() {
    let limiter = RateTier::builder()
        .tier("free", Quota::per_hour(100))
        .build();
    assert!(matches!(limiter.on_missing(), OnMissing::UseDefault));
}

#[tokio::test]
async fn on_missing_can_be_set() {
    let limiter = RateTier::builder()
        .tier("free", Quota::per_hour(100))
        .on_missing(OnMissing::Allow)
        .build();
    assert!(matches!(limiter.on_missing(), OnMissing::Allow));
}

#[tokio::test]
async fn check_basic_allow_and_deny() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let limiter = RateTier::builder()
        .tier("free", Quota::per_second(2))
        .clock(clock)
        .build();

    assert!(limiter.check("u1", "free", 1).await.unwrap().is_ok());
    assert!(limiter.check("u1", "free", 1).await.unwrap().is_ok());
    assert!(limiter.check("u1", "free", 1).await.unwrap().is_err());
}

#[tokio::test]
async fn check_unlimited_always_allows() {
    let limiter = RateTier::builder()
        .tier("enterprise", Quota::unlimited())
        .clock(FakeClock::new())
        .build();

    for _ in 0..1000 {
        assert!(limiter.check("u1", "enterprise", 1).await.unwrap().is_ok());
    }
}

#[tokio::test]
async fn check_recovery_after_time() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let limiter = RateTier::builder()
        .tier("free", Quota::per_second(1))
        .clock(clock.clone())
        .build();

    assert!(limiter.check("u1", "free", 1).await.unwrap().is_ok());
    assert!(limiter.check("u1", "free", 1).await.unwrap().is_err());

    clock.advance(Duration::from_secs(1));
    assert!(limiter.check("u1", "free", 1).await.unwrap().is_ok());
}

#[tokio::test]
async fn check_different_tiers_different_limits() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let limiter = RateTier::builder()
        .tier("free", Quota::per_second(1))
        .tier("pro", Quota::per_second(5))
        .clock(clock)
        .build();

    // Free user exhausts after 1
    assert!(limiter.check("u1", "free", 1).await.unwrap().is_ok());
    assert!(limiter.check("u1", "free", 1).await.unwrap().is_err());

    // Pro user still has quota
    for _ in 0..5 {
        assert!(limiter.check("u2", "pro", 1).await.unwrap().is_ok());
    }
    assert!(limiter.check("u2", "pro", 1).await.unwrap().is_err());
}

#[tokio::test]
#[should_panic(expected = "unknown tier")]
async fn check_unknown_tier_panics() {
    let limiter = RateTier::builder()
        .tier("free", Quota::per_hour(100))
        .clock(FakeClock::new())
        .build();

    let _ = limiter.check("u1", "nonexistent", 1).await;
}

#[tokio::test]
async fn build_with_custom_gc_interval() {
    let _limiter = RateTier::builder()
        .tier("free", Quota::per_hour(100))
        .gc_interval(Duration::from_secs(30))
        .build();
}

#[tokio::test]
async fn check_with_cost() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let limiter = RateTier::builder()
        .tier("free", Quota::per_second(10))
        .clock(clock)
        .build();

    let info = limiter.check("u1", "free", 7).await.unwrap().unwrap();
    assert_eq!(info.remaining, 3);

    assert!(limiter.check("u1", "free", 5).await.unwrap().is_err());
    assert!(limiter.check("u1", "free", 3).await.unwrap().is_ok());
}
