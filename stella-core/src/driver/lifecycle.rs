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

use super::{StepOutcome, TurnOutcome};

/// What `agent.turn.completed` carries.
///
/// The reason string rides only the *aborted* arm: a completed turn's answer
/// text is transcript content and has no business on an observability event,
/// while an abort reason is the engine's own diagnosis and is what an
/// observer is watching this boundary for.
pub(super) fn turn_outcome_payload(outcome: &TurnOutcome, steps: usize) -> serde_json::Value {
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
