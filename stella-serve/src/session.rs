//! A single engine session: one turn driven on a dedicated OS thread.
//!
//! The engine's turn future is deliberately `!Send` (it holds provider futures
//! and the retry-jitter RNG across awaits — `stella-cli::fleet_cmd`), so it
//! cannot be `tokio::spawn`ed onto the server's multi-thread runtime. Instead
//! each session gets its own OS thread running a **current-thread** runtime that
//! `block_on`s a loop over `stella_engine::run_step`; the server side
//! communicates with it purely through
//! `Send` channels — an outbound [`ServerFrame`] stream and the shared
//! [`Pending`] one-shot registry. This is the fleet's bridge, reused for a
//! long-lived server instead of a batch worker.
//!
//! Scope note: one [`Session`] drives one turn. Multi-turn sessions (retaining
//! the message history across turns) layer on top of this without changing the
//! transport; per-step checkpointing happens here (see [`drive_turn`]) through
//! the `CheckpointSink` on `EngineConfig`, so a served turn interrupted
//! mid-flight resumes from its last step boundary rather than being re-run
//! from the prompt. That sink comes from one of two places:
//! [`SessionSpec::checkpoint`], which names a key in the server's
//! [`crate::checkpoint::CheckpointStore`], or an embedder setting
//! `config.checkpoint_sink` itself.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use stella_core::EngineConfig;
use stella_engine::{
    BudgetGuard, CancelToken, Engine, EventSender, StepOutcome, TurnOutcome, step_cap_reason,
};
use stella_protocol::{
    AgentEvent, CompletionMessage, CompletionResult, ProviderError, StageKind, ToolOutput,
    ToolSchema,
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
    /// Tool schemas the model may call, including `read_only` flags.
    pub tools: Vec<ToolSchema>,
    /// The already-assembled conversation to run this turn.
    pub messages: Vec<CompletionMessage>,
    /// Engine knobs (step cap, compaction budget, sampling). `Default` is a
    /// sane starting point.
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
}

