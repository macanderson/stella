// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One committed step: the state a step mutates, what a step returns, and how
//! a host stops or snapshots a turn **between** steps (#971).
//!
//! [`crate::driver::Engine::run_turn`] used to own the whole
//! `for step in 0..max_steps` loop, so every per-turn value lived as a local
//! in that one stack frame and nothing outside could see — let alone
//! checkpoint — the boundary between two steps. A durable host (Oxagen's
//! runner, and `stella-serve`) needs exactly that boundary: it drives one
//! step, persists what came out, and decides whether to take another.
//!
//! So the locals became [`TurnState`], the loop body became
//! [`crate::driver::Engine::run_step`], and `run_turn` became a loop over it.
//! There is still exactly ONE code path — the extraction is what makes the
//! step boundary addressable, not a second implementation of it.
//!
//! # The three ways a turn can stop, and what each one costs
//!
//! - **Soft stop** ([`crate::driver::SOFT_STOP_REASON`]) — the user asked, via
//!   [`crate::ports::TurnSteering`]. Checked at the step boundary; every
//!   completed step is kept and the history stays valid for the next turn.
//! - **Cancel** ([`CancelToken`], [`CANCELLED_REASON`]) — the *host* asked.
//!   Also checked at the step boundary, never mid-tool, and it likewise keeps
//!   completed steps. It differs from the soft stop only in who asked and how
//!   it reads to an operator, which is why the two carry distinct reasons.
//! - **Hard drop** — the caller drops the turn future. This is not a boundary
//!   at all: it lands wherever the future happened to be awaiting. See
//!   [`CancelToken`] for what that loses, and prefer cancelling.
//!
//! # What a [`Checkpoint`] carries, and what it deliberately does not
//!
//! A checkpoint is the state a *resumed* turn must not lose: the transcript,
//! the money, which model served last, whether the loop steer was already
//! spent, and how far the turn got. It does NOT carry `TurnMemos` — the
//! per-turn memos (receipt registry, warning dedup, result identities,
//! summarizer latch) exist to make repeated work cheap and repeated warnings
//! quiet within one process. Rebuilding them costs a resumed turn one
//! re-registration of its blocks and (at most) one repeated budget warning;
//! serializing them would put four internal caches into a wire format that a
//! host then has to keep valid across versions. That trade is deliberate and
//! is the reason `TurnMemos` is not `Serialize`.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use stella_protocol::{
    AgentEvent, BudgetMode, CompletionMessage, CompletionResult, MessageRole, ProviderError,
    ToolCall, ToolOutput, ToolResult,
};

use crate::budget::BudgetGuard;
use crate::driver::{EngineConfig, SPECULATION_DISCARD_ATTEMPT_FAILED, TurnMemos, TurnOutcome};
use crate::event_sender::EventSender;
use crate::speculation::SpeculationPool;

/// The [`TurnOutcome::Aborted`] reason of a host-requested cancellation.
/// Callers match on this — as they already do on
/// [`crate::driver::SOFT_STOP_REASON`] — to render "cancelled" rather than
/// "failed", and to keep (never truncate) the turn's completed work.
///
/// Distinct from the soft stop on purpose: both stop at the same safe
/// boundary and both keep completed steps, but one is the person at the
/// keyboard pressing stop and the other is the host process reclaiming a
/// turn (a deadline, a shutdown drain, a superseded request). An operator
/// reading a transcript needs to be able to tell those apart.
pub const CANCELLED_REASON: &str =
    "cancelled at step boundary by the host — completed steps kept, nothing interrupted mid-tool";

/// The `ToolOutput::Error` message that closes a `tool_use` left open when a
/// cancellation lands. Mirrors the budget-overshoot wording in
/// `Engine::handle_committed_result`: the model is told the call did not run,
/// so the pairing is honest as well as structurally valid.
const CANCELLED_TOOL_RESULT: &str = "not executed — turn cancelled at a step boundary";

/// A cheap, clonable "stop this turn at the next safe boundary" flag.
///
/// Clone it, hand a clone to whatever watches for shutdown, and call
/// [`cancel`](Self::cancel). The engine reads it at the top of every step —
/// the same boundary that already governs the pause gate, the soft stop and
/// the budget check — and never between announcing a tool call and recording
/// its result. Cancelling therefore cannot leave the workspace and the
/// model's view of it disagreeing, which is the whole reason the budget
/// enforcer is not allowed to kill a tool mid-flight either
/// (`crate::budget`'s module contract).
///
/// # Why this exists when dropping the future already "cancels"
///
/// Dropping the turn future stops it too, and stops it *immediately* — which
/// is exactly the problem. A drop lands wherever the future was awaiting:
///
/// - a tool may be half-way through mutating the workspace, and its
///   `tool_result` never reaches the transcript, so the borrowed history ends
///   on an unpaired `tool_use` that the next provider call rejects outright
///   (`Engine::execute_tool_calls` documents this contract: a hard-cancelling
///   caller must truncate the turn out of history itself);
/// - a model call may be in flight and already billed — `CancelUsageGuard`
///   exists solely to leave one `UsageIncomplete { Cancelled }` envelope
///   behind so that money is not lost from the accounting stream;
/// - speculative read-only calls that already ran get one
///   `SpeculationDiscarded` each from `SpeculationDropGuard`, and nothing
///   else survives.
///
/// A token cancel gives up none of that: the step that was running finishes
/// and commits, the transcript stays valid, and the turn returns
/// [`TurnOutcome::Aborted`] with [`CANCELLED_REASON`]. The cost is latency —
/// it waits for the in-flight step. Reach for a drop only when you cannot
/// afford to wait, and expect to discard the turn's history when you do.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A fresh, un-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Idempotent, and callable from any thread —
    /// nothing here awaits, so a signal handler or a supervisor task can call
    /// it directly.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    /// Whether cancellation has been requested. `SeqCst` rather than
    /// `Relaxed`: this is read a handful of times per step, against a step
    /// that costs a model call, so the ordering is free — and "the flag I set
    /// before shutting down is the flag the engine reads" is the property a
    /// host will assume without checking.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// How one call to [`crate::driver::Engine::run_step`] ended.
