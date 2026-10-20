#![cfg(feature = "redis")]
//! `SCRIPT FLUSH` against a real Redis, in its own test binary.
//!
//! Flushing empties the script cache of the whole server. Cargo runs test
//! binaries one after another, so here it cannot race with the tests in
//! `redis_tests.rs`, whose retry after `NOSCRIPT` could be flushed again.

mod support;

use tower_rate_tier::storage::Storage;
use tower_rate_tier::{Quota, RedisStorage, StorageKey};

use support::{connect, unique_prefix};

#[tokio::test]
async fn reloads_the_script_after_script_flush() {
    let Some(mut conn) = connect().await else {
        return;
    };
    let redis = RedisStorage::new(conn.clone()).key_prefix(unique_prefix("noscript"));
    let key = StorageKey::new("alice", "free");
    let quota = Quota::per_hour(10);

    redis
        .check_and_update(key, &quota, 1, 0)
        .await
        .unwrap()
        .unwrap();
    let _: () = redis::cmd("SCRIPT")
        .arg("FLUSH")
        .query_async(&mut conn)
        .await
        .unwrap();

    // EVALSHA now answers NOSCRIPT; the storage must load the script again.
    let info = redis
        .check_and_update(key, &quota, 1, 0)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(info.remaining, 8);
}
