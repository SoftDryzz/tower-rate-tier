-- GCRA (Generic Cell Rate Algorithm) check for RedisStorage.
--
-- Redis runs the whole script atomically: no other command touches the key
-- while it executes, so concurrent requests from any instance are safe.
-- The Rust reference implementation is `check_gcra` in src/gcra.rs. Two
-- test suites compare this script's results with it:
--   cargo test --features redis --lib gcra_script   (embedded Lua 5.1, no
--                                                     Redis server needed)
--   cargo test --features redis --test redis_tests  (a real Redis)
--
-- Input
--   KEYS[1]  bucket key: one user within one tier.
--   ARGV[1]  current time in microseconds since the Unix epoch, as an integer
--            string. An empty string means "read it with redis.call('TIME')",
--            which is the default so every instance shares Redis's clock.
--   ARGV[2]  emission interval in microseconds (time per request), >= 1.
--   ARGV[3]  burst offset in microseconds (emission interval * max burst).
--   ARGV[4]  cost of this request in cells, >= 0. It is never above the max
--            burst: the Rust side rejects that before calling the script.
--
-- Stored value
--   The TAT (theoretical arrival time) in microseconds, as an integer string.
--   A missing key, or a TAT in the past, means a fresh bucket.
--
-- Output: an array of four integers
--   { allowed, remaining, retry_after_us, reset_after_us }
--   allowed         1 if the request is allowed, 0 if it is rate limited.
--   remaining       requests left after this one (0 when limited).
--   retry_after_us  wait before this request would be allowed (0 when allowed).
--   reset_after_us  time until the bucket is full again. When limited, measure
--                   it from the stored TAT: the rejected request consumed
--                   nothing.
--
-- Effects
--   allowed  store the new TAT with SET key value PX ms, where ms is the
--            time until the bucket is full again, rounded UP: rounding down
--            would let the key expire before the TAT and grant requests
--            early. PX must be >= 1, so store nothing when the new TAT is
--            not in the future (a cost of 0 on a fresh bucket).
--   limited  write nothing, except to store a capped TAT (see Safety).
--
-- Safety
--   * Every write carries an expiry, so no key outlives its bucket.
--   * Only KEYS[1] is read or written (required by Redis Cluster).
--   * Malformed arguments, or values past the exact range of Lua numbers,
--     return an error reply without writing. The message never includes the
--     key or the arguments, since plain keys contain user ids.
--   * A stored value that is not a number counts as a fresh bucket and is
--     overwritten, so a corrupted key heals itself instead of failing every
--     request for that user.
--   * A TAT further ahead than one full burst can only come from a clock that
--     moved back (for example, a failover to a replica whose clock is behind).
--     It is capped at a full bucket and the cap is stored, even when the
--     request is limited: otherwise every retry would meet the old TAT again
--     and the user would stay locked out for as long as the clock moved back.
--
-- Pitfalls
--   * Lua numbers are doubles, exact for integers up to 2^53. That covers
--     microseconds until the year 2255; nanoseconds would lose precision.
--   * Numbers passed straight to redis.call() or returned from the script
--     are converted exactly by Redis. Lua's own conversions are not:
--     tostring(n) and "x" .. n use "%.14g", so a 16-digit TAT becomes
--     "1.7900000011235e+15". This script never converts numbers itself.
--   * Calling TIME before a write needs effects replication: always on in
--     Redis 7; redis.replicate_commands() enables it on Redis 5 and 6.

-- Every integer below this is exact in a Lua 5.1 number (a double).
local MAX_EXACT = 2 ^ 53

local function fail(reason)
  return redis.error_reply("ERR tower-rate-tier: " .. reason)
end

-- A non-negative integer in the exact range, or nil. tonumber() in Lua 5.1
-- also accepts "nan", "inf" and fractions, which are all rejected here.
local function exact_integer(value)
  local n = tonumber(value)
  if n == nil or n ~= n or n < 0 or n >= MAX_EXACT or n ~= math.floor(n) then
    return nil
  end
  return n
end

if #KEYS ~= 1 or #ARGV ~= 4 then
  return fail("expected 1 key and 4 arguments")
end

local emission_interval = exact_integer(ARGV[2])
if emission_interval == nil or emission_interval < 1 then
  return fail("invalid emission interval")
end
local burst_offset = exact_integer(ARGV[3])
if burst_offset == nil or burst_offset < emission_interval then
  return fail("invalid burst offset")
end
local cost = exact_integer(ARGV[4])
if cost == nil then
  return fail("invalid cost")
end

local now
if ARGV[1] == "" then
  if redis.replicate_commands then
    redis.replicate_commands()
  end
  local time = redis.call("TIME")
  now = exact_integer(time[1] * 1000000 + time[2])
else
  now = exact_integer(ARGV[1])
end
if now == nil then
  return fail("invalid time")
end

-- Not an integer (this includes NaN) means a corrupted value: start fresh.
local tat = tonumber(redis.call("GET", KEYS[1]))
local capped = false
if tat == nil or tat ~= math.floor(tat) or tat < now then
  tat = now
elseif tat > now + burst_offset then
  tat = now + burst_offset
  capped = true
end

local increment = emission_interval * cost
if increment > MAX_EXACT - tat then
  return fail("time values exceed the exact range of Lua numbers")
end
local new_tat = tat + increment
local allow_at = new_tat - burst_offset

if allow_at > now then
  if capped then
    redis.call("SET", KEYS[1], tat, "PX", math.ceil((tat - now) / 1000))
  end
  return { 0, 0, allow_at - now, tat - now }
end

if new_tat > now then
  redis.call("SET", KEYS[1], new_tat, "PX", math.ceil((new_tat - now) / 1000))
end

local remaining = math.floor((burst_offset - (new_tat - now)) / emission_interval)
return { 1, remaining, 0, new_tat - now }
