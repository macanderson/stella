//! The host half of the **driver channel** — the gate, the ceiling, and the
//! report, for a plugin that drives turns rather than sitting inside one.
//!
//! `doc:backlog-self-driving` §3.0. [`super::host_call`] is the identical
//! apparatus for the wrapper socket, and this module is deliberately its
//! sibling rather than its generalisation: the two share the refusal
//! vocabulary ([`HostCallRefusal`]) because the reasons a host declines are
//! properties of asking, not of who asked — and share nothing else, because the
//! *grants* are different consents and merging them is precisely how `arbiter`
//! would come to mean "may merge to `main`".
//!
//! ```text
//!  driver           transport           DriverCallGate        DriverCapabilities
//!    │  {"call":…}      │                     │                       │
//!    ├─────────────────>│  session.call(…)    │                       │
//!    │                  ├────────────────────>│ permits_call? spent?  │
//!    │                  │                     ├──────────────────────>│
//!    │  {"result":…}    │                     │<──────── ok / err ────┤
//!    │<─────────────────┤<────────────────────┤                       │
//! ```
//!
//! # The three bounds, restated for a session that outlives a turn
//!
//! 1. **The allowance is per driver session**, not per point, because a session
//!    is the unit a driver is dispatched in. [`DriverCallGate::open`] hands out
//!    a fresh one each time, which is what makes a long-running loop's next
//!    cycle unaffected by the last one's spending.
//! 2. **The ceiling is the host's, and the manifest's number is an ask.**
//!    `max_calls.unwrap_or(ceiling).min(ceiling)` — the
//!    [`HostCallGate`](super::HostCallGate) clamp, unchanged.
//! 3. **A refused or failed call is answered, not fatal.** This is the bound
//!    the witness for B0 is about: a driver denied `deliver_merge` degrades to
//!    opening the pull request and stopping, and says so. Killing it would turn
//!    an undeclared capability into a crashed loop, and a loop that dies on a
//!    refusal teaches its author nothing.
//!
//! # What this host can actually do at B0
//!
//! Nothing yet, and that is stated rather than implied: [`NoDriverCapabilities`]
//! answers every verb [`HostCallRefusal::Unsupported`], which is invariant 10's
//! discipline pointed at capabilities — a declared gap delivered *to* the
//! driver, never a silence. B1–B6 (#3599) implement the verbs, one family at a
//! time; this module is what makes it possible for any of them to be held.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use stella_plugin::{
    DriverCall, DriverCallOutcome, DriverGrant, DriverOk, HostCallFailure, HostCallRefusal,
};

/// The host's own ceiling on capability asks per driver session, when a caller
/// states none.
///
/// A driver session is a whole cycle — claim an issue, work it, deliver it,
/// sweep, and around again — so the number is an order of magnitude above
/// [`DEFAULT_HOST_MAX_CALLS`](super::DEFAULT_HOST_MAX_CALLS)'s eight, which
/// bounds one point of one turn. It is still a bound rather than a courtesy:
/// each ask is work the host performs inside a budget the user is paying for,
/// and a driver that has asked a hundred and twenty-eight times without
/// reaching a `next` is not making progress, it is spinning.
pub const DEFAULT_DRIVER_MAX_CALLS: u32 = 128;

/// What a host can actually do when a driver asks.
///
/// One method over the closed [`DriverCall`], so the compiler makes an
/// implementor answer for every capability rather than letting an unhandled one
/// become a silence. A host that implements only some of them returns
/// [`HostCallRefusal::Unsupported`] for the rest.
///
/// This is the port the *gate* calls **after** it has checked the grant, so an
/// implementor is never the thing that decides whether a driver may ask. It
/// takes no arguments and returns no payload today for the reason
/// `stella_plugin::driver` states: no verb is implemented at B0, so there is
/// nothing honest for either shape to carry. The phase that implements a
/// family widens this signature along with the wire.
#[async_trait]
pub trait DriverCapabilities: Send + Sync {
    /// Perform `call`, or say why not.
    async fn perform(&self, call: DriverCall) -> Result<DriverOk, HostCallFailure>;
}

/// A host that implements no driver capability yet.
///
/// The honest state of `stella-cli` at B0, and a real value rather than a test
/// double: a driver bound to this one is told `unsupported` for every verb and
/// keeps running, which is the same answer it will get in production until the
/// phase that serves the verb lands.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoDriverCapabilities;

#[async_trait]
impl DriverCapabilities for NoDriverCapabilities {
    async fn perform(&self, call: DriverCall) -> Result<DriverOk, HostCallFailure> {
        Err(HostCallFailure::new(
            HostCallRefusal::Unsupported,
            format!(
                "this host does not perform \"{call}\" yet; the driver channel is B0 and the \
                 {} capabilities land in B1-B6 (#3599)",
                call.family()
            ),
        ))
    }
}

/// One capability ask the gate answered with a refusal or a failure.
///
/// Recorded so the host can say what a driver did not get. The driver already
/// knows — it read the `err` — and a host that cannot see the same fact is a
/// host whose user cannot be told why the loop degraded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedDriverCall {
    /// The capability that was asked for.
    pub call: DriverCall,
    /// Which refusal, as the driver read it.
    pub refusal: HostCallRefusal,
    /// Why, in the words the driver was given.
    pub detail: String,
}

