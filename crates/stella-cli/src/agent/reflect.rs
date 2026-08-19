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
    OutputFormat, ReflectionReport, SessionMemory, TurnEvidence, TurnFriction, reflect_routed,
    remaining_budget, settle_reflection_budget, should_reflect_on, turn_warrants_reflection,
};
use crate::config::Config;

/// What one interactive turn produced, as the three things reflection reads
/// about it.
///
/// Grouped rather than passed flat because #3946's friction ledger was the
/// eighth argument, and `too_many_arguments` was right to object: `messages`
/// and `turn_start` are one fact (the transcript, and where this turn starts
/// in it) and `friction` is the event-stream half of that same turn. Bundling
/// them is the move `reflect_routed`'s own comment already names — "them as
/// [`TurnEvidence`] is what keeps this at seven arguments" — applied one layer
/// up, rather than an `#[allow]` asserting the lint was wrong.
pub(super) struct InteractiveTurn<'a> {
    /// The whole session history, not this turn's slice — selection is the
    /// digest's job (#2460).
    pub(super) messages: &'a [CompletionMessage],
    /// Where this turn's own work starts in `messages`; the reflection gate
    /// reads only that slice, so a conversational turn spends no model call.
    pub(super) turn_start: usize,
    /// This turn's folded event ledger (#3946). The transcript records what
    /// was said; only this records what it cost, how long it took, and whether
    /// the turn retried or looped — none of which a `CompletionMessage`
    /// carries.
    pub(super) friction: &'a TurnFriction,
}

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
    turn: InteractiveTurn<'_>,
    result: &Result<T, E>,
    budget: &mut BudgetGuard,
) {
    if should_reflect_on(result)
        && turn_warrants_reflection(&turn.messages[turn.turn_start..])
        && let Some(m) = memory
    {
        let mut report = reflect_routed(
            m,
            cfg,
            provider,
            TurnEvidence::with_friction(turn.messages, turn.friction, result.is_ok()),
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
    // What the turn actually learned. Until now the only production reader of
    // `recorded` was the staged pipeline's JSON envelope, so on every surface a
    // user can see, reflection paid for a model call and then never said
    // whether it had come back with anything — a silent success and a silent
    // "nothing worth keeping" were the same output. Text mode only, for the
    // same reason as the loop above: a machine stream's `Complete` stays the
    // unique final frame.
    if format == OutputFormat::Text && report.recorded > 0 {
        let plural = if report.recorded == 1 { "" } else { "s" };
        eprintln!(
            "  {} reflection recorded {} lesson{plural}",
            "✦".magenta(),
            report.recorded
        );
    }
    if let Some(err) = &report.model_error {
        eprintln!(
            "  {} post-turn reflection skipped — model call failed: {err}",
            "!".yellow()
        );
    }
}
