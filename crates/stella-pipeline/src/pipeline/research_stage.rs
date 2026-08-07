//! The pre-plan research stage (#1778): fan triage's questions out as
//! parallel **read-only** sub-agents and hand the planner their bounded
//! findings. Split out of `pipeline.rs` on the same grounds as
//! `fanout_stage` — this is I/O sequencing around a concurrency decision,
//! and `pipeline.rs` is closed to growth.
//!
//! # Shape
//!
//! The dispatch pattern is the one that already exists twice: each question
//! becomes a [`SubAgentSpec`] run through [`Engine::run_sub_agent_with_sender`]
//! (the `stella-core` goal verifier's shape), and the money follows
//! [`crate::candidate_fanout`]'s arrangement — a [`FanOutBudget`] pre-dispatch
//! gate plus a per-question carve of `headroom / width`, settled in completion
//! order. Children run behind `ReadOnlyTools` (`write_access: false`) and
//! thread depth 1; the sub-agent primitive enforces both.
//!
//! # Degradation is the contract
//!
//! Research is advisory grounding. Every failure — an unresolvable provider,
//! a refused carve, a child that aborts, a child that outlives the latency
//! ceiling — degrades to *fewer findings*, never to a failed turn, and zero
//! findings leave the planner prompt byte-for-byte what it was before this
//! stage existed. The per-child ceiling is what guarantees the stage cannot
//! wedge a turn: children run concurrently, so the stage's wall-clock is
//! bounded by one ceiling, not one per question.

use super::*;

use stella_core::subagent::{SubAgentHost, SubAgentOutcome, SubAgentSpec};

use crate::candidate_fanout::FanOutBudget;
use crate::research::{
    RESEARCH_FINDING_CHARS, RESEARCH_SYSTEM_PROMPT, ResearchFinding, bound_research_findings,
    research_instruction,
};

/// Model calls one research child may make: a handful of evidence lookups,
/// not a work session — the same bound the goal verifier runs under.
const RESEARCH_MAX_STEPS: usize = 8;

impl Pipeline<'_> {
    /// Answer triage's research questions with parallel read-only sub-agents,
    /// returning the bounded findings for the planner prompt. Empty questions
    /// return empty findings with **no events and no spend** — the skip path
    /// must leave the stream byte-for-byte untouched (L-E2).
    pub(super) async fn research_stage(
        &self,
        goal: &str,
        questions: &[String],
        budget: &mut BudgetGuard,
        total: &mut f64,
    ) -> Vec<ResearchFinding> {
        if questions.is_empty() {
            return Vec::new();
        }
        // Resolution failure degrades to no research — never fail a turn on
        // an advisory stage. Plan-tier on purpose: the findings exist for the
        // planner, so they ride the planner's model choice and overrides.
        let Ok(resolved) = self.resolve_provider(Role::Plan) else {
            return Vec::new();
        };
        self.emit(AgentEvent::Stage {
            name: StageKind::Research,
        });
        if let Some(fb) = &resolved.fallback {
            self.emit_fallback(fb);
        }

        let mut engine = Engine::with_sleeper(
            resolved.provider,
            self.tools,
            self.config.engine.clone(),
            self.sleeper,
        );
        if let Some((hooks, runner)) = self.hooks {
            engine = engine.with_hooks(hooks, runner);
        }

        let width = u32::try_from(questions.len()).unwrap_or(u32::MAX);
        let fan = FanOutBudget::new(*budget, width);
        let ceiling = self.config.research_latency_ceiling;

        let runs = questions.iter().enumerate().map(|(index, question)| {
            let engine = &engine;
            let fan = &fan;
            let provider = resolved.provider;
            async move {
                // The pre-dispatch gate: a parent already over its cap funds
                // nothing, and the skip is a missing finding, not an abort.
                let Some(mut child_budget) = fan.claim() else {
                    return (None, 0.0);
                };
                let spec = SubAgentSpec {
                    agent_id: format!("research-{index}"),
                    system_prompt: Some(RESEARCH_SYSTEM_PROMPT.to_string()),
                    instruction: research_instruction(goal, question),
                    max_steps: RESEARCH_MAX_STEPS,
                    max_output_tokens: None,
                    temperature: Some(0.0),
                    max_report_chars: RESEARCH_FINDING_CHARS,
                    budget_usd: None,
                    write_access: false,
                    role: ModelCallRole::Research,
                    // Each child needs its own receipt slot: receipts key on
                    // `(execution_id, turn_instance, step, call_seq)` and every
                    // child restarts `step` at 0, so concurrent children
                    // sharing a slot would overwrite each other's manifests.
                    turn_instance: self
                        .config
                        .engine
                        .turn_instance
                        .saturating_add(1)
                        .saturating_add(index as u32),
                    // MUST be parent_depth + 1 (the sub-agent nesting
                    // contract); the pipeline dispatches from the top level.
                    depth: 1,
                    compaction_budget_tokens: None,
                };
                // The ceiling is per child, INSIDE the future, so a timed-out
                // child settles its spend through the sub-agent primitive's
                // drop guards and the stage still returns — research degrading
                // to fewer findings must never wedge the turn. The dropped
                // child's stream stays whole without help here (#1954): the
                // primitive's `CancelBracket` closes the `Started`/`Finished`
                // bracket with the committed step count and cost, and the
                // engine's own cancel guard emits the abandoned call's
                // `UsageIncomplete { Cancelled }` envelope.
                let outcome = tokio::time::timeout(
                    ceiling,
                    engine.run_sub_agent_with_sender(
                        SubAgentHost::new(provider),
                        &spec,
                        &mut child_budget,
                        &self.events,
                    ),
                )
                .await;
                let finding = match outcome {
                    Ok(SubAgentOutcome::Completed(report)) if !report.summary.is_empty() => {
                        Some(ResearchFinding {
                            question: question.clone(),
                            answer: report.summary,
                        })
                    }
                    // Refusals, aborts, empty answers, and a child past the
                    // ceiling are missing findings, not errors — partial work
                    // is not evidence worth planning on.
                    Ok(_) | Err(_) => None,
                };
                fan.settle(&child_budget);
                (finding, child_budget.session_spent_usd())
            }
        });

        let answered = futures_util::future::join_all(runs).await;

        // Fold the fan-out's spend back the way the candidate fan-out does:
        // the guard from `into_parent`, the ledger summed in index order —
        // float addition is not associative, and a run's reported cost must
        // not depend on which child happened to finish first.
        *budget = fan.into_parent();
        let mut findings = Vec::with_capacity(answered.len());
        for (finding, cost_usd) in answered {
            *total += cost_usd;
            findings.extend(finding);
        }

        bound_research_findings(findings)
    }
}
