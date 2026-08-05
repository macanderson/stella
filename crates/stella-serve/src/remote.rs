//! The engine's ports, remoted.
//!
//! Server-side the engine must never run a model call or a tool with ambient
//! authority — the whole containment story of ADR-033 Option B rests on this.
//! So [`RemoteProvider`] and [`RemoteToolExecutor`] implement the engine's
//! `Provider` / `ToolExecutor` ports by emitting a reverse-RPC request frame to
//! the host and awaiting the host's answer. The host runs the effect through
//! its own governance (`kernel.invoke()`, `@oxagen/ai`) and posts the result
//! back, which resolves the one-shot the port is parked on.
//!
//! Both ports are constructed on, and driven by, the session thread's
//! current-thread runtime; the one-shot receivers they await are fired from the
//! server runtime. That cross-runtime wake is the `!Send`-future bridge.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use stella_core::bus::{self, HookBus, HookDecision, HookEventDraft, names as hook_names};
use stella_core::ports::ToolExecutor;
use stella_core::retry::Sleeper;
use stella_pipeline::ports::{
    ApprovalGate, CmdKind, CmdOutcome, DiagnosticInvocation, DiagnosticRunner, ScopeDecision,
    TestInvocation, TestRunner,
};
use stella_protocol::{
    CompletionRequestRef, CompletionResult, Provider, ProviderError, ScopeProposal,
    ToolCallObserver, ToolOutput, ToolSchema,
};
use tokio::sync::{mpsc, oneshot};

use crate::frame::{ProviderDelta, ServerFrame};
use crate::observe::event::{RequestId, ReverseKind, ServeEvent, millis};
use crate::pending::Pending;

/// Record that a reverse request reached the host, and start its clock.
///
/// Emitted *after* the frame is sent, never before: a request the host never
/// received was not dispatched, and counting it as such would leave the
/// in-flight gauge permanently short of an answer.
fn dispatched(pending: &Pending, request_id: &str, kind: ReverseKind) -> Instant {
    pending.observer().emit(&ServeEvent::ReverseDispatched {
        turn: pending.turn().clone(),
        request_id: RequestId::sanitized(request_id),
        kind,
    });
    Instant::now()
}

/// Record that the host answered, and how long the engine step waited.
fn answered(pending: &Pending, request_id: &str, kind: ReverseKind, started: Instant) {
    pending.observer().emit(&ServeEvent::ReverseAnswered {
        turn: pending.turn().clone(),
        request_id: RequestId::sanitized(request_id),
        kind,
        waited_ms: millis(started.elapsed()),
    });
}

/// Record that the host never answered and the port gave up.
///
/// **The** wedge signal. This is the moment a turn has burned its whole
/// reverse-request deadline waiting on a host that said nothing, and before this
/// it was a silent `HashMap::remove` inside `Pending::abandon` — the single most
/// diagnostic event this service can produce, and it produced nothing.
fn timed_out(pending: &Pending, request_id: &str, kind: ReverseKind, started: Instant) {
    pending.observer().emit(&ServeEvent::ReverseTimedOut {
        turn: pending.turn().clone(),
        request_id: RequestId::sanitized(request_id),
        kind,
        waited_ms: millis(started.elapsed()),
    });
}

/// How long a reverse request waits for the host before the port gives up.
///
/// A reverse request is the host running a model completion or a tool call, so
/// the bound has to clear the slowest legitimate one: an extended-thinking
/// completion, or a tool that shells out to a test suite. Both run in minutes,
/// not seconds. Five minutes sits comfortably above that while still turning
/// "wedged forever" into "fails in minutes" — the whole point, since the parked
/// step also holds an OS thread.
///
/// For a provider request this is an **idle** bound, not a total one: a host
/// that streams fragments back (`POST /v1/turns/{id}/provider-delta`, #1165)
/// resets the clock with each batch, so a completion that keeps producing is
/// never cut however long it runs — the same "measure silence, not elapsed
/// time" rule as `EngineConfig::model_timeout`. A host that does not stream
/// sees exactly the fixed window it always had.
///
/// Per-turn overridable via [`crate::SessionSpec::reverse_request_timeout`] (on
/// the wire: `reverse_request_timeout_ms` on `POST /v1/turns`), because a host
/// with genuinely slower tools should raise it rather than have the engine
/// pretend its tools failed.
pub(crate) const DEFAULT_REVERSE_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Mints [`RemoteProvider`] instance tags. Process-wide rather than
/// per-session because uniqueness only has to hold within one turn's
/// [`Pending`] registry, and a global counter reaches that without threading
/// an allocator through every construction site.
static PROVIDER_INSTANCES: AtomicU64 = AtomicU64::new(0);

/// Hand one streamed fragment to the engine's observer, on the channel that
/// keeps answer text and thinking apart. A `None` observer (the plain
/// `complete_ref` path) drops the fragment: the aggregated result is
/// authoritative either way.
fn forward_delta(observer: Option<&dyn ToolCallObserver>, delta: &ProviderDelta) {
    let Some(observer) = observer else { return };
    match delta {
        ProviderDelta::Text { text } => observer.text_delta(text),
        ProviderDelta::Reasoning { text } => observer.reasoning_delta(text),
    }
}

