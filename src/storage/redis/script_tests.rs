//! Runs `gcra.lua` in an embedded Lua 5.1, the version Redis embeds, against
//! a small in-memory imitation of the Redis commands a GCRA script needs.
//!
//! No server or background process is involved, so these tests run anywhere:
//!
//! ```text
//! cargo test --features redis --lib gcra_script
//! ```
//!
//! The imitation follows Redis where scripts tend to trip: arguments passed to
//! `redis.call` are converted exactly (integers stay integers), `TIME` returns
//! two strings, keys set with `PX` expire, and creating or reading an
//! undeclared global is an error. Only `GET`, `SET` (with `PX` or `EX`),
//! `DEL`, `EXISTS`, `PTTL` and `TIME` are available, and the `bit`, `cjson`
//! and `struct` libraries are not. The script gets only the `table`, `string`
//! and `math` libraries, so it cannot touch files or processes.
//! `tests/redis_tests.rs` runs the same script against a real Redis.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use mlua::{Lua, LuaOptions, StdLib, Table, Value, Variadic};

use super::*;
use crate::gcra::check_gcra;

/// In-memory stand-in for the Redis keyspace, with a clock in microseconds.
#[derive(Debug, Default)]
struct FakeRedis {
    now_us: u64,
    /// key -> (value, expiry time in microseconds)
    data: HashMap<String, (String, Option<u64>)>,
    /// Every `SET` the script issued: (key, value, PX in milliseconds).
    sets: Vec<(String, String, Option<u64>)>,
    /// Keys holding a value that is not a string, such as a hash.
    other_types: HashSet<String>,
}

impl FakeRedis {
    fn live(&self, key: &str) -> Option<&String> {
        match self.data.get(key) {
            Some((value, expiry)) if expiry.map_or(true, |at| self.now_us < at) => Some(value),
            _ => None,
        }
    }

    /// Runs one command, the way Redis would answer it.
    fn command(&mut self, args: &[String]) -> Result<Answer, String> {
        let name = args
            .first()
            .map(|n| n.to_ascii_uppercase())
            .unwrap_or_default();
        match (name.as_str(), &args[1..]) {
            ("GET", [key]) if self.other_types.contains(key) => {
                Err("WRONGTYPE Operation against a key holding the wrong kind of value".to_owned())
            }
            ("GET", [key]) => Ok(self.live(key).cloned().map_or(Answer::Nil, Answer::Bulk)),
            ("SET", [key, value, options @ ..]) => {
                let expiry_ms = match options {
                    [] => None,
                    [unit, amount] if unit.eq_ignore_ascii_case("PX") => Some(positive(amount)?),
                    [unit, amount] if unit.eq_ignore_ascii_case("EX") => {
                        Some(positive(amount)? * 1_000)
                    }
                    _ => return Err(format!("SET options {options:?} are not in the harness")),
                };
                let expiry = expiry_ms.map(|ms| self.now_us + ms * 1_000);
                // SET replaces a value of any type.
                self.other_types.remove(key);
                self.data.insert(key.clone(), (value.clone(), expiry));
                self.sets.push((key.clone(), value.clone(), expiry_ms));
                Ok(Answer::Ok)
            }
            ("DEL", keys) | ("EXISTS", keys) if !keys.is_empty() => {
                let live = keys
                    .iter()
                    .filter(|key| self.live(key).is_some() || self.other_types.contains(*key))
                    .count();
                if name == "DEL" {
                    for key in keys {
                        self.data.remove(key);
                        self.other_types.remove(key);
                    }
                }
                Ok(Answer::Int(live as i64))
            }
            ("PTTL", [key]) => Ok(Answer::Int(match (self.live(key), self.data.get(key)) {
                (None, _) => -2,
                (Some(_), Some((_, Some(at)))) => ((at - self.now_us) / 1_000) as i64,
                (Some(_), _) => -1,
            })),
            ("TIME", []) => Ok(Answer::Array(vec![
                (self.now_us / 1_000_000).to_string(),
                (self.now_us % 1_000_000).to_string(),
            ])),
            _ => Err(format!("command {args:?} is not in the harness")),
        }
    }
}

