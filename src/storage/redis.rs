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
/// Keys look like `trt:<tier>:<sha1 of the user id>`, so user ids never
/// appear in Redis in plain text. Ids with little entropy, such as IP
/// addresses or emails, also need a [`key_secret`](Self::key_secret): without
/// one they can be recovered by hashing every candidate. Each key expires when
/// its bucket is full again, so no garbage collection is needed.
///
/// A check that gets no answer within the [timeout](Self::timeout) (100 ms by
/// default) fails with a [`StorageError`], which the
/// [`OnStorageError`](crate::OnStorageError) policy then handles.
///
/// The script is defensive: every key it writes has an expiry, it touches
/// only its own key, malformed input is refused without writing (and the
/// error never includes the key), a corrupted value or a key of another type
/// counts as a fresh bucket instead of failing every request, and if Redis's
/// clock moves back the wait is capped at what a full bucket would cost. Keys
/// under the [prefix](Self::key_prefix) belong to this storage.
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
    key_secret: Option<Vec<u8>>,
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
            key_secret: None,
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

    /// Hashes user ids with a secret key (HMAC-SHA1) instead of plain SHA-1.
    ///
    /// Plain SHA-1 hides high-entropy ids such as API keys, but ids with
    /// little entropy (IP addresses, emails, numeric ids) can be recovered by
    /// hashing every candidate. With a secret kept out of Redis they cannot.
    /// Every instance must use the same secret, and changing it starts every
    /// bucket afresh. It has no effect together with
    /// [`plain_user_ids`](Self::plain_user_ids).
    pub fn key_secret(mut self, secret: impl AsRef<[u8]>) -> Self {
        self.key_secret = Some(secret.as_ref().to_vec());
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
            let digest = match &self.key_secret {
                Some(secret) => hmac_sha1(secret, key.user_id.as_bytes()),
                None => sha1_smol::Sha1::from(key.user_id).digest(),
            };
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

/// The key and arguments of one GCRA script call.
#[derive(Debug)]
struct ScriptCall {
    key: String,
    /// Microseconds since the Unix epoch, or empty to use Redis's `TIME`.
    now: String,
    emission_interval: u64,
    burst_offset: u64,
    cost: u32,
}

impl<C> RedisStorage<C> {
    /// Builds the script's arguments, in microseconds.
    fn script_call(&self, key: StorageKey<'_>, quota: &Quota, cost: u32, now: Nanos) -> ScriptCall {
        let emission_interval = micros_ceil(quota.emission_interval_nanos()).max(1);
        ScriptCall {
            key: self.redis_key(key),
            now: if self.client_clock {
                (now / 1_000).to_string()
            } else {
                String::new()
            },
            emission_interval,
            burst_offset: emission_interval.saturating_mul(u64::from(quota.max_burst())),
            cost,
        }
    }
}

impl<C> fmt::Debug for RedisStorage<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RedisStorage")
            .field("key_prefix", &self.key_prefix)
            .field("timeout", &self.timeout)
            .field("hash_user_ids", &self.hash_user_ids)
            .field("key_secret", &self.key_secret.is_some())
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
            let call = self.script_call(key, quota, cost, now);
            let mut invocation = self.script.prepare_invoke();
            invocation
                .key(call.key)
                .arg(call.now)
                .arg(call.emission_interval)
                .arg(call.burst_offset)
                .arg(call.cost);

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

/// HMAC-SHA1 (RFC 2104) of `message` under `secret`.
fn hmac_sha1(secret: &[u8], message: &[u8]) -> sha1_smol::Digest {
    const BLOCK: usize = 64;
    let mut key = [0u8; BLOCK];
    if secret.len() > BLOCK {
        key[..20].copy_from_slice(&sha1_smol::Sha1::from(secret).digest().bytes());
    } else {
        key[..secret.len()].copy_from_slice(secret);
    }

    let mut inner = sha1_smol::Sha1::new();
    inner.update(&key.map(|b| b ^ 0x36));
    inner.update(message);
    let mut outer = sha1_smol::Sha1::new();
    outer.update(&key.map(|b| b ^ 0x5c));
    outer.update(&inner.digest().bytes());
    outer.digest()
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
mod script_tests;

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
    fn hmac_sha1_matches_rfc_2202() {
        let cases: [(&[u8], &[u8], &str); 3] = [
            (
                &[0x0b; 20],
                b"Hi There",
                "b617318655057264e28bc0b6fb378c8ef146be00",
            ),
            (
                b"Jefe",
                b"what do ya want for nothing?",
                "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79",
            ),
            (
                &[0xaa; 80],
                b"Test Using Larger Than Block-Size Key - Hash Key First",
                "aa4ae5e15272d00e95705637ce8a3b55ed402112",
            ),
        ];
        for (secret, message, expected) in cases {
            assert_eq!(hmac_sha1(secret, message).to_string(), expected);
        }
    }

    #[test]
    fn a_key_secret_changes_every_key() {
        let key = StorageKey::new("203.0.113.7", "free");
        let plain_hash = RedisStorage::new(NeverAnswers).redis_key(key);
        let first = RedisStorage::new(NeverAnswers)
            .key_secret("one")
            .redis_key(key);
        let again = RedisStorage::new(NeverAnswers)
            .key_secret("one")
            .redis_key(key);
        let second = RedisStorage::new(NeverAnswers)
            .key_secret("two")
            .redis_key(key);

        assert_eq!(first, again, "the same secret must give the same key");
        assert_ne!(first, plain_hash);
        assert_ne!(first, second);
        assert!(first.starts_with("trt:free:") && first.len() == "trt:free:".len() + 40);
        assert!(!first.contains("203.0.113.7"));
    }

    #[test]
    fn debug_never_shows_the_key_secret() {
        let storage = RedisStorage::new(NeverAnswers).key_secret("hunter2-secret");

        let out = format!("{storage:?}");

        assert!(!out.contains("hunter2"), "{out}");
        assert!(out.contains("key_secret: true"), "{out}");
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