/// A Tokio-backed [`Sleeper`] for the session runtime's retry backoff. The
/// session runtime is built with the time driver enabled, so `sleep` resolves
/// there.
pub(crate) struct TokioSleeper;

#[async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration_ms: u64) {
        tokio::time::sleep(std::time::Duration::from_millis(duration_ms)).await;
    }
}

/// The `Provider` port as a reverse-RPC to the host. `complete_ref` emits a
/// [`ServerFrame::ProviderRequest`] and blocks the step on the host's answer.
pub(crate) struct RemoteProvider {
    id: String,
    /// What this provider's calls are FOR (#1297), stamped onto every request
    /// frame so a host can route a judge or a sub-agent to a different model
    /// than the worker. One per purpose: a turn that runs a judged goal loop
    /// builds two of these, not one shared instance with a mutable role.
    role: stella_protocol::ModelCallRole,
    /// Disambiguates this provider's request ids from every other provider
    /// sharing the turn's [`Pending`] registry (#1297).
    ///
    /// Load-bearing since a turn stopped meaning one provider: the counter
    /// below starts at zero per instance, so a worker and a sub-agent's
    /// provider would both mint `prov-0`, and the second registration would
    /// collide with the first's parked request. A process-wide instance tag
    /// costs one atomic per provider and makes that collision
    /// unrepresentable rather than unlikely.
    instance: u64,
    frames: crate::backlog::FrameSink,
    pending: Pending,
    counter: AtomicU64,
    timeout: Duration,
}

impl RemoteProvider {
    pub(crate) fn new(
        id: String,
        frames: crate::backlog::FrameSink,
        pending: Pending,
        timeout: Duration,
    ) -> Self {
        Self {
            id,
            role: stella_protocol::ModelCallRole::Worker,
            instance: PROVIDER_INSTANCES.fetch_add(1, Ordering::Relaxed),
            frames,
            pending,
            counter: AtomicU64::new(0),
            timeout,
        }
    }

    /// The same remoted provider, announcing a different role — the judge of
    /// a goal run, or a sub-agent's own calls (#1297).
    ///
    /// A separate instance rather than a setter: the role is stamped on every
    /// frame this provider emits, and two agents share one turn's frame sink,
    /// so a mutable role would be a race between the worker's next call and
    /// the judge's.
    pub(crate) fn with_role(mut self, role: stella_protocol::ModelCallRole) -> Self {
        self.role = role;
        self
    }

    /// This provider's id — what a host reads off the request frame to pick a
    /// model. Cloned per frame; ids are short.
    pub(crate) fn id_string(&self) -> String {
        self.id.clone()
    }
}

impl RemoteProvider {
    /// The one remoted completion path, shared by both trait methods.
    ///
    /// With an observer this is what closes #1165: fragments the host POSTs to
    /// `/v1/turns/{id}/provider-delta` land on the registered feed and are
    /// forwarded inline, so the engine's gate emits `TextDelta` / `Reasoning`
    /// events and the frames flow into `FrameHistory` — ordering, `seq`, and
    /// replay for free, exactly as with a local streaming adapter. Without an
    /// observer the fragments are drained and dropped; the aggregated result
    /// stays authoritative either way.
    async fn complete_remoted(
        &self,
        req: CompletionRequestRef<'_>,
        observer: Option<&dyn ToolCallObserver>,
    ) -> Result<CompletionResult, ProviderError> {
        // Refuse to park a cancelled turn. `Cancelled` is not retryable, so this
        // is what unwinds the turn after `POST /v1/turns/{id}/cancel` — including
        // when the engine reaches for the model again after a cancelled tool.
        if self.pending.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let request_id = format!(
            "prov-{}-{}",
            self.instance,
            self.counter.fetch_add(1, Ordering::Relaxed)
        );
        let (tx, mut rx) = oneshot::channel();
        let (feed, mut deltas) = mpsc::unbounded_channel::<ProviderDelta>();
        // Register before emitting so the entry always exists by the time the
        // host can answer (register → send ordering closes the race). A refused
        // registration means `cancel()` landed since the check above — its
        // wake-everyone clear has already run, so parking now would wait out
        // the full deadline with nobody left to wake this step.
        if !self.pending.register_provider(request_id.clone(), tx, feed) {
            return Err(ProviderError::Cancelled);
        }
        if self
            .frames
            .send(ServerFrame::ProviderRequest {
                request_id: request_id.clone(),
                provider_id: self.id.clone(),
                role: self.role,
                // The one boundary that genuinely needs the copy #921 removed
                // from the engine: this frame outlives the call, crossing from
                // the session thread to the server runtime, so it cannot
                // borrow the caller's transcript. Taken explicitly, once, here
                // — not silently once per retry attempt inside the driver.
                request: req.into_owned(),
            })
            .is_err()
        {
            // The host stream is gone; a disconnect mid-flight is a retryable
            // transport condition, classified here at the adapter as upstream.
            self.pending.abandon(&request_id);
            return Err(ProviderError::transport(
                "serve host disconnected before the model call could be dispatched".to_string(),
            ));
        }
        let started = dispatched(&self.pending, &request_id, ReverseKind::Provider);
        // Drain fragments while parked on the result. Each loop iteration
        // arms a fresh sleep, which is what makes the deadline an *idle*
        // bound for a streaming host (see DEFAULT_REVERSE_REQUEST_TIMEOUT):
        // a batch landing resets it, silence does not.
        let mut feed_open = true;
        loop {
            tokio::select! {
                result = &mut rx => {
                    // The result races fragments already queued on the feed:
                    // both can be ready in the same poll, and the host sent
                    // the fragments first. Drain them before consuming the
                    // result, or the tail of the stream is silently — and
                    // nondeterministically — dropped.
                    while let Ok(delta) = deltas.try_recv() {
                        forward_delta(observer, &delta);
                    }
                    return match result {
                        Ok(result) => {
                            answered(&self.pending, &request_id, ReverseKind::Provider, started);
                            result
                        }
                        // Sender dropped. Cancellation is the deliberate case
                        // and reports itself as such; otherwise the session is
                        // being torn down. Either way `Pending`'s own clear
                        // reports the discard, so there is no per-request
                        // event here.
                        Err(_) if self.pending.is_cancelled() => Err(ProviderError::Cancelled),
                        Err(_) => Err(ProviderError::transport(
                            "serve host dropped the model call without answering".to_string(),
                        )),
                    };
                }
                maybe = deltas.recv(), if feed_open => match maybe {
                    Some(delta) => forward_delta(observer, &delta),
                    // The registry entry is gone — the result has been taken
                    // (its reply fires next poll) or the turn was cleared
                    // (the reply errors next poll). Either way the branch is
                    // disabled rather than spun on: `recv` on a closed
                    // channel returns instantly forever.
                    None => feed_open = false,
                },
                _ = tokio::time::sleep(self.timeout) => {
                    self.pending.abandon(&request_id);
                    timed_out(&self.pending, &request_id, ReverseKind::Provider, started);
                    // Deliberately `Terminal`, not `Transport`: `Transport` is
                    // retryable, so a host that is simply not answering would be
                    // handed the same unbounded wait again once per retry —
                    // multiplying the very window this deadline exists to close.
                    return Err(ProviderError::Terminal(format!(
                        "serve host did not answer the model call within {:?} \
                         (reverse-request deadline)",
                        self.timeout
                    )));
                }
            }
        }
    }
}