///
/// The step-scoped twin of [`TurnOutcome`]: `Continue` is the case a turn
/// outcome cannot express (the step committed and the turn goes on), and the
/// other two are that turn outcome verbatim — same reason strings, same
/// running cost — so a host driving `run_step` itself sees exactly what
/// `run_turn` would have returned.
#[derive(Debug, Clone, PartialEq)]
pub enum StepOutcome {
    /// The step committed and the turn should take another one.
    Continue,
    /// The model produced a final text response with no further tool calls.
    Done {
        /// The answer text, exactly as `TurnOutcome::Completed` carries it.
        text: String,
        /// Turn spend to date, in USD.
        cost_usd: f64,
    },
    /// The turn ended before completion: cancelled, soft-stopped, budget
    /// enforced, a loop detected, retries exhausted, or an empty response.
    /// Always a *clean* stop — never mid-tool (see `crate::driver`).
    Aborted {
        /// Human-readable cause. [`CANCELLED_REASON`] and
        /// [`crate::driver::SOFT_STOP_REASON`] are the two a host matches on.
        reason: String,
        /// Turn spend to date, in USD.
        cost_usd: f64,
    },
}

impl StepOutcome {
    /// The turn outcome this step implies, or `None` when the turn continues.
    /// The whole of `run_turn`'s loop condition, in one place.
    #[must_use]
    pub fn into_turn_outcome(self) -> Option<TurnOutcome> {
        match self {
            StepOutcome::Continue => None,
            StepOutcome::Done { text, cost_usd } => Some(TurnOutcome::Completed { text, cost_usd }),
            StepOutcome::Aborted { reason, cost_usd } => {
                Some(TurnOutcome::Aborted { reason, cost_usd })
            }
        }
    }
}

impl From<TurnOutcome> for StepOutcome {
    /// Lift a phase's terminal outcome into a step outcome. Every phase
    /// function inside a step still returns `Option<TurnOutcome>` — they end
    /// the *turn*, not just the step — and this is the one conversion, so no
    /// phase can drift into reporting a different reason or cost than
    /// `run_turn` used to report for it.
    fn from(outcome: TurnOutcome) -> Self {
        match outcome {
            TurnOutcome::Completed { text, cost_usd } => StepOutcome::Done { text, cost_usd },
            TurnOutcome::Aborted { reason, cost_usd } => StepOutcome::Aborted { reason, cost_usd },
        }
    }
}

/// Everything one turn mutates as it runs — the locals that used to live in
/// `run_turn_with_sender`'s stack frame, hoisted so a host can own them
/// across steps.
///
/// Construct one with [`TurnState::new`] (a fresh turn) or
/// [`TurnState::from_checkpoint`] (a resumed one), drive it through
/// [`crate::driver::Engine::run_step`], and read the transcript back out with
/// [`messages`](Self::messages) / [`into_messages`](Self::into_messages).
///
/// It owns its message vector rather than borrowing one. `run_turn` still
/// takes `&mut Vec<CompletionMessage>` and still writes every append back to
/// the caller's vector — including when the turn future is dropped mid-step
/// (see `BorrowedTurn`) — so that seam is unchanged; a host driving steps
/// directly gets the simpler ownership instead.
pub struct TurnState {
    /// The conversation, appended to by every step and rewritten in place by
    /// compaction. Message index 0 is the system prompt and nothing here ever
    /// touches it (the prompt-cache stability contract).
    pub(crate) messages: Vec<CompletionMessage>,
    /// The money meter. Owned rather than borrowed for the same reason the
    /// messages are; `run_turn` copies the caller's guard in and back out.
    pub(crate) budget: BudgetGuard,
    /// This turn's spend, which is NOT the same number as the guard's: the
    /// guard accumulates across turns in a session, this is what the turn
    /// outcome reports.
    pub(crate) total_cost_usd: f64,
    /// The model string of the last committed step, for reading the per-model
    /// drift correction. `None` until the first result lands — `CalibrationMap
    /// ::factor` then falls back to the session's single seeded entry.
    pub(crate) calibration_model: Option<String>,
    /// Whether this turn already spent its one stuck-loop steering warning
    /// (`Engine::check_loop_detection`) — the next detection aborts instead of
    /// warning again.
    pub(crate) loop_steered: bool,
    /// How many times the transcript has been rewritten in place rather than
    /// appended to. The live invalidation counter lives with the memos it
    /// invalidates (`TurnMemos`) and is an opaque type with no accessor; this
    /// is its checkpointable twin — see [`Checkpoint::transcript_rewrites`].
    pub(crate) transcript_rewrites: u64,
    /// The index of the step that will run next. Advanced only by a step that
    /// COMMITTED and continued, so a terminal outcome leaves this naming the
    /// step that ended the turn.
    pub(crate) step: usize,
    /// The per-turn memos a checkpoint deliberately drops (module docs).
    pub(crate) memos: TurnMemos,
    /// In-turn continuations already spent on steps that ended at the
    /// output-token limit with no tool call (`Engine::dispatch_completion`).
    /// Bounded per turn so a model that keeps truncating cannot loop the
    /// spend; deliberately NOT checkpointed — a resumed turn starts the
    /// allowance over, which only re-permits a bounded amount of work.
    pub(crate) length_continuations: u32,
    /// When this turn began, for deciding whether a length continuation is
    /// affordable against `EngineConfig::turn_budget`.
    ///
    /// Deliberately NOT checkpointed, and set fresh by `from_checkpoint`: a
    /// resumed turn is resumed by a caller with its own deadline, so carrying
    /// the original start would make the budget read as long spent before any
    /// work happened. Same reasoning as `length_continuations` starting over.
    pub(crate) started_at: std::time::Instant,
    /// How long the most recent model call took, as the estimate of what one
    /// more continuation would cost. A continuation re-runs the work that just
    /// truncated, so its predecessor's duration is the honest forecast — and
    /// the only one available without predicting the model.
    pub(crate) last_step: Option<std::time::Duration>,
    /// The host's stop signal, checked at the top of every step.
    pub(crate) cancel: CancelToken,
}