/// A command's reply, before it is handed to Lua.
enum Answer {
    Nil,
    Ok,
    Int(i64),
    Bulk(String),
    Array(Vec<String>),
}

fn positive(amount: &str) -> Result<u64, String> {
    match amount.parse::<u64>() {
        Ok(n) if n > 0 => Ok(n),
        _ => Err(format!(
            "ERR invalid expire time in 'set' command: {amount:?}"
        )),
    }
}

/// Converts a `redis.call` argument like Redis 7 does: integers exactly.
fn redis_arg(value: &Value) -> Result<String, String> {
    match value {
        Value::String(s) => Ok(s.to_string_lossy()),
        Value::Integer(n) => Ok(n.to_string()),
        Value::Number(n) if n.fract() == 0.0 && n.abs() < 9.0e18 => Ok((*n as i64).to_string()),
        Value::Number(n) => Ok(n.to_string()),
        other => Err(format!(
            "Lua redis lib command arguments must be strings or integers, got {}",
            other.type_name()
        )),
    }
}

/// Converts a command's reply to Lua the way Redis does.
fn to_lua(lua: &Lua, answer: Answer) -> mlua::Result<Value> {
    Ok(match answer {
        Answer::Nil => Value::Boolean(false),
        Answer::Ok => {
            let status = lua.create_table()?;
            status.raw_set("ok", "OK")?;
            Value::Table(status)
        }
        Answer::Int(n) => Value::Integer(n),
        Answer::Bulk(s) => Value::String(lua.create_string(&s)?),
        Answer::Array(items) => Value::Table(lua.create_sequence_from(items)?),
    })
}

/// Reads a script's return value as Redis would reply it, then as the four
/// integers `RedisStorage` expects.
fn to_reply(value: Value) -> Result<Reply, String> {
    let Value::Table(table) = value else {
        return Err(format!(
            "expected an array of four integers, got {}",
            value.type_name()
        ));
    };
    if let Ok(Some(err)) = table.raw_get::<Option<String>>("err") {
        return Err(err);
    }
    if table.raw_len() != 4 {
        return Err(format!("expected four values, got {}", table.raw_len()));
    }
    let mut out = [0i64; 4];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = match table.raw_get::<Value>(i + 1).map_err(|e| e.to_string())? {
            Value::Integer(n) => n,
            // Redis truncates a returned Lua number to an integer.
            Value::Number(n) => n as i64,
            Value::String(s) => s.to_string_lossy().parse().map_err(|_| {
                format!(
                    "value {} is not an integer: {:?}",
                    i + 1,
                    s.to_string_lossy()
                )
            })?,
            other => return Err(format!("value {} is a {}", i + 1, other.type_name())),
        };
    }
    Ok((out[0], out[1], out[2], out[3]))
}

/// Builds the `redis` table the script sees.
fn redis_api(lua: &Lua, fake: &Rc<RefCell<FakeRedis>>) -> mlua::Result<Table> {
    let api = lua.create_table()?;

    let call_fake = fake.clone();
    api.raw_set(
        "call",
        lua.create_function(move |lua, args: Variadic<Value>| {
            let args = args
                .iter()
                .map(redis_arg)
                .collect::<Result<Vec<_>, _>>()
                .map_err(mlua::Error::RuntimeError)?;
            let answer = call_fake
                .borrow_mut()
                .command(&args)
                .map_err(mlua::Error::RuntimeError)?;
            to_lua(lua, answer)
        })?,
    )?;

    let pcall_fake = fake.clone();
    api.raw_set(
        "pcall",
        lua.create_function(move |lua, args: Variadic<Value>| {
            let result = args
                .iter()
                .map(redis_arg)
                .collect::<Result<Vec<_>, _>>()
                .and_then(|args| pcall_fake.borrow_mut().command(&args));
            match result {
                Ok(answer) => to_lua(lua, answer),
                Err(err) => {
                    let reply = lua.create_table()?;
                    reply.raw_set("err", err)?;
                    Ok(Value::Table(reply))
                }
            }
        })?,
    )?;

    for (name, field) in [("error_reply", "err"), ("status_reply", "ok")] {
        api.raw_set(
            name,
            lua.create_function(move |lua, message: String| {
                let reply = lua.create_table()?;
                reply.raw_set(field, message)?;
                Ok(reply)
            })?,
        )?;
    }
    api.raw_set("replicate_commands", lua.create_function(|_, ()| Ok(true))?)?;
    api.raw_set("log", lua.create_function(|_, _: Variadic<Value>| Ok(()))?)?;
    for (level, name) in ["LOG_DEBUG", "LOG_VERBOSE", "LOG_NOTICE", "LOG_WARNING"]
        .iter()
        .enumerate()
    {
        api.raw_set(*name, level as i64)?;
    }
    Ok(api)
}