#[async_trait]
impl Provider for RemoteProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.complete_remoted(req, None).await
    }

    /// The override that puts a served turn's streamed text on the engine's
    /// event stream (#1165). The engine drives every model call through this
    /// method with its speculation gate as the observer, so forwarding the
    /// host's fragments here is all it takes for `AgentEvent::TextDelta` to
    /// fire under serve exactly as it does with a local provider.
    async fn complete_observed_ref(
        &self,
        req: CompletionRequestRef<'_>,
        observer: &dyn ToolCallObserver,
    ) -> Result<CompletionResult, ProviderError> {
        self.complete_remoted(req, Some(observer)).await
    }
}

/// The `ToolExecutor` port as a reverse-RPC to the host. `schemas` returns the
/// tool set the host advertised at session start (including `read_only` flags,
/// so the engine's partitioned concurrency still applies); `execute` emits a
/// [`ServerFrame::ToolRequest`] and blocks on the host's answer.
pub(crate) struct RemoteToolExecutor {
    schemas: Vec<ToolSchema>,
    frames: crate::backlog::FrameSink,
    pending: Pending,
    counter: AtomicU64,
    timeout: Duration,
    /// This turn's operator hook plane (#1298), `None` when the deployment
    /// installed no extensions.
    ///
    /// The port owns it rather than the engine because the interception point
    /// has to be *before the request frame leaves*: a `Deny` that arrived
    /// after the host had already been asked to run the tool would be a veto
    /// of the answer, not of the act. Same placement, and same reason, as
    /// `ToolRegistry`'s gate on the CLI side.
    bus: Option<HookBus>,
}

impl RemoteToolExecutor {
    pub(crate) fn new(
        schemas: Vec<ToolSchema>,
        frames: crate::backlog::FrameSink,
        pending: Pending,
        timeout: Duration,
        bus: Option<HookBus>,
    ) -> Self {
        Self {
            schemas,
            frames,
            pending,
            counter: AtomicU64::new(0),
            timeout,
            bus,
        }
    }