impl TurnState {
    /// A fresh turn over `messages`, metered by `budget`. `config` supplies
    /// only the two receipt-keying settings (`turn_instance`,
    /// `lifecycle_enabled`) — the engine keeps its own config and this never
    /// diverges from it.
    #[must_use]
    pub fn new(
        messages: Vec<CompletionMessage>,
        budget: BudgetGuard,
        config: &EngineConfig,
    ) -> Self {
        Self {
            messages,
            budget,
            total_cost_usd: 0.0,
            calibration_model: None,
            loop_steered: false,
            transcript_rewrites: 0,
            step: 0,
            memos: TurnMemos::new(config.turn_instance, config.lifecycle_enabled),
            length_continuations: 0,
            started_at: std::time::Instant::now(),
            last_step: None,
            cancel: CancelToken::new(),
        }
    }

    /// Rebuild a turn from a [`Checkpoint`]. Fresh memos, restored transcript,
    /// money, calibration model, loop-steer latch and step index — see the
    /// module docs for exactly what resuming costs.
    #[must_use]
    pub fn from_checkpoint(checkpoint: Checkpoint, config: &EngineConfig) -> Self {
        Self {
            messages: checkpoint.messages,
            budget: checkpoint.budget.restore(),
            total_cost_usd: checkpoint.total_cost_usd,
            calibration_model: checkpoint.calibration_model,
            loop_steered: checkpoint.loop_steered,
            transcript_rewrites: checkpoint.transcript_rewrites,
            step: checkpoint.step,
            memos: TurnMemos::new(config.turn_instance, config.lifecycle_enabled),
            length_continuations: 0,
            started_at: std::time::Instant::now(),
            last_step: None,
            cancel: CancelToken::new(),
        }
    }

    /// Snapshot this turn for durable storage. Cheap-ish (it clones the
    /// transcript) and safe to take at any step boundary — take it right after
    /// [`StepOutcome::Continue`], which is the only moment the transcript is
    /// guaranteed to have every `tool_use` paired with its `tool_result`.
    #[must_use]
    pub fn to_checkpoint(&self) -> Checkpoint {
        Checkpoint {
            version: CHECKPOINT_VERSION,
            step: self.step,
            messages: self.messages.clone(),
            budget: BudgetSnapshot::of(&self.budget),
            total_cost_usd: self.total_cost_usd,
            calibration_model: self.calibration_model.clone(),
            loop_steered: self.loop_steered,
            transcript_rewrites: self.transcript_rewrites,
        }
    }

    /// The conversation so far.
    #[must_use]
    pub fn messages(&self) -> &[CompletionMessage] {
        &self.messages
    }

    /// The conversation, consumed — the usual way to get history back out of a
    /// finished turn.
    #[must_use]
    pub fn into_messages(self) -> Vec<CompletionMessage> {
        self.messages
    }

    /// The conversation, mutably. Appending between steps is safe and is how a
    /// host injects an out-of-band observation. REWRITING an existing message
    /// in place is not: two memos are keyed by message position, so call
    /// [`mark_transcript_rewritten`](Self::mark_transcript_rewritten)
    /// afterwards or they will serve digests for bytes that moved.
    pub fn messages_mut(&mut self) -> &mut Vec<CompletionMessage> {
        &mut self.messages
    }

    /// Declare that the transcript was rewritten in place rather than appended
    /// to, invalidating both position-keyed memos. The engine calls this for
    /// every rewrite it performs; a host only needs it after its own.
    pub fn mark_transcript_rewritten(&mut self) {
        self.memos.mark_rewritten();
        self.transcript_rewrites = self.transcript_rewrites.saturating_add(1);
    }

    /// The money meter, including session-scoped spend.
    #[must_use]
    pub fn budget(&self) -> &BudgetGuard {
        &self.budget
    }

    /// This turn's spend so far, in USD — the number the outcome reports.
    #[must_use]
    pub fn total_cost_usd(&self) -> f64 {
        self.total_cost_usd
    }

    /// The index of the step that will run next.
    #[must_use]
    pub fn step(&self) -> usize {
        self.step
    }

    /// A clone of this turn's stop signal, for whatever watches for shutdown.
    #[must_use]
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Drive this turn from an existing token instead of the fresh one every
    /// constructor mints — for a host that cancels a whole session's turns
    /// from one signal.
    #[must_use]
    pub fn with_cancel_token(mut self, cancel: CancelToken) -> Self {
        self.cancel = cancel;
        self
    }

