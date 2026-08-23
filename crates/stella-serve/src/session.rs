//! A single engine session: one turn driven on a dedicated OS thread.
//!
//! The engine's turn future is deliberately `!Send` (it holds provider futures
//! and the retry-jitter RNG across awaits — `stella-cli::fleet_cmd`), so it
//! cannot be `tokio::spawn`ed onto the server's multi-thread runtime. Instead
//! each session gets its own OS thread running a **current-thread** runtime that
//! `block_on`s `stella_engine::Engine::drive` over a `TurnState` this crate
//! owns; the server side communicates with it purely through
//! `Send` channels — an outbound [`ServerFrame`] stream and the shared
//! [`Pending`] one-shot registry. This is the fleet's bridge, reused for a
//! long-lived server instead of a batch worker.
//!
//! Scope note: one [`Session`] drives one turn. Multi-turn sessions (retaining
//! the message history across turns) layer on top of this without changing the
//! transport; per-step checkpointing is the engine loop's (see [`drive_turn`])
//! through the `CheckpointSink` on `EngineConfig`, so a served turn interrupted
//! mid-flight resumes from its last step boundary rather than being re-run
//! from the prompt. That sink comes from one of two places:
//! [`SessionSpec::checkpoint`], which names a key in the server's
//! [`crate::checkpoint::CheckpointStore`], or an embedder setting
//! `config.checkpoint_sink` itself.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use stella_core::EngineConfig;
use stella_core::event_sender::RunEnding;
use stella_core::ports::{AuthzGate, NoAuthz, Principal};
use stella_engine::{BudgetGuard, CancelToken, Engine, EventSender, TurnOutcome};
use stella_protocol::{
    AgentEvent, CompletionMessage, CompletionResult, ProviderError, ToolContract, ToolOutput,
};
use tokio::runtime::Builder;
use tokio::sync::mpsc;

use crate::controls::{ControlPorts, Controls};
use crate::error::ServeError;
use crate::frame::{ServerFrame, TurnOutcomeWire};
use crate::history::{Encoded, FrameHistory, encode_seq_frame};
use crate::observe::SharedObserver;
use crate::observe::event::{ServeEvent, SettledOutcome, TurnRef, TurnTally, millis};
use crate::observe::tally::TallyFold;
use crate::pending::Pending;
use crate::remote::{
    DEFAULT_REVERSE_REQUEST_TIMEOUT, RemoteProvider, RemoteToolExecutor, TokioSleeper,
};