    /// Run the `tool.call.requested` blocking chain before the request frame
    /// is built.
    ///
    /// `Ok(None)`: allowed unchanged. `Ok(Some(input))`: allowed with a
    /// policy-rewritten input, which is what the host is then asked to run.
    /// `Err(output)`: refused — the model sees this error instead of a result,
    /// and the host is never asked at all.
    ///
    /// The chain receives the RAW input: the blocking path is the explicitly
    /// privileged interception point (`stella_core::bus`), and a policy that
    /// cannot see the shell command it is judging cannot judge it. Observable
    /// events below carry `sanitize_tool_input`'d copies instead.
    ///
    /// Deliberately the same decision handling as the CLI's `ToolRegistry`,
    /// down to the dropped-`input` case: two surfaces answering one `Deny`
    /// differently is exactly the drift `stella-parity` exists to catch.
    fn gate_tool_call(
        bus: &HookBus,
        name: &str,
        input: &Value,
    ) -> Result<Option<Value>, ToolOutput> {
        let outcome = bus.emit_blocking(HookEventDraft::new(
            hook_names::TOOL_CALL_REQUESTED,
            serde_json::json!({ "tool": name, "input": input }),
        ));
        match outcome.decision {
            HookDecision::Deny { reason } => Err(ToolOutput::Error {
                message: format!("`{name}` was denied by an extension policy: {reason}"),
            }),
            HookDecision::RequireApproval { reason } => Err(ToolOutput::Error {
                message: format!("`{name}` requires approval before it can run: {reason}"),
            }),
            _ => {
                if !outcome.modified {
                    return Ok(None);
                }
                match outcome.event.payload.get("input") {
                    Some(new_input) => Ok(Some(new_input.clone())),
                    None => {
                        // A `modify` that dropped `input` is a broken policy
                        // handler. Surface it and keep the original rather
                        // than asking the host to run garbage.
                        bus.emit_named(
                            hook_names::EXTENSION_ERROR,
                            serde_json::json!({
                                "failed_event": hook_names::TOOL_CALL_REQUESTED,
                                "error": "modify decision dropped the `input` field; original input kept",
                            }),
                        );
                        Ok(None)
                    }
                }
            }
        }
    }

    /// Announce how the host answered, on the observable channel.
    fn report_tool_outcome(bus: &HookBus, name: &str, output: &ToolOutput, duration_ms: u64) {
        match output {
            ToolOutput::Error { message } => bus.emit_named(
                hook_names::TOOL_CALL_FAILED,
                serde_json::json!({
                    "tool": name, "error": message, "duration_ms": duration_ms,
                }),
            ),
            ToolOutput::Ok { .. } => bus.emit_named(
                hook_names::TOOL_CALL_COMPLETED,
                serde_json::json!({ "tool": name, "duration_ms": duration_ms }),
            ),
        };
    }
}

#[async_trait]
impl ToolExecutor for RemoteToolExecutor {
    fn schemas(&self) -> Vec<ToolSchema> {
        self.schemas.clone()
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        // Every exit here is a `ToolOutput::Error`: the port contract is that
        // `execute` never returns `Err`, since a tool failure is model-visible
        // data. Cancellation therefore ends the turn one step later, at the
        // provider port, which *can* report `ProviderError::Cancelled`.
        if self.pending.is_cancelled() {
            return ToolOutput::Error {
                message: "turn cancelled before the tool call could be dispatched".to_string(),
            };
        }
        let Some(bus) = &self.bus else {
            return self.dispatch(name, input).await;
        };
        // Operator policy, before anything is dispatched (#1298). A refusal
        // returns here, so a denied tool costs the host nothing: no frame is
        // sent, no reverse request is registered, and no deadline is armed —
        // and no `started` is announced for a call that never began.
        let gated = match Self::gate_tool_call(bus, name, input) {
            Ok(replacement) => replacement,
            Err(refused) => return refused,
        };
        let input = gated.as_ref().unwrap_or(input);
        bus.emit_named(
            hook_names::TOOL_CALL_STARTED,
            serde_json::json!({
                "tool": name,
                "input": bus::sanitize_tool_input(input),
            }),
        );
        // `started` and its outcome bracket ONE call with exactly one return
        // between them, which is the whole reason `dispatch` is a separate
        // function: it has six exits (cancelled registration, a dead frame
        // sink, four ways the host's answer can land or fail to), and a
        // report distributed across all of them is one added early return
        // away from leaving an observer holding a call that never closes.
        // Same argument, and the same shape, as the engine emitting
        // `agent.step.completed` in a wrapper around `run_step_inner` rather
        // than at each of its own exits.
        let started_at = Instant::now();
        let output = self.dispatch(name, input).await;
        Self::report_tool_outcome(bus, name, &output, millis(started_at.elapsed()));
        output
    }
}

impl RemoteToolExecutor {
    /// Ask the host to run one tool call and wait for its answer.
    ///
    /// Every exit is a `ToolOutput` — see [`ToolExecutor::execute`] for why —
    /// and the caller is what brackets this with the hook plane's
    /// `started`/outcome pair.
    async fn dispatch(&self, name: &str, input: &Value) -> ToolOutput {
        let request_id = format!("tool-{}", self.counter.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        // Same refused-registration handling as the provider port: a cancel
        // landing between the check above and this call must not park the step
        // until its deadline.
        if !self.pending.register_tool(request_id.clone(), tx) {
            return ToolOutput::Error {
                message: "turn cancelled before the tool call could be dispatched".to_string(),
            };
        }
        if self
            .frames
            .send(ServerFrame::ToolRequest {
                request_id: request_id.clone(),
                name: name.to_string(),
                input: input.clone(),
            })
            .is_err()
        {
            self.pending.abandon(&request_id);
            return ToolOutput::Error {
                message: "serve host disconnected before the tool call could be dispatched"
                    .to_string(),
            };
        }
        let started = dispatched(&self.pending, &request_id, ReverseKind::Tool);
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(output)) => {
                answered(&self.pending, &request_id, ReverseKind::Tool, started);
                output
            }
            Ok(Err(_)) if self.pending.is_cancelled() => ToolOutput::Error {
                message: "turn cancelled while the tool call was in flight".to_string(),
            },
            Ok(Err(_)) => ToolOutput::Error {
                message: "serve host dropped the tool call without answering".to_string(),
            },
            Err(_) => {
                self.pending.abandon(&request_id);
                timed_out(&self.pending, &request_id, ReverseKind::Tool, started);
                ToolOutput::Error {
                    message: format!(
                        "serve host did not answer the `{name}` tool call within {:?} \
                         (reverse-request deadline)",
                        self.timeout
                    ),
                }
            }
        }
    }
}

