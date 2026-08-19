//! Post-turn reflection at the CLI's turn boundaries: the interactive-turn
//! dispatch and the two report renderers.
//!
//! Split verbatim out of `agent.rs` — no behavior change in the move — because
//! that file sits at its file-size ratchet ceiling and this cluster is a
//! coherent unit: everything here is about what happens *after* a turn's last
//! model call, and nothing else in `agent.rs` calls into it.
//!
//! This module used to also carry `FrictionTap`, the event-stream fold that
//! gave the staged pipeline's reflection gate its tool-call evidence (the
//! pipeline path kept the worker's tool-calling turns out of `messages`, so
//! `turn_warrants_reflection(&messages)` alone was always false there). That
//! pipeline is gone from this build (#3865); the raw step-loop's `messages`
//! already carries every tool call, so `turn_warrants_reflection` alone is
//! sufficient for it and always was.

use colored::Colorize;
use stella_core::BudgetGuard;
use stella_protocol::{AgentEvent, CompletionMessage, Provider};

use super::{
    OutputFormat, ReflectionReport, SessionMemory, TurnEvidence, reflect_routed, remaining_budget,
    settle_reflection_budget, should_reflect_on, turn_warrants_reflection,
};
use crate::config::Config;

/// Post-turn reflection for one interactive REPL turn, shared by the plain
/// prompt handler and `/goal` — the two carried byte-identical copies of
/// this block, which is exactly the drift this helper removes.
///
/// Failures reflect too (the one-shot pipeline path has always treated a
/// failed run as a high-value learning signal); only a user-chosen soft stop
/// is excluded (`should_reflect_on`, issue #373 item 7). The gate reads only
/// this turn's message slice (`turn_start..`) so a conversational turn never
/// spends a model call.
///
/// The whole history is handed to reflection, not the tail of it: selection is
/// the digest's job now (#2460), and pre-truncating here would hide exactly the
/// middle the selection exists to find.
pub(super) async fn reflect_on_interactive_turn<T, E: std::fmt::Display>(
    provider: &dyn Provider,
    cfg: &Config,
    memory: &mut Option<SessionMemory>,
    messages: &[CompletionMessage],
    turn_start: usize,
    result: &Result<T, E>,
    budget: &mut BudgetGuard,
) {
    if should_reflect_on(result)
        && turn_warrants_reflection(&messages[turn_start..])
        && let Some(m) = memory
    {
        let mut report = reflect_routed(
            m,
            cfg,
            provider,
            TurnEvidence::from_transcript(messages, result.is_ok()),
            false,
            remaining_budget(budget),
        )
        .await;
        settle_reflection_budget(&mut report, budget);
        surface_reflection(&report, OutputFormat::Text);
    }
}

/// Surface a post-turn [`ReflectionReport`] for human text output. Machine
/// streams route reflection events through their execution renderer so
/// `Complete` remains the unique final frame; this helper never writes a
/// second, unframed stdout sequence after that terminal barrier.
pub(crate) fn surface_reflection(report: &ReflectionReport, format: OutputFormat) {
    if format == OutputFormat::Text {
        for event in &report.events {
            match event {
                AgentEvent::StepUsage {
                    role,
                    provider,
                    model,
                    input_tokens,
                    output_tokens,
                    cost_usd,
                    retries,
                    complete,
                    ..
                } => eprintln!(
                    "  {} {:?} {provider}/{model}: {input_tokens} in, {output_tokens} out, \
                     ${cost_usd:.4}, {retries} retries, complete={complete}",
                    "✦".magenta(),
                    role
                ),
                AgentEvent::UsageIncomplete {
                    role,
                    provider,
                    model,
                    reason,
                    retries,
                    ..
                } => eprintln!(
                    "  {} {:?} {provider}/{model}: usage incomplete ({reason:?}, retries={retries:?})",
                    "!".yellow(),
                    role
                ),
                _ => {}
            }
        }
    }
    if let Some(err) = &report.model_error {
        eprintln!(
            "  {} post-turn reflection skipped — model call failed: {err}",
            "!".yellow()
        );
    }
}

/// No production caller since #3865 — its one use was the staged pipeline's
/// `--output-format json` envelope, whose `reflection` key does not exist on
/// [`crate::agent::RawRunSummary`] (the raw step-loop's own JSON envelope
/// never carried one). Kept for `reflection_json_preserves_full_paid_call_envelope_and_cost`'s
/// coverage of the shape rather than deleted outright, since a future
/// envelope wanting a reflection summary would want this exact mapping.
#[allow(dead_code)]
pub(super) fn reflection_json(report: &ReflectionReport) -> serde_json::Value {
    serde_json::json!({
        "recorded": report.recorded,
        "error": report.model_error,
        "cost_usd": report.cost_usd,
        "events": report.events,
    })
}
