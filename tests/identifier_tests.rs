use std::future::Future;
use std::pin::Pin;

use http::HeaderMap;
use tower_rate_tier::identifier::TierIdentity;
use tower_rate_tier::TierIdentifier;

struct MockIdentifier;

impl TierIdentifier for MockIdentifier {
    fn identify(
        &self,
        headers: &HeaderMap,
    ) -> Pin<Box<dyn Future<Output = Option<TierIdentity>> + Send + '_>> {
        let result = headers
            .get("X-Api-Key")
            .and_then(|v| v.to_str().ok())
            .map(|key| TierIdentity::new(key, "free"));
        Box::pin(std::future::ready(result))
    }
}

#[tokio::test]
async fn trait_identifier_extracts_from_header() {
    let id = MockIdentifier;
    let mut headers = HeaderMap::new();
    headers.insert("X-Api-Key", "test-key-123".parse().unwrap());

    let result = id.identify(&headers).await;
    assert!(result.is_some());
    let identity = result.unwrap();
    assert_eq!(identity.user_id, "test-key-123");
    assert_eq!(identity.tier, "free");
}

#[tokio::test]
async fn trait_identifier_returns_none_on_missing_header() {
    let id = MockIdentifier;
    let headers = HeaderMap::new();
    assert!(id.identify(&headers).await.is_none());
}

#[tokio::test]
async fn identify_with_body_defaults_to_identify() {
    let id = MockIdentifier;
    let mut headers = HeaderMap::new();
    headers.insert("X-Api-Key", "body-test".parse().unwrap());
    let body = bytes::Bytes::from("some body");

    let result = id.identify_with_body(&headers, &body).await;
    assert!(result.is_some());
    assert_eq!(result.unwrap().user_id, "body-test");
}

#[test]
fn tier_identity_new() {
    let id = TierIdentity::new("user1", "pro");
    assert_eq!(id.user_id, "user1");
    assert_eq!(id.tier, "pro");
}

#[test]
fn tier_identity_from_string() {
    let id = TierIdentity::new(String::from("user1"), String::from("pro"));
    assert_eq!(id.user_id, "user1");
    assert_eq!(id.tier, "pro");
}

#[test]
fn tier_identity_equality() {
    let a = TierIdentity::new("u1", "free");
    let b = TierIdentity::new("u1", "free");
    assert_eq!(a, b);
}

#[test]
fn tier_identity_inequality() {
    let a = TierIdentity::new("u1", "free");
    let b = TierIdentity::new("u1", "pro");
    assert_ne!(a, b);
}
