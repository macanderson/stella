// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella-engine` — the step-scoped facade over `stella-core` (#971).
//!
//! [`Engine::run_turn`] drives a whole turn and returns when it is over. That
//! is the right shape for a CLI and the wrong one for a durable host: Oxagen's
//! runner (and `stella-serve`) has to persist progress between steps, stop a
//! turn on a deadline or a shutdown, and resume it in a different process.
//!
//! This crate exposes the same engine one step at a time:
//!
//! ```no_run
//! use stella_engine::{
//!     BudgetGuard, BudgetMode, CancelToken, Engine, EventSender, StepOutcome, step_cap_reason,
//! };
//! # async fn drive(engine: &Engine<'_>, events: &EventSender, save: impl Fn(&str))
//! # -> Option<String> {
//! let messages = Vec::new();
//! let budget = BudgetGuard::new(BudgetMode::Off, None, None);
//! let cancel = CancelToken::new();
//! let mut state = engine.new_turn(messages, budget).with_cancel_token(cancel.clone());
//!
//! loop {
//!     // The step cap is the HOST's loop bound when driving steps directly —
//!     // `run_turn` applies it internally, but `run_step` deliberately does
//!     // not (the cap is the host's, exposed via [`Engine::max_steps`] so it
//!     // never keeps a second copy of the number). Without this gate a model
//!     // that keeps varying its arguments — the shape loop detection
//!     // deliberately ignores — drives the loop forever. A real host also
//!     // emits the non-retryable `Error` event `run_turn` pairs with this
//!     // exit (see `stella-serve`'s session loop for the production pattern).
//!     if state.step() >= engine.max_steps() {
//!         eprintln!("turn ended: {}", step_cap_reason(engine.max_steps()));
//!         return None;
//!     }
//!     match engine.run_step(&mut state, events).await {
//!         StepOutcome::Continue => {
//!             // The one moment the transcript is guaranteed well-paired.
//!             if let Ok(json) = state.to_checkpoint().to_json() {
//!                 save(&json);
//!             }
//!         }
//!         StepOutcome::Done { text, .. } => return Some(text),
//!         StepOutcome::Aborted { reason, .. } => {
//!             eprintln!("turn ended: {reason}");
//!             return None;
//!         }
//!     }
//! }
//! # }
//! ```
//!
//! Nothing here re-implements the loop. [`Engine::run_turn`] is itself a loop
//! over [`Engine::run_step`], so a host driving steps and a CLI driving turns
//! run the identical per-step code and emit the identical per-step
//! [`AgentEvent`] sequence. What `run_turn` adds is turn *framing* the host
//! owns for itself in this mode: the `Stage(Execute)` event before the first
//! step, the step-cap gate above, and — on that exit — a non-retryable `Error`
//! carrying [`step_cap_reason`].
//!
//! # Stopping a turn: cancel, do not drop
//!
//! [`CancelToken`] is the supported stop. It is read at the top of every step —
//! the same safe boundary the pause gate, the soft stop and the budget
//! enforcer already use — so the step that was running finishes and commits
//! first. The turn then ends with [`TurnOutcome::Aborted`] carrying
//! [`CANCELLED_REASON`], every completed step is kept, and the transcript is
//! valid to hand straight back to the next turn.
//!
//! Dropping the turn future also stops it, immediately, and that immediacy is
//! the cost:
//!
//! - **A tool may be mid-mutation.** The engine appends the assistant
//!   `tool_use` message before dispatching and the answering `tool_result`
//!   only after, so a drop in that window leaves an unpaired `tool_use` in the
//!   borrowed history. The next provider call rejects it outright (`tool_use`
//!   must be followed by `tool_result`). A caller that keeps a hard-dropped
//!   turn's history has to close the pairing itself — which is exactly what
//!   the cancel path does for you.
//! - **A model call may already be billed.** All that survives is the one
//!   content-free `UsageIncomplete { Cancelled }` envelope the engine's drop
//!   guard emits, so the spend stays visible but the result is gone.
//! - **Speculative read-only work is lost.** Each completed speculation emits
//!   one `SpeculationDiscarded` on the way out; the I/O it did is accounted,
//!   not recovered.
//! - **Everything not yet checkpointed is gone**, including the current step's
//!   partial progress.
//!
//! Prefer the token. Reach for a drop only when you cannot wait for the
//! in-flight step, and expect to discard that turn's history when you do.
//!
//! # The turn future is `!Send`
//!
//! A step borrows `&dyn Provider` / `&dyn ToolExecutor` and holds non-`Send`
//! futures across its awaits, so neither [`Engine::run_step`] nor
//! [`Engine::run_turn`] can be `tokio::spawn`ed onto a multi-thread runtime.
//! Drive them on a **current-thread** runtime. A host serving many sessions
//! gives each session its own OS thread with its own current-thread runtime
//! and bridges to the async world with `Send` channels — the pattern
//! `stella-cli`'s fleet worker already uses. Spawning instead is a compile
//! error rather than a subtle bug, but it is the first thing a host author
//! hits, so it is stated here rather than discovered.
//!
//! # This crate performs no I/O
//!
//! It re-exports and documents; it opens no sockets, spawns no processes and
//! touches no files, exactly like the `stella-core` it fronts. Everything the
//! engine needs from the outside world arrives through the ports
//! ([`Provider`], [`ToolExecutor`], [`TurnGate`], [`TurnSteering`]), which the
//! host implements.

#![forbid(unsafe_code)]

// ── the step-scoped API ───────────────────────────────────────────────────
pub use stella_core::step::{
    BudgetSnapshot, CANCELLED_REASON, CHECKPOINT_VERSION, CancelToken, Checkpoint, CheckpointError,
    StepOutcome, TurnState,
};

// ── the engine those steps run on ─────────────────────────────────────────
pub use stella_core::driver::{
    Engine, EngineConfig, SOFT_STOP_REASON, TurnOutcome, step_cap_reason,
};
pub use stella_core::estimator::CalibrationMap;
pub use stella_core::event_sender::{EventSendError, EventSender};

// ── what a host has to supply, and what it gets back ──────────────────────
pub use stella_core::budget::{BudgetAxis, BudgetGuard, BudgetOutcome};
pub use stella_core::ports::{ToolExecutor, TurnGate, TurnSteering};
pub use stella_core::retry::{RetryPolicy, Sleeper};
pub use stella_protocol::{
    AgentEvent, BudgetMode, CompletionMessage, MessageRole, Provider, ProviderError, ToolCall,
    ToolOutput, ToolResult, ToolSchema,
};

/// Encode a checkpoint for durable storage.
///
/// A thin alias for [`Checkpoint::to_json`], present so a host can hand
/// `stella_engine::encode_checkpoint` to a storage boundary without reaching
/// for the type's inherent method. Deterministic: [`Checkpoint`] is a struct,
/// so key order is declaration order on every encode — which is what lets a
/// content-addressed store see an unchanged turn as unchanged.
pub fn encode_checkpoint(checkpoint: &Checkpoint) -> Result<String, CheckpointError> {
    checkpoint.to_json()
}

/// Decode a checkpoint written by [`encode_checkpoint`], refusing a
/// [`CHECKPOINT_VERSION`] this build does not understand rather than resuming
/// a turn from a shape it half-recognizes.
pub fn decode_checkpoint(json: &str) -> Result<Checkpoint, CheckpointError> {
    Checkpoint::from_json(json)
}

#[cfg(test)]
mod tests;
