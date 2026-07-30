//! Scope review through the deck — the interactive [`ApprovalGate`] the
//! staged pipeline blocks on when a plan exceeds the scope thresholds.

use std::sync::Arc;

use async_trait::async_trait;
use stella_pipeline::{ApprovalGate, ScopeDecision};
use stella_protocol::{AgentEvent, ScopeProposal};
use stella_tui::{Inbound, ScopeDecision as DeckScopeDecision};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

/// The deck-native approval gate — the "deck-native host approval" the
/// pipeline turn's doc comment used to name as a follow-up.
///
/// The deck could never use `StdioApprovalGate` (the alternate screen owns the
/// terminal), so it previously ran the pipeline with `headless: true` and an
/// `AlwaysAbortGate`. That made every plan past the scope thresholds — more
/// than 5 steps, 8 files, or $1.00 — a dead end: the turn aborted with advice
/// to set `headless_scope_bypass`, a setting only `stella run` reads, so
/// following the advice changed nothing. Meanwhile the TUI's approval card
/// and its decision keys were fully built and wired to nothing.
///
/// This closes that loop the same way `DeckAskUserIo` closes `ask_user`: emit
/// the card, park on a channel, let the input pump deliver the keypress. The
/// pipeline emits its own `ScopeReview` event on the non-headless branch, which
/// is what raises the card, so this gate only awaits the answer.
pub(crate) struct DeckApprovalGate {
    pub(crate) agent: String,
    pub(crate) inbound: UnboundedSender<Inbound>,
    pub(crate) decisions: Arc<tokio::sync::Mutex<UnboundedReceiver<DeckScopeDecision>>>,
    /// The step ceiling `Trim` trims to — the deck's card has no per-step
    /// picker, so "trim" means "keep the largest prefix that would not have
    /// tripped review in the first place".
    pub(crate) max_steps: usize,
}

impl DeckApprovalGate {
    /// Build the gate for a deck session. `max_steps` comes from the same
    /// [`ScopeThresholds`] defaults the pipeline gates on, so the trim target
    /// can never drift from the threshold that raised the card.
    ///
    /// [`ScopeThresholds`]: stella_pipeline::scope::ScopeThresholds
    pub(crate) fn new(
        agent: String,
        inbound: UnboundedSender<Inbound>,
        decisions: UnboundedReceiver<DeckScopeDecision>,
    ) -> Self {
        Self {
            agent,
            inbound,
            decisions: Arc::new(tokio::sync::Mutex::new(decisions)),
            max_steps: stella_pipeline::scope::ScopeThresholds::default()
                .max_steps
                .unwrap_or(5),
        }
    }

    /// The pure half of [`ApprovalGate::review`]: map a deck keypress onto the
    /// pipeline's decision, given how many steps the proposal carries. Split
    /// out so the mapping — especially `Trim`'s arithmetic — is testable
    /// without a live channel or a running deck.
    pub(crate) fn decide(&self, decision: DeckScopeDecision, step_count: usize) -> ScopeDecision {
        match decision {
            DeckScopeDecision::Approve => ScopeDecision::Approve,
            DeckScopeDecision::Abort => ScopeDecision::Abort,
            DeckScopeDecision::Trim => ScopeDecision::Trim {
                keep_steps: (0..self.max_steps.min(step_count)).collect(),
            },
            // The reviewer typed a note instead of pressing a key. It passes
            // through verbatim — the pipeline folds it into the next planner
            // prompt, so anything this layer "helpfully" normalized would be
            // words the human did not write.
            DeckScopeDecision::Revise { note } => ScopeDecision::Revise { note },
        }
    }
}

