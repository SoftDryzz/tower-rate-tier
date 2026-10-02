use std::time::{Duration, SystemTime, UNIX_EPOCH};

use http::header::HeaderValue;
use http::{Response, StatusCode};

use crate::gcra::{RateLimitInfo, RateLimited};

/// Inject `X-RateLimit-*` headers into a successful response.
///
/// `now` is the current wall-clock time; `X-RateLimit-Reset` is the Unix
/// timestamp `now + reset_after`, rounded up to whole seconds.
pub fn inject_headers<B>(response: &mut Response<B>, info: &RateLimitInfo, now: SystemTime) {
    let headers = response.headers_mut();
    headers.insert("X-RateLimit-Limit", HeaderValue::from(info.limit));
    headers.insert("X-RateLimit-Remaining", HeaderValue::from(info.remaining));
    headers.insert(
        "X-RateLimit-Reset",
        reset_header_value(info.reset_after, now),
    );
}

/// Build a 429 Too Many Requests response with JSON body and rate limit headers.
///
/// `Retry-After` and the body's `retry_after` are the same value, rounded up
/// to whole seconds so a client that waits that long is allowed. `now` is the
/// current wall-clock time, used for `X-RateLimit-Reset`.
pub fn rate_limited_response(
    limited: &RateLimited,
    tier: &str,
    now: SystemTime,
) -> Response<String> {
    let retry_after_secs = ceil_secs(limited.retry_after);

    let escaped_tier = escape_json_string(tier);
    let body = format!(
        r#"{{"error":"rate limit exceeded","tier":"{}","retry_after":{}}}"#,
        escaped_tier, retry_after_secs
    );

    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header("Content-Type", "application/json")
        .header("Retry-After", retry_after_secs)
        .header("X-RateLimit-Limit", limited.limit)
        .header("X-RateLimit-Remaining", 0u32)
        .header(
            "X-RateLimit-Reset",
            reset_header_value(limited.reset_after, now),
        )
        .body(body)
        .unwrap()
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

/// Unix timestamp, in whole seconds rounded up, of `now + reset_after`.
fn reset_header_value(reset_after: Duration, now: SystemTime) -> HeaderValue {
    let secs = now
        .checked_add(reset_after)
        .and_then(|reset| reset.duration_since(UNIX_EPOCH).ok())
        .map_or(u64::MAX, ceil_secs);
    HeaderValue::from(secs)
}

/// Whole seconds, rounded up.
fn ceil_secs(duration: Duration) -> u64 {
    let round_up = u64::from(duration.subsec_nanos() > 0);
    duration.as_secs().saturating_add(round_up)
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

/// Escape a string for safe embedding in a JSON string value.
///
/// Handles `"`, `\`, and control characters per RFC 8259.
fn escape_json_string(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            c if c.is_control() => {
                escaped.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limited(retry_after: Duration) -> RateLimited {
        RateLimited {
            limit: 1,
            retry_after,
            reset_after: Duration::from_secs(60),
        }
    }

    #[test]
    fn retry_after_is_rounded_up_in_header_and_body() {
        let resp =
            rate_limited_response(&limited(Duration::from_millis(29_500)), "free", UNIX_EPOCH);

        assert_eq!(resp.headers()["retry-after"], "30");
        assert!(
            resp.body().contains(r#""retry_after":30"#),
            "{}",
            resp.body()
        );
    }

    #[test]
    fn sub_second_retry_after_is_one_in_header_and_body() {
        let resp = rate_limited_response(&limited(Duration::from_millis(500)), "free", UNIX_EPOCH);

        assert_eq!(resp.headers()["retry-after"], "1");
        assert!(
            resp.body().contains(r#""retry_after":1"#),
            "{}",
            resp.body()
        );
    }

    #[test]
    fn reset_header_is_unix_time_rounded_up() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let info = RateLimitInfo {
            limit: 10,
            remaining: 9,
            reset_after: Duration::from_millis(2_500),
        };
        let mut resp = Response::new(());

        inject_headers(&mut resp, &info, now);

        assert_eq!(resp.headers()["x-ratelimit-reset"], "1003");
    }
}
