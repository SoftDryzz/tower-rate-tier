use std::fmt;
use std::time::Duration;

use redis::aio::{ConnectionLike, ConnectionManager};
use redis::Script;

use crate::gcra::{RateLimitInfo, RateLimited};
use crate::quota::{Nanos, Quota};
use crate::storage::{Storage, StorageError, StorageFuture, StorageKey};

/// The GCRA check, run atomically by Redis.
const GCRA_SCRIPT: &str = include_str!("gcra.lua");

/// How long a check waits for Redis before it counts as a storage error.
const DEFAULT_TIMEOUT: Duration = Duration::from_millis(100);

/// Script reply: `[allowed, remaining, retry_after_us, reset_after_us]`.
type Reply = (i64, i64, i64, i64);

/// Redis-backed rate limit storage, for deployments with several instances.
///
/// Each check runs one Lua script atomically with `EVALSHA`, and the script
/// is loaded again automatically if Redis answers `NOSCRIPT` (after a
/// restart, a `SCRIPT FLUSH` or a failover). By default the script reads the
/// time with Redis's `TIME`, so instances never disagree about the clock.
///
/// Keys look like `trt:<tier>:<sha1 of the user id>`, so user ids such as
/// API keys never appear in Redis. Each key expires when its bucket is full
/// again, so no garbage collection is needed.
///
/// A check that gets no answer within the [timeout](Self::timeout) (100 ms by
/// default) fails with a [`StorageError`], which the
/// [`OnStorageError`](crate::OnStorageError) policy then handles.
///
/// The connection can be any [`ConnectionLike`] that is cheap to clone, such
/// as [`ConnectionManager`] (the default), a `MultiplexedConnection`, or a
/// cluster connection.
///
/// Requires the `redis` feature.
///
/// # Examples
///
/// ```rust,no_run
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// use std::sync::Arc;
/// use tower_rate_tier::{Quota, RateTier, RedisStorage};
///
/// let client = redis::Client::open("redis://127.0.0.1:6379")?;
/// let conn = client.get_connection_manager().await?;
///
/// let rate_tier = RateTier::builder()
///     .tier("free", Quota::per_hour(100))
///     .storage(Arc::new(RedisStorage::new(conn)))
///     .build();
/// # Ok(())
/// # }
/// ```
pub struct RedisStorage<C = ConnectionManager> {
    conn: C,
    script: Script,
    key_prefix: String,
    timeout: Duration,
    hash_user_ids: bool,
    client_clock: bool,
}

impl<C> RedisStorage<C> {
    /// Creates a storage that sends its checks over `conn`.
    pub fn new(conn: C) -> Self {
        Self {
            conn,
            script: Script::new(GCRA_SCRIPT),
            key_prefix: "trt:".to_owned(),
            timeout: DEFAULT_TIMEOUT,
            hash_user_ids: true,
            client_clock: false,
        }
    }

    /// Sets the prefix of every key. Default: `trt:`.
    pub fn key_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.key_prefix = prefix.into();
        self
    }

    /// Sets how long a check waits for Redis. Default: 100 ms.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Puts user ids in keys as they are, instead of their SHA-1 hash.
    ///
    /// Keys become readable in `redis-cli`, but every user id (often an API
    /// key) is stored in Redis in plain text.
    pub fn plain_user_ids(mut self) -> Self {
        self.hash_user_ids = false;
        self
    }

    /// Uses the rate limiter's [`Clock`](crate::clock::Clock) instead of
    /// Redis's `TIME`.
    ///
    /// Meant for tests with [`FakeClock`](crate::clock::FakeClock). The
    /// default `SystemClock` counts from when each process started, so
    /// instances sharing Redis must keep the default server time.
    pub fn use_client_clock(mut self) -> Self {
        self.client_clock = true;
        self
    }

    /// The Redis key that holds the state of `key`.
    ///
    /// Distinct keys always map to distinct Redis keys: the hashed form ends
    /// with a fixed-length digest, and the plain form length-prefixes the tier.
    pub fn redis_key(&self, key: StorageKey<'_>) -> String {
        if self.hash_user_ids {
            let digest = sha1_smol::Sha1::from(key.user_id).digest().to_string();
            format!("{}{}:{}", self.key_prefix, key.tier, digest)
        } else {
            format!(
                "{}{}:{}:{}",
                self.key_prefix,
                key.tier.len(),
                key.tier,
                key.user_id
            )
        }
    }
}

impl<C> fmt::Debug for RedisStorage<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RedisStorage")
            .field("key_prefix", &self.key_prefix)
            .field("timeout", &self.timeout)
            .field("hash_user_ids", &self.hash_user_ids)
            .field("client_clock", &self.client_clock)
            .finish_non_exhaustive()
    }
}

impl<C> Storage for RedisStorage<C>
where
    C: ConnectionLike + Clone + Send + Sync + 'static,
{
    fn check_and_update<'a>(
        &'a self,
        key: StorageKey<'a>,
        quota: &'a Quota,
        cost: u32,
        now: Nanos,
    ) -> StorageFuture<'a> {
        Box::pin(async move {
            let emission_interval = micros_ceil(quota.emission_interval_nanos()).max(1);
            let burst_offset = emission_interval.saturating_mul(u64::from(quota.max_burst()));
            // An empty time tells the script to read Redis's own clock.
            let now_arg = if self.client_clock {
                (now / 1_000).to_string()
            } else {
                String::new()
            };

            let mut invocation = self.script.prepare_invoke();
            invocation
                .key(self.redis_key(key))
                .arg(now_arg)
                .arg(emission_interval)
                .arg(burst_offset)
                .arg(cost);

            let mut conn = self.conn.clone();
            let call = invocation.invoke_async::<Reply>(&mut conn);
            match tokio::time::timeout(self.timeout, call).await {
                Ok(Ok(reply)) => Ok(decode_reply(reply, quota.max_burst())),
                Ok(Err(err)) => Err(StorageError(Box::new(err))),
                Err(_) => Err(StorageError(
                    format!("redis did not answer within {:?}", self.timeout).into(),
                )),
            }
        })
    }
}

