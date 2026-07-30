//! A single engine session: one turn driven on a dedicated OS thread.
//!
//! The engine's turn future is deliberately `!Send` (it holds provider futures
//! and the retry-jitter RNG across awaits — `stella-cli::fleet_cmd`), so it
//! cannot be `tokio::spawn`ed onto the server's multi-thread runtime. Instead
//! each session gets its own OS thread running a **current-thread** runtime that
//! `block_on`s `run_turn`; the server side communicates with it purely through
//! `Send` channels — an outbound [`ServerFrame`] stream and the shared
//! [`Pending`] one-shot registry. This is the fleet's bridge, reused for a
//! long-lived server instead of a batch worker.
//!
//! Scope note: one [`Session`] drives one turn. Multi-turn sessions (retaining
//! the message history across turns) and per-step checkpointing layer on top of
//! this without changing the transport.

use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use stella_core::{BudgetGuard, Engine, EngineConfig};
use stella_protocol::{
    AgentEvent, CompletionMessage, CompletionResult, ProviderError, ToolOutput, ToolSchema,
};
use tokio::runtime::Builder;
use tokio::sync::mpsc;

use crate::error::ServeError;
use crate::frame::{ServerFrame, TurnOutcomeWire};
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
    thread: Option<JoinHandle<()>>,
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
        // Fallible spawn on purpose: a host that opens more turns than the OS
        // will grant threads must see a cleanly aborted turn on the wire, not
        // the panic bare `std::thread::spawn` raises when creation fails.
        // Naming the thread also makes a wedged session legible in a dump.
        let abort_tx = frame_tx.clone();
        let abort_observer = spec.observer.clone();
        let abort_turn = spec.turn.clone();
        let thread_pending = pending.clone();
        let builder = std::thread::Builder::new().name("stella-serve-session".to_string());
        let spawned = builder.spawn(move || run_session(spec, frame_tx, thread_pending));
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
            thread,
        }
    }

    /// Await the next frame from the engine. `None` once the session thread has
    /// finished and dropped its sender (i.e. after `TurnComplete`).
    pub async fn next_frame(&mut self) -> Option<ServerFrame> {
        self.frames.recv().await
    }

    /// A handle to this session's reverse-RPC registry, cloneable and `Send`.
    /// The HTTP layer keeps one alongside the session so a POST resolve handler
    /// can answer a reverse request without holding the (exclusively-streamed)
    /// session itself.
    pub fn pending(&self) -> Pending {
        self.pending.clone()
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
    pub fn cancel(&self) {
        self.pending.cancel();
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
        self.pending.cancel();
        drop(self.thread.take());
    }
}

/// The body of the session thread: build a current-thread runtime, construct the
/// remoted ports, and drive one turn to its outcome.
fn run_session(spec: SessionSpec, frame_tx: mpsc::UnboundedSender<ServerFrame>, pending: Pending) {
    let observer = spec.observer.clone();
    let turn = spec.turn.clone();
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
        let engine = Engine::with_sleeper(&provider, &tools, spec.config, &sleeper);

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

        let mut messages = spec.messages;
        let mut budget = spec.budget;
        let outcome = engine.run_turn(&mut messages, &mut budget, &event_tx).await;

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