/// The `ApprovalGate` port as a reverse-RPC to the host (#1288): a
/// pipeline-driven turn's scope-review gate, remoted exactly like a tool or
/// provider call. `review` emits a [`ServerFrame::ScopeReviewRequest`] and
/// blocks the pipeline stage on the host's [`ScopeDecision`].
///
/// [`ApprovalGate::confirm`] is left at its default (`false`, i.e. "never
/// isolate in a worktree") — a served run has no
/// `CandidateWorkspacePort` wired yet ([`crate::pipeline_run`]'s module docs
/// name this as a declared follow-up), so the question this method answers
/// never has a workspace to say yes to.
pub(crate) struct RemoteApprovalGate {
    frames: crate::backlog::FrameSink,
    pending: Pending,
    counter: AtomicU64,
    timeout: Duration,
}

impl RemoteApprovalGate {
    pub(crate) fn new(
        frames: crate::backlog::FrameSink,
        pending: Pending,
        timeout: Duration,
    ) -> Self {
        Self {
            frames,
            pending,
            counter: AtomicU64::new(0),
            timeout,
        }
    }
}

#[async_trait]
impl ApprovalGate for RemoteApprovalGate {
    /// Fail-closed on every exit that is not an explicit host decision —
    /// cancellation, a disconnected host, a wedged one — matching
    /// [`stella_pipeline::ports::AlwaysAbortGate`] and the fail-closed EOF
    /// handling in [`stella_pipeline::ports::StdioApprovalGate`]: an
    /// unanswered scope review must never relocate into "run it anyway".
    async fn review(&self, proposal: &ScopeProposal) -> ScopeDecision {
        if self.pending.is_cancelled() {
            return ScopeDecision::Abort;
        }
        let request_id = format!("scope-{}", self.counter.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = oneshot::channel();
        if !self.pending.register_scope_review(request_id.clone(), tx) {
            return ScopeDecision::Abort;
        }
        if self
            .frames
            .send(ServerFrame::ScopeReviewRequest {
                request_id: request_id.clone(),
                proposal: proposal.clone(),
            })
            .is_err()
        {
            self.pending.abandon(&request_id);
            return ScopeDecision::Abort;
        }
        let started = dispatched(&self.pending, &request_id, ReverseKind::ScopeReview);
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(decision)) => {
                answered(
                    &self.pending,
                    &request_id,
                    ReverseKind::ScopeReview,
                    started,
                );
                decision
            }
            // The sender was dropped: cancellation or teardown already
            // reported itself through `Pending::clear`/`cancel`, so nothing
            // further is recorded here — same reasoning as
            // `RemoteProvider::complete_remoted`'s cancelled-sender arm.
            Ok(Err(_)) => ScopeDecision::Abort,
            Err(_) => {
                self.pending.abandon(&request_id);
                timed_out(
                    &self.pending,
                    &request_id,
                    ReverseKind::ScopeReview,
                    started,
                );
                ScopeDecision::Abort
            }
        }
    }
}

/// The typed process-outcome ports (`TestRunner`, `DiagnosticRunner`) as a
/// reverse-RPC to the host (#1288), reusing the exact `ToolRequest` /
/// `ToolResultIn` wire vocabulary every other tool call already goes
/// through — the verification ladder's process launches are not a new
/// containment decision, they are the existing bring-your-own-tools
/// boundary ([`ToolExecutor`]'s doc, and `stella-parity`'s
/// `tools.local_execution` row) applied to two more typed callers. See
/// [`crate::pipeline_run`] for why this is the answer rather than a new
/// wire primitive.
///
/// Reserved names, in a `stella.verify.*` namespace no real tool schema may
/// occupy: the host recognizes them and answers with a JSON-encoded
/// [`CmdOutcomeWire`] as the tool's `content`; a host that has not
/// implemented them answers as it would any unknown tool (an error), which
/// this type reads as [`CmdKind::Infra`] — "no toolchain for this", already
/// a first-class, gracefully-handled outcome on the typed ladder.
pub(crate) const PIPELINE_TEST_TOOL: &str = "stella.verify.run_test";
pub(crate) const PIPELINE_DIAGNOSTIC_TOOL: &str = "stella.verify.run_diagnostic";

