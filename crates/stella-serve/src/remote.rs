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
use serde_json::Value;
use stella_core::bus::{self, HookBus, HookEventDraft, names as hook_names};
use stella_core::hooks::decision::{GateVerdict, OperatorPosture, resolve_precedence};
use stella_core::ports::authz::authz_verdict;
use stella_core::ports::{AuthzGate, DispatchAdmission, DispatchGate, Principal, ToolExecutor};
use stella_core::retry::Sleeper;
use stella_protocol::{
    CompletionRequestRef, CompletionResult, Provider, ProviderError, ToolCallObserver, ToolOutput,
    ToolSchema,
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

/// Mints [`RemoteToolExecutor`] instance tags, for the same reason and on the
/// same terms as [`PROVIDER_INSTANCES`] (#1496).
///
/// The tool port needs this exactly as much as the provider port does, and for
/// longer than anyone noticed: `run_session` builds two executors over one
/// [`Pending`] registry — the parent's and the sub-agent view's — so two
/// zero-based counters both mint `tool-0`. `Pending::register_tool` inserts
/// through `HashMap::insert`, which *replaces* on collision, so the displaced
/// oneshot sender drops, its waiter wakes with "serve host dropped the tool
/// call without answering", and the host's single answer for `tool-0` resolves
/// whichever request happened to register last. A sub-agent could therefore be
/// handed the parent's tool result.
static TOOL_INSTANCES: AtomicU64 = AtomicU64::new(0);

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
    /// frame so a host can route a verifier or a sub-agent to a different model
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

    /// The same remoted provider, announcing a different role — the verifier of
    /// a goal run, or a sub-agent's own calls (#1297).
    ///
    /// A separate instance rather than a setter: the role is stamped on every
    /// frame this provider emits, and two agents share one turn's frame sink,
    /// so a mutable role would be a race between the worker's next call and
    /// the verifier's.
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
    /// The host-declared contracts (#3286): schema plus governance metadata.
    /// The advertised schemas derive from these; the gate below reasons over
    /// them.
    contracts: Vec<stella_protocol::ToolContract>,
    /// The turn's authorization gate, applied before a request frame leaves
    /// — same seam, same position as the CLI's `GatedToolSet`.
    gate: std::sync::Arc<dyn AuthzGate>,
    /// Who the calls are made as — a host-supplied opaque identity the
    /// engine never interprets (invariant #1).
    principal: Principal,
    /// Disambiguates this executor's request ids from every other executor
    /// sharing the turn's [`Pending`] registry (#1496) — the same guarantee,
    /// and the same mechanism, as [`RemoteProvider::instance`].
    instance: u64,
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
        contracts: Vec<stella_protocol::ToolContract>,
        gate: std::sync::Arc<dyn AuthzGate>,
        principal: Principal,
        frames: crate::backlog::FrameSink,
        pending: Pending,
        timeout: Duration,
        bus: Option<HookBus>,
    ) -> Self {
        Self {
            contracts,
            gate,
            principal,
            instance: TOOL_INSTANCES.fetch_add(1, Ordering::Relaxed),
            frames,
            pending,
            counter: AtomicU64::new(0),
            timeout,
            bus,
        }
    }

    /// The contract this executor authorizes `name` against. A name outside
    /// the declared set resolves to an untrusted `High` contract rather than
    /// skipping authorization — the same fail-closed reading as the CLI's
    /// `contracts::unknown_contract`, built locally because this crate
    /// deliberately does not depend on `stella-tools`.
    fn contract_for(&self, name: &str) -> stella_protocol::ToolContract {
        self.contracts
            .iter()
            .find(|contract| contract.name() == name)
            .cloned()
            .unwrap_or_else(|| {
                stella_protocol::ToolContract::declared(ToolSchema {
                    name: name.to_string(),
                    description: String::new(),
                    input_schema: serde_json::json!({}),
                    read_only: false,
                    speculation_safe: false,
                })
            })
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
    /// The decision fold is the CLI's `ToolRegistry`'s, called rather than
    /// restated: `resolve_precedence` over one `HookDecision`, refusals
    /// classified [`stella_protocol::ErrorClass::RefusedByPolicy`]. Two
    /// surfaces answering one `Deny` differently is exactly the drift
    /// `stella-parity` exists to catch, and this file had already drifted
    /// into an unclassified error on both refusal arms (#3843).
    fn gate_tool_call(
        bus: &HookBus,
        name: &str,
        input: &Value,
    ) -> Result<Option<Value>, ToolOutput> {
        let outcome = bus.emit_blocking(HookEventDraft::new(
            hook_names::TOOL_CALL_REQUESTED,
            serde_json::json!({ "tool": name, "input": input }),
        ));
        match resolve_precedence(&OperatorPosture::NoOpinion, Ok(&outcome.decision), false) {
            GateVerdict::Deny { reason } => Err(ToolOutput::classified_error(
                stella_protocol::ErrorClass::RefusedByPolicy,
                format!("`{name}` was denied by an extension policy: {reason}"),
            )),
            // A served turn has no human to park on, so the structured
            // refusal is the honest answer — the same posture, the same
            // class and the same sentence the authorization gate's own
            // approval arm takes below. Routing this to a host-side approval
            // exchange is #3288, and it is the one thing that would let a
            // served session ask; inventing a local responder here would ask
            // nobody and answer anyway.
            GateVerdict::RequireApproval { reason } => Err(ToolOutput::classified_error(
                stella_protocol::ErrorClass::RefusedByPolicy,
                format!("`{name}` requires approval before it can run: {reason}"),
            )),
            GateVerdict::Allow => {
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
            ToolOutput::Error { message, .. } => bus.emit_named(
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
        self.contracts
            .iter()
            .map(|contract| contract.schema.clone())
            .collect()
    }

    /// The host-declared contracts, verbatim (#3286) — what lets an outer
    /// layer (or a parity witness) see the same governance metadata the gate
    /// reasons over.
    fn contracts(&self) -> Vec<stella_protocol::ToolContract> {
        self.contracts.clone()
    }

    // `live_services` keeps the empty default, declared rather than
    // forgotten (#2764). This executor owns no process table: every call is
    // remoted, so any long-running child belongs to the host, and there is no
    // reverse request that asks about one. The end-of-turn assertion is
    // therefore silent on a served run — the host, which spawned the child
    // and holds the only handle to it, is the surface that can answer. Adding
    // an `engine.tools.live_services` reverse request is the way in if a host
    // ever wants Stella asking; it would be a wire-contract change, not a
    // forward, and it is #2818.

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        // Every exit here is a `ToolOutput::Error`: the port contract is that
        // `execute` never returns `Err`, since a tool failure is model-visible
        // data. Cancellation therefore ends the turn one step later, at the
        // provider port, which *can* report `ProviderError::Cancelled`.
        if self.pending.is_cancelled() {
            return ToolOutput::error(
                "turn cancelled before the tool call could be dispatched".to_string(),
            );
        }
        // The authorization gate, before anything else (#3286) — the same
        // seam, at the same position, as the CLI's `GatedToolSet`: a denied
        // call costs the host nothing, no frame is ever built, and the
        // verdict folds through the one shared ladder so an `Err` from the
        // gate denies whatever any softening flag says.
        let evaluation = self
            .gate
            .check_traced(&self.contract_for(name), &self.principal, input);
        // The rule-by-rule account (#3362), journaled before the fold
        // consumes the evaluation — same builder, same event name, same
        // payload shape as the CLI's `GatedToolSet`, so a host reading the
        // policy plane sees one vocabulary across both surfaces. Telemetry:
        // a session with no bus journals nothing and the call is unaffected.
        if let Some(bus) = &self.bus {
            bus.emit_named(
                hook_names::POLICY_EVALUATED,
                stella_core::ports::authz::evaluation_journal_payload(
                    name,
                    &self.principal,
                    self.gate.name(),
                    &evaluation,
                ),
            );
        }
        let evaluation = evaluation.map(|evaluation| evaluation.decision);
        match authz_verdict(&OperatorPosture::NoOpinion, evaluation, false) {
            GateVerdict::Allow => {}
            GateVerdict::Deny { reason } => {
                return ToolOutput::classified_error(
                    stella_protocol::ErrorClass::RefusedByPolicy,
                    reason,
                );
            }
            // A served turn has no human to park on: the structured refusal
            // is the honest answer, exactly as the CLI's headless
            // `ApprovalBroker` refuses. Routing this through a host-side
            // approval exchange is #3288's territory, not silently allowed
            // here.
            GateVerdict::RequireApproval { reason } => {
                return ToolOutput::classified_error(
                    stella_protocol::ErrorClass::RefusedByPolicy,
                    format!("`{name}` requires approval before it can run: {reason}"),
                );
            }
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

    /// This executor owns the served session's blocking policy chain, so it
    /// is the surface's one [`DispatchGate`] (#2793, #3843) — the accessor a
    /// decorator that dispatches a name of its own reaches it through. Serve
    /// has exactly one such decorator, [`crate::subagents::DelegatingTools`],
    /// and its engine-side `delegate` reached the model with no extension
    /// policy consulted until it did.
    fn dispatch_gate(&self) -> Option<&dyn DispatchGate> {
        Some(self)
    }
}

/// The served surface's one dispatch gate: the `tool.call.requested` chain
/// and the fold over it, offered through the port rather than copied by every
/// decorator that needs it.
///
/// Deliberately **not** the authorization gate as well. [`AuthzGate`] answers
/// "may this principal call this tool at all" from a contract and runs inside
/// [`ToolExecutor::execute`] above; this answers "what did this deployment's
/// extension policy decide about this exact call". Folding both in here would
/// run the authorization ladder twice for every remoted call, and the CLI's
/// own `impl DispatchGate for ToolRegistry` — the reference this one keeps
/// parity with — draws the line in the same place.
#[async_trait]
impl DispatchGate for RemoteToolExecutor {
    async fn admit(&self, name: &str, input: &Value) -> DispatchAdmission {
        // No bus, no chains: the same "policy plane absent" case
        // `execute` has always had, and the same answer.
        let Some(bus) = &self.bus else {
            return DispatchAdmission::Admit;
        };
        match Self::gate_tool_call(bus, name, input) {
            Ok(None) => DispatchAdmission::Admit,
            Ok(Some(amended)) => DispatchAdmission::AmendedInput(amended),
            Err(refusal) => DispatchAdmission::Refuse(refusal),
        }
    }
}

impl RemoteToolExecutor {
    /// Ask the host to run one tool call and wait for its answer.
    ///
    /// Every exit is a `ToolOutput` — see [`ToolExecutor::execute`] for why —
    /// and the caller is what brackets this with the hook plane's
    /// `started`/outcome pair.
    async fn dispatch(&self, name: &str, input: &Value) -> ToolOutput {
        let request_id = format!(
            "tool-{}-{}",
            self.instance,
            self.counter.fetch_add(1, Ordering::Relaxed)
        );
        let (tx, rx) = oneshot::channel();
        // Same refused-registration handling as the provider port: a cancel
        // landing between the check above and this call must not park the step
        // until its deadline.
        if !self.pending.register_tool(request_id.clone(), tx) {
            return ToolOutput::error(
                "turn cancelled before the tool call could be dispatched".to_string(),
            );
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
            return ToolOutput::error(
                "serve host disconnected before the tool call could be dispatched".to_string(),
            );
        }
        let started = dispatched(&self.pending, &request_id, ReverseKind::Tool);
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(output)) => {
                answered(&self.pending, &request_id, ReverseKind::Tool, started);
                output
            }
            Ok(Err(_)) if self.pending.is_cancelled() => {
                ToolOutput::error("turn cancelled while the tool call was in flight".to_string())
            }
            Ok(Err(_)) => {
                ToolOutput::error("serve host dropped the tool call without answering".to_string())
            }
            Err(_) => {
                self.pending.abandon(&request_id);
                timed_out(&self.pending, &request_id, ReverseKind::Tool, started);
                ToolOutput::error(format!(
                    "serve host did not answer the `{name}` tool call within {:?} \
                         (reverse-request deadline)",
                    self.timeout
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests;
