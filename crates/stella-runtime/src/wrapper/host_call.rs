//! The host half of the host-call channel — the gate, the ceiling, and the
//! report.
//!
//! `doc:wrapper-socket` §6b: **a plugin may ask the host for a capability; it
//! may never reach for one.** `stella_plugin::host_call` is the wire half — what
//! a plugin may write and what it reads back. This module is the half that
//! decides, and the division is the whole point: the plugin asks, and the *host*
//! performs the retrieval, applies the grant a human consented to at install,
//! and returns only what that grant permits.
//!
//! ```text
//!  plugin           transport            HostCallGate          HostCapabilities
//!    │  {"call":…}      │                     │                       │
//!    ├─────────────────>│  channel.call(args) │                       │
//!    │                  ├────────────────────>│ permits_call? spent?  │
//!    │                  │                     ├──────────────────────>│
//!    │  {"result":…}    │                     │<──────── ok / err ────┤
//!    │<─────────────────┤<────────────────────┤                       │
//! ```
//!
//! # Three bounds, and each one is contract rather than implementation detail
//!
//! A conversation can hang where a single exchange could not, so §6b makes the
//! bounds part of the socket:
//!
//! 1. **The per-point timeout covers the whole conversation**, not each turn of
//!    it — a plugin cannot buy time by talking. That one lives in the transport
//!    ([`SubprocessWrapper`](super::SubprocessWrapper)), because it is the thing
//!    that owns the clock.
//! 2. **A host-clamped ceiling on calls per point.** [`HostCallGate::declare`]
//!    applies `max_calls.unwrap_or(ceiling).min(ceiling)`, which is
//!    [`again`](super::again)'s `max_holds` clamp one layer down and for the same
//!    reason: the plugin's number is an ask, never an authority.
//! 3. **A refused or failed call is answered, not fatal.** Every refusal is a
//!    [`HostCallFailure`] delivered *to the plugin*, so it can degrade honestly
//!    — the fail-open direction the Stop gate already argues for
//!    (`crates/stella-core/src/user_hooks.rs:55-59`). Killing a plugin for
//!    asking is how a plugin author learns nothing.
//!
//! The third bound is why the ceiling is not also a kill switch: a plugin that
//! keeps asking past its allowance is answered `allowance-spent` every time and
//! buys nothing, and the conversation timeout is what ends it. Those two are
//! deliberately different bounds — one bounds the *work* a host does on a
//! plugin's behalf, the other bounds the *time* the point may take.
//!
//! # A refusal is reported, never silent
//!
//! The plugin is told, and so is the host: every `err` answer is recorded on the
//! gate and readable through [`HostCallGate::refusals`]. A capability quietly
//! not performed is a bug its author cannot diagnose — the same argument
//! [`AdmittedWrapper::refused`](super::AdmittedWrapper) makes for a withheld
//! environment variable, and the reason that field is `#[must_use]`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use stella_plugin::{
    HostCall, HostCallArgs, HostCallFailure, HostCallOk, HostCallOutcome, HostCallRefusal,
    LoopGrant, RecallFrame, RecallResult,
};

/// The host's own ceiling on host calls per point, when a caller states none.
///
/// Eight, and the number is the host's rather than the manifest's on purpose:
/// [`LoopGrant::max_calls`] is a plugin's *ask* and [`HostCallGate::declare`]
/// clamps it against this. Each call is work the host performs on a plugin's
/// behalf inside a budget the user is paying for, and eight is the smallest
/// allowance that still lets a retrieval plugin do the thing retrieval is for —
/// a broad ask, a few narrowings, and a re-ask per stage of a round.
pub const DEFAULT_HOST_MAX_CALLS: u32 = 8;

/// The most frames one `recall` returns, when a caller states no ceiling.
///
/// The plugin's [`RecallArgs::limit`](stella_plugin::RecallArgs) is an ask
/// clamped against this. Frames ride into the turn as volatile context, so an
/// unbounded answer is an unbounded prompt.
pub const DEFAULT_RECALL_FRAMES: u32 = 8;