/// Wire projection of [`CmdOutcome`]. Carried as a JSON string inside a
/// [`ToolOutput::Ok`]'s `content` — the same "structured payload inside a
/// tool's string content" shape several production tools already use — so
/// answering a verification call costs the host no new response envelope,
/// only a documented `content` shape for these two reserved names.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CmdOutcomeWire {
    Completed {
        exit_code: i32,
        #[serde(default)]
        stdout_tail: String,
        #[serde(default)]
        stderr_tail: String,
    },
    TimedOut,
    OutOfMemory,
    Infra,
}

impl From<CmdOutcomeWire> for CmdOutcome {
    fn from(wire: CmdOutcomeWire) -> Self {
        match wire {
            CmdOutcomeWire::Completed {
                exit_code,
                stdout_tail,
                stderr_tail,
            } => CmdOutcome {
                exit_code,
                stdout_tail,
                stderr_tail,
                kind: CmdKind::Completed,
            },
            CmdOutcomeWire::TimedOut => infra_outcome(CmdKind::TimedOut),
            CmdOutcomeWire::OutOfMemory => infra_outcome(CmdKind::OutOfMemory),
            CmdOutcomeWire::Infra => infra_outcome(CmdKind::Infra),
        }
    }
}

fn infra_outcome(kind: CmdKind) -> CmdOutcome {
    CmdOutcome {
        exit_code: -1,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        kind,
    }
}

/// A `CmdOutcome` reporting that the host never answered, or answered with
/// something this crate cannot read as a [`CmdOutcomeWire`] — never a
/// fabricated pass or a fabricated assertion failure, per
/// [`CmdOutcome::assertion_result`]'s contract: infrastructure noise must
/// read as unobserved, not as failing.
fn unreachable_outcome() -> CmdOutcome {
    infra_outcome(CmdKind::Infra)
}

/// Ask the host to run one reserved verification call and wait for its
/// answer, exactly like [`RemoteToolExecutor::dispatch`] — a free function
/// rather than a shared base, because the two callers ([`RemoteVerificationRunner`]'s
/// two trait methods) differ only in the tool name and the input they send.
async fn dispatch_verification_call(
    frames: &crate::backlog::FrameSink,
    pending: &Pending,
    timeout: Duration,
    name: &str,
    input: Value,
) -> CmdOutcome {
    if pending.is_cancelled() {
        return unreachable_outcome();
    }
    let request_id = format!("verify-{}", next_verify_call_id());
    let (tx, rx) = oneshot::channel();
    if !pending.register_tool(request_id.clone(), tx) {
        return unreachable_outcome();
    }
    if frames
        .send(ServerFrame::ToolRequest {
            request_id: request_id.clone(),
            name: name.to_string(),
            input,
        })
        .is_err()
    {
        pending.abandon(&request_id);
        return unreachable_outcome();
    }
    let started = dispatched(pending, &request_id, ReverseKind::Tool);
    let output = match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(output)) => {
            answered(pending, &request_id, ReverseKind::Tool, started);
            output
        }
        Ok(Err(_)) => return unreachable_outcome(),
        Err(_) => {
            pending.abandon(&request_id);
            timed_out(pending, &request_id, ReverseKind::Tool, started);
            return unreachable_outcome();
        }
    };
    match output {
        ToolOutput::Ok { content } => serde_json::from_str::<CmdOutcomeWire>(&content)
            .map(CmdOutcome::from)
            .unwrap_or_else(|_| unreachable_outcome()),
        ToolOutput::Error { .. } => unreachable_outcome(),
    }
}

/// A per-request-id counter for [`dispatch_verification_call`]. Process-wide
/// rather than per-runner: [`RemoteVerificationRunner`] answers both
/// `TestRunner` and `DiagnosticRunner` from one shared `&self`, so a
/// per-instance counter would need its own synchronization anyway, and a
/// pipeline run mints a fresh runner per turn — the same tradeoff
/// [`PROVIDER_INSTANCES`] makes for [`RemoteProvider`].
static VERIFY_CALL_INSTANCES: AtomicU64 = AtomicU64::new(0);

fn next_verify_call_id() -> u64 {
    VERIFY_CALL_INSTANCES.fetch_add(1, Ordering::Relaxed)
}

/// Both typed process-launching ports the verification ladder needs
/// (#1288), sharing one reverse-RPC dispatch. See [`PIPELINE_TEST_TOOL`] /
/// [`PIPELINE_DIAGNOSTIC_TOOL`] for the wire contract.
pub(crate) struct RemoteVerificationRunner {
    frames: crate::backlog::FrameSink,
    pending: Pending,
    timeout: Duration,
}

impl RemoteVerificationRunner {
    pub(crate) fn new(
        frames: crate::backlog::FrameSink,
        pending: Pending,
        timeout: Duration,
    ) -> Self {
        Self {
            frames,
            pending,
            timeout,
        }
    }
}

#[async_trait]
impl TestRunner for RemoteVerificationRunner {
    async fn run_test(&self, invocation: &TestInvocation) -> CmdOutcome {
        dispatch_verification_call(
            &self.frames,
            &self.pending,
            self.timeout,
            PIPELINE_TEST_TOOL,
            serde_json::json!({
                "program": invocation.program,
                "args": invocation.args,
            }),
        )
        .await
    }
}

