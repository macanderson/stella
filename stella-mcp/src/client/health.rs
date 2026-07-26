//! Per-server connection health and the reconnect backoff clock — the state
//! [`super::McpClient`] folds every call and reconnect outcome into, and the
//! snapshot ([`ServerHealth`]) the CLI/TUI/telemetry renders.
//!
//! Split out of `client.rs` verbatim (#629's 1500-line ratchet); the contract
//! itself is documented on [`HealthState`].

use std::time::{Duration, Instant};

use crate::error::McpError;
use crate::transport::Transport;

/// Reconnect backoff floor: the first re-attempt after the *second* straight
/// failure waits this long, doubling each further failure.
const RECONNECT_BASE: Duration = Duration::from_secs(1);
/// Reconnect backoff ceiling — the delay never grows past this, so a
/// long-dead server is still probed roughly twice a minute forever.
const RECONNECT_CAP: Duration = Duration::from_secs(30);

/// Health of one server's connection, as surfaced to the CLI/TUI/telemetry so
/// a mid-session drop is a *visible, non-fatal* diagnostic rather than a
/// silent degradation.
///
/// # What these three states promise the deck
///
/// They answer exactly one question: *would a `tools/call` issued right now be
/// expected to reach this server and come back?*
///
/// - [`HealthState::Live`] — **yes.** The server answered the last thing this
///   client asked it: the handshake (`initialize` + `tools/list`), or a
///   `tools/call`. This is the only state that claims the server works.
/// - [`HealthState::Reconnecting`] — a respawn + handshake is in flight.
/// - [`HealthState::Down`] — **no.** The last thing we asked it failed;
///   [`ServerHealth::last_error`] says how, and nothing has proven otherwise
///   since. `Down` is never terminal — the next call retries (immediately, or
///   once [`ServerHealth::retry_in`] elapses).
///
/// The load-bearing rule (#638): **a successful reconnect proves *connect*
/// health, not *call* health.** Re-establishing the transport clears
/// [`ServerHealth::connect_failures`] and disarms the backoff clock, but it
/// never clears [`ServerHealth::call_failures`] and never promotes a
/// call-failing server back to `Live` — only a request that *returned* does
/// that. Before this split, a server that accepted every connection and then
/// dropped every `tools/call` reported `Live` with a zeroed failure count
/// forever, and the backoff never grew (each heal reset the streak), so a
/// permanently broken server was hammered once per call and looked healthy on
/// the deck the whole time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthState {
    /// The server answered the last request this client sent it (handshake or
    /// `tools/call`).
    Live,
    /// A reconnect attempt is in flight right now.
    Reconnecting,
    /// The last request failed and the server has not answered one since —
    /// either the transport is gone (a reconnect is pending; see
    /// [`ServerHealth::retry_in`]) or it reconnects fine and its *calls* keep
    /// failing.
    Down,
}

/// A point-in-time snapshot of a server's connection health.
#[derive(Debug, Clone)]
pub struct ServerHealth {
    /// The server name (tool namespace segment).
    pub name: String,
    /// Live / Reconnecting / Down — see [`HealthState`] for what each promises.
    pub state: HealthState,
    /// How many `tools/call`s in a row have failed (dropped or timed out)
    /// since the last one that returned. **A successful reconnect does not
    /// reset this** (#638): the connection being back does not mean the
    /// server's calls work.
    pub call_failures: u32,
    /// How many reconnect attempts in a row have failed since the last one
    /// that completed its handshake. `0` for a server that reconnects fine —
    /// including one whose every call then fails.
    pub connect_failures: u32,
    /// The last failure's model-safe message, if any.
    pub last_error: Option<String>,
    /// When a reconnect is pending, roughly how long until it is allowed.
    /// `None` when none is armed — which includes the `Down`-but-connected
    /// case (the transport is up, the calls on it are failing).
    pub retry_in: Option<Duration>,
}

impl ServerHealth {
    /// The failure streak the reconnect backoff is computed from: consecutive
    /// failed calls **plus** consecutive failed reconnects, i.e. how many
    /// attempts in a row this server has not served a request.
    ///
    /// Replaces the old single `consecutive_failures` field, which conflated
    /// the two and was zeroed by a successful reconnect — the bug in #638. Use
    /// [`ServerHealth::call_failures`] / [`ServerHealth::connect_failures`] to
    /// tell "its calls fail" from "we cannot reach it at all".
    pub fn consecutive_failures(&self) -> u32 {
        self.call_failures.saturating_add(self.connect_failures)
    }
}

/// The mutable half of a client: the current transport (`None` once it has
/// been torn down) and its rolling health.
pub(super) struct Connection {
    pub(super) transport: Option<Box<dyn Transport>>,
    pub(super) health: Health,
}

/// Rolling connection health + the backoff clock. The two failure counters are
/// deliberately separate — see [`HealthState`] for the contract they encode.
pub(super) struct Health {
    pub(super) state: HealthState,
    /// Consecutive failed `tools/call`s; only a call that returned clears it.
    pub(super) call_failures: u32,
    /// Consecutive failed reconnects; a completed handshake clears it.
    pub(super) connect_failures: u32,
    pub(super) last_error: Option<String>,
    /// Earliest instant a reconnect may be attempted (set when the transport
    /// is torn down).
    pub(super) next_retry_at: Option<Instant>,
}