    /// The cancellation check `run_step` runs at its safe boundaries: `None`
    /// when the turn may proceed, otherwise the clean abort.
    ///
    /// Closing any open `tool_use` before returning mirrors the budget-abort
    /// discipline in `Engine::handle_committed_result`. At a step boundary the
    /// engine's own path never leaves one open — `dispatch_completion` appends
    /// the results before it returns — so this only ever fires on history a
    /// caller handed in (or appended to) mid-turn. It is cheap, and the
    /// alternative is a resumed session whose first provider call is rejected
    /// outright for an unanswered `tool_use`.
    ///
    /// No `Error` event: a cancellation is a decision, not a failure, exactly
    /// as the soft stop is. The `Aborted` outcome carries [`CANCELLED_REASON`]
    /// and that is what a host renders.
    pub(crate) fn cancel_outcome(&mut self, events: &EventSender) -> Option<StepOutcome> {
        if !self.cancel.is_cancelled() {
            return None;
        }
        close_open_tool_calls(&mut self.messages, CANCELLED_TOOL_RESULT, events);
        Some(StepOutcome::Aborted {
            reason: CANCELLED_REASON.to_string(),
            cost_usd: self.total_cost_usd,
        })
    }
}

/// Append synthetic error `tool_result`s for every `tool_use` in `messages`
/// that nothing has answered, so the history stays valid for reuse.
///
/// Pairing follows the same rule as `driver::loop_evidence`: a result claims
/// the most recent still-unanswered call with a matching `call_id`, because
/// providers only guarantee ids unique within one response and two of them
/// mint `call_{ordinal}` fresh on every step.
///
/// The results are mirrored onto the event stream (no `ToolStart` — these
/// calls never ran) so a transcript reconstructed from events resolves every
/// announced call the same way `messages` does.
///
/// Shared by the two places a turn can stop holding an unanswered call: the
/// cancellation check above, and the budget abort in
/// `Engine::handle_committed_result` (which appends the assistant message
/// carrying the calls it is about to refuse to dispatch, then closes them
/// here). They differ only in `message`. Keeping one implementation matters
/// because the failure mode is silent and delayed — a missed pairing is not
/// noticed until the NEXT provider call rejects the history outright.
pub(crate) fn close_open_tool_calls(
    messages: &mut Vec<CompletionMessage>,
    message: &str,
    events: &EventSender,
) {
    let tool_results: Vec<ToolResult> = {
        let mut unanswered: Vec<&ToolCall> = Vec::new();
        for entry in messages.iter() {
            match entry.role {
                MessageRole::Assistant => unanswered.extend(entry.tool_calls.iter()),
                MessageRole::Tool => {
                    for result in &entry.tool_results {
                        if let Some(position) = unanswered
                            .iter()
                            .rposition(|call| call.call_id == result.call_id)
                        {
                            unanswered.remove(position);
                        }
                    }
                }
                MessageRole::System | MessageRole::User => {}
            }
        }
        unanswered
            .into_iter()
            .map(|call| ToolResult {
                call_id: call.call_id.clone(),
                output: ToolOutput::Error {
                    message: message.to_string(),
                },
            })
            .collect()
    };
    if tool_results.is_empty() {
        return;
    }
    for tool_result in &tool_results {
        let _ = events.send(AgentEvent::ToolResult {
            call_id: tool_result.call_id.clone(),
            output: tool_result.output.clone(),
            duration_ms: 0,
            speculated: false,
        });
    }
    messages.push(CompletionMessage {
        role: MessageRole::Tool,
        content: String::new(),
        tool_calls: Vec::new(),
        tool_results,
        attachments: Vec::new(),
    });
}

/// The wire version of [`Checkpoint`]. Bumped when a field's meaning changes;
/// [`Checkpoint::from_json`] refuses anything it does not recognize rather
/// than silently resuming a turn from a shape it half-understands.
pub const CHECKPOINT_VERSION: u32 = 1;

/// A serde-serializable snapshot of a [`TurnState`], sufficient to resume the
/// turn in another process.
///
/// A plain struct, not an ad-hoc `serde_json::Value`: this crate's
/// `serde_json` has no `preserve_order`, so a `json!`-built object would come
/// back with its keys sorted and the round-trip would not be byte-identical.
/// Field order here IS the wire order, and
/// `checkpoint_round_trips_byte_identically` pins it.
///
/// # What is in one
///
/// [`Self::messages`] is the whole conversation, verbatim — which means the
/// full content of every file the agent read, including whatever was in those
/// files. That is deliberate (the engine's next model call needs the bytes),
/// but it makes a checkpoint as sensitive as the workspace it was taken from.
///
/// Anything that moves one somewhere the workspace is not — a host, a server, a
/// support bundle, a log — is making an egress decision, and should make it
/// knowing that. Stated here rather than left to be discovered, because this is
/// the type someone reads before writing the code that moves it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// [`CHECKPOINT_VERSION`] at the time of writing.
    pub version: u32,
    /// The index of the step that runs next on resume.
    pub step: usize,
    /// The whole conversation. Stored verbatim rather than as a digest: the
    /// engine's next model call needs the bytes, and a host that already has
    /// them elsewhere can strip this field and re-attach it on resume.
    pub messages: Vec<CompletionMessage>,
    /// The money meter's state, on both axes.
    pub budget: BudgetSnapshot,
    /// This turn's spend so far, in USD.
    pub total_cost_usd: f64,
    /// The model that served the last committed step, for drift correction.
    pub calibration_model: Option<String>,
    /// Whether this turn already spent its one stuck-loop steering warning.
    /// Restored so a resumed turn cannot earn a second warning for the same
    /// loop it was already told about.
    pub loop_steered: bool,
    /// How many in-place transcript rewrites this turn has performed.
    /// Informational on resume — the live revision counter restarts at zero
    /// because every memo keyed by it is rebuilt too (see
    /// [`TurnState::from_checkpoint`]) — but it is the only record of how
    /// much compaction a turn has done, so it is carried rather than dropped.
    pub transcript_rewrites: u64,
}

