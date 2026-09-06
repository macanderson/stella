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
//!     // A step cap is the HOST's loop bound when driving steps directly.
//!     // `run_turn` applies it; `run_step` does not, so the host reads it
//!     // from [`Engine::max_steps`] rather than keeping its own copy. The
//!     // default config carries none (ADR 0030): a turn ends on evidence,
//!     // never on a count. A real host also emits the non-retryable `Error`
//!     // event `run_turn` pairs with this exit (see `stella-serve`'s
//!     // session loop for the production pattern).
//!     if let Some(cap) = engine.max_steps()
//!         && state.step() >= cap
//!     {
//!         eprintln!("turn ended: {}", step_cap_reason(cap));
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
//! engine needs from the outside world arrives through the ports, which the
//! host implements: [`Sleeper`], which [`Engine::with_sleeper`] requires
//! because it is the only constructor, and then [`Provider`],
//! [`ToolExecutor`], [`TurnGate`], [`TurnSteering`], [`SteeringRequery`],
//! [`TurnHalt`], [`ProviderOutcomes`], [`FallbackResolver`] and
//! [`CheckpointSink`], each of which is optional.
//!
//! # What earns a re-export: the closure rule
//!
//! This is the crate's whole editorial policy, and `README.md` states it in
//! the same words. A narrower rule in one of the two places is what leaves a
//! builder reachable through the facade and uncallable through it.
//!
//! **The rule.** A host must be able to write, naming nothing but
//! `stella_engine::` paths:
//!
//! 1. an `impl` of every port this facade's engine accepts,
//! 2. a construction of every value it accepts — every [`EngineConfig`] field
//!    and every builder argument, and
//! 3. a `match` on every value it hands back that the host must branch on.
//!
//! Closure is transitive through those three obligations and stops where they
//! stop. [`GenerationParams`] is re-exported because a host fills that config
//! field, so [`Verbosity`] and [`ServiceTier`] come with it; [`AgentEvent`] is
//! re-exported because a host receives one, and its payload types are not,
//! because a host forwards or serializes an event rather than constructing
//! one. A facade closed over mere reachability would be `stella-protocol`
//! with extra steps.
//!
//! This is a coherence property of what is *already* exported, not a widening
//! for an imagined caller — the distinction that separates it from #2481,
//! which asked for turn-boundary payload shapers no exported signature names
//! and was closed as speculative. Nothing below adds a capability; each entry
//! makes an already-reachable one writable.
//!
//! ## The hook plane is the wrong layer, by design
//!
//! Two of [`Engine`]'s builder methods are **not** closed over:
//!
//! - **[`Engine::with_hooks`]** (`stella_core::hooks::{Hooks, HookRunner}`)
//!   and **`Engine::with_bus`** (`stella_core::bus::HookBus`). Their closure
//!   is the shell-command hook plane — `HookAction`, `HookExecResult`,
//!   `HookExecError`, `HookMatcher`, `HookEvent`, `HookDecision`,
//!   `HookEventDraft` — an extension surface whose whole purpose is to
//!   *execute* things, fronted by a crate that inherits `stella-core`'s
//!   I/O-free posture. The supported host-extension door is
//!   `stella-runtime`'s wrapper socket (#3380,
//!   `stella_runtime::wrapper::WrapperDispatch::bind`), which lives one layer
//!   above this facade precisely because two of its four points do I/O.
//!
//! #3768 asked whether that is a permanent exclusion, whether the observer
//! half (`with_bus`) should cross, or whether both should. **The answer is
//! the first**, and this is the decision rather than a gap someone has yet to
//! close. Closing over either method would let a host reach the engine's
//! shell-execution authority by naming `stella_engine::` paths alone, which
//! is the one thing a facade over an I/O-free engine must not make look
//! ordinary.
//!
//! Nothing is stranded by that. `stella-serve` — the embedded host this crate
//! exists for, and the one that would have been the observer half's first
//! caller — already links `stella-core` directly and mints its bus from
//! `stella_core::bus::HookBus` (`stella-serve/src/extensions.rs`,
//! `session.rs`). A host that wants the plane takes the dependency that
//! carries it and says so in its own manifest.
//!
//! Enforced rather than declared: `tests/embedding.rs`'s
//! `the_hook_plane_is_still_outside_the_facade` stops compiling the moment
//! any of `Hooks`, `HookRunner`, `HookBus`, `HookDecision` or
//! `HookEventDraft` becomes reachable through `stella_engine::*`.

#![forbid(unsafe_code)]

// ── the step-scoped API ───────────────────────────────────────────────────
//
// `CheckpointSink` is here rather than only on `EngineConfig` because a
// durable host — the audience named in this crate's first paragraph — has to
// *implement* it, and `EngineConfig::checkpoint_sink` is an
// `Option<Arc<dyn CheckpointSink>>`. Re-exporting the config while withholding
// the trait it holds left the field unfillable through the facade (#1494);
// `stella-serve` reached around it with `use stella_core::step::CheckpointSink`.
//
// `AbortKind` is here for the same reason one layer up. Both
// [`StepOutcome::Aborted`] and [`TurnOutcome::Aborted`] carry it, and its own
// doc calls it "the half of `reason` a consumer may branch on" — so a host
// that cannot name it is left matching on the prose of `reason` to tell a
// `DeliberateStop` from a failure, which is AGENTS.md #5's defect wearing a
// facade (#3715).
pub use stella_core::step::{
    AbortKind, BudgetSnapshot, CANCELLED_REASON, CHECKPOINT_VERSION, CancelToken, Checkpoint,
    CheckpointError, CheckpointSink, StepOutcome, TurnState,
};