#[async_trait]
impl DiagnosticRunner for RemoteVerificationRunner {
    async fn run_diagnostic(&self, invocation: &DiagnosticInvocation) -> CmdOutcome {
        let input = match invocation {
            DiagnosticInvocation::GitDiff => serde_json::json!({ "invocation": "git_diff" }),
            DiagnosticInvocation::UntrackedNumstat { path } => serde_json::json!({
                "invocation": "untracked_numstat",
                "path": path,
            }),
        };
        dispatch_verification_call(
            &self.frames,
            &self.pending,
            self.timeout,
            PIPELINE_DIAGNOSTIC_TOOL,
            input,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use stella_core::bus::{HookBus, HookEvent, names as hook_names};
    use stella_core::ports::ToolExecutor;
    use stella_pipeline::ports::{
        ApprovalGate, CmdKind, DiagnosticInvocation, DiagnosticRunner, ScopeDecision,
        TestInvocation, TestRunner,
    };
    use stella_protocol::{ScopeProposal, ToolOutput};

    use super::{
        CmdOutcomeWire, PIPELINE_DIAGNOSTIC_TOOL, PIPELINE_TEST_TOOL, RemoteApprovalGate,
        RemoteToolExecutor, RemoteVerificationRunner,
    };
    use crate::frame::ServerFrame;
    use crate::observe::event::TurnRef;
    use crate::pending::Pending;

    /// A tool port whose frame sink is already dead — the host-disconnected
    /// path — plus a bus recording every event it sees.
    fn disconnected_port() -> (RemoteToolExecutor, Arc<Mutex<Vec<String>>>) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        // Dropping the receiver is what makes `send` fail, which is exactly
        // what a subscriber's connection going away does in production.
        drop(rx);
        let sink = crate::backlog::FrameSink::new(
            tx,
            crate::backlog::FrameBacklog::new(crate::backlog::DEFAULT_MAX_QUEUED_FRAMES),
        );
        let pending = Pending::new(
            crate::observe::null_observer(),
            TurnRef::new("turn-remotetest"),
        );
        let bus = HookBus::new("turn-remotetest");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        bus.on("tool.call.*", move |event: &HookEvent| {
            recorder
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(event.name.clone());
            Ok(())
        })
        .detach();
        let port = RemoteToolExecutor::new(
            Vec::new(),
            sink,
            pending,
            std::time::Duration::from_millis(50),
            Some(bus),
        );
        (port, seen)
    }

    /// A `tool.call.started` with no outcome after it leaves an observer
    /// holding a call that never closes — and it would do so on precisely the
    /// paths worth watching, the ones where the host went away. The bracket is
    /// structural (`execute` wraps `dispatch`), and this is what keeps it so.
    #[tokio::test]
    async fn a_tool_call_the_host_never_receives_still_reports_an_outcome() {
        let (port, seen) = disconnected_port();
        let output = port
            .execute("echo", &serde_json::json!({ "text": "hi" }))
            .await;
        assert!(
            matches!(&output, ToolOutput::Error { message } if message.contains("disconnected")),
            "expected the host-disconnected error, got {output:?}"
        );
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [
                hook_names::TOOL_CALL_STARTED.to_string(),
                hook_names::TOOL_CALL_FAILED.to_string(),
            ],
            "every `started` must be closed by an outcome, however the call ended"
        );
    }