/// The host's context plane, as the socket needs it.
///
/// Deliberately shaped like `stella_pipeline::ContextRecallPort::recall` — one
/// goal in, the frames out, never an error the caller has to special-case
/// (weak or absent context degrades to "no frames", L-C6) — so a host that
/// already has a recall port implements this by projecting its frames into the
/// wire view and nothing else.
///
/// It is **not** that trait, and that is not an oversight:
/// `crates/stella-runtime/tests/no_pipeline_edge.rs` asserts executably that the
/// assembly seam declares no edge to the staged pipeline, because a wrapper
/// `stella-cli` can drive and `stella-serve` cannot is a CLI feature wearing a
/// socket's name. The projection is the driver's, and it is mechanical.
///
/// **No shipping driver has written that projection yet** — `stella-cli` builds
/// its transport without a gate, so an installed plugin's `recall` reaches no
/// context plane on the `stella run` path. That is #3561, declared here rather
/// than left to be discovered as an empty frame list.
#[async_trait]
pub trait RecallHost: Send + Sync {
    /// Recall material relevant to `goal`.
    ///
    /// An empty answer is ordinary — nothing was relevant, or this host has no
    /// context plane configured.
    async fn recall(&self, goal: &str) -> Vec<RecallFrame>;
}

/// What a host can actually do when a plugin asks.
///
/// One method over the closed [`HostCallArgs`], so the compiler makes an
/// implementor answer for every capability rather than letting an unhandled one
/// become a silence. A host that implements only some of them returns
/// [`HostCallRefusal::Unsupported`] for the rest — a declared gap, delivered to
/// the plugin, which is invariant 10's discipline pointed at capabilities.
///
/// This is the port the *gate* calls **after** it has checked the grant, so an
/// implementor is never the thing that decides whether a plugin may ask.
#[async_trait]
pub trait HostCapabilities: Send + Sync {
    /// Perform one call the gate has already admitted.
    ///
    /// # Errors
    ///
    /// [`HostCallFailure`] when this host does not implement the capability, has
    /// no plane behind it, or tried and failed. Every one of them reaches the
    /// plugin as an `err` answer rather than ending the point.
    async fn call(&self, args: HostCallArgs) -> Result<HostCallOk, HostCallFailure>;
}

/// The capabilities of a host that has a context plane and nothing else.
///
/// The shipped implementation, and an honest one: `recall` is performed, and
/// `child_turn` and `run_test` are refused as [`HostCallRefusal::Unsupported`]
/// with the issue that will wire them named in the detail. That is a *declared*
/// gap — the plugin is told, the refusal is recorded — rather than a capability
/// that appears to exist and returns nothing.
pub struct RecallOnly<R> {
    recall: R,
    max_frames: u32,
}

impl<R: RecallHost> RecallOnly<R> {
    /// Serve `recall` from this host's context plane, at the default frame
    /// ceiling.
    #[must_use]
    pub fn new(recall: R) -> Self {
        Self {
            recall,
            max_frames: DEFAULT_RECALL_FRAMES,
        }
    }

    /// Set this host's ceiling on frames per call, whatever a plugin asks for.
    #[must_use]
    pub fn with_max_frames(mut self, frames: u32) -> Self {
        self.max_frames = frames;
        self
    }
}

#[async_trait]
impl<R: RecallHost> HostCapabilities for RecallOnly<R> {
    async fn call(&self, args: HostCallArgs) -> Result<HostCallOk, HostCallFailure> {
        match args {
            HostCallArgs::Recall(recall) => {
                // The plugin's limit is an ask; the host's ceiling is the
                // authority. `unwrap_or(ceiling).min(ceiling)` is the same
                // clamp shape `again` uses for `max_holds`, so "absent" means
                // the ceiling rather than unbounded.
                let limit = recall.limit.unwrap_or(self.max_frames).min(self.max_frames);
                let mut frames = self.recall.recall(&recall.goal).await;
                frames.truncate(limit as usize);
                Ok(HostCallOk::Recall(RecallResult { frames }))
            }
            HostCallArgs::ChildTurn(_) => Err(HostCallFailure::new(
                HostCallRefusal::Unsupported,
                "this host does not run child turns for plugins yet; the role intent a plugin \
                 names in its before_turn response is the path that exists today (#3564)",
            )),
            HostCallArgs::RunTest(_) => Err(HostCallFailure::new(
                HostCallRefusal::Unsupported,
                "this host does not re-run a candidate's tests for plugins yet; the test plan in \
                 the candidate grant is the path that exists today (#3564)",
            )),
        }
    }
}

/// One call the gate answered with a refusal or a failure.
///
/// Recorded so the host can say what a plugin did not get. The plugin already
/// knows — it read the `err` — and a host that cannot see the same fact is a
/// host whose user cannot be told why a plugin degraded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedCall {
    /// The capability that was asked for.
    pub call: HostCall,
    /// Which refusal, as the plugin read it.
    pub refusal: HostCallRefusal,
    /// Why, in the words the plugin was given.
    pub detail: String,
}