/// Everything needed to run one turn. The host assembles this — it owns prompt
/// construction, recall, model selection, and the tool set (advertised as
/// schemas here; execution is remoted). The engine only orchestrates.
pub struct SessionSpec {
    /// Stable provider id echoed on `Provider::id()` (telemetry/labeling only —
    /// the model call itself is remoted to the host).
    pub provider_id: String,
    /// The full contracts for the tools the model may call (#3286): the
    /// advertised schema plus the governance metadata (risk, provenance,
    /// approval, claims) an authorization plane reasons over — so a host no
    /// longer needs a name → capability side-table. The route layer accepts
    /// a bare schema from older hosts and upgrades it to
    /// [`ToolContract::declared`], the fail-closed reading.
    pub tools: Vec<ToolContract>,
    /// Who this turn's tool calls are made *as*, as far as [`Self::gate`] is
    /// concerned. A host supplies its own opaque identity
    /// ([`Principal::Host`]); the engine deliberately does not interpret it
    /// (invariant #1).
    pub principal: Principal,
    /// The authorization gate every remoted tool call passes before its
    /// request frame leaves (#3286) — the same seam, at the same position,
    /// as the CLI's `GatedToolSet`. [`NoAuthz`] **by name** when the host
    /// configures none: a decision somebody typed, never a nullable slot
    /// defaulting open.
    pub gate: Arc<dyn AuthzGate>,
    /// The already-assembled conversation to run this turn.
    pub messages: Vec<CompletionMessage>,
    /// Engine knobs (step cap, compaction budget, sampling). `Default` is a
    /// sane starting point.
    ///
    /// One field here is session-scoped rather than turn-scoped and so is not
    /// served by `Default` on a multi-turn embedder:
    /// `EngineConfig::session_output_ceilings`, the carry that stops every
    /// turn re-paying the affordability 402 it already learned about (#3307).
    /// The `/v1/sessions` route fills it from the session registry (#3568); a
    /// host driving [`Session`] directly across several turns attaches one
    /// handle of its own and clones it into each turn's config. A handle
    /// minted per turn is worse than none — it reads as wired and carries
    /// nothing.
    pub config: EngineConfig,
    /// Spend guard for the turn.
    pub budget: BudgetGuard,
    /// How long each reverse request (model call, tool call) waits for the host
    /// before the port gives up on it — the bound that stops an unanswered
    /// reverse request from parking an OS thread forever. Defaults to
    /// [`SessionSpec::DEFAULT_REVERSE_REQUEST_TIMEOUT`].
    pub reverse_request_timeout: Duration,
    /// This turn's truncated id, carried into every record the session emits.
    ///
    /// Supplied by the caller rather than derived here because only the turn
    /// registry can mint an id, and it does so under the lock that admits the
    /// turn — see `ServerState::register_turn`.
    pub turn: TurnRef,
    /// Where this session's boundary events go. `observe::null_observer()` for
    /// callers using [`Session`] as a library type without a sink.
    pub observer: SharedObserver,
    /// Called exactly once when the turn settles, with the final transcript
    /// and spend — before the terminal frame is emitted, so a host that sees
    /// `turn_complete` and immediately reads its session observes the
    /// write-back. `None` for a stateless turn (the pre-#931 behavior: the
    /// mutated transcript is simply dropped).
    ///
    /// If the hook is never *called* — the session thread could not be spawned
    /// — it is still *dropped*, and the owner's drop-side bookkeeping runs; a
    /// session-owned turn uses that to release its live-turn slot rather than
    /// wedge the session forever (see `crate::sessions`).
    pub on_settled: Option<SettleHook>,
    /// Where this turn's resume point is written, and under what key
    /// (`crate::checkpoint`).
    ///
    /// This is the durable identity whose absence is why a served turn used to
    /// die with the process: `drive_turn` has always reached both write
    /// seams, but with nothing to key a store on there was nowhere for the
    /// bytes to go and nowhere to read them back from.
    ///
    /// `None` is the pre-#1198 behavior and the default — `config
    /// .checkpoint_sink` stays unset, so `Engine::persist_checkpoint` returns
    /// before serializing anything and a non-durable turn pays nothing per
    /// step.
    ///
    /// Ignored when `config.checkpoint_sink` is already set: an embedder that
    /// named a sink outright has said exactly where its checkpoints go, and
    /// this field is the server's convenience, not an override of it.
    pub checkpoint: Option<crate::checkpoint::TurnCheckpoint>,
    /// Run this turn as a judged multi-round goal run rather than a single
    /// turn (#1297). `None` — the default — is the pre-#1297 behavior
    /// exactly.
    ///
    /// A mode on a turn rather than its own resource because everything a
    /// goal run needs from the transport is already here and already
    /// witnessed: the SSE stream carries its rounds, cancel stops it at a step
    /// boundary, and the settlement hook writes its transcript back. See
    /// [`GoalRun`](crate::GoalRun)'s own module for the full argument.
    pub goal: Option<crate::goal::GoalRun>,
    /// What this turn's sub-agents may do (#1297), after the operator's
    /// [`crate::SubAgentPolicy`] has clamped the caller's request. `None`
    /// advertises no `delegate` tool at all, so the model never learns children
    /// exist — the default, and the whole of the pre-#1297 behavior.
    pub sub_agents: Option<crate::subagents::EffectiveSubAgents>,
    /// The operator's hook plane for this turn (#1298), installed on a
    /// freshly-minted `HookBus` before the first step runs.
    ///
    /// Empty — the default, and what the shipping binary uses — mints no bus
    /// at all, so every engine emit site stays behind its `if let Some(bus)`
    /// and a turn pays nothing for hooks nobody registered.
    ///
    /// **Operator-supplied only.** Nothing on the wire reaches this field;
    /// see `crate::extensions` for why that boundary is where it is.
    pub extensions: crate::extensions::Extensions,
    /// Where this turn's `(estimated, actual)` token pairs are recorded, and
    /// the correction it estimates under (#1298).
    ///
    /// Shared with the server's registry rather than owned per turn, because
    /// calibration is only useful *across* turns: a map minted here would be
    /// discarded before it cleared its three-sample floor. `None` is the
    /// pre-#1298 behavior — the turn estimates uncorrected and contributes no
    /// samples.
    pub calibration: Option<Arc<stella_core::estimator::CalibrationMap>>,
}

/// The settlement callback a session owner installs on a turn — see
/// [`SessionSpec::on_settled`].
pub type SettleHook = Box<dyn FnOnce(TurnSettlement) + Send>;

/// What a settled turn hands back to its owner: the transcript as the engine
/// left it, and the budget guard carrying the turn's true spend.
pub struct TurnSettlement {
    /// `true` for [`TurnOutcomeWire::Completed`]. An aborted turn still
    /// settles — its spend is real even though its transcript may not be
    /// worth keeping (a hard cancel can leave a dangling `tool_use` that
    /// would poison the next model call).
    pub completed: bool,
    /// The conversation as the engine left it, compaction rewrites included.
    pub messages: Vec<CompletionMessage>,
    /// The guard the turn ran under; `session_spent_usd` is the running total.
    pub budget: BudgetGuard,
}

impl SessionSpec {
    /// The default [`SessionSpec::reverse_request_timeout`]: five minutes. See
    /// the constant's own docs in `remote.rs` for how that number was picked.
    pub const DEFAULT_REVERSE_REQUEST_TIMEOUT: Duration = DEFAULT_REVERSE_REQUEST_TIMEOUT;

