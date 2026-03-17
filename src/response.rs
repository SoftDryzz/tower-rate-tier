use http::header::HeaderValue;
use http::{Response, StatusCode};

use crate::gcra::{RateLimitInfo, RateLimited};

/// Inject `X-RateLimit-*` headers into a successful response.
///
/// `unix_offset_nanos` is added to `reset_at` to produce a Unix timestamp
/// for the `X-RateLimit-Reset` header.
pub fn inject_headers<B>(response: &mut Response<B>, info: &RateLimitInfo, unix_offset_nanos: u64) {
    let headers = response.headers_mut();
    headers.insert(
        "X-RateLimit-Limit",
        HeaderValue::from(info.limit),
    );
    headers.insert(
        "X-RateLimit-Remaining",
        HeaderValue::from(info.remaining),
    );
    headers.insert(
        "X-RateLimit-Reset",
        reset_header_value(info.reset_at, unix_offset_nanos),
    );
}

/// Build a 429 Too Many Requests response with JSON body and rate limit headers.
///
/// `unix_offset_nanos` is added to `reset_at` to produce a Unix timestamp
/// for the `X-RateLimit-Reset` header.
pub fn rate_limited_response(limited: &RateLimited, tier: &str, unix_offset_nanos: u64) -> Response<String> {
    let retry_after_secs = limited.retry_after.as_secs();

    let body = format!(
        r#"{{"error":"rate limit exceeded","tier":"{}","retry_after":{}}}"#,
        tier, retry_after_secs
    );

    let mut response = Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header("Content-Type", "application/json")
        .header("Retry-After", retry_after_secs)
        .header("X-RateLimit-Limit", limited.limit)
        .header("X-RateLimit-Remaining", 0u32)
        .header("X-RateLimit-Reset", reset_header_value(limited.reset_at, unix_offset_nanos))
        .body(body)
        .unwrap();

    // Ensure Retry-After is at least 1 second when there's a non-zero duration
    if retry_after_secs == 0 && !limited.retry_after.is_zero() {
        response
            .headers_mut()
            .insert("Retry-After", HeaderValue::from(1u64));
    }

    response
}

/// Build a response for when the identifier cannot determine the user/tier
/// and the policy is `OnMissing::Deny(status)`.
pub fn deny_response(status: StatusCode) -> Response<String> {
    Response::builder()
        .status(status)
        .header("Content-Type", "application/json")
        .body(format!(r#"{{"error":"{}"}}"#, canonical_reason(status)))
        .unwrap()
}

/// Build a 503 Service Unavailable response for storage errors.
pub fn storage_error_response() -> Response<String> {
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("Content-Type", "application/json")
        .body(r#"{"error":"service unavailable"}"#.to_string())
        .unwrap()
}

/// Convert an internal `reset_at` value to a Unix-timestamp `HeaderValue`.
///
/// Adds `unix_offset_nanos` to convert from process-local epoch to Unix epoch,
/// then divides by 1e9 to get seconds.
fn reset_header_value(reset_at_nanos: u64, unix_offset_nanos: u64) -> HeaderValue {
    let unix_nanos = reset_at_nanos.saturating_add(unix_offset_nanos);
    let secs = unix_nanos / 1_000_000_000;
    HeaderValue::from(secs)
}

/// Build a 400 Bad Request response for body read errors.
pub fn bad_request_response() -> Response<String> {
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .header("Content-Type", "application/json")
        .body(r#"{"error":"failed to read request body"}"#.to_string())
        .unwrap()
}

fn canonical_reason(status: StatusCode) -> &'static str {
    status.canonical_reason().unwrap_or("request denied")
}