impl std::fmt::Display for RefusedCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} refused ({}): {}",
            self.call, self.refusal, self.detail
        )
    }
}

/// The declared grant, the host's ceiling, and the capabilities behind them.
///
/// Held by a transport for the whole of a plugin's life and opened once per
/// point ([`HostCallGate::open`]), because the allowance is *per point*: a
/// plugin that spent its calls researching `before_turn` must still be able to
/// ask when `after_turn` arrives.
pub struct HostCallGate {
    declared: LoopGrant,
    max_calls: u32,
    capabilities: Box<dyn HostCapabilities>,
    refusals: Mutex<Vec<RefusedCall>>,
}

impl std::fmt::Debug for HostCallGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostCallGate")
            .field("declared", &self.declared.calls)
            .field("max_calls", &self.max_calls)
            .finish_non_exhaustive()
    }
}

impl HostCallGate {
    /// Bind a plugin's declared grant to what this host can do.
    ///
    /// `host_max_calls` is this host's ceiling; the manifest's
    /// [`LoopGrant::max_calls`] is clamped against it, and a manifest that asks
    /// for nothing inherits the ceiling rather than zero — the
    /// [`again`](super::again) rule for `max_holds`, restated where the second
    /// allowance lives.
    ///
    /// The whole [`LoopGrant`] is taken rather than its `calls` list because
    /// [`LoopGrant::permits_call`] is the authoritative filter and it grades the
    /// participation too: a grant assembled by hand below `steering` must never
    /// leak a capability, however its `calls` list reads.
    #[must_use]
    pub fn declare(
        grant: LoopGrant,
        host_max_calls: u32,
        capabilities: Box<dyn HostCapabilities>,
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

    /// The effective allowance per point, after the clamp.
    #[must_use]
    pub fn max_calls(&self) -> u32 {
        self.max_calls
    }

    /// Whether any call this gate could admit exists.
    ///
    /// A transport asks before opening a conversation, because the answer
    /// decides whether the plugin's stdin stays open after the request. It is
    /// not an optimisation: a plugin that declares no calls reads its request
    /// with `cat` or `sys.stdin.read()` — both of which wait for EOF — and
    /// holding stdin open for a conversation it can never have would hang it
    /// until the point timeout.
    #[must_use]
    pub fn offers_calls(&self) -> bool {
        self.max_calls > 0
            && self
                .declared
                .calls
                .iter()
                .any(|call| self.declared.permits_call(*call))
    }

    /// Open the conversation for one point.
    ///
    /// A fresh allowance each time, which is what "per point" means. The channel
    /// borrows the gate, so every call it admits is recorded on the one report a
    /// host reads.
    #[must_use]
    pub fn open(&self) -> PointChannel<'_> {
        PointChannel {
            gate: self,
            spent: AtomicU32::new(0),
        }
    }

    /// Every call this gate refused or failed, in the order it happened.
    ///
    /// The host's half of "a refusal is reported, never silent". Empty in the
    /// ordinary case.
    #[must_use]
    pub fn refusals(&self) -> Vec<RefusedCall> {
        // A poisoned lock means some other call panicked mid-record; the
        // recorded refusals are still exactly what they were, and losing the
        // report because of an unrelated panic would be the silence this whole
        // apparatus refuses. `unwrap` on runtime state is also invariant 5's
        // line.
        self.refusals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Record an answer the plugin will read as `err`.
    fn record(&self, call: HostCall, failure: &HostCallFailure) {
        self.refusals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(RefusedCall {
                call,
                refusal: failure.refusal,
                detail: failure.detail.clone(),
            });
    }
}

/// The conversation a plugin holds while one point is open.
///
/// Both transports drive this same object — the subprocess one relays JSON
/// through it, the in-process one hands it to a handler — so a Rust plugin can
/// ask for exactly what a Python plugin can ask for and no more
/// (`doc:pipeline-as-plugins` §5 commitment 2).
#[async_trait]
pub trait HostCallChannel: Send + Sync {
    /// Ask the host for a capability.
    ///
    /// Infallible by construction: a refusal is an [`HostCallOutcome::Err`]
    /// value the plugin reads, never an error that ends the point. That is the
    /// bound §6b states — "the response to a refused or failed call is delivered
    /// to the plugin so it can degrade honestly, rather than killing it".
    async fn call(&self, args: HostCallArgs) -> HostCallOutcome;
}

/// The gate's channel for one point.
///
/// [`Self::spent`] is per point and per channel, which is the unit the ceiling
/// is declared in. It is atomic rather than behind a `&mut` because the channel
/// is shared by reference with whatever is answering the point, and a plugin
/// that pipelines two calls must not be able to spend the same allowance twice.
#[derive(Debug)]
pub struct PointChannel<'gate> {
    gate: &'gate HostCallGate,
    spent: AtomicU32,
}

