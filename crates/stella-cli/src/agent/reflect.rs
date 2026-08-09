//! Post-turn reflection at the CLI's turn boundaries: the interactive-turn
//! dispatch, the friction tap that gives reflection its event-derived evidence,
//! and the two report renderers.
//!
//! Split verbatim out of `agent.rs` — no behavior change in the move — because
//! that file sits at its file-size ratchet ceiling and this cluster is a
//! coherent unit: everything here is about what happens *after* a turn's last
//! model call, and nothing else in `agent.rs` calls into it.

use colored::Colorize;
use stella_core::{BudgetGuard, EventSender};
use stella_protocol::{AgentEvent, CompletionMessage, Provider};

use super::{
    OutputFormat, ReflectionReport, SessionMemory, TurnEvidence, TurnFriction, reflect_routed,
    remaining_budget, settle_reflection_budget, should_reflect_on, turn_warrants_reflection,
};
use crate::config::Config;

/// The live fold that gives reflection what a transcript cannot carry.
///
/// A [`TurnFriction`] has to be built from the turn's [`AgentEvent`] stream, and
/// the only place that stream is complete *and* still in this process's hands is
/// the sender every producer emits on. So the tap wraps that sender: fold, then
/// forward untouched.
///
/// It deliberately does not read the persisted journal instead. Events reach
/// `store.db` from the renderer task (`agent::persistence::spawn_renderer`),
/// which is still draining when reflection runs, so a read there would return a
/// prefix of the turn whose length depends on scheduling — and the digest would
/// stop being a function of the turn.
#[derive(Clone, Default)]
pub(crate) struct FrictionTap {
    ledger: std::sync::Arc<std::sync::Mutex<TurnFriction>>,
}

impl FrictionTap {
    /// Wrap `downstream` so every event folded here is still delivered there.
    ///
    /// A poisoned lock is stepped over rather than propagated: this is a
    /// best-effort observation of a turn that has already happened, and a panic
    /// on the event path would take down a turn to protect a digest.
    pub(crate) fn tap(&self, downstream: EventSender) -> EventSender {
        let ledger = self.ledger.clone();
        EventSender::from_fn(move |event| {
            ledger
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .observe(&event);
            downstream.send(event)
        })
    }

    /// What the tap has folded so far. Cloned rather than drained: the turn's
    /// senders may still be live (reflection's own accounting events ride the
    /// same channel), and a drain would make this method's result depend on how
    /// many times it had been called.
    pub(crate) fn snapshot(&self) -> TurnFriction {
        self.ledger
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
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
pub(super) async fn reflect_on_interactive_turn<E: std::fmt::Display>(
    provider: &dyn Provider,
    cfg: &Config,
    memory: &mut Option<SessionMemory>,
    messages: &[CompletionMessage],
    turn_start: usize,
    result: &Result<(), E>,
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

pub(super) fn reflection_json(report: &ReflectionReport) -> serde_json::Value {
    serde_json::json!({
        "recorded": report.recorded,
        "error": report.model_error,
        "cost_usd": report.cost_usd,
        "events": report.events,
    })
}
