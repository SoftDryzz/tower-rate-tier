#![cfg(feature = "buffered-body")]

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use http::{HeaderMap, Request, Response, StatusCode};
use http_body::Frame;
use http_body_util::Full;
use tower_layer::Layer;
use tower_rate_tier::clock::FakeClock;
use tower_rate_tier::{Quota, RateTier, TierIdentifier, TierIdentity, TierLimitLayer};
use tower_service::Service;

/// Service that echoes the body back, accepting Full<Bytes>.
#[derive(Clone)]
struct EchoService;

impl Service<Request<Full<Bytes>>> for EchoService {
    type Response = Response<String>;
    type Error = std::convert::Infallible;
    type Future = std::future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: Request<Full<Bytes>>) -> Self::Future {
        std::future::ready(Ok(Response::new("ok".to_string())))
    }
}

/// Identifier that extracts user_id from JSON body.
struct BodyJsonIdentifier;

impl TierIdentifier for BodyJsonIdentifier {
    fn identify(
        &self,
        _headers: &HeaderMap,
    ) -> Pin<Box<dyn Future<Output = Option<TierIdentity>> + Send + '_>> {
        Box::pin(std::future::ready(None))
    }

    fn identify_with_body(
        &self,
        _headers: &HeaderMap,
        body: &Bytes,
    ) -> Pin<Box<dyn Future<Output = Option<TierIdentity>> + Send + '_>> {
        let result = (|| {
            let body_str = std::str::from_utf8(body).ok()?;
            let parsed: serde_json::Value = serde_json::from_str(body_str).ok()?;
            let user_id = parsed.get("user_id")?.as_str()?;
            let tier = parsed
                .get("tier")
                .and_then(|v| v.as_str())
                .unwrap_or("free");
            Some(TierIdentity::new(user_id, tier))
        })();
        Box::pin(std::future::ready(result))
    }
}

fn make_buffered_layer(clock: FakeClock) -> tower_rate_tier::BufferedTierLimitLayer {
    let rate_tier = RateTier::builder()
        .tier("free", Quota::per_second(2))
        .default_tier("free")
        .clock(clock)
        .build();

    TierLimitLayer::new(rate_tier)
        .identifier(BodyJsonIdentifier)
        .buffer_body()
}

fn json_request(body: &str) -> Request<Full<Bytes>> {
    Request::builder()
        .header("Content-Type", "application/json")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap()
}

#[tokio::test]
async fn body_based_identification_allows() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let layer = make_buffered_layer(clock);
    let mut svc = layer.layer(EchoService);

    let req = json_request(r#"{"user_id": "alice", "tier": "free"}"#);
    let resp = svc.call(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().contains_key("x-ratelimit-limit"));
}

#[tokio::test]
async fn body_based_identification_rate_limits() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let layer = make_buffered_layer(clock);
    let mut svc = layer.layer(EchoService);

    // Exhaust free tier (2/sec)
    svc.call(json_request(r#"{"user_id": "alice"}"#))
        .await
        .unwrap();
    svc.call(json_request(r#"{"user_id": "alice"}"#))
        .await
        .unwrap();

    let resp = svc
        .call(json_request(r#"{"user_id": "alice"}"#))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn body_too_large_returns_413() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let layer = make_buffered_layer(clock).max_body_size(32);
    let mut svc = layer.layer(EchoService);

    // Body larger than 32 bytes
    let large_body = r#"{"user_id": "alice", "extra": "this is too long for the limit"}"#;
    let req = json_request(large_body);
    let resp = svc.call(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn body_within_limit_passes() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let layer = make_buffered_layer(clock).max_body_size(1024);
    let mut svc = layer.layer(EchoService);

    let req = json_request(r#"{"user_id": "bob"}"#);
    let resp = svc.call(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn invalid_body_falls_to_on_missing() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let layer = make_buffered_layer(clock);
    let mut svc = layer.layer(EchoService);

    // Invalid JSON → identify_with_body returns None → UseDefault → "free" tier
    let req = json_request("not json at all");
    let resp = svc.call(req).await.unwrap();
    // UseDefault with default_tier("free") → rate limited as anonymous
    assert_eq!(resp.status(), StatusCode::OK);
}

/// A body that always errors when polled.
struct ErrorBody;

impl http_body::Body for ErrorBody {
    type Data = Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(Some(Err("simulated body read error".into())))
    }
}

#[tokio::test]
async fn body_read_error_returns_400() {
    let clock = FakeClock::new();
    clock.set(1_000_000_000);

    let layer = make_buffered_layer(clock);
    let mut svc = layer.layer(EchoService);

    let req = Request::builder()
        .header("Content-Type", "application/json")
        .body(ErrorBody)
        .unwrap();

    let resp = svc.call(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