impl std::fmt::Display for RefusedDriverCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} refused ({}): {}",
            self.call, self.refusal, self.detail
        )
    }
}

/// The declared `[driver]` grant, the host's ceiling, and the capabilities
/// behind them.
///
/// Held for the whole of a driver's life and opened once per session
/// ([`DriverCallGate::open`]).
pub struct DriverCallGate {
    declared: DriverGrant,
    max_calls: u32,
    capabilities: Box<dyn DriverCapabilities>,
    refusals: Mutex<Vec<RefusedDriverCall>>,
}

impl std::fmt::Debug for DriverCallGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriverCallGate")
            .field("declared", &self.declared.calls)
            .field("max_calls", &self.max_calls)
            .finish_non_exhaustive()
    }
}

impl DriverCallGate {
    /// Bind a driver's declared grant to what this host can do.
    ///
    /// The whole [`DriverGrant`] is taken rather than its `calls` list because
    /// [`DriverGrant::permits_call`] is the authoritative filter, and a gate
    /// that consulted a bare list would be a second implementation of the rule
    /// the manifest crate already owns.
    #[must_use]
    pub fn declare(
        grant: DriverGrant,
        host_max_calls: u32,
        capabilities: Box<dyn DriverCapabilities>,
    ) -> Self {
        let max_calls = grant
            .max_calls
            .unwrap_or(host_max_calls)
            .min(host_max_calls);
        Self {
            declared: grant,
            max_calls,
            capabilities,
            refusals: Mutex::new(Vec::new()),
        }
    }

    /// The effective allowance per session, after the clamp.
    #[must_use]
    pub fn max_calls(&self) -> u32 {
        self.max_calls
    }

    /// Whether any call this gate could admit exists.
    ///
    /// A transport asks before opening a session, for the reason
    /// [`HostCallGate::offers_calls`](super::HostCallGate::offers_calls) is
    /// asked: a driver that can never be served must not be left holding a
    /// conversation open until the session timeout.
    #[must_use]
    pub fn offers_calls(&self) -> bool {
        self.max_calls > 0
            && self
                .declared
                .calls
                .iter()
                .any(|call| self.declared.permits_call(*call))
    }

    /// Open the conversation for one driver session.
    #[must_use]
    pub fn open(&self) -> DriverSession<'_> {
        DriverSession {
            gate: self,
            spent: AtomicU32::new(0),
        }
    }

    /// Every call this gate refused or failed, in the order it happened.
    #[must_use]
    pub fn refusals(&self) -> Vec<RefusedDriverCall> {
        // A poisoned lock means some other call panicked mid-record; losing the
        // report because of an unrelated panic would be the silence this
        // apparatus refuses, which is also invariant 5's line for an `unwrap`
        // on runtime state.
        self.refusals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Record an answer the driver will read as `err`.
    fn record(&self, call: DriverCall, failure: &HostCallFailure) {
        self.refusals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(RefusedDriverCall {
                call,
                refusal: failure.refusal,
                detail: failure.detail.clone(),
            });
    }
}

/// The conversation a driver holds while one session is open.
///
/// [`Self::spent`] is per session, which is the unit the ceiling is declared
/// in. Atomic rather than behind a `&mut` because the session is shared by
/// reference with whatever is answering it, and a driver that pipelines two
/// asks must not be able to spend the same allowance twice.
#[derive(Debug)]
pub struct DriverSession<'gate> {
    gate: &'gate DriverCallGate,
    spent: AtomicU32,
}