impl Checkpoint {
    /// Encode as JSON. Deterministic: every field is a struct field, so key
    /// order is declaration order on every encode.
    pub fn to_json(&self) -> Result<String, CheckpointError> {
        serde_json::to_string(self).map_err(CheckpointError::Encode)
    }

    /// Decode from JSON, refusing a version this build does not understand.
    pub fn from_json(json: &str) -> Result<Self, CheckpointError> {
        let checkpoint: Checkpoint = serde_json::from_str(json).map_err(CheckpointError::Decode)?;
        if checkpoint.version != CHECKPOINT_VERSION {
            return Err(CheckpointError::Version {
                found: checkpoint.version,
                expected: CHECKPOINT_VERSION,
            });
        }
        Ok(checkpoint)
    }
}

/// Why a [`Checkpoint`] could not be encoded, decoded, or accepted.
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    /// The snapshot could not be serialized — a non-finite `f64` in the money
    /// fields is the only realistic cause.
    #[error("checkpoint could not be encoded: {0}")]
    Encode(#[source] serde_json::Error),
    /// The bytes were not a checkpoint.
    #[error("checkpoint could not be decoded: {0}")]
    Decode(#[source] serde_json::Error),
    /// The bytes were a checkpoint from a different build.
    #[error("checkpoint version {found} is not supported (this build reads version {expected})")]
    Version {
        /// The version the bytes claimed.
        found: u32,
        /// The version this build writes and reads.
        expected: u32,
    },
}

/// Where a durable host puts the [`Checkpoint`] a turn writes at each step
/// boundary.
///
/// The engine owns *when*: a checkpoint is written at the one moment the
/// transcript is guaranteed well-paired (no `tool_use` without its
/// `tool_result`), and discarded the moment the turn reaches a terminal
/// outcome, because resuming a turn that already ended would replay it. The
/// host owns *where* — this crate does no file I/O, and a sink can just as
/// well be a database row or an HTTP call.
///
/// # Failures are the sink's to absorb
///
/// Neither method returns a `Result`, and that is deliberate. A checkpoint
/// that cannot be written must never fail the turn that produced it: without
/// one the turn is exactly as durable as it was before (its prompt is
/// re-queued and the work is re-run), so a full disk would be trading a
/// recoverable turn for a dead one. Sinks that want the failure seen should
/// report it out of band — once, not per step, since this is called on every
/// step of every turn.
///
/// Implementations are invoked on the turn's own task and block it, so
/// `persist` must be cheap. The file-backed sink is one temp+fsync+rename.
///
/// # What a sink is being handed
///
/// The bytes are a whole conversation, so they carry the full content of every
/// file the agent read — see [`Checkpoint`]. Choosing where a sink puts them is
/// choosing where that content lives.
pub trait CheckpointSink: Send + Sync + std::fmt::Debug {
    /// Record `json` as the resume point for this turn, replacing any earlier
    /// one. `json` is [`Checkpoint::to_json`] output and is a complete
    /// snapshot, never a delta.
    fn persist(&self, json: &str);

    /// Drop the resume point — the turn reached a terminal outcome.
    ///
    /// Must be idempotent: a turn that never checkpointed (it ended on its
    /// first step) still discards on the way out, and a crash between
    /// `persist` and `discard` leaves a stale point that the next `discard`
    /// clears.
    fn discard(&self);
}

/// [`BudgetGuard`]'s state as data.
///
/// The guard keeps its fields private (it is a meter, not a record) and is not
/// itself `Serialize`, so this mirrors it through the public accessors and
/// rebuilds it through the public constructor. Keeping the mirror here rather
/// than deriving `Serialize` on the guard is deliberate: a checkpoint is a
/// wire format with a version, and the meter should not have to keep its
/// private layout wire-stable to serve it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BudgetSnapshot {
    /// Off / observed / enforced.
    pub mode: BudgetMode,
    /// The per-turn cap, if any.
    pub turn_limit_usd: Option<f64>,
    /// The per-session cap, if any.
    pub session_limit_usd: Option<f64>,
    /// Spend since the last `begin_turn`.
    pub turn_spent_usd: f64,
    /// Spend since the guard was constructed.
    pub session_spent_usd: f64,
}

impl BudgetSnapshot {
    /// Mirror a live guard.
    #[must_use]
    pub fn of(budget: &BudgetGuard) -> Self {
        Self {
            mode: budget.mode(),
            turn_limit_usd: budget.turn_limit_usd(),
            session_limit_usd: budget.session_limit_usd(),
            turn_spent_usd: budget.spent_usd(),
            session_spent_usd: budget.session_spent_usd(),
        }
    }

    /// Rebuild a guard with both axes exactly where they were.
    ///
    /// `record_spend` moves both axes together, so the turn axis is set first
    /// and the session axis is then overwritten with the seam the resume path
    /// already exists for (`reseed_session_spend`) — the same two calls
    /// `stella resume` makes when it reopens a session.
    #[must_use]
    pub fn restore(&self) -> BudgetGuard {
        let mut budget = BudgetGuard::new(self.mode, self.turn_limit_usd, self.session_limit_usd);
        let _ = budget.record_spend(self.turn_spent_usd);
        budget.reseed_session_spend(self.session_spent_usd);
        budget
    }
}