impl PointChannel<'_> {
    /// How many calls have been admitted at this point so far.
    #[must_use]
    pub fn spent(&self) -> u32 {
        self.spent.load(Ordering::Relaxed)
    }
}

#[async_trait]
impl HostCallChannel for PointChannel<'_> {
    async fn call(&self, args: HostCallArgs) -> HostCallOutcome {
        let call = args.call();

        // The grant first, and before anything is spent: an undeclared call is
        // refused the way an undeclared hook is never invoked, and it costs the
        // plugin nothing from an allowance it was never entitled to use.
        if !self.gate.declared.permits_call(call) {
            let failure = HostCallFailure::new(
                HostCallRefusal::Undeclared,
                format!(
                    "this plugin's manifest does not declare \"{call}\" in [loop] calls, so the \
                     host did not perform it"
                ),
            );
            self.gate.record(call, &failure);
            return HostCallOutcome::Err(failure);
        }

        // `fetch_add` rather than load-then-store: two calls in flight must not
        // both read the last unit of the allowance.
        let spent = self.spent.fetch_add(1, Ordering::Relaxed);
        if spent >= self.gate.max_calls {
            let failure = HostCallFailure::new(
                HostCallRefusal::AllowanceSpent,
                format!(
                    "this point's host-call allowance of {} is spent; answer with what you have",
                    self.gate.max_calls
                ),
            );
            self.gate.record(call, &failure);
            return HostCallOutcome::Err(failure);
        }

        match self.gate.capabilities.call(args).await {
            Ok(ok) => HostCallOutcome::Ok(ok),
            Err(failure) => {
                self.gate.record(call, &failure);
                HostCallOutcome::Err(failure)
            }
        }
    }
}

/// The channel a transport serves when no gate was attached.
///
/// Every call is refused as [`HostCallRefusal::Unavailable`] — this host offers
/// no capabilities — and the plugin is told so rather than left waiting for an
/// answer that will never come. A driver that has not wired a gate yet is a
/// declared gap; a plugin hanging on a silent socket is a hang.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoHostCalls;