#[async_trait]
impl ApprovalGate for DeckApprovalGate {
    async fn review(&self, proposal: &ScopeProposal) -> ScopeDecision {
        let mut decisions = self.decisions.lock().await;
        // Drop decisions stranded by a cancelled turn — they belong to a card
        // that no longer exists (same hygiene as `DeckAskUserIo::prompt`).
        while decisions.try_recv().is_ok() {}

        let Some(decision) = decisions.recv().await else {
            // The deck closed with the gate still open. Fail closed — the same
            // posture as every other gate in the system.
            return ScopeDecision::Abort;
        };

        let resolved = self.decide(decision, proposal.steps.len());
        if let ScopeDecision::Trim { keep_steps } = &resolved {
            // Announce exactly what survives. A trim that silently dropped
            // steps would be the failure mode `StdioApprovalGate` avoids by
            // refusing to offer Trim at all; naming the count is what makes
            // offering it honest here.
            let keep = keep_steps.len();
            let dropped = proposal.steps.len().saturating_sub(keep);
            let _ = self.inbound.send(Inbound::Event {
                agent: self.agent.clone(),
                event: AgentEvent::Text {
                    delta: format!(
                        "\n[scope trimmed to the first {keep} step{} — {dropped} dropped]\n",
                        if keep == 1 { "" } else { "s" },
                    ),
                },
            });
        }
        resolved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn gate(max_steps: usize) -> DeckApprovalGate {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (_dtx, drx) = mpsc::unbounded_channel();
        DeckApprovalGate {
            agent: "lead".to_string(),
            inbound: tx,
            decisions: Arc::new(tokio::sync::Mutex::new(drx)),
            max_steps,
        }
    }

    #[test]
    fn approve_and_abort_pass_straight_through() {
        let g = gate(5);
        assert_eq!(
            g.decide(DeckScopeDecision::Approve, 9),
            ScopeDecision::Approve
        );
        assert_eq!(g.decide(DeckScopeDecision::Abort, 9), ScopeDecision::Abort);
    }

    /// The reported case: a 9-step scaffold plan against the default ceiling
    /// of 5. Trim keeps the first five, in order.
    #[test]
    fn trim_keeps_the_leading_steps_up_to_the_threshold() {
        assert_eq!(
            gate(5).decide(DeckScopeDecision::Trim, 9),
            ScopeDecision::Trim {
                keep_steps: vec![0, 1, 2, 3, 4]
            }
        );
    }

    /// A note passes through verbatim. This layer must not normalize it: the
    /// pipeline folds it into the next planner prompt, so any "helpful" rewrite
    /// here puts words the human did not write in front of the planner.
    #[test]
    fn a_revision_note_passes_through_untouched() {
        assert_eq!(
            gate(5).decide(
                DeckScopeDecision::Revise {
                    note: "  only the Ctrl+O dialog — skip Ctrl+W!  ".to_string()
                },
                9
            ),
            ScopeDecision::Revise {
                note: "  only the Ctrl+O dialog — skip Ctrl+W!  ".to_string()
            }
        );
    }

    /// A plan shorter than the ceiling must never mint out-of-range indices —
    /// the pipeline ignores those, but a Trim that claims steps the plan does
    /// not have would misreport what ran.
    #[test]
    fn trim_never_exceeds_the_plan_length() {
        assert_eq!(
            gate(5).decide(DeckScopeDecision::Trim, 2),
            ScopeDecision::Trim {
                keep_steps: vec![0, 1]
            }
        );
    }

    /// A closed decision channel (the deck went away mid-gate) fails closed.
    #[tokio::test]
    async fn a_closed_deck_aborts_rather_than_approving() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (dtx, drx) = mpsc::unbounded_channel();
        drop(dtx);
        let g = DeckApprovalGate {
            agent: "lead".to_string(),
            inbound: tx,
            decisions: Arc::new(tokio::sync::Mutex::new(drx)),
            max_steps: 5,
        };
        let proposal = ScopeProposal {
            summary: "big".into(),
            steps: vec!["a".into(), "b".into()],
            estimated_files: 9,
            estimated_cost_usd: None,
        };
        assert_eq!(g.review(&proposal).await, ScopeDecision::Abort);
    }
}