    /// The same bracket, on the path the host *does* answer badly: a deadline
    /// nobody meets. Distinct from the case above because it exits from the
    /// other end of `dispatch`.
    #[tokio::test]
    async fn a_tool_call_the_host_never_answers_still_reports_an_outcome() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = crate::backlog::FrameSink::new(
            tx,
            crate::backlog::FrameBacklog::new(crate::backlog::DEFAULT_MAX_QUEUED_FRAMES),
        );
        let pending = Pending::new(
            crate::observe::null_observer(),
            TurnRef::new("turn-remotetest"),
        );
        let bus = HookBus::new("turn-remotetest");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        bus.on("tool.call.*", move |event: &HookEvent| {
            recorder
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(event.name.clone());
            Ok(())
        })
        .detach();
        let port = RemoteToolExecutor::new(
            Vec::new(),
            sink,
            pending,
            std::time::Duration::from_millis(20),
            Some(bus),
        );
        let output = port.execute("echo", &serde_json::json!({})).await;
        assert!(
            matches!(&output, ToolOutput::Error { message } if message.contains("deadline")),
            "expected the reverse-request deadline error, got {output:?}"
        );
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            [
                hook_names::TOOL_CALL_STARTED.to_string(),
                hook_names::TOOL_CALL_FAILED.to_string(),
            ],
            "a wedged host is exactly when an observer must not be left waiting"
        );
    }

    /// A fresh, connected reverse-RPC channel triple: the pending registry, a
    /// frame receiver a test can drain, and the sink both new ports
    /// (#1288) are constructed over.
    fn live_channel() -> (
        crate::backlog::FrameSink,
        tokio::sync::mpsc::UnboundedReceiver<ServerFrame>,
        Pending,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = crate::backlog::FrameSink::new(
            tx,
            crate::backlog::FrameBacklog::new(crate::backlog::DEFAULT_MAX_QUEUED_FRAMES),
        );
        let pending = Pending::new(
            crate::observe::null_observer(),
            TurnRef::new("turn-remotetest"),
        );
        (sink, rx, pending)
    }

    /// A scope review the host approves round-trips into
    /// [`ScopeDecision::Approve`] — the pipeline's `ApprovalGate::review`,
    /// remoted (#1288).
    #[tokio::test]
    async fn a_host_answered_scope_review_reaches_the_pipeline_as_the_hosts_decision() {
        let (sink, mut rx, pending) = live_channel();
        let gate = RemoteApprovalGate::new(sink, pending.clone(), Duration::from_secs(5));
        let proposal = ScopeProposal {
            summary: "do the thing".to_string(),
            steps: vec!["step one".to_string()],
            estimated_files: 1,
            estimated_cost_usd: None,
            ..Default::default()
        };

        let review = tokio::spawn(async move { gate.review(&proposal).await });

        let ServerFrame::ScopeReviewRequest { request_id, .. } = rx.recv().await.unwrap() else {
            panic!("expected a scope review request frame");
        };
        assert!(
            pending
                .resolve_scope_review(&request_id, ScopeDecision::Approve)
                .is_ok()
        );
        assert_eq!(review.await.unwrap(), ScopeDecision::Approve);
    }

    /// A scope review the host never answers fails closed
    /// ([`ScopeDecision::Abort`]) rather than defaulting to running the plan
    /// unattended — the same fail-closed contract
    /// [`stella_pipeline::ports::StdioApprovalGate`] gives a read error.
    #[tokio::test]
    async fn an_unanswered_scope_review_fails_closed() {
        let (sink, _rx, pending) = live_channel();
        let gate = RemoteApprovalGate::new(sink, pending, Duration::from_millis(20));
        let proposal = ScopeProposal {
            summary: "do the thing".to_string(),
            steps: vec!["step one".to_string()],
            estimated_files: 1,
            estimated_cost_usd: None,
            ..Default::default()
        };
        assert_eq!(gate.review(&proposal).await, ScopeDecision::Abort);
    }

    /// A well-formed [`CmdOutcomeWire`] the host answers with round-trips into
    /// the matching [`CmdOutcome`], for both the test-runner and the
    /// diagnostic-runner traits [`RemoteVerificationRunner`] answers (#1288).
    #[tokio::test]
    async fn a_hosts_cmd_outcome_reaches_the_ladder_as_the_matching_typed_result() {
        let (sink, mut rx, pending) = live_channel();
        let runner = RemoteVerificationRunner::new(sink, pending.clone(), Duration::from_secs(5));

        let call = tokio::spawn(async move {
            runner
                .run_test(&TestInvocation {
                    program: "cargo".to_string(),
                    args: vec!["test".to_string()],
                })
                .await
        });

        let ServerFrame::ToolRequest {
            request_id,
            name,
            input,
        } = rx.recv().await.unwrap()
        else {
            panic!("expected a tool request frame");
        };
        assert_eq!(name, PIPELINE_TEST_TOOL);
        assert_eq!(input["program"], "cargo");

        let content = serde_json::to_string(&CmdOutcomeWire::Completed {
            exit_code: 1,
            stdout_tail: "FAILED".to_string(),
            stderr_tail: String::new(),
        })
        .unwrap();
        assert!(
            pending
                .resolve_tool(&request_id, ToolOutput::Ok { content })
                .is_ok()
        );

        let outcome = call.await.unwrap();
        assert_eq!(outcome.kind, CmdKind::Completed);
        assert_eq!(outcome.exit_code, 1);
        assert_eq!(outcome.stdout_tail, "FAILED");
        assert_eq!(outcome.assertion_result(), Some(false));
    }

    /// A host that has not implemented the reserved verification tool names
    /// answers as it would any unknown tool — an error — and the ladder must
    /// read that as "no toolchain" ([`CmdKind::Infra`]), never as a failing
    /// assertion: an infra outcome and a genuine test failure must not be
    /// confused, per [`CmdOutcome::assertion_result`]'s own contract.
    #[tokio::test]
    async fn a_host_that_does_not_implement_verification_degrades_to_infra_not_a_failure() {
        let (sink, mut rx, pending) = live_channel();
        let runner = RemoteVerificationRunner::new(sink, pending.clone(), Duration::from_secs(5));

        let call =
            tokio::spawn(
                async move { runner.run_diagnostic(&DiagnosticInvocation::GitDiff).await },
            );

        let ServerFrame::ToolRequest {
            request_id, name, ..
        } = rx.recv().await.unwrap()
        else {
            panic!("expected a tool request frame");
        };
        assert_eq!(name, PIPELINE_DIAGNOSTIC_TOOL);
        assert!(
            pending
                .resolve_tool(
                    &request_id,
                    ToolOutput::Error {
                        message: "unknown tool".to_string(),
                    },
                )
                .is_ok()
        );

        let outcome = call.await.unwrap();
        assert_eq!(outcome.kind, CmdKind::Infra);
        assert_eq!(
            outcome.assertion_result(),
            None,
            "an unimplemented verification call observed no assertion either way"
        );
    }
}