    /// The [`SessionSpec::gate`] a served turn runs under when the embedding
    /// host configures none: [`NoAuthz`], **chosen by name** — "this
    /// deployment does not authorize tool calls" is a decision a reviewer
    /// can see, never the absence of a value (#2716). The CLI's
    /// `agent::tool_stack::session_gate` is the same one function on the
    /// other surface — and since #3482 it returns an installed plugin's
    /// accepted grant when the workspace has one. This surface does not,
    /// because a served session has no workspace of its own to read a roster
    /// from: the embedding host supplies the gate, and a host that installs
    /// plugins supplies one that knows about them.
    #[must_use]
    pub fn default_gate() -> Arc<dyn AuthzGate> {
        Arc::new(NoAuthz)
    }
}

/// A running session. The host drives it by reading [`ServerFrame`]s with
/// [`Session::next_frame`] and answering reverse-RPC requests with
/// [`Session::resolve_tool`] / [`Session::resolve_provider`].
pub struct Session {
    frames: mpsc::UnboundedReceiver<ServerFrame>,
    /// This turn's queue bound, shared with the sink that fills it (#1266).
    /// Receiving is what returns capacity, so the receiver owns this half.
    backlog: Arc<crate::backlog::FrameBacklog>,
    pending: Pending,
    /// The sender half of this turn's mid-turn controls (pause/steer). Shared
    /// with the registry entry the same way [`Pending`] is, and for the same
    /// reason: the session object itself is exclusively owned by whichever
    /// stream is running, so control POSTs need a handle that is not it.
    controls: Controls,
    /// This turn's step-boundary stop signal (#1129), shared with the registry
    /// entry exactly like [`Pending`] and [`Controls`] and for the same
    /// reason: a cancel POST cannot reach the session object, which whichever
    /// `/events` stream is running has checked out.
    cancel: CancelToken,
    thread: Option<JoinHandle<()>>,
    /// This turn's sequenced, bounded frame tail.
    ///
    /// Shared with the registry entry (`Arc`) rather than owned, because the
    /// point of retention is to outlive *this* handle: a subscriber whose
    /// connection drops takes the session with it, and the reconnect has to
    /// find the history still there to replay from.
    history: Arc<FrameHistory>,
}

impl Session {
    /// Spawn the session thread and begin the turn. Frames start arriving
    /// immediately; the terminal one is [`ServerFrame::TurnComplete`].
    ///
    /// Infallible by construction: if the thread cannot be created the returned
    /// session already holds a `TurnComplete { Aborted }` frame, so the caller
    /// drives one code path either way.
    pub fn start(spec: SessionSpec) -> Session {
        let (raw_frame_tx, frame_rx) = mpsc::unbounded_channel::<ServerFrame>();
        // The queue's bound (#1266). Held by both halves: the sink refuses
        // droppable frames above the cap, and the receive funnel below gives
        // the capacity back as frames are delivered.
        let backlog = crate::backlog::FrameBacklog::new(crate::backlog::DEFAULT_MAX_QUEUED_FRAMES);
        let frame_tx = crate::backlog::FrameSink::new(raw_frame_tx, Arc::clone(&backlog));
        let pending = Pending::new(spec.observer.clone(), spec.turn.clone());
        // The controls' *port* half carries the frame sender, so a hold can
        // announce itself on the stream; the registry-held `Controls` must not
        // (a sender parked there would never let the frame channel close — see
        // `crate::controls`).
        let (controls, control_ports) = Controls::new(frame_tx.clone());
        let cancel = CancelToken::new();
        let thread_cancel = cancel.clone();
        // Fallible spawn on purpose: a host that opens more turns than the OS
        // will grant threads must see a cleanly aborted turn on the wire, not
        // the panic bare `std::thread::spawn` raises when creation fails.
        // Naming the thread also makes a wedged session legible in a dump.
        let abort_tx = frame_tx.clone();
        let abort_observer = spec.observer.clone();
        let abort_turn = spec.turn.clone();
        let thread_pending = pending.clone();
        let builder = std::thread::Builder::new().name("stella-serve-session".to_string());
        let spawned = builder.spawn(move || {
            run_session(spec, frame_tx, thread_pending, control_ports, thread_cancel)
        });
        let thread = match spawned {
            Ok(thread) => Some(thread),
            Err(err) => {
                let _ = abort_tx.send(ServerFrame::TurnComplete {
                    outcome: TurnOutcomeWire::Aborted {
                        reason: ServeError::SessionThreadFailed(err.to_string()).to_string(),
                        cost_usd: 0.0,
                    },
                });
                // A turn that never got a thread still settled, and still has
                // to say so: reporting only the turns that started is exactly
                // the shape of silence this crate is being fixed for.
                abort_observer.emit(&ServeEvent::TurnSettled {
                    turn: abort_turn,
                    outcome: SettledOutcome::Aborted,
                    duration_ms: 0,
                    tally: TurnTally::default(),
                });
                None
            }
        };
        Session {
            frames: frame_rx,
            backlog,
            pending,
            controls,
            cancel,
            thread,
            history: Arc::new(FrameHistory::new()),
        }
    }