// ── the engine those steps run on ─────────────────────────────────────────
//
// `TurnHalt` is the `CheckpointSink` shape exactly: `EngineConfig::turn_halt`
// is an `Option<Arc<dyn TurnHalt>>` and `Engine::with_turn_halt` takes the
// same `Arc`, so withholding the trait left both unfillable. It is how a
// durable host says "the goal is already met" and ends the turn as a success
// rather than through the soft stop, which returns `Aborted` and reaches a
// harness as the agent crashing.
//
// `LoopDetectionConfig` and `SessionOutputCeilings` are the remaining
// `EngineConfig` fields whose types a host could not name.
pub use stella_core::driver::output_budget_recovery::SessionOutputCeilings;
pub use stella_core::driver::{
    Engine, EngineConfig, SOFT_STOP_REASON, TurnHalt, TurnOutcome, step_cap_reason,
};
// `Engine::assemble` is the blessed constructor, and its one non-port
// parameter is a `TurnCapabilities`. Without the type here a host could name
// the method and never call it — obligation 1 of the closure rule, read at the
// parameter. `OwnedTurnCapabilities` rides along because a host that builds
// its seams with no frame above holding them is the case that struct exists
// for, which is exactly an out-of-process host.
pub use stella_core::driver::capabilities::{OwnedTurnCapabilities, TurnCapabilities};
pub use stella_core::estimator::CalibrationMap;
pub use stella_core::event_sender::{EventSendError, EventSender};
pub use stella_core::loop_detect::LoopDetectionConfig;

// ── what a host has to supply, and what it gets back ──────────────────────
//
// Three of the ports here are builder methods the closure rule reaches that
// the pre-#3715 list did not: `SteeringRequery` (`Engine::with_requery`)
// together with the `TurnSignal` its one method takes — neither was nameable
// here, so the method could be called by nobody — plus `ProviderOutcomes` and
// `FallbackResolver`, whose `ResolvedFallback` return type borrows a
// `Provider` this crate already exports.
//
// `DeadlineOutcome` is obligation 3, not 1: a host that sets a task deadline
// reads `BudgetGuard::check_deadline` and must branch on the answer.
//
// `RECALL_MARKER` is obligation 1 read strictly: `SteeringRequery::requery`
// returns a `String`, so a host can write the signature without it — and
// cannot satisfy the port's contract without it, because the block it answers
// with must carry the marker or `driver::loop_evidence::turn_start_index`
// reads injected context as a user turn. Withholding the constant does not
// withhold the obligation; it only forces the host to hardcode the literal,
// which is the second-source-of-truth defect `Engine::max_steps` exists to
// avoid.
pub use stella_core::budget::{BudgetAxis, BudgetGuard, BudgetOutcome, DeadlineOutcome};
// The `ToolExecutor` half is obligation 1 read at the method level, which the
// two earlier passes did not reach. Four of its methods carry a
// "# Decorators MUST forward this" section, and a host that wraps its own
// executor — a tap, a filter, a policy layer — could name none of their types
// here: `WaitRequest`, `LiveService`, `DispatchGate` and `ToolContract`. Each
// has a trait default, so the wrapper compiles clean and silently takes the
// default instead of the inner executor's answer: parked waits are dropped and
// the model goes back to burning steps on polling, the end-of-turn
// live-service assertion stops firing, and a dispatching decorator above the
// tap finds no gate and goes ungated. `DispatchAdmission` and `admit_dispatch`
// come with the gate, because forwarding it is only half of what the port asks
// — a decorator that dispatches a name of its own has to run the admission.
pub use stella_core::ports::{
    DispatchAdmission, DispatchGate, FallbackResolver, LiveService, ProviderOutcomes,
    ResolvedFallback, SteeringRequery, ToolExecutor, TurnGate, TurnSteering, admit_dispatch,
};
pub use stella_core::receipts::RECALL_MARKER;
pub use stella_core::retry::{RetryPolicy, Sleeper};
pub use stella_core::steering::TurnSignal;
// `WaitCall` rides along because it is the type of `WaitRequest`'s public
// `probe` and `on_wake` fields: a host that can name the request and not its
// call can forward one but never build one, which is obligation 1 again one
// field deeper.
pub use stella_core::waiting::{WaitCall, WaitRequest};
// Obligation 1 of the closure rule (stated in full in this module's docs, and
// not restated here — a rule written twice is a rule that
// drifts). `Provider` alone was not enough — `complete_ref` takes a
// `CompletionRequestRef` and returns a `CompletionResult`, whose `usage` and
// `finish_reason` are a `CompletionUsage` and a `FinishReason`, and
// `complete_ref_observed` takes a `&dyn ToolCallObserver`. Without those, Mode-A
// embedding (docs/spec/engine-embedding.md) required linking `stella-protocol`
// as well, which is the thing the facade exists to avoid — and this crate's own
// tests proved it by importing them straight from `stella_protocol` (#1494).
//
// The second half of that block is obligation 2 applied to the values a host
// hands the engine rather than the ports it implements: `ModelCallRole` is
// what `Engine::call_role` returns and `with_call_role` takes;
// `ReasoningEffort` and `GenerationParams` are `EngineConfig` fields, and
// `Verbosity`/`ServiceTier` are the two types `GenerationParams` itself names;
// `Attachment` and its two companions are the public `attachments` field of
// the `CompletionMessage` a host builds for a multimodal turn.
pub use stella_protocol::{
    AgentEvent, Attachment, AttachmentKind, AttachmentSource, BudgetMode, CompletionMessage,
    CompletionRequestRef, CompletionResult, CompletionUsage, FinishReason, GenerationParams,
    MessageRole, ModelCallRole, Provider, ProviderError, ReasoningEffort, ServiceTier, ToolCall,
    ToolCallObserver, ToolContract, ToolOutput, ToolResult, ToolSchema, Verbosity,
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
