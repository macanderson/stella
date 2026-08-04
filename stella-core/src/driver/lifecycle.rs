// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Payload shaping for the `HookBus` lifecycle events the driver emits
//! (#1133).
//!
//! Separated from the emit sites deliberately: *where* an event fires is a
//! property of the engine's control flow and belongs next to the boundary,
//! but *what* it carries is a contract with observers — and that contract has
//! one rule worth isolating so it is hard to break by accident.
//!
//! # The rule: counts and identifiers, never transcript content
//!
//! These events are observability, and an observability stream that carries
//! prompts and answers is a second copy of the conversation in whatever an
//! extension does with it — a log file, a metrics backend, a webhook. The
//! transcript already has a considered exposure story; this must not quietly
//! open a parallel one.
//!
//! So: message *counts*, token *counts*, cost, model id, step index. The one
//! free-text field anywhere here is an abort `reason`, which is the engine's
//! own diagnosis of why a turn ended and already reaches `AgentEvent::Error`
//! and the transcript.
//!
//! # Why the turn-boundary shapers are public
//!
//! `Engine::run_turn_with_sender` is not the only turn driver: a durable host
//! loops over [`Engine::run_step`](super::Engine::run_step) itself, and owns
//! the turn framing that loop does not carry ("The step-scoped facade" in
//! `stella-engine`). `stella-serve` is that host. Its turn therefore has to
//! emit the same two boundary events with the same payload shape, or an
//! extension watching the bus can tell a served turn from a local one — which
//! is precisely the drift the shared `run_step` exists to prevent. Sharing the
//! shaper is what keeps it one contract rather than two spellings of one.

use super::{StepOutcome, TurnOutcome};

/// What `agent.turn.started` carries: how much conversation the turn opened
/// with, the loop bound it runs under, and which role is driving it. No
/// message *content* — see the module docs.
pub fn turn_started_payload(
    message_count: usize,
    max_steps: usize,
    role: stella_protocol::ModelCallRole,
) -> serde_json::Value {
    serde_json::json!({
        "message_count": message_count,
        "max_steps": max_steps,
        "role": role,
    })
}

/// What `model.request.started` carries.
///
/// A message *count* says how large the request was without saying what was in
/// it, which is what an observer applying policy at this boundary actually
/// needs.
pub(super) fn model_request_started_payload(
    step: usize,
    message_count: usize,
    role: stella_protocol::ModelCallRole,
) -> serde_json::Value {
    serde_json::json!({
        "step": step,
        "message_count": message_count,
        "role": role,
    })
}

/// What `model.request.completed` carries: the model that served the call and
/// what it cost, in tokens and dollars. Tool calls are *counted*, never named
/// — the names and their arguments are the tool plane's own events.
pub(super) fn model_request_completed_payload(
    step: usize,
    result: &stella_protocol::CompletionResult,
) -> serde_json::Value {
    serde_json::json!({
        "step": step,
        "model": result.model,
        "cost_usd": result.cost_usd,
        "input_tokens": result.usage.input_tokens,
        "output_tokens": result.usage.output_tokens,
        "tool_calls": result.tool_calls.len(),
    })
}

/// What `agent.turn.completed` carries.
///
/// The reason string rides only the *aborted* arm: a completed turn's answer
/// text is transcript content and has no business on an observability event,
/// while an abort reason is the engine's own diagnosis and is what an
/// observer is watching this boundary for.
pub fn turn_outcome_payload(outcome: &TurnOutcome, steps: usize) -> serde_json::Value {
    match outcome {
        TurnOutcome::Completed { cost_usd, .. } => serde_json::json!({
            "outcome": "completed",
            "steps": steps,
            "cost_usd": cost_usd,
        }),
        TurnOutcome::Aborted { reason, cost_usd } => serde_json::json!({
            "outcome": "aborted",
            "steps": steps,
            "cost_usd": cost_usd,
            "reason": reason,
        }),
    }
}

/// How one step ended, as the lifecycle stream names it.
///
/// Three labels rather than the full reason string: an observer pairing
/// `agent.step.started` with `agent.step.completed` wants to know whether the
/// turn continues, and the reason a turn *ended* is already carried by
/// `agent.turn.completed`. Repeating it here would put a model-authored or
/// provider-authored string on two events instead of one.
pub(super) fn step_outcome_label(outcome: &StepOutcome) -> &'static str {
    match outcome {
        StepOutcome::Continue => "continue",
        StepOutcome::Done { .. } => "done",
        StepOutcome::Aborted { .. } => "aborted",
    }
}