    /// Await the next frame from the engine. `None` once the session thread has
    /// finished and dropped its sender (i.e. after `TurnComplete`).
    pub async fn next_frame(&mut self) -> Option<ServerFrame> {
        let frame = self.frames.recv().await;
        if frame.is_some() {
            // Capacity comes back on delivery, not on encode: this and
            // `next_seq_frame` are the only two ways a frame leaves the
            // queue, so both give the slot back (#1266).
            self.backlog.leave();
        }
        frame
    }

    /// Await the next frame, sequenced and retained for replay.
    ///
    /// This — not [`Session::next_frame`] — is the funnel the server streams
    /// through, and it is the only place a `seq` is assigned. Numbering here
    /// rather than at the senders is deliberate: frames reach the channel from
    /// the `AgentEvent` forwarder *and*, bypassing it, from the two remote
    /// ports, so production order is not delivery order. A resuming client
    /// means delivery order.
    ///
    /// Encoding happens under the history lock because the `seq` has to be
    /// inside the bytes that get both written and retained — they cannot be
    /// produced separately without a window in which the two disagree.
    pub(crate) async fn next_seq_frame(&mut self) -> Option<Encoded> {
        let frame = self.frames.recv().await?;
        self.backlog.leave();
        let terminal = matches!(frame, ServerFrame::TurnComplete { .. });
        let mut unencodable = None;
        let (seq, json) = self.history.record(|seq| {
            let (json, err) = encode_seq_frame(seq, frame);
            unencodable = err;
            json
        });
        Some(Encoded {
            seq,
            json,
            terminal,
            unencodable,
        })
    }

    /// A handle to this turn's retained frame tail, cloneable and `Send`.
    ///
    /// The registry keeps one so a reconnect can replay even though the
    /// session itself is owned by the (now-dead) previous stream.
    pub(crate) fn history(&self) -> Arc<FrameHistory> {
        Arc::clone(&self.history)
    }

    /// A handle to this session's reverse-RPC registry, cloneable and `Send`.
    /// The HTTP layer keeps one alongside the session so a POST resolve handler
    /// can answer a reverse request without holding the (exclusively-streamed)
    /// session itself.
    pub fn pending(&self) -> Pending {
        self.pending.clone()
    }

    /// A handle to this turn's mid-turn controls, cloneable and `Send` — same
    /// contract as [`Session::pending`], for the pause/steer POST handlers.
    pub(crate) fn controls(&self) -> Controls {
        self.controls.clone()
    }

    /// A handle to this turn's step-boundary cancel token, cloneable and
    /// `Send` — same contract as [`Session::pending`], for `handle_cancel`.
    pub(crate) fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Answer a [`ServerFrame::ToolRequest`] with the host-run tool output.
    pub fn resolve_tool(&self, request_id: &str, output: ToolOutput) -> Result<(), ServeError> {
        self.pending.resolve_tool(request_id, output)
    }

    /// Answer a [`ServerFrame::ProviderRequest`] with a completed model result.
    pub fn resolve_provider(
        &self,
        request_id: &str,
        result: CompletionResult,
    ) -> Result<(), ServeError> {
        self.pending.resolve_provider(request_id, Ok(result))
    }

    /// Feed streamed fragments to an in-flight
    /// [`ServerFrame::ProviderRequest`] (#1165), ahead of the terminating
    /// [`Session::resolve_provider`] / [`Session::fail_provider`].
    ///
    /// Optional and advisory: a host that cannot stream never calls this, and
    /// the aggregated result stays authoritative. Fragments surface on the
    /// frame stream as `AgentEvent::TextDelta` / `AgentEvent::Reasoning`
    /// events, so every subscriber — not just the caller running the model —
    /// sees text as it arrives, and a resuming client replays it.
    pub fn resolve_provider_delta(
        &self,
        request_id: &str,
        deltas: Vec<crate::frame::ProviderDelta>,
    ) -> Result<(), ServeError> {
        self.pending.resolve_provider_delta(request_id, deltas)
    }

    /// Answer a [`ServerFrame::ProviderRequest`] with a classified failure.
    pub fn fail_provider(&self, request_id: &str, error: ProviderError) -> Result<(), ServeError> {
        self.pending.resolve_provider(request_id, Err(error))
    }

    /// Cancel the turn. Any parked reverse request wakes immediately and no new
    /// one is allowed to park, so the turn unwinds to
    /// [`TurnOutcomeWire::Aborted`] within one engine step and its thread is
    /// released.
    ///
    /// Idempotent, and safe to call from the server runtime while the session
    /// thread is mid-turn — [`Pending`] is the shared handle both sides hold. It
    /// does **not** kill the thread outright: the turn is allowed to unwind so
    /// the terminal frame still reaches a host that is streaming events.
    ///
    /// Also releases the pause gate: the engine only re-checks its cancel
    /// latch once `wait_if_paused` returns, so a cancel that leaves a paused
    /// turn parked would hold its OS thread for a resume that is never coming
    /// (see `crate::controls`).
    ///
    /// Three signals, because they stop three different waits (#1129):
    ///
    /// - The [`CancelToken`] is read at the top of every engine step, so a
    ///   turn doing CPU-bound work *between* reverse requests — compaction,
    ///   loop detection — stops at the next boundary. Without it, such a turn
    ///   observed the cancel only when it next happened to ask the host
    ///   something, which for a long compaction pass is an unbounded wait.
    /// - `Pending::cancel` wakes a turn already parked on a reverse request
    ///   and refuses new ones, so nothing parks after the fact.
    /// - `Controls::resume` releases a turn parked on the pause gate, which
    ///   neither of the others can reach.
    pub fn cancel(&self) {
        self.cancel.cancel();
        self.pending.cancel();
        self.controls.resume();
    }