/// A [`TurnState`] that writes its transcript and its meter back into the
/// caller's borrowed ones when it is dropped.
///
/// `Engine::run_turn` borrows `&mut Vec<CompletionMessage>` and
/// `&mut BudgetGuard` and always has; `TurnState` owns both. Rather than
/// copying back at each of the loop's exits — where a missed path silently
/// loses a turn's history — the copy-back is a `Drop`, so it also runs on the
/// path that has no exit at all: a caller-side HARD CANCEL that drops the turn
/// future mid-step. That is what keeps the documented hard-drop contract
/// intact (the caller still sees the partial, possibly unpaired history it has
/// always seen, and still owns truncating it).
pub(crate) struct BorrowedTurn<'a> {
    messages: &'a mut Vec<CompletionMessage>,
    budget: &'a mut BudgetGuard,
    pub(crate) state: TurnState,
}

impl<'a> BorrowedTurn<'a> {
    /// Take ownership of the caller's transcript and meter for the duration of
    /// one turn.
    pub(crate) fn adopt(
        messages: &'a mut Vec<CompletionMessage>,
        budget: &'a mut BudgetGuard,
        config: &EngineConfig,
    ) -> Self {
        let state = TurnState::new(std::mem::take(messages), *budget, config);
        Self {
            messages,
            budget,
            state,
        }
    }
}

impl Drop for BorrowedTurn<'_> {
    fn drop(&mut self) {
        *self.messages = std::mem::take(&mut self.state.messages);
        *self.budget = self.state.budget;
    }
}

/// Consecutive overflow-summarizer failures this turn that trip the give-up
/// latch ([`SummarizerHealth`]). Each failed attempt is a wasted completion
/// and its latency; past this many in a row the pass stops re-firing and
/// lets the next model call surface one clear overflow instead of N.
pub(crate) const SUMMARIZER_FAILURE_LATCH: u32 = 2;

/// Per-turn health of the overflow summarizer. A cheap summarizer model that
/// keeps erroring, timing out, or returning nothing must not re-fire every
/// remaining step of the turn: this latches after
/// [`SUMMARIZER_FAILURE_LATCH`] consecutive non-progress results, and a
/// successful splice clears it. Per-turn, and one of the memos a checkpoint
/// drops — a resumed turn gets a fresh chance at summarizing, which is the
/// right default when the resume may be minutes later against a recovered
/// provider.
#[derive(Default)]
pub(crate) struct SummarizerHealth {
    pub(crate) consecutive_failures: u32,
}

impl SummarizerHealth {
    pub(crate) fn record_failure(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
    }

    pub(crate) fn reset(&mut self) {
        self.consecutive_failures = 0;
    }

    pub(crate) fn is_latched(&self) -> bool {
        self.consecutive_failures >= SUMMARIZER_FAILURE_LATCH
    }
}

/// What one compaction pass did: what it cost, and whether it rewrote the
/// transcript in place.
///
/// `#[must_use]` is load-bearing. `rewrote` is the only signal that the two
/// position-keyed memos — the loop detector's result identities and the receipt
/// ledger's block digests — must drop their keys, and a caller that ignored it
/// would leave both serving digests for bytes that no longer exist. Making the
/// return value impossible to discard silently turns "remember to invalidate"
/// from a convention into a compile error.
#[must_use]
pub(crate) struct CompactionPass {
    /// The summarizer's spend, if the overflow fallback ran; zero otherwise.
    pub(crate) cost_usd: f64,
    /// True when a pass stubbed, aged, superseded or spliced anything.
    pub(crate) rewrote: bool,
}

// ─────────────────────────── abandoned work stays accounted ───────────────────
//
// Three pieces of the machinery that decides what happens to a step's work
// when nobody is going to wait for it: the generation deadline (the call is
// taking longer than any answer is worth), and the two drop guards that fire
// on the hard-drop path this module's docs argue against — one for a possibly
// billed model call, one for speculative tool work that already ran real I/O.
// Neither guard can prevent the loss; both make it visible.

/// Monotonic count of stream fragments a provider dispatch has delivered.
///
/// The one signal that separates "wedged" from "working": a provider still
/// emitting fragments is answering, however slowly. Cloned into the gate's
/// delta path and read by [`bounded_generation`]; `Relaxed` because the only
/// question asked of it is "did this change since I last looked", which no
/// ordering with other memory affects.
#[derive(Clone, Debug, Default)]
pub(crate) struct StreamProgress(Arc<AtomicU64>);