/// A running session. The host drives it by reading [`ServerFrame`]s with
/// [`Session::next_frame`] and answering reverse-RPC requests with
/// [`Session::resolve_tool`] / [`Session::resolve_provider`].
pub struct Session {
    frames: mpsc::UnboundedReceiver<ServerFrame>,
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
        let (frame_tx, frame_rx) = mpsc::unbounded_channel::<ServerFrame>();
        let pending = Pending::new(spec.observer.clone(), spec.turn.clone());
        let (controls, control_ports) = Controls::new();
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
        self.frames.recv().await
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
/// means owning a [`TurnState`] instead, so they come back as values — which
/// is also what makes the transcript available to checkpoint *between* steps
/// rather than only after the turn.
struct DrivenTurn {
    outcome: TurnOutcome,
    messages: Vec<CompletionMessage>,
    budget: BudgetGuard,
}

/// Drive one turn as a loop over [`Engine::run_step`], the step-scoped facade
/// `stella-engine` exists to expose (#1129).
///
/// This is deliberately the same loop as `Engine::run_turn_with_sender` —
/// which is itself a loop over `run_step`, so there is one implementation of
/// a step and no second engine here. What driving it from this side buys is
/// the two things a whole-turn call cannot give a durable host:
///
/// - **A step-boundary cancel.** [`CancelToken`] is read at the top of every
///   step, so `POST /v1/turns/{id}/cancel` interrupts CPU-bound work
///   *between* reverse requests — compaction, loop detection — instead of
///   only at the next time the engine happens to ask the host something.
///   Before this, a turn that was mid-compaction observed a cancel only once
///   it next parked on a reverse request.
/// - **A checkpoint seam.** `StepOutcome::Continue` is the one moment the
///   transcript is guaranteed well-paired (no `tool_use` without its
///   `tool_result`), which is exactly where a durable runner would persist
///   `state.to_checkpoint()`. It does: given a `CheckpointSink` on
///   `EngineConfig`, every step boundary of a served turn writes a resume
///   point, exactly as a CLI turn does — the two drivers call the same
///   `Engine::persist_checkpoint`, so an HTTP turn and a local turn are
///   equally recoverable rather than merely similar.
///
///   That parity is load-bearing in the other direction too, which is why the
///   terminal arms below discard unconditionally instead of keeping the
///   resume point of a turn that was *cancelled*. Retaining it is arguable —
///   a cancelled turn was interrupted, not finished — but it is a change to
///   what a checkpoint means, and making it here alone would leave a served
///   turn and a CLI turn recoverable under different rules while this comment
///   claimed otherwise. The consequence, stated so it is not discovered:
///   what survives is the turn that died *without* unwinding (a `SIGKILL`, a
///   panic, a lost machine), not the tail of a graceful drain.
async fn drive_turn(
    engine: &Engine<'_>,
    messages: Vec<CompletionMessage>,
    budget: BudgetGuard,
    events: &mpsc::UnboundedSender<AgentEvent>,
    cancel: CancelToken,
) -> DrivenTurn {
    let events = EventSender::new(events.clone());
    // The stage marker `run_turn` opens with. Emitted here for the same
    // reason the loop is here: a host driving steps must produce the identical
    // event sequence, or a client cannot tell the two drivers apart — which is
    // the whole promise of `run_turn` being a loop over `run_step`.
    let _ = events.send(AgentEvent::Stage {
        name: StageKind::Execute,
    });
    let max_steps = engine.max_steps();
    let mut state = engine.new_turn(messages, budget).with_cancel_token(cancel);

    let outcome = loop {
        if state.step() >= max_steps {
            // The belt-and-suspenders backstop, reported exactly as
            // `run_turn` reports it — the string is shared rather than
            // copied, because two spellings of one failure read as two
            // failures.
            let reason = step_cap_reason(max_steps);
            let _ = events.send(AgentEvent::Error {
                message: reason.clone(),
                retryable: false,
            });
            engine.discard_checkpoint();
            break TurnOutcome::Aborted {
                reason,
                cost_usd: state.total_cost_usd(),
            };
        }
        match engine.run_step(&mut state, &events).await {
            // The checkpoint seam: the one moment the transcript is
            // guaranteed well-paired. A served turn that dies here — a dropped
            // connection, a killed process — resumes from this point instead
            // of re-running the prompt and paying for the work twice.
            StepOutcome::Continue => {
                engine.persist_checkpoint(&state);
                continue;
            }
            terminal => {
                if let Some(outcome) = terminal.into_turn_outcome() {
                    // The turn ended; a resume point that outlives it would
                    // offer to replay work the client already saw finish.
                    engine.discard_checkpoint();
                    break outcome;
                }
                // Unreachable: `into_turn_outcome` is `None` only for
                // `Continue`, which the arm above already took. Breaking
                // rather than panicking keeps a future variant from taking
                // down a session thread.
                break TurnOutcome::Aborted {
                    reason: "engine step returned no outcome".to_string(),
                    cost_usd: state.total_cost_usd(),
                };
            }
        }
    };

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
    frame_tx: mpsc::UnboundedSender<ServerFrame>,
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
        let provider = RemoteProvider::new(
            spec.provider_id,
            frame_tx.clone(),
            pending.clone(),
            spec.reverse_request_timeout,
        );
        let tools = RemoteToolExecutor::new(
            spec.tools,
            frame_tx.clone(),
            pending.clone(),
            spec.reverse_request_timeout,
        );
        let sleeper = TokioSleeper;
        // The controls' receiver half is lent to the engine for the turn: the
        // gate parks each step while `POST /pause` holds, and the steering tap
        // drains `POST /steer` messages into the transcript at the same
        // boundary. The sender half stays on the registry entry.
        let (gate, steering) = control_ports.into_ports();
        let engine = Engine::with_sleeper(&provider, &tools, spec.config, &sleeper)
            .with_gate(&gate)
            .with_steering(&*steering);

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

        let outcome = drive_turn(&engine, spec.messages, spec.budget, &event_tx, cancel).await;
        let messages = outcome.messages;
        let budget = outcome.budget;
        let outcome = outcome.outcome;

        // Close the event channel so the forwarder drains and exits before the
        // terminal frame — a well-behaved host thus sees every event first.
        drop(event_tx);
        // A forwarder that panicked still leaves a turn that must be reported;
        // an empty tally is the honest answer for a fold that did not survive.
        let tally = forwarder.await.unwrap_or_default();

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