    /// Whether the session thread has finished — i.e. the turn has produced
    /// its terminal frame, delivered or still buffered for a stream that never
    /// opened. `true` also when the thread could not be spawned, since the
    /// aborted terminal frame is buffered from the start. This is what lets
    /// the server's turn registry tell a finished-but-unstreamed turn (safe to
    /// evict under memory pressure) from a live one (never evicted).
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// How many frames are buffered for a stream that has not opened.
    ///
    /// Read when the registry evicts a finished-but-unstreamed turn, so the
    /// record can say how much the host threw away by never subscribing.
    #[must_use]
    pub fn buffered_frames(&self) -> usize {
        self.frames.len()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Cancel, not merely clear: clearing alone wakes a parked reverse
        // request with a *retryable* transport error, so a turn torn down
        // mid-step would burn its provider retries — and their backoff sleeps —
        // against a registry nobody answers anymore. Latching the cancel flag
        // makes the woken step observe `Cancelled` (not retryable), so the
        // turn aborts within one engine step and the thread can finish, rather
        // than leaking a thread wedged on a host that will never answer. We do
        // not join (a still-running turn could take arbitrarily long) — the
        // thread detaches and drains itself.
        // Same three signals as `Session::cancel`, and for the same reasons:
        // the token stops a step that is computing, `Pending` wakes one that
        // is parked on the host, and the gate release wakes one that is
        // paused.
        self.cancel.cancel();
        self.pending.cancel();
        // A paused turn is parked on the gate, not on a reverse request, so the
        // cancel above cannot wake it — the release here is what lets the
        // detached thread observe the latch and unwind (see `crate::controls`).
        self.controls.resume();
        drop(self.thread.take());
    }
}

/// What a driven turn hands back: the outcome, plus the state the engine
/// mutated on the way there.
///
/// `Engine::run_turn` writes those back through `&mut` borrows. Driving steps
/// means owning a `TurnState` instead, so they come back as values — which
/// is also what makes the transcript available to checkpoint *between* steps
/// rather than only after the turn.
pub(crate) struct DrivenTurn {
    pub(crate) outcome: TurnOutcome,
    pub(crate) messages: Vec<CompletionMessage>,
    pub(crate) budget: BudgetGuard,
}

/// Drive one turn through [`Engine::drive`], the engine's own turn loop, over
/// a [`TurnState`] this host owns.
///
/// The loop itself used to live here, as a third copy of the same five
/// obligations `Engine::run_turn_with_sender` and the engine's resume driver
/// each kept — and, exactly as three copies predict, this one silently kept
/// only four: an `EngineConfig::turn_halt` armed by an embedder through
/// [`SessionSpec::config`] was never consulted (#2452). Owning the
/// [`TurnState`] rather than the loop is what actually buys a durable host the
/// two things a whole-turn call cannot give it, and both survive the move:
///
/// - **A step-boundary cancel.** [`CancelToken`] rides on the state, and
///   `Engine::run_step` reads it at the top of every step — so
///   `POST /v1/turns/{id}/cancel` interrupts CPU-bound work *between* reverse
///   requests (compaction, loop detection) instead of only at the next time
///   the engine happens to ask the host something.
/// - **The finished transcript by value.** `Engine::run_turn` writes its
///   result back through `&mut` borrows; owning the state means the messages
///   and the spend come back as values, which is what lets this settle them
///   into the session registry.
///
/// The checkpoint seam is the engine's, and always was — both drivers called
/// the same `Engine::persist_checkpoint`, so an HTTP turn and a local turn are
/// equally recoverable rather than merely similar. That includes discarding
/// the resume point of a turn that was *cancelled*. Retaining it is arguable —
/// a cancelled turn was interrupted, not finished — but it is a change to what
/// a checkpoint means, and making it for served turns alone would leave the
/// two recoverable under different rules. The consequence, stated so it is not
/// discovered: what survives is the turn that died *without* unwinding (a
/// `SIGKILL`, a panic, a lost machine), not the tail of a graceful drain.
///
/// [`TurnState`]: stella_core::step::TurnState
pub(crate) async fn drive_turn(
    engine: &Engine<'_>,
    messages: Vec<CompletionMessage>,
    budget: BudgetGuard,
    events: &mpsc::UnboundedSender<AgentEvent>,
    cancel: CancelToken,
) -> DrivenTurn {
    // A served session is one run of one turn, so this is the run's owner and
    // owes it a terminal event (#3379): the engine ends the turn with
    // `TurnComplete` and says nothing about the run, because it cannot know
    // whether its caller wants another. The wrapper emits `RunComplete` when it
    // drops at the end of this function — after the turn, last on the stream
    // the host is reading.
    //
    // It owes the run's stage boundaries too, because the engine emits none
    // (#3416): `pairing_stage_complete` puts the closing `Stage(Complete)`
    // immediately ahead of the engine's `TurnComplete`, and the opening
    // `Stage(Execute)` goes out below.
    let events = RunEnding::sealing(EventSender::new(events.clone()).pairing_stage_complete());
    let _ = events.send(AgentEvent::Stage {
        name: stella_protocol::StageKind::Execute.into(),
        scope: stella_protocol::StageScope::Run,
    });
    let mut state = engine.new_turn(messages, budget).with_cancel_token(cancel);
    let outcome = engine.drive(&mut state, &events).await;
    let budget = *state.budget();
    DrivenTurn {
        outcome,
        messages: state.into_messages(),
        budget,
    }
}

