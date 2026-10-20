//! The circuit breaker's state machine, kept as a pure function so every
//! transition can be checked without storage, clocks or threads.

use crate::quota::Nanos;

/// Where the breaker sends requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    /// Requests go to the primary storage. `failures` counts the consecutive
    /// failed calls since the last success.
    Closed { failures: u32 },
    /// Requests go to the fallback. The first request at or after `until`
    /// becomes the probe.
    Open { until: Nanos },
    /// A probe is in flight; every other request goes to the fallback.
    HalfOpen,
}

/// How a call reached the primary storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    /// An ordinary call, admitted while the breaker was closed.
    Primary,
    /// The single trial call admitted when the open period ended.
    Probe,
}

/// How a call to the primary storage ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The primary answered, whether it allowed or limited the request.
    Success,
    /// The primary returned a storage error (including a timeout).
    Failure,
}

/// When the breaker opens and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Policy {
    /// Consecutive failures that open the breaker. Never 0.
    pub(crate) failure_threshold: u32,
    /// How long the breaker stays open before a probe, in nanoseconds.
    pub(crate) open_for: Nanos,
}

/// Returns the breaker's state after a call to the primary storage finished.
///
/// `state` is the state when the call finished, which may differ from the
/// state when it started: other calls can open the breaker in between.
///
/// | `state`              | `route`   | `outcome` | next state |
/// |----------------------|-----------|-----------|------------|
/// | `Closed { failures }`| `Primary` | `Success` | `Closed { failures: 0 }` |
/// | `Closed { failures }`| `Primary` | `Failure` | `Open { until: now + open_for }` if `failures + 1 >= failure_threshold`, otherwise `Closed { failures: failures + 1 }` |
/// | `HalfOpen`           | `Probe`   | `Success` | `Closed { failures: 0 }` |
/// | `HalfOpen`           | `Probe`   | `Failure` | `Open { until: now + open_for }` |
/// | `Open` or `HalfOpen` | `Primary` | either    | unchanged |
/// | `Closed` or `Open`   | `Probe`   | either    | unchanged |
///
/// `now + open_for` saturates at `u64::MAX` instead of overflowing.
///
/// The `Primary` rows for `Open` and `HalfOpen` cover stale results: the call
/// was admitted before the breaker opened and finished afterwards. It says
/// nothing about the primary now, so a slow call that finally succeeds must
/// not close the breaker without a probe, and a late failure must not push
/// `until` further away.
pub(crate) fn after_call(
    state: State,
    route: Route,
    outcome: Outcome,
    now: Nanos,
    policy: &Policy,
) -> State {
    // TODO(SoftDryzz): write the transition described in the table above.
    // `cargo test --lib breaker::transition` checks every row.
    let _ = (state, route, outcome, now, policy);
    todo!("after_call: the transition is not written yet")
}

#[cfg(test)]
mod tests {
    use super::Outcome::{Failure, Success};
    use super::Route::{Primary, Probe};
    use super::State::{Closed, HalfOpen, Open};
    use super::*;

    const S: Nanos = 1_000_000_000;

    fn policy() -> Policy {
        Policy {
            failure_threshold: 3,
            open_for: 5 * S,
        }
    }

    #[test]
    fn closed_success_resets_failures() {
        let next = after_call(Closed { failures: 2 }, Primary, Success, 10, &policy());
        assert_eq!(next, Closed { failures: 0 });
    }

    #[test]
    fn closed_failure_below_threshold_counts() {
        let next = after_call(Closed { failures: 0 }, Primary, Failure, 10, &policy());
        assert_eq!(next, Closed { failures: 1 });
    }

    #[test]
    fn closed_failure_at_threshold_opens() {
        let next = after_call(Closed { failures: 2 }, Primary, Failure, 10, &policy());
        assert_eq!(next, Open { until: 10 + 5 * S });
    }

    #[test]
    fn threshold_one_opens_on_first_failure() {
        let policy = Policy {
            failure_threshold: 1,
            ..policy()
        };
        let next = after_call(Closed { failures: 0 }, Primary, Failure, 10, &policy);
        assert_eq!(next, Open { until: 10 + 5 * S });
    }

    #[test]
    fn half_open_probe_success_closes() {
        let next = after_call(HalfOpen, Probe, Success, 50, &policy());
        assert_eq!(next, Closed { failures: 0 });
    }

    #[test]
    fn half_open_probe_failure_reopens() {
        let next = after_call(HalfOpen, Probe, Failure, 50, &policy());
        assert_eq!(next, Open { until: 50 + 5 * S });
    }

    #[test]
    fn stale_primary_result_leaves_open_unchanged() {
        let open = Open { until: 99 };
        assert_eq!(after_call(open, Primary, Success, 10, &policy()), open);
        assert_eq!(after_call(open, Primary, Failure, 10, &policy()), open);
    }

    #[test]
    fn stale_primary_result_leaves_half_open_unchanged() {
        assert_eq!(
            after_call(HalfOpen, Primary, Success, 10, &policy()),
            HalfOpen
        );
        assert_eq!(
            after_call(HalfOpen, Primary, Failure, 10, &policy()),
            HalfOpen
        );
    }

    #[test]
    fn probe_result_outside_half_open_is_ignored() {
        let closed = Closed { failures: 1 };
        assert_eq!(after_call(closed, Probe, Failure, 10, &policy()), closed);
        let open = Open { until: 5 };
        assert_eq!(after_call(open, Probe, Success, 10, &policy()), open);
    }

    #[test]
    fn open_until_saturates() {
        let now = u64::MAX - 1;
        assert_eq!(
            after_call(Closed { failures: 2 }, Primary, Failure, now, &policy()),
            Open { until: u64::MAX }
        );
        assert_eq!(
            after_call(HalfOpen, Probe, Failure, now, &policy()),
            Open { until: u64::MAX }
        );
    }
}