impl DriverSession<'_> {
    /// How many calls have been admitted in this session so far.
    #[must_use]
    pub fn spent(&self) -> u32 {
        self.spent.load(Ordering::Relaxed)
    }

    /// Ask the host for a capability.
    ///
    /// Infallible by construction: a refusal is a [`DriverCallOutcome::Err`]
    /// value the driver reads, never an error that ends the session.
    pub async fn call(&self, call: DriverCall) -> DriverCallOutcome {
        // The grant first, and before anything is spent: an undeclared call
        // costs the driver nothing from an allowance it was never entitled to
        // use.
        if !self.gate.declared.permits_call(call) {
            let failure = HostCallFailure::new(
                HostCallRefusal::Undeclared,
                format!(
                    "this plugin's manifest does not declare \"{call}\" in [driver] calls, so the \
                     host did not perform it"
                ),
            );
            self.gate.record(call, &failure);
            return DriverCallOutcome::Err(failure);
        }

        // Then the allowance. `fetch_add` rather than load-then-store so two
        // pipelined asks cannot both read the last slot as free.
        let spent = self.spent.fetch_add(1, Ordering::Relaxed);
        if spent >= self.gate.max_calls {
            let failure = HostCallFailure::new(
                HostCallRefusal::AllowanceSpent,
                format!(
                    "this driver session's allowance of {} capability asks is spent; the host \
                     will perform no more until the next session",
                    self.gate.max_calls
                ),
            );
            self.gate.record(call, &failure);
            return DriverCallOutcome::Err(failure);
        }

        match self.gate.capabilities.perform(call).await {
            Ok(ok) => DriverCallOutcome::Ok(ok),
            Err(failure) => {
                self.gate.record(call, &failure);
                DriverCallOutcome::Err(failure)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host that serves everything, so a test can isolate the *gate's* answer
    /// from the capability's.
    struct Serves;

    #[async_trait]
    impl DriverCapabilities for Serves {
        async fn perform(&self, _call: DriverCall) -> Result<DriverOk, HostCallFailure> {
            Ok(DriverOk {})
        }
    }

    fn grant(calls: &[DriverCall], max_calls: Option<u32>) -> DriverGrant {
        DriverGrant {
            calls: calls.to_vec(),
            max_calls,
        }
    }

    /// **The B0 witness.** Both directions, because either alone is half a
    /// gate: an undeclared capability is refused with a code the driver can
    /// branch on *and the session keeps going*, and a declared one is served.
    #[tokio::test]
    async fn an_undeclared_call_is_refused_and_the_session_keeps_running() {
        let gate = DriverCallGate::declare(
            grant(&[DriverCall::BacklogNext], None),
            DEFAULT_DRIVER_MAX_CALLS,
            Box::new(Serves),
        );
        let session = gate.open();

        let refused = session.call(DriverCall::DeliverMerge).await;
        match refused {
            DriverCallOutcome::Err(failure) => {
                assert_eq!(failure.refusal, HostCallRefusal::Undeclared);
                assert!(failure.detail.contains("[driver] calls"), "{failure}");
            }
            DriverCallOutcome::Ok(_) => panic!("an undeclared capability was performed"),
        }
        // Refused, and not charged for: the refusal must not eat the allowance
        // the driver was actually granted.
        assert_eq!(session.spent(), 0);

        // And the session is still alive and still able to be served — the
        // half that makes the refusal a value rather than a death.
        assert_eq!(
            session.call(DriverCall::BacklogNext).await,
            DriverCallOutcome::Ok(DriverOk {})
        );
        assert_eq!(session.spent(), 1);

        // The host's own half of "a refusal is reported, never silent".
        let refusals = gate.refusals();
        assert_eq!(refusals.len(), 1);
        assert_eq!(refusals[0].call, DriverCall::DeliverMerge);
        assert_eq!(refusals[0].refusal, HostCallRefusal::Undeclared);
    }

    #[tokio::test]
    async fn the_manifests_allowance_is_an_ask_and_the_host_clamps_it() {
        let gate = DriverCallGate::declare(
            grant(&[DriverCall::BacklogNext], Some(9_000)),
            4,
            Box::new(Serves),
        );
        assert_eq!(gate.max_calls(), 4);

        let session = gate.open();
        for _ in 0..4 {
            assert!(matches!(
                session.call(DriverCall::BacklogNext).await,
                DriverCallOutcome::Ok(_)
            ));
        }
        match session.call(DriverCall::BacklogNext).await {
            DriverCallOutcome::Err(failure) => {
                assert_eq!(failure.refusal, HostCallRefusal::AllowanceSpent);
            }
            DriverCallOutcome::Ok(_) => panic!("the ceiling did not hold"),
        }
    }

    #[tokio::test]
    async fn each_session_gets_a_fresh_allowance() {
        let gate = DriverCallGate::declare(
            grant(&[DriverCall::SweepAudit], Some(1)),
            8,
            Box::new(Serves),
        );
        for _ in 0..3 {
            let session = gate.open();
            assert!(matches!(
                session.call(DriverCall::SweepAudit).await,
                DriverCallOutcome::Ok(_)
            ));
        }
        assert!(gate.refusals().is_empty());
    }

    #[tokio::test]
    async fn this_host_serves_no_capability_yet_and_says_so() {
        let gate = DriverCallGate::declare(
            grant(&[DriverCall::WorkStart], None),
            DEFAULT_DRIVER_MAX_CALLS,
            Box::new(NoDriverCapabilities),
        );
        match gate.open().call(DriverCall::WorkStart).await {
            DriverCallOutcome::Err(failure) => {
                assert_eq!(failure.refusal, HostCallRefusal::Unsupported);
                // The gap names the family and the epic, so a driver author
                // reading the log knows which phase they are waiting on.
                assert!(failure.detail.contains("work"), "{failure}");
                assert!(failure.detail.contains("#3599"), "{failure}");
            }
            DriverCallOutcome::Ok(_) => panic!("a capability nothing implements was performed"),
        }
    }

    #[test]
    fn a_grant_that_can_never_be_served_offers_nothing() {
        assert!(!DriverCallGate::declare(grant(&[], None), 8, Box::new(Serves)).offers_calls());
        assert!(
            DriverCallGate::declare(grant(&[DriverCall::CurateList], None), 8, Box::new(Serves))
                .offers_calls()
        );
        // A zero ceiling the host itself imposed leaves nothing admissible, and
        // the transport must be able to see that before it opens a session.
        assert!(
            !DriverCallGate::declare(grant(&[DriverCall::CurateList], None), 0, Box::new(Serves))
                .offers_calls()
        );
    }
}