/// The body of the session thread: build a current-thread runtime, construct the
/// remoted ports, and drive one turn to its outcome.
fn run_session(
    mut spec: SessionSpec,
    frame_tx: crate::backlog::FrameSink,
    pending: Pending,
    control_ports: ControlPorts,
    cancel: CancelToken,
) {
    let observer = spec.observer.clone();
    let turn = spec.turn.clone();
    let on_settled = spec.on_settled.take();
    // Lower the durable identity into the sink the engine actually reads.
    // Here rather than at the call sites so `routes` never has to know what a
    // `CheckpointSink` is, and after the `is_none` check so an embedder's own
    // sink always wins (see `SessionSpec::checkpoint`).
    if let Some(checkpoint) = spec.checkpoint.take()
        && spec.config.checkpoint_sink.is_none()
    {
        spec.config.checkpoint_sink = Some(checkpoint.sink(observer.clone(), turn.clone()));
    }
    let started = Instant::now();
    let runtime = match Builder::new_current_thread().enable_time().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            let _ = frame_tx.send(ServerFrame::TurnComplete {
                outcome: TurnOutcomeWire::Aborted {
                    reason: ServeError::RuntimeBuild(err.to_string()).to_string(),
                    cost_usd: 0.0,
                },
            });
            observer.emit(&ServeEvent::TurnSettled {
                turn,
                outcome: SettledOutcome::Aborted,
                duration_ms: millis(started.elapsed()),
                tally: TurnTally::default(),
            });
            return;
        }
    };

    runtime.block_on(async move {
        // Minted before the ports, because the tool port carries it: the
        // `tool.call.requested` chain has to run before a request frame
        // leaves, and installing extensions afterwards would open a window in
        // which a turn ran ungated. `None` when the operator installed
        // nothing — see `crate::extensions`.
        //
        // Keyed on the `TurnRef`, which is the turn id already truncated for
        // recording. Every sealed `HookEvent` embeds that key in its own id,
        // so the hook plane inherits the same posture as the record plane —
        // enough to correlate a turn, never enough to address one.
        let bus = crate::extensions::install_for_turn(&spec.extensions, turn.to_string());
        let provider = RemoteProvider::new(
            spec.provider_id,
            frame_tx.clone(),
            pending.clone(),
            spec.reverse_request_timeout,
        );
        // Bound before the executor takes ownership: a sub-agent gets its own
        // remoted executor over the same declared contracts, gate, and
        // principal, so all three have to survive the move.
        let contracts = spec.tools;
        let authz_gate = spec.gate;
        let principal = spec.principal;
        let tools = RemoteToolExecutor::new(
            contracts.clone(),
            Arc::clone(&authz_gate),
            principal.clone(),
            frame_tx.clone(),
            pending.clone(),
            spec.reverse_request_timeout,
            bus.clone(),
        );
        let sleeper = TokioSleeper;
        // The controls' receiver half is lent to the engine for the turn: the
        // gate parks each step while `POST /pause` holds, and the steering tap
        // drains `POST /steer` messages into the transcript at the same
        // boundary. The sender half stays on the registry entry.
        let (gate, steering) = control_ports.into_ports();

        // The engine emits `AgentEvent`s; wrap each as a frame for the host. The
        // reverse-RPC request frames bypass this and go straight to `frame_tx`
        // from the ports, so a starved forwarder never stalls the turn — it only
        // affects UI-event latency.
        //
        // This is also where the engine's stream is *tapped* for the turn's
        // tally. It has to be here, on the producing side: tapping in the SSE
        // loop would miss every turn nobody streams, and those are precisely the
        // abandoned ones worth observing. The fold only increments integers —
        // `AgentEvent::TextDelta` fires per token, so anything costlier here
        // would make this server slower than the model it waits on.
        let (event_tx, mut event_rx) = mpsc::unbounded_channel::<AgentEvent>();
        let forward_tx = frame_tx.clone();
        let forwarder = tokio::spawn(async move {
            let mut fold = TallyFold::default();
            while let Some(event) = event_rx.recv().await {
                fold.observe(&event);
                if forward_tx.send(ServerFrame::Event { event }).is_err() {
                    break;
                }
            }
            fold.finish()
        });

        // Sub-agents (#1297), when the operator's policy and the caller's
        // request agree. Built before the engine because the engine borrows
        // the tool set, and the delegating view IS the tool set once children
        // are allowed — a host tool call still remotes exactly as before; only
        // `delegate` is executed here.
        let spend_ledger: stella_core::subagent::SubAgentSpendLedger = Default::default();
        let dispatcher = spec.sub_agents.as_ref().map(|effective| {
            let child_provider = crate::remote::RemoteProvider::new(
                effective
                    .provider_id
                    .clone()
                    .unwrap_or_else(|| provider.id_string()),
                frame_tx.clone(),
                pending.clone(),
                spec.reverse_request_timeout,
            );
            Arc::new(crate::subagents::ServedSubAgents::new(
                Arc::new(child_provider),
                // The same hook plane the parent's executor carries (#1298).
                // An operator policy that can refuse a tool call must not be
                // escapable by delegating that call to a child: the child
                // remotes to the same host, on the same account, through the
                // same reverse-RPC — so it meets the same gate.
                Arc::new(RemoteToolExecutor::new(
                    contracts.clone(),
                    Arc::clone(&authz_gate),
                    principal.clone(),
                    frame_tx.clone(),
                    pending.clone(),
                    spec.reverse_request_timeout,
                    bus.clone(),
                )),
                spec.config.clone(),
                effective,
                event_tx.clone(),
                Arc::clone(&spend_ledger),
            )) as Arc<dyn stella_core::subagent::SubAgentDispatcher>
        });
        let delegating =
            dispatcher
                .as_ref()
                .zip(spec.sub_agents.as_ref())
                .map(|(dispatcher, effective)| {
                    crate::subagents::DelegatingTools::new(
                        &tools,
                        Arc::clone(dispatcher),
                        effective.child_steps,
                        Arc::clone(&spend_ledger),
                    )
                });
        let tool_view: &dyn stella_core::ports::ToolExecutor = match &delegating {
            Some(delegating) => delegating,
            None => &tools,
        };
        // Built over `tool_view` rather than `tools`: with sub-agents allowed
        // the delegating view IS this turn's tool set (#1297), and with none
        // allowed it is `tools` itself, so one construction covers both.
        //
        // Every attachment below is by reference for the engine's lifetime, so
        // the owners have to outlive it — hence the bindings above rather than
        // temporaries here.
        let mut engine = Engine::with_sleeper(&provider, tool_view, spec.config.clone(), &sleeper)
            .with_gate(&gate)
            .with_steering(&*steering);
        if let Some(bus) = &bus {
            engine = engine.with_bus(bus);
        }
        if let Some(calibration) = &spec.calibration {
            engine = engine.with_calibration(calibration);
        }

        // Two mutually exclusive modes: plain turn, judged goal run (#1297).
        let outcome = match &spec.goal {
            // A judged multi-round run (#1297). Its verifier announces its
            // own provider id and `role: verifier` on every frame, so a host
            // can route it to a different family than the worker — the
            // property the goal loop exists for, and the one the engine
            // cannot enforce because the host owns the model calls.
            Some(run) => {
                let verifier_provider = crate::remote::RemoteProvider::new(
                    run.verifier_provider_id
                        .clone()
                        .unwrap_or_else(|| provider.id_string()),
                    frame_tx.clone(),
                    pending.clone(),
                    spec.reverse_request_timeout,
                )
                .with_role(stella_protocol::ModelCallRole::Verdict);
                crate::goal::drive_goal(
                    &engine,
                    &verifier_provider,
                    run,
                    spec.config.turn_instance,
                    spec.messages,
                    spec.budget,
                    &event_tx,
                    cancel,
                )
                .await
            }
            None => drive_turn(&engine, spec.messages, spec.budget, &event_tx, cancel).await,
        };
        let messages = outcome.messages;
        let budget = outcome.budget;
        let outcome = outcome.outcome;

        // Release everything holding a clone of the event sender BEFORE
        // closing the channel, in dependency order: the engine borrows the
        // tool view, the tool view holds the dispatcher, and the dispatcher
        // holds the sender a child emits its metering on. Miss one and
        // `event_rx.recv()` never returns `None`, the forwarder never drains,
        // and a turn that has already produced its `Complete` event hangs
        // without ever emitting its terminal frame.
        drop(engine);
        drop(delegating);
        drop(dispatcher);
        // Close the event channel so the forwarder drains and exits before the
        // terminal frame — a well-behaved host thus sees every event first.
        drop(event_tx);
        // A forwarder that panicked still leaves a turn that must be reported;
        // an empty tally is the honest answer for a fold that did not survive.
        let mut tally = forwarder.await.unwrap_or_default();
        // Read the drop count here, not in the forwarder: the two remote
        // ports and the terminal path all send through the same sink, so the
        // only moment the count is final is after the last send (#1266).
        tally.frames_dropped = frame_tx.backlog().dropped();

        let wire: TurnOutcomeWire = outcome.into();
        let settled = match &wire {
            TurnOutcomeWire::Completed { .. } => SettledOutcome::Completed,
            TurnOutcomeWire::Aborted { .. } => SettledOutcome::Aborted,
        };
        // Settle the owning session BEFORE the terminal frame goes out: the
        // frame is the host's cue that the turn is over, and a `GET` on the
        // session issued the moment it arrives must read the written-back
        // history, not the previous turn's.
        if let Some(on_settled) = on_settled {
            on_settled(TurnSettlement {
                completed: matches!(settled, SettledOutcome::Completed),
                messages,
                budget,
            });
        }
        let _ = frame_tx.send(ServerFrame::TurnComplete { outcome: wire });
        observer.emit(&ServeEvent::TurnSettled {
            turn,
            outcome: settled,
            duration_ms: millis(started.elapsed()),
            tally,
        });
        // A clean turn leaves nothing parked; belt-and-suspenders for an aborted
        // one, so no reverse-RPC one-shot outlives the session. Anything still
        // registered here is work being thrown away, and `clear` reports it.
        pending.clear();
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    use async_trait::async_trait;
    use stella_core::driver::TurnHalt;
    use stella_protocol::{
        BudgetMode, CompletionRequestRef, CompletionUsage, Provider, ProviderError, ToolCall,
        ToolSchema,
    };

    use super::*;

    /// Answers a tool call first, then plain text — so the first step is a
    /// committed `Continue` boundary and the second would finish the turn.
    struct ToolThenText {
        calls: Arc<AtomicU32>,
    }

    #[async_trait]
    impl Provider for ToolThenText {
        fn id(&self) -> &str {
            "scripted"
        }
        async fn complete_ref(
            &self,
            _req: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let mut result = CompletionResult {
                upstream_provider: None,
                text: String::new(),
                tool_calls: vec![],
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 1,
                    ..CompletionUsage::default()
                },
                model: "scripted".into(),
                cost_usd: 0.0,
                finish_reason: None,
            };
            if call == 0 {
                result.tool_calls = vec![ToolCall {
                    call_id: "call_0".into(),
                    name: "touch".into(),
                    input: serde_json::json!({}),
                }];
            } else {
                result.text = "ran a second step the halt should have prevented".into();
            }
            Ok(result)
        }
    }

    struct OkTool;

    #[async_trait]
    impl stella_core::ports::ToolExecutor for OkTool {
        fn schemas(&self) -> Vec<ToolSchema> {
            vec![ToolSchema {
                name: "touch".into(),
                description: "touch a file".into(),
                input_schema: serde_json::json!({"type": "object"}),
                read_only: false,
                speculation_safe: false,
            }]
        }
        async fn execute(&self, _name: &str, _input: &serde_json::Value) -> ToolOutput {
            ToolOutput::Ok {
                content: "ok".into(),
                data: None,
            }
        }
    }

    /// Armed from the start, so the first committed boundary ends the turn.
    #[derive(Debug)]
    struct AlwaysHalt;
    impl TurnHalt for AlwaysHalt {
        fn halt_reason(&self) -> Option<String> {
            Some("the tracked test flipped".into())
        }
    }

    async fn drive_scripted_turn(config: EngineConfig, calls: Arc<AtomicU32>) -> TurnOutcome {
        let provider = ToolThenText { calls };
        let tools = OkTool;
        let sleeper = TokioSleeper;
        let engine = Engine::with_sleeper(&provider, &tools, config, &sleeper);
        let (tx, _rx) = mpsc::unbounded_channel();
        drive_turn(
            &engine,
            vec![
                CompletionMessage::system("sys"),
                CompletionMessage::user("go"),
            ],
            BudgetGuard::new(BudgetMode::Off, None, None),
            &tx,
            CancelToken::new(),
        )
        .await
        .outcome
    }

    /// **The obligation the served loop never had.** `EngineConfig::turn_halt`
    /// is a public field of the public [`SessionSpec::config`], so an embedder
    /// can arm it — and before #2452 this driver's own copy of the step loop
    /// never consulted it, silently, while the CLI's fresh and restored turns
    /// both stopped at the boundary. Fails on the old shape: the turn ran on to
    /// a second provider call and finished with the model's text.
    #[tokio::test]
    async fn a_served_turn_honors_the_turn_halt() {
        let calls = Arc::new(AtomicU32::new(0));
        let config = EngineConfig {
            turn_halt: Some(Arc::new(AlwaysHalt)),
            ..EngineConfig::default()
        };

        let outcome = drive_scripted_turn(config, calls.clone()).await;

        let TurnOutcome::Completed { .. } = outcome else {
            panic!("a fired halt ends the turn as a success, not a failure: {outcome:?}");
        };
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the halt fires at the first committed boundary — a second call \
             means the served driver never consulted it"
        );
    }

    /// The control: no halt, and the same served turn runs to its ordinary end.
    /// Without it the assertion above would also pass on a driver that stopped
    /// after one step for some entirely different reason.
    #[tokio::test]
    async fn an_unhalted_served_turn_runs_to_its_ordinary_end() {
        let calls = Arc::new(AtomicU32::new(0));

        let outcome = drive_scripted_turn(EngineConfig::default(), calls.clone()).await;

        let TurnOutcome::Completed { text, .. } = outcome else {
            panic!("an unhalted served turn completes: {outcome:?}");
        };
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(text, "ran a second step the halt should have prevented");
    }
}