/// Runs `source` once with `call`'s key and arguments, like `EVALSHA` would.
fn run_script(
    source: &str,
    fake: &Rc<RefCell<FakeRedis>>,
    call: &ScriptCall,
) -> Result<Reply, String> {
    let argv = [
        call.now.clone(),
        call.emission_interval.to_string(),
        call.burst_offset.to_string(),
        call.cost.to_string(),
    ];
    run_with_args(source, fake, &call.key, &argv)
}

/// Runs `source` once with any key and arguments, including malformed ones.
fn run_with_args(
    source: &str,
    fake: &Rc<RefCell<FakeRedis>>,
    key: &str,
    argv: &[String],
) -> Result<Reply, String> {
    let libs = StdLib::TABLE | StdLib::STRING | StdLib::MATH;
    let lua = Lua::new_with(libs, LuaOptions::default()).map_err(|e| e.to_string())?;
    let setup = || -> mlua::Result<()> {
        let globals = lua.globals();
        globals.raw_set("KEYS", lua.create_sequence_from([key.to_owned()])?)?;
        globals.raw_set("ARGV", lua.create_sequence_from(argv.to_vec())?)?;
        globals.raw_set("redis", redis_api(&lua, fake)?)?;
        // Redis refuses scripts that create or read undeclared globals.
        lua.load(
            r#"setmetatable(_G, {
                __newindex = function(_, name)
                    error("Script attempted to create global variable '" .. tostring(name) .. "'", 2)
                end,
                __index = function(_, name)
                    error("Script attempted to access nonexistent global variable '" .. tostring(name) .. "'", 2)
                end,
            })"#,
        )
        .exec()
    };
    setup().map_err(|e| e.to_string())?;

    let value = lua
        .load(source)
        .set_name("gcra.lua")
        .eval::<Value>()
        .map_err(|e| e.to_string())?;
    to_reply(value)
}

const USER: StorageKey<'static> = StorageKey {
    user_id: "alice",
    tier: "free",
};

/// Runs one check through the GCRA script, the way `RedisStorage` would.
fn script_check(
    fake: &Rc<RefCell<FakeRedis>>,
    storage: &RedisStorage<()>,
    quota: &Quota,
    cost: u32,
    now: Nanos,
) -> Result<Result<RateLimitInfo, RateLimited>, String> {
    fake.borrow_mut().now_us = now / 1_000;
    let call = storage.script_call(USER, quota, cost, now);
    run_script(GCRA_SCRIPT, fake, &call).map(|reply| decode_reply(reply, quota.max_burst()))
}

/// The Rust reference: `check_gcra` over one stored TAT.
#[derive(Default)]
struct Reference {
    tat: Option<Nanos>,
}

impl Reference {
    fn check(
        &mut self,
        quota: &Quota,
        cost: u32,
        now: Nanos,
    ) -> Result<RateLimitInfo, RateLimited> {
        let ei = quota.emission_interval_nanos();
        let bo = quota.burst_offset_nanos();
        check_gcra(self.tat, now, ei, bo, cost).map(|(new_tat, info)| {
            self.tat = Some(new_tat);
            info
        })
    }
}

fn ms(value: u64) -> Nanos {
    value * 1_000_000
}