/// Microseconds, rounded up so a quota never becomes looser in Redis.
fn micros_ceil(nanos: Nanos) -> u64 {
    nanos / 1_000 + u64::from(nanos % 1_000 != 0)
}

/// Turns the script's reply into the GCRA decision.
fn decode_reply(
    (allowed, remaining, retry_after_us, reset_after_us): Reply,
    limit: u32,
) -> Result<RateLimitInfo, RateLimited> {
    let micros = |value: i64| Duration::from_micros(u64::try_from(value).unwrap_or(0));
    if allowed == 1 {
        Ok(RateLimitInfo {
            limit,
            remaining: u32::try_from(remaining.max(0)).unwrap_or(u32::MAX),
            reset_after: micros(reset_after_us),
        })
    } else {
        Err(RateLimited {
            limit,
            retry_after: micros(retry_after_us),
            reset_after: micros(reset_after_us),
        })
    }
}

#[cfg(test)]
mod tests {
    use redis::{Cmd, Pipeline, RedisFuture, Value};

    use super::*;

    /// A connection whose commands never get an answer.
    #[derive(Clone)]
    struct NeverAnswers;

    impl ConnectionLike for NeverAnswers {
        fn req_packed_command<'a>(&'a mut self, _cmd: &'a Cmd) -> RedisFuture<'a, Value> {
            Box::pin(std::future::pending())
        }

        fn req_packed_commands<'a>(
            &'a mut self,
            _pipeline: &'a Pipeline,
            _offset: usize,
            _count: usize,
        ) -> RedisFuture<'a, Vec<Value>> {
            Box::pin(std::future::pending())
        }

        fn get_db(&self) -> i64 {
            0
        }
    }

    /// A connection whose commands all fail, like a refused socket.
    #[derive(Clone)]
    struct Refused;

    impl ConnectionLike for Refused {
        fn req_packed_command<'a>(&'a mut self, _cmd: &'a Cmd) -> RedisFuture<'a, Value> {
            Box::pin(std::future::ready(Err(refused())))
        }

        fn req_packed_commands<'a>(
            &'a mut self,
            _pipeline: &'a Pipeline,
            _offset: usize,
            _count: usize,
        ) -> RedisFuture<'a, Vec<Value>> {
            Box::pin(std::future::ready(Err(refused())))
        }

        fn get_db(&self) -> i64 {
            0
        }
    }

    fn refused() -> redis::RedisError {
        std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused").into()
    }

    #[test]
    fn keys_hash_user_ids_by_default() {
        let storage = RedisStorage::new(NeverAnswers);

        let key = storage.redis_key(StorageKey::new("sk_live_123", "pro"));

        assert_eq!(
            key,
            format!("trt:pro:{}", sha1_smol::Sha1::from("sk_live_123").digest())
        );
        assert!(!key.contains("sk_live_123"));
    }

    #[test]
    fn keys_never_collide_across_separators() {
        for storage in [
            RedisStorage::new(NeverAnswers),
            RedisStorage::new(NeverAnswers).plain_user_ids(),
        ] {
            // A naive "user:tier" or "tier:user" join collides on one of these.
            for (first, second) in [(("a:b", "c"), ("a", "b:c")), (("c", "a:b"), ("b:c", "a"))] {
                let a = storage.redis_key(StorageKey::new(first.0, first.1));
                let b = storage.redis_key(StorageKey::new(second.0, second.1));
                assert_ne!(a, b, "{first:?} and {second:?}");
            }
        }
    }

    #[test]
    fn plain_keys_keep_the_user_id_readable() {
        let storage = RedisStorage::new(NeverAnswers)
            .key_prefix("app:")
            .plain_user_ids();

        let key = storage.redis_key(StorageKey::new("alice", "free"));

        assert_eq!(key, "app:4:free:alice");
    }

    #[test]
    fn micros_round_up() {
        assert_eq!(micros_ceil(333_333_333), 333_334);
        assert_eq!(micros_ceil(250_000_000), 250_000);
        assert_eq!(micros_ceil(1), 1);
    }

    #[test]
    fn replies_decode_into_the_gcra_decision() {
        let allowed = decode_reply((1, 3, 0, 1_500_000), 4);
        assert_eq!(
            allowed,
            Ok(RateLimitInfo {
                limit: 4,
                remaining: 3,
                reset_after: Duration::from_millis(1_500),
            })
        );

        let limited = decode_reply((0, 0, 250_000, 1_000_000), 4);
        assert_eq!(
            limited,
            Err(RateLimited {
                limit: 4,
                retry_after: Duration::from_millis(250),
                reset_after: Duration::from_secs(1),
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_redis_times_out_as_a_storage_error() {
        let storage = RedisStorage::new(NeverAnswers).timeout(Duration::from_millis(50));
        let quota = Quota::per_second(1);

        let result = storage
            .check_and_update(StorageKey::new("u1", "free"), &quota, 1, 0)
            .await;

        let err = result.expect_err("a check with no answer must fail");
        assert!(
            err.to_string().contains("did not answer within 50ms"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn connection_errors_become_storage_errors() {
        let storage = RedisStorage::new(Refused);
        let quota = Quota::per_second(1);

        let result = storage
            .check_and_update(StorageKey::new("u1", "free"), &quota, 1, 0)
            .await;

        let err = result.expect_err("a refused connection must fail");
        assert!(err.to_string().contains("connection refused"), "{err}");
    }
}