impl Default for Health {
    fn default() -> Self {
        Self {
            state: HealthState::Live,
            call_failures: 0,
            connect_failures: 0,
            last_error: None,
            next_retry_at: None,
        }
    }
}

impl Health {
    /// How many attempts in a row this server has failed to serve — the
    /// exponent the reconnect backoff is derived from. Summing the two
    /// counters is what makes a server that reconnects perfectly and fails
    /// every call back off: its `call_failures` keeps climbing even though
    /// each reconnect zeroes `connect_failures`.
    fn failure_streak(&self) -> u32 {
        self.call_failures.saturating_add(self.connect_failures)
    }
}

impl Connection {
    /// A request *returned* — the handshake or a `tools/call`. This is the
    /// only transition into [`HealthState::Live`]: it is the only evidence
    /// that the server actually serves requests.
    pub(super) fn mark_call_success(&mut self) {
        self.health.state = HealthState::Live;
        self.health.call_failures = 0;
        self.health.connect_failures = 0;
        self.health.last_error = None;
        self.health.next_retry_at = None;
    }

    /// A reconnect completed its handshake. The transport is trustworthy
    /// again, so the connect streak and the backoff clock reset — but this
    /// says nothing about whether `tools/call` works, so a server with failing
    /// calls stays [`HealthState::Down`] (with its `last_error` intact) until
    /// a call returns. Zeroing everything here is exactly the #638 bug.
    pub(super) fn mark_connected(&mut self) {
        self.health.connect_failures = 0;
        self.health.next_retry_at = None;
        if self.health.call_failures == 0 {
            self.health.state = HealthState::Live;
            self.health.last_error = None;
        } else {
            self.health.state = HealthState::Down;
        }
    }

    /// A `tools/call` failed (dropped or timed out): drop the transport and arm
    /// the backoff clock so the next reconnect waits an increasing, capped
    /// interval.
    pub(super) fn note_call_failure(&mut self, err: &McpError) {
        self.health.call_failures = self.health.call_failures.saturating_add(1);
        self.tear_down(err);
    }

    /// A reconnect attempt failed (spawn or handshake): same teardown, but the
    /// *connect* counter moves. A call-failing server never accumulates these,
    /// which is how a consumer tells "its calls fail" from "it is unreachable".
    pub(super) fn note_connect_failure(&mut self, err: &McpError) {
        self.health.connect_failures = self.health.connect_failures.saturating_add(1);
        self.tear_down(err);
    }

    /// Shared teardown for both failure kinds: no transport, `Down`, and the
    /// backoff clock armed from the combined streak.
    fn tear_down(&mut self, err: &McpError) {
        self.transport = None;
        self.health.last_error = Some(err.user_message());
        self.health.state = HealthState::Down;
        self.health.next_retry_at =
            Some(Instant::now() + backoff_delay(self.health.failure_streak()));
    }

    /// How long until a reconnect is allowed (`Some(0)` = now), or `None` when
    /// no reconnect is armed.
    pub(super) fn retry_in(&self) -> Option<Duration> {
        self.health
            .next_retry_at
            .map(|at| at.saturating_duration_since(Instant::now()))
    }
}

/// Bounded exponential backoff. The first failure retries immediately (so a
/// single blip self-heals within the turn); each further consecutive failure
/// doubles the wait from [`RECONNECT_BASE`], capped at [`RECONNECT_CAP`].
///
/// `streak` is [`Health::failure_streak`] — failed calls *and* failed
/// reconnects — so a server that reconnects cleanly and then fails the call
/// still backs off instead of being respawned once per call forever (#638).
fn backoff_delay(streak: u32) -> Duration {
    if streak <= 1 {
        return Duration::ZERO;
    }
    // failures=2 -> 2^0·base, =3 -> 2^1·base, … clamp the exponent so the
    // shift can never overflow (the cap dominates long before this bites).
    let exp = (streak - 2).min(20);
    let secs = RECONNECT_BASE.as_secs().saturating_mul(1u64 << exp);
    Duration::from_secs(secs).min(RECONNECT_CAP)
}

/// Whether an error means the *connection* died (spawn/pipe/stream failure or
/// a closed transport) — the only errors a reconnect can fix.
pub(super) fn is_connection_death(err: &McpError) -> bool {
    matches!(err, McpError::Transport(_) | McpError::Closed(_))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_is_zero_on_first_failure_then_doubles_to_the_cap() {
        assert_eq!(backoff_delay(0), Duration::ZERO);
        assert_eq!(backoff_delay(1), Duration::ZERO);
        assert_eq!(backoff_delay(2), Duration::from_secs(1));
        assert_eq!(backoff_delay(3), Duration::from_secs(2));
        assert_eq!(backoff_delay(4), Duration::from_secs(4));
        // Grows exponentially but never past the cap, even at absurd counts.
        assert_eq!(backoff_delay(50), RECONNECT_CAP);
        assert_eq!(backoff_delay(u32::MAX), RECONNECT_CAP);
    }
}