/// Quotas whose intervals are whole microseconds, so both sides agree exactly.
fn quotas() -> [Quota; 4] {
    [
        Quota::per_second(4),
        Quota::per_minute(3),
        Quota::per_hour(10),
        Quota::with_window(7, Duration::from_millis(7)),
    ]
}

// ---------------------------------------------------------------------------
// The harness itself (independent of gcra.lua)
// ---------------------------------------------------------------------------

fn run_snippet(source: &str, fake: &Rc<RefCell<FakeRedis>>) -> Result<Reply, String> {
    let call = RedisStorage::new(()).script_call(USER, &Quota::per_second(1), 1, 0);
    run_script(source, fake, &call)
}

#[test]
fn harness_runs_lua_5_1_like_redis() {
    let fake = Rc::new(RefCell::new(FakeRedis::default()));

    let reply = run_snippet("return {_VERSION == 'Lua 5.1' and 1 or 0, 0, 0, 0}", &fake);

    assert_eq!(reply, Ok((1, 0, 0, 0)));
}

#[test]
fn harness_passes_integer_arguments_exactly() {
    let fake = Rc::new(RefCell::new(FakeRedis::default()));

    let reply = run_snippet(
        "redis.call('SET', KEYS[1], 1790000001123456)
         return {redis.call('GET', KEYS[1]), 0, 0, 0}",
        &fake,
    );

    assert_eq!(reply, Ok((1_790_000_001_123_456, 0, 0, 0)));
}

#[test]
fn harness_shows_that_tostring_loses_digits() {
    let fake = Rc::new(RefCell::new(FakeRedis::default()));

    let reply = run_snippet("return {tostring(1790000001123456), 0, 0, 0}", &fake);

    assert_eq!(
        reply,
        Err("value 1 is not an integer: \"1.7900000011235e+15\"".to_owned())
    );
}