#[async_trait]
impl HostCallChannel for NoHostCalls {
    async fn call(&self, args: HostCallArgs) -> HostCallOutcome {
        HostCallOutcome::Err(HostCallFailure::new(
            HostCallRefusal::Unavailable,
            format!(
                "this host offers no host calls, so \"{}\" was not performed",
                args.call()
            ),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_plugin::{Participation, RecallArgs, WrapperPoint};

    /// A recall plane that answers with `count` numbered frames.
    struct Frames(usize);

    #[async_trait]
    impl RecallHost for Frames {
        async fn recall(&self, goal: &str) -> Vec<RecallFrame> {
            (0..self.0)
                .map(|n| RecallFrame {
                    label: format!("frame {n}"),
                    kind: "memory".to_string(),
                    source: "context.db".to_string(),
                    uri: None,
                    content: format!("{goal} #{n}"),
                })
                .collect()
        }
    }

    fn grant(calls: Vec<HostCall>, max_calls: Option<u32>) -> LoopGrant {
        LoopGrant {
            participation: Participation::Steering,
            hooks: Vec::new(),
            points: vec![WrapperPoint::BeforeTurn],
            calls,
            max_calls,
            max_holds: None,
        }
    }

    fn recall(goal: &str) -> HostCallArgs {
        HostCallArgs::Recall(RecallArgs {
            goal: goal.to_string(),
            limit: None,
        })
    }

    fn gate(calls: Vec<HostCall>, max_calls: Option<u32>, frames: usize) -> HostCallGate {
        HostCallGate::declare(
            grant(calls, max_calls),
            DEFAULT_HOST_MAX_CALLS,
            Box::new(RecallOnly::new(Frames(frames))),
        )
    }

    #[tokio::test]
    async fn a_declared_call_reaches_the_host_plane() {
        let gate = gate(vec![HostCall::Recall], None, 2);
        let channel = gate.open();
        match channel.call(recall("the parser")).await {
            HostCallOutcome::Ok(HostCallOk::Recall(result)) => {
                assert_eq!(result.frames.len(), 2);
                assert_eq!(result.frames[0].content, "the parser #0");
            }
            other => panic!("expected recalled frames, got {other:?}"),
        }
        assert!(gate.refusals().is_empty());
    }

    #[tokio::test]
    async fn an_undeclared_call_is_refused_and_reported() {
        let gate = gate(vec![HostCall::Recall], None, 1);
        let channel = gate.open();
        let outcome = channel
            .call(HostCallArgs::ChildTurn(stella_plugin::ChildTurnArgs {
                role: "verifier".to_string(),
                instruction: "check it".to_string(),
            }))
            .await;
        match outcome {
            HostCallOutcome::Err(failure) => {
                assert_eq!(failure.refusal, HostCallRefusal::Undeclared);
            }
            other => panic!("an undeclared call must be refused, got {other:?}"),
        }
        let refusals = gate.refusals();
        assert_eq!(refusals.len(), 1, "the refusal is reported, never silent");
        assert_eq!(refusals[0].call, HostCall::ChildTurn);
        assert_eq!(
            channel.spent(),
            0,
            "an undeclared call costs an allowance it was never entitled to"
        );
    }

    /// The manifest's number is an ask; the host's ceiling is the authority.
    #[tokio::test]
    async fn the_declared_allowance_is_clamped_to_the_hosts_ceiling() {
        let generous = HostCallGate::declare(
            grant(vec![HostCall::Recall], Some(1_000)),
            2,
            Box::new(RecallOnly::new(Frames(1))),
        );
        assert_eq!(generous.max_calls(), 2);

        let channel = generous.open();
        for _ in 0..2 {
            assert!(matches!(
                channel.call(recall("x")).await,
                HostCallOutcome::Ok(_)
            ));
        }
        match channel.call(recall("x")).await {
            HostCallOutcome::Err(failure) => {
                assert_eq!(failure.refusal, HostCallRefusal::AllowanceSpent);
            }
            other => panic!("the allowance must be spent, got {other:?}"),
        }
        assert_eq!(generous.refusals().len(), 1);

        // Per point, not per plugin: the next point opens with a fresh
        // allowance, which is what a wrapper answering two points needs.
        let next = generous.open();
        assert!(matches!(
            next.call(recall("x")).await,
            HostCallOutcome::Ok(_)
        ));
    }

    #[tokio::test]
    async fn a_plugins_frame_limit_is_an_ask_the_host_clamps() {
        let gate = HostCallGate::declare(
            grant(vec![HostCall::Recall], None),
            DEFAULT_HOST_MAX_CALLS,
            Box::new(RecallOnly::new(Frames(50)).with_max_frames(3)),
        );
        let channel = gate.open();
        let asked = HostCallArgs::Recall(RecallArgs {
            goal: "everything".to_string(),
            limit: Some(40),
        });
        match channel.call(asked).await {
            HostCallOutcome::Ok(HostCallOk::Recall(result)) => assert_eq!(result.frames.len(), 3),
            other => panic!("expected clamped frames, got {other:?}"),
        }
    }

    /// A capability this host does not implement is a declared gap the plugin
    /// is told about, not a silence and not a death.
    #[tokio::test]
    async fn an_unsupported_capability_is_answered_rather_than_fatal() {
        let gate = gate(vec![HostCall::Recall, HostCall::RunTest], None, 1);
        let channel = gate.open();
        let outcome = channel
            .call(HostCallArgs::RunTest(stella_plugin::RunTestArgs {
                candidate: stella_protocol::CandidateHandle::new("candidate-1"),
            }))
            .await;
        match outcome {
            HostCallOutcome::Err(failure) => {
                assert_eq!(failure.refusal, HostCallRefusal::Unsupported);
            }
            other => panic!("expected an unsupported refusal, got {other:?}"),
        }
        assert_eq!(gate.refusals().len(), 1);
    }

    #[tokio::test]
    async fn a_host_with_no_gate_refuses_rather_than_hangs() {
        match NoHostCalls.call(recall("x")).await {
            HostCallOutcome::Err(failure) => {
                assert_eq!(failure.refusal, HostCallRefusal::Unavailable);
            }
            other => panic!("expected unavailable, got {other:?}"),
        }
    }
}