impl StreamProgress {
    pub(crate) fn record(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    fn count(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Bound one provider dispatch by [`EngineConfig::model_timeout`], measured as
/// **idle time** — time since the last streamed fragment — rather than total
/// call duration.
///
/// The deadline exists to close an unbounded wait on a provider that is not
/// answering. Total duration cannot express that: it also fires on a provider
/// that is answering *the whole time*, just slowly. That distinction stopped
/// being academic when reasoning models arrived — a single hard-task call at
/// high effort can legitimately stream for well past ten minutes, and a
/// wall-clock bound kills it mid-answer and reports it as a provider fault.
/// Idle time asks the question the deadline actually means: has anything
/// arrived recently?
///
/// A provider that streams nothing at all is bounded exactly as before, since
/// with no fragments the idle clock and the wall clock are the same clock.
///
/// The trip is [`ProviderError::Terminal`] on purpose. `Transport` is
/// retryable, so classifying it that way would hand a provider that is simply
/// not answering the same unbounded wait again once per attempt — multiplying
/// the very window the deadline exists to close. `stella-serve`'s reverse-RPC
/// deadline made the same call for the same reason.
///
/// Wrapping the dispatch rather than the whole retry future means the bound is
/// per *generation*: backoff sleeps between attempts are not charged against
/// it, and a slow-but-progressing provider is never cut off by time another
/// attempt already spent.
pub(crate) async fn bounded_generation<F>(
    limit: Option<Duration>,
    progress: &StreamProgress,
    call: F,
) -> Result<CompletionResult, ProviderError>
where
    F: Future<Output = Result<CompletionResult, ProviderError>>,
{
    let Some(limit) = limit else {
        return call.await;
    };
    let mut call = std::pin::pin!(call);
    let mut seen = progress.count();
    loop {
        match tokio::time::timeout(limit, &mut call).await {
            Ok(result) => return result,
            Err(_) => {
                // The window elapsed. Whether that is a fault depends on what
                // arrived during it: any fragment at all means the provider is
                // answering, so re-arm and keep waiting. Only a window that
                // passed in complete silence is the wedge this bound is for.
                let now = progress.count();
                if now == seen {
                    return Err(ProviderError::Terminal(format!(
                        "generation stalled: no stream fragment for {}s (model deadline)",
                        limit.as_secs()
                    )));
                }
                seen = now;
            }
        }
    }
}

/// Drop guard for the paid-call window ([`Engine::run_model_call`]): armed
/// before the retried provider dispatch, disarmed on both normal exits. It
/// fires only when the turn future is dropped mid-await — the caller-side
/// hard cancel — AND a paid attempt was genuinely in flight
/// (`attempt_in_flight`, stored by the attempt closure exactly around its
/// dispatch), leaving one content-free `Cancelled` usage envelope so a
/// possibly-billed in-flight call never vanishes from the accounting stream.
///
/// The in-flight latch is what keeps a drop landing in a BACKOFF SLEEP
/// silent: no attempt is running then, and the failed attempt that preceded
/// the sleep already reported its own per-attempt `ProviderError` envelope —
/// a second `Cancelled` envelope for the same single dispatch would
/// double-report it. Mirrors `run_accounted_call`'s timeout-during-backoff
/// discipline.
///
/// Content-free by construction, same privacy rule as every other
/// `UsageIncomplete` envelope: no request or response body is representable.
pub(crate) struct CancelUsageGuard {
    pub(crate) events: EventSender,
    pub(crate) role: stella_protocol::ModelCallRole,
    pub(crate) provider: String,
    pub(crate) started: std::time::Instant,
    pub(crate) armed: bool,
    pub(crate) attempt_in_flight: Arc<AtomicBool>,
}

impl CancelUsageGuard {
    pub(crate) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelUsageGuard {
    fn drop(&mut self) {
        if !self.armed || !self.attempt_in_flight.load(Ordering::SeqCst) {
            return;
        }
        let _ = self.events.send(AgentEvent::UsageIncomplete {
            role: self.role,
            provider: self.provider.clone(),
            model: "unknown".into(),
            reason: stella_protocol::UsageIncompleteReason::Cancelled,
            duration_ms: self.started.elapsed().as_millis() as u64,
            retries: None,
            // A hard cancel drops the call future mid-flight, so no adapter
            // ever returned an error to salvage from. The server-side cost of
            // an abandoned call stays genuinely unknowable.
            partial: None,
        });
    }
}

/// Accumulates one attempt's completed speculative executions
/// (`crate::speculation`) and, if the pump future is dropped before its pool
/// is harvested, emits one `SpeculationDiscarded` per entry. The drop path
/// is a failed stream attempt (the retry builds a fresh pool) or a hard
/// cancel mid-drain: read-only work that already ran real I/O but whose
/// result never reaches the transcript. The committed path calls
/// [`Self::harvest`] to disarm the guard and hand the pool to dispatch,
/// where each entry is instead harvested or discarded per call (#370).
pub(crate) struct SpeculationDropGuard {
    pub(crate) events: EventSender,
    pub(crate) pool: SpeculationPool,
    pub(crate) armed: bool,
}

impl SpeculationDropGuard {
    /// Disarm and hand the accumulated pool to the committed dispatch path.
    pub(crate) fn harvest(&mut self) -> SpeculationPool {
        self.armed = false;
        std::mem::take(&mut self.pool)
    }
}

impl Drop for SpeculationDropGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        for (call_id, result) in self.pool.drain() {
            let _ = self.events.send(AgentEvent::SpeculationDiscarded {
                call_id,
                name: result.name,
                reason: SPECULATION_DISCARD_ATTEMPT_FAILED.to_string(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::ToolCall;

    fn checkpoint_fixture() -> Checkpoint {
        let mut budget = BudgetGuard::new(BudgetMode::Enforced, Some(2.5), Some(10.0));
        let _ = budget.record_spend(0.375);
        budget.reseed_session_spend(1.125);
        let messages = vec![
            CompletionMessage::system("you are stella"),
            CompletionMessage::user("read the file"),
            CompletionMessage {
                role: MessageRole::Assistant,
                content: "on it".into(),
                tool_calls: vec![ToolCall {
                    call_id: "c1".into(),
                    name: "read_file".into(),
                    input: serde_json::json!({"path": "a.rs"}),
                }],
                tool_results: Vec::new(),
                attachments: Vec::new(),
            },
            CompletionMessage {
                role: MessageRole::Tool,
                content: String::new(),
                tool_calls: Vec::new(),
                tool_results: vec![ToolResult {
                    call_id: "c1".into(),
                    output: ToolOutput::Ok {
                        content: "fn main() {}".into(),
                    },
                }],
                attachments: Vec::new(),
            },
        ];
        let state = TurnState::new(messages, budget, &EngineConfig::default());
        let mut state = state;
        state.total_cost_usd = 0.375;
        state.calibration_model = Some("glm-5.2".into());
        state.loop_steered = true;
        state.step = 3;
        state.mark_transcript_rewritten();
        state.mark_transcript_rewritten();
        state.to_checkpoint()
    }

    #[test]
    fn checkpoint_round_trips_byte_identically() {
        // serialize → deserialize → serialize must produce the same bytes.
        // The repo has no `preserve_order` on serde_json, so this is only true
        // because `Checkpoint` is a struct: a `json!`-built object would come
        // back key-sorted and fail here.
        let checkpoint = checkpoint_fixture();
        let first = checkpoint.to_json().expect("encode");
        let decoded = Checkpoint::from_json(&first).expect("decode");
        let second = decoded.to_json().expect("re-encode");
        assert_eq!(
            first, second,
            "checkpoint JSON must be stable across a round trip"
        );
        assert_eq!(checkpoint, decoded, "and the value must survive it");
    }

    #[test]
    fn a_restored_turn_state_carries_the_whole_checkpoint() {
        let checkpoint = checkpoint_fixture();
        let state = TurnState::from_checkpoint(checkpoint.clone(), &EngineConfig::default());

        assert_eq!(state.step(), checkpoint.step);
        assert_eq!(state.messages(), checkpoint.messages.as_slice());
        assert!((state.total_cost_usd() - checkpoint.total_cost_usd).abs() < 1e-12);
        assert_eq!(state.calibration_model.as_deref(), Some("glm-5.2"));
        assert!(
            state.loop_steered,
            "the spent loop steer must not be re-earned"
        );
        assert_eq!(state.transcript_rewrites, checkpoint.transcript_rewrites);
        // And re-snapshotting the restored turn reproduces the checkpoint, so
        // resume → checkpoint → resume is a fixed point.
        assert_eq!(state.to_checkpoint(), checkpoint);
    }

    #[test]
    fn a_restored_budget_meters_from_exactly_where_it_stopped() {
        let checkpoint = checkpoint_fixture();
        let restored = checkpoint.budget.restore();
        assert_eq!(restored.mode(), BudgetMode::Enforced);
        assert_eq!(restored.turn_limit_usd(), Some(2.5));
        assert_eq!(restored.session_limit_usd(), Some(10.0));
        assert!((restored.spent_usd() - 0.375).abs() < 1e-12, "turn axis");
        assert!(
            (restored.session_spent_usd() - 1.125).abs() < 1e-12,
            "session axis"
        );
    }

    #[test]
    fn a_checkpoint_from_another_build_is_refused_not_half_understood() {
        let mut checkpoint = checkpoint_fixture();
        checkpoint.version = CHECKPOINT_VERSION + 7;
        let json = checkpoint.to_json().expect("encode");
        match Checkpoint::from_json(&json) {
            Err(CheckpointError::Version { found, expected }) => {
                assert_eq!(found, CHECKPOINT_VERSION + 7);
                assert_eq!(expected, CHECKPOINT_VERSION);
            }
            other => panic!("expected a version refusal, got {other:?}"),
        }
        assert!(matches!(
            Checkpoint::from_json("{ not json"),
            Err(CheckpointError::Decode(_))
        ));
    }

    #[test]
    fn a_cancel_closes_every_open_tool_use_so_the_history_stays_reusable() {
        // The shape a provider rejects outright: an assistant `tool_use` with
        // no answering `tool_result`.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        let mut state = TurnState::new(
            vec![
                CompletionMessage::user("go"),
                CompletionMessage {
                    role: MessageRole::Assistant,
                    content: String::new(),
                    tool_calls: vec![
                        ToolCall {
                            call_id: "c1".into(),
                            name: "bash".into(),
                            input: serde_json::json!({}),
                        },
                        ToolCall {
                            call_id: "c2".into(),
                            name: "bash".into(),
                            input: serde_json::json!({}),
                        },
                    ],
                    tool_results: Vec::new(),
                    attachments: Vec::new(),
                },
            ],
            BudgetGuard::new(BudgetMode::Off, None, None),
            &EngineConfig::default(),
        );
        assert!(
            state.cancel_outcome(&events).is_none(),
            "an un-cancelled token must not touch the transcript"
        );

        state.cancel_token().cancel();
        let outcome = state.cancel_outcome(&events).expect("cancelled");
        assert!(matches!(
            outcome,
            StepOutcome::Aborted { ref reason, .. } if reason == CANCELLED_REASON
        ));

        let closing = state.messages().last().expect("a closing Tool message");
        assert_eq!(closing.role, MessageRole::Tool);
        let ids: Vec<&str> = closing
            .tool_results
            .iter()
            .map(|r| r.call_id.as_str())
            .collect();
        assert_eq!(ids, ["c1", "c2"], "every open call is closed, in order");
        assert!(
            closing
                .tool_results
                .iter()
                .all(|r| matches!(&r.output, ToolOutput::Error { message } if message == CANCELLED_TOOL_RESULT))
        );

        // Mirrored onto the event stream, so an events-only reconstruction of
        // the transcript resolves the same calls.
        let mut seen = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::ToolResult { call_id, .. } = event {
                seen.push(call_id);
            }
        }
        assert_eq!(seen, ["c1", "c2"]);
    }

    #[test]
    fn closing_open_calls_is_a_no_op_on_a_well_paired_transcript() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        let checkpoint = checkpoint_fixture();
        let mut state = TurnState::from_checkpoint(checkpoint, &EngineConfig::default());
        let before = state.messages().len();
        state.cancel_token().cancel();
        let _ = state.cancel_outcome(&events);
        assert_eq!(
            state.messages().len(),
            before,
            "nothing to close means nothing appended"
        );
    }

    #[test]
    fn a_cloned_token_cancels_the_turn_it_came_from() {
        let state = TurnState::new(
            Vec::new(),
            BudgetGuard::new(BudgetMode::Off, None, None),
            &EngineConfig::default(),
        );
        let handed_off = state.cancel_token();
        assert!(!state.cancel.is_cancelled());
        handed_off.cancel();
        assert!(state.cancel.is_cancelled(), "a clone shares the flag");
    }
}