#[test]
fn harness_returns_time_as_two_strings() {
    let fake = Rc::new(RefCell::new(FakeRedis {
        now_us: 1_790_000_000_123_456,
        ..FakeRedis::default()
    }));

    let reply = run_snippet(
        "local t = redis.call('TIME')
         return {t[1], t[2], type(t[1]) == 'string' and 1 or 0, #t}",
        &fake,
    );

    assert_eq!(reply, Ok((1_790_000_000, 123_456, 1, 2)));
}

#[test]
fn harness_expires_keys_set_with_px() {
    let fake = Rc::new(RefCell::new(FakeRedis::default()));
    run_snippet(
        "redis.call('SET', KEYS[1], 'v', 'PX', 5) return {0, 0, 0, 0}",
        &fake,
    )
    .unwrap();

    fake.borrow_mut().now_us = 4_999;
    let before = run_snippet(
        "return {redis.call('EXISTS', KEYS[1]), redis.call('PTTL', KEYS[1]), 0, 0}",
        &fake,
    );
    fake.borrow_mut().now_us = 5_000;
    let after = run_snippet(
        "return {redis.call('EXISTS', KEYS[1]), redis.call('PTTL', KEYS[1]), 0, 0}",
        &fake,
    );

    assert_eq!(before, Ok((1, 0, 0, 0)));
    assert_eq!(after, Ok((0, -2, 0, 0)));
}

#[test]
fn harness_rejects_px_zero_like_redis() {
    let fake = Rc::new(RefCell::new(FakeRedis::default()));

    let reply = run_snippet(
        "redis.call('SET', KEYS[1], 'v', 'PX', 0) return {0, 0, 0, 0}",
        &fake,
    );

    assert!(reply.unwrap_err().contains("invalid expire time"));
}

#[test]
fn harness_rejects_undeclared_globals() {
    let fake = Rc::new(RefCell::new(FakeRedis::default()));

    let created = run_snippet("tat = 1 return {0, 0, 0, 0}", &fake);
    let read = run_snippet("return {undefined_value, 0, 0, 0}", &fake);

    assert!(created
        .unwrap_err()
        .contains("create global variable 'tat'"));
    assert!(read
        .unwrap_err()
        .contains("nonexistent global variable 'undefined_value'"));
}

#[test]
fn harness_passes_error_replies_through() {
    let fake = Rc::new(RefCell::new(FakeRedis::default()));

    let reply = run_snippet("return redis.error_reply('boom')", &fake);

    assert_eq!(reply, Err("boom".to_owned()));
}

#[test]
fn harness_has_no_io_or_os_library() {
    let fake = Rc::new(RefCell::new(FakeRedis::default()));

    let reply = run_snippet(
        "return {io == nil and 1 or 0, os == nil and 1 or 0, 0, 0}",
        &fake,
    );

    // Reading them is an error (undeclared globals), which also proves they are absent.
    assert!(reply
        .unwrap_err()
        .contains("nonexistent global variable 'io'"));
}

// ---------------------------------------------------------------------------
// gcra.lua: the specification the script has to meet
// ---------------------------------------------------------------------------

#[test]
fn gcra_script_matches_check_gcra_step_by_step() {
    let fake = Rc::new(RefCell::new(FakeRedis::default()));
    let storage = RedisStorage::new(()).use_client_clock();
    let quota = Quota::per_second(4); // 250 ms per request
    let mut reference = Reference::default();

    let steps: &[(u64, u32)] = &[
        (0, 1),
        (0, 1),
        (0, 1),
        (0, 1),
        (0, 1), // over the burst
        (100, 1),
        (250, 2),
        (300, 0),
        (1_000, 4),
        (1_000, 0),
        (5_000, 3),
        (5_000, 2), // over what is left
    ];
    for (i, &(at_ms, cost)) in steps.iter().enumerate() {
        let expected = reference.check(&quota, cost, ms(at_ms));
        let actual = script_check(&fake, &storage, &quota, cost, ms(at_ms))
            .unwrap_or_else(|err| panic!("step {i}: script failed: {err}"));
        assert_eq!(actual, expected, "step {i}: t={at_ms}ms cost={cost}");
    }
}

#[test]
fn gcra_script_matches_check_gcra_on_random_sequences() {
    for server_time in [false, true] {
        for quota in quotas() {
            let fake = Rc::new(RefCell::new(FakeRedis::default()));
            let storage = if server_time {
                RedisStorage::new(())
            } else {
                RedisStorage::new(()).use_client_clock()
            };
            let mut reference = Reference::default();
            // Deterministic pseudo-random steps (a small LCG), so failures repeat.
            let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
            let mut next = |bound: u64| {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                (seed >> 33) % bound
            };
            let interval_us = quota.emission_interval_nanos() / 1_000;
            let mut now_us: u64 = 1_790_000_000_000_000;

            for step in 0..300 {
                now_us += next(interval_us * 2 + 1);
                let cost = next(u64::from(quota.max_burst()) + 1) as u32;
                let now = now_us * 1_000;
                let expected = reference.check(&quota, cost, now);
                let actual = script_check(&fake, &storage, &quota, cost, now)
                    .unwrap_or_else(|err| panic!("step {step}: script failed: {err}"));
                assert_eq!(
                    actual, expected,
                    "server_time={server_time} quota={quota:?} step={step} cost={cost}"
                );

                // No leaks: every write expires, and only KEYS[1] is ever touched.
                let key = storage.redis_key(USER);
                let state = fake.borrow();
                assert!(
                    state
                        .sets
                        .iter()
                        .all(|(k, _, px)| *k == key && px.is_some()),
                    "step {step}: {:?}",
                    state.sets
                );
                assert!(state.data.keys().all(|k| *k == key), "step {step}");
            }
        }
    }
}

#[test]
fn gcra_script_stores_the_tat_as_an_exact_integer() {
    let fake = Rc::new(RefCell::new(FakeRedis::default()));
    let storage = RedisStorage::new(()).use_client_clock();
    let now_us: u64 = 1_790_000_000_123_456;

    script_check(&fake, &storage, &Quota::per_second(1), 1, now_us * 1_000)
        .expect("script ran")
        .expect("the first request is allowed");

    let key = storage.redis_key(USER);
    assert_eq!(
        fake.borrow()
            .data
            .get(&key)
            .map(|(value, _)| value.as_str()),
        Some("1790000001123456")
    );
}

#[test]
fn gcra_script_expires_the_key_when_the_bucket_is_full_again() {
    let fake = Rc::new(RefCell::new(FakeRedis::default()));
    let storage = RedisStorage::new(()).use_client_clock();

    let info = script_check(&fake, &storage, &Quota::per_second(4), 1, ms(1_000))
        .expect("script ran")
        .expect("the first request is allowed");

    assert_eq!(info.reset_after, Duration::from_millis(250));
    let sets = &fake.borrow().sets;
    assert_eq!(sets.len(), 1, "one SET for one allowed request: {sets:?}");
    assert_eq!(
        sets[0].2,
        Some(250),
        "PX must equal the time until the bucket is full"
    );
}

#[test]
fn gcra_script_stores_nothing_for_a_free_request_on_a_fresh_bucket() {
    let fake = Rc::new(RefCell::new(FakeRedis::default()));
    let storage = RedisStorage::new(()).use_client_clock();

    let info = script_check(&fake, &storage, &Quota::per_second(4), 0, ms(1_000))
        .expect("script ran")
        .expect("a free request is always allowed");

    assert_eq!(info.remaining, 4);
    assert!(fake.borrow().sets.is_empty(), "{:?}", fake.borrow().sets);
}

#[test]
fn gcra_script_writes_nothing_when_it_denies() {
    let fake = Rc::new(RefCell::new(FakeRedis::default()));
    let storage = RedisStorage::new(()).use_client_clock();
    let quota = Quota::per_second(1);
    script_check(&fake, &storage, &quota, 1, ms(0))
        .expect("script ran")
        .expect("the first request is allowed");
    let writes_before = fake.borrow().sets.len();

    script_check(&fake, &storage, &quota, 1, ms(0))
        .expect("script ran")
        .expect_err("the second request is over the burst");

    assert_eq!(fake.borrow().sets.len(), writes_before);
}

fn fresh() -> Rc<RefCell<FakeRedis>> {
    Rc::new(RefCell::new(FakeRedis::default()))
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

#[test]
fn gcra_script_rejects_malformed_arguments_without_writing() {
    // Plain keys hold the user id; errors must never echo it.
    let secret = "sk_live_secret";
    let key = RedisStorage::new(())
        .plain_user_ids()
        .redis_key(StorageKey::new(secret, "free"));
    let cases: &[(&str, &[&str])] = &[
        ("missing argument", &["", "250000", "1000000"]),
        ("zero interval", &["", "0", "1000000", "1"]),
        ("text interval", &["", "abc", "1000000", "1"]),
        ("fractional interval", &["", "1.5", "1000000", "1"]),
        ("negative cost", &["", "250000", "1000000", "-1"]),
        ("NaN cost", &["", "250000", "1000000", "nan"]),
        ("burst below interval", &["", "250000", "1000", "1"]),
        ("infinite time", &["inf", "250000", "1000000", "1"]),
        (
            "time at 2^53",
            &["9007199254740992", "250000", "1000000", "1"],
        ),
        ("negative time", &["-1", "250000", "1000000", "1"]),
    ];
    for (label, argv) in cases {
        let fake = fresh();

        let err = run_with_args(GCRA_SCRIPT, &fake, &key, &strings(argv)).expect_err(label);

        assert!(err.starts_with("ERR tower-rate-tier: "), "{label}: {err}");
        assert!(
            !err.contains(secret),
            "{label}: the error leaks the key: {err}"
        );
        assert!(
            fake.borrow().sets.is_empty(),
            "{label}: {:?}",
            fake.borrow().sets
        );
    }
}

#[test]
fn gcra_script_refuses_values_beyond_the_exact_range() {
    let fake = fresh();
    let key = RedisStorage::new(()).redis_key(USER);
    // Ten microseconds below 2^53, plus a 100 µs interval, is no longer exact.
    let argv = strings(&["9007199254740982", "100", "100", "1"]);

    let err = run_with_args(GCRA_SCRIPT, &fake, &key, &argv).expect_err("must refuse");

    assert!(err.starts_with("ERR tower-rate-tier: "), "{err}");
    assert!(fake.borrow().sets.is_empty());
}

#[test]
fn gcra_script_heals_a_corrupt_stored_value() {
    let fake = fresh();
    let storage = RedisStorage::new(()).use_client_clock();
    let key = storage.redis_key(USER);
    fake.borrow_mut()
        .data
        .insert(key.clone(), ("not a number".to_owned(), None));

    let info = script_check(&fake, &storage, &Quota::per_second(4), 1, ms(1_000))
        .expect("a corrupt value must not fail every request")
        .expect("it counts as a fresh bucket");

    assert_eq!(info.remaining, 3);
    let state = fake.borrow();
    let (value, expiry) = &state.data[&key];
    assert_eq!(value, "1250000");
    assert!(expiry.is_some(), "the healed value must expire");
}

#[test]
fn gcra_script_caps_the_wait_when_the_clock_moves_back() {
    let fake = fresh();
    let storage = RedisStorage::new(()).use_client_clock();
    // A TAT 100 s ahead, as left by a server whose clock was ahead.
    fake.borrow_mut()
        .data
        .insert(storage.redis_key(USER), ("101000000".to_owned(), None));

    let limited = script_check(&fake, &storage, &Quota::per_second(4), 1, ms(1_000))
        .expect("script ran")
        .expect_err("the bucket counts as full");

    // Never longer than a full bucket: one interval to retry, one window to reset.
    assert_eq!(limited.retry_after, Duration::from_millis(250));
    assert_eq!(limited.reset_after, Duration::from_secs(1));
}

#[test]
fn gcra_script_recovers_one_interval_after_the_clock_moves_back() {
    let fake = fresh();
    let storage = RedisStorage::new(()).use_client_clock();
    let quota = Quota::per_second(4);
    // A TAT 100 s ahead, as left by a server whose clock was ahead.
    fake.borrow_mut()
        .data
        .insert(storage.redis_key(USER), ("101000000".to_owned(), None));

    let limited = script_check(&fake, &storage, &quota, 1, ms(1_000))
        .expect("script ran")
        .expect_err("the bucket counts as full");
    let retry_at = ms(1_000) + limited.retry_after.as_nanos() as u64;

    // Waiting Retry-After must be enough; the old TAT must not keep the user
    // locked out for the whole 100 s the clock moved back.
    let info = script_check(&fake, &storage, &quota, 1, retry_at)
        .expect("script ran")
        .expect("a client that waits Retry-After is allowed");
    assert_eq!(info.remaining, 0);
}

#[test]
fn harness_reports_wrongtype_like_redis() {
    let fake = fresh();
    let key = RedisStorage::new(()).redis_key(USER);
    fake.borrow_mut().other_types.insert(key);

    let raised = run_snippet("return {redis.call('GET', KEYS[1]), 0, 0, 0}", &fake);
    let caught = run_snippet(
        "local reply = redis.pcall('GET', KEYS[1])
         return {type(reply) == 'table' and reply.err and 1 or 0, 0, 0, 0}",
        &fake,
    );

    assert!(raised.unwrap_err().contains("WRONGTYPE"));
    assert_eq!(caught, Ok((1, 0, 0, 0)));
}

#[test]
fn gcra_script_heals_a_key_of_the_wrong_type() {
    let fake = fresh();
    let storage = RedisStorage::new(()).use_client_clock();
    let key = storage.redis_key(USER);
    fake.borrow_mut().other_types.insert(key.clone());

    let info = script_check(&fake, &storage, &Quota::per_second(4), 1, ms(1_000))
        .expect("a key of another type must not fail every request")
        .expect("it counts as a fresh bucket");

    assert_eq!(info.remaining, 3);
    let state = fake.borrow();
    assert!(!state.other_types.contains(&key));
    let (value, expiry) = &state.data[&key];
    assert_eq!(value, "1250000");
    assert!(expiry.is_some(), "the healed value must expire");
}
