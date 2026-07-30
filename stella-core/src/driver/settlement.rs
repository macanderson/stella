use stella_protocol::AgentEvent;

use super::TurnOutcome;
use crate::budget::{BudgetAxis, BudgetGuard, BudgetOutcome};
use crate::event_sender::EventSender;

/// Which budget axes have already surfaced an observed-mode warning during
/// the current turn. A breach persists across every remaining settled call
/// in the turn, so without this the same warning would repeat on each one;
/// the ledger claims the first warning per axis and swallows the rest.
/// Constructed fresh per turn in `run_turn_with_sender`, so it resets when a
/// new turn begins.
#[derive(Default)]
pub(super) struct BudgetWarnings {
    turn_warned: bool,
    session_warned: bool,
}

impl BudgetWarnings {
    /// Returns `true` the first time `axis` warns this turn, `false` on every
    /// later call for the same axis.
    fn claim(&mut self, axis: BudgetAxis) -> bool {
        let slot = match axis {
            BudgetAxis::Turn => &mut self.turn_warned,
            BudgetAxis::Session => &mut self.session_warned,
        };
        !std::mem::replace(slot, true)
    }
}

pub(super) fn axis_label(axis: BudgetAxis) -> &'static str {
    match axis {
        BudgetAxis::Turn => "turn",
        BudgetAxis::Session => "session",
    }
}

/// Surface a [`BudgetOutcome::Warn`] as the one-line retryable warning the
/// accounted-call path also emits — at most once per axis per turn (see
/// [`BudgetWarnings`]). A non-`Warn` outcome is a no-op, so callers can pass
/// any outcome unconditionally. `BudgetTick` alone cannot carry this: it
/// reports only turn-scoped numbers, so a session-axis breach would
/// otherwise be invisible to every event-stream consumer
/// (`BudgetOutcome::Warn`'s contract is that the driver surfaces it).
pub(super) fn emit_budget_warning(
    outcome: BudgetOutcome,
    warnings: &mut BudgetWarnings,
    events: &EventSender,
) {
    let BudgetOutcome::Warn {
        axis,
        spent_usd,
        limit_usd,
    } = outcome
    else {
        return;
    };
    if !warnings.claim(axis) {
        return;
    }
    let axis = axis_label(axis);
    let _ = events.send(AgentEvent::Error {
        message: format!(
            "budget warning: spent ${spent_usd:.4} against a ${limit_usd:.2} observed {axis} \
             limit; continuing"
        ),
        retryable: true,
    });
}

pub(super) fn record_settled_cost(
    budget: &mut BudgetGuard,
    cost_usd: f64,
    warnings: &mut BudgetWarnings,
    events: &EventSender,
) -> BudgetOutcome {
    let outcome = budget.record_spend(cost_usd);
    let _ = events.send(AgentEvent::BudgetTick {
        spent_usd: budget.spent_usd(),
        limit_usd: budget.turn_limit_usd(),
        mode: budget.mode(),
        session_spent_usd: Some(budget.session_spent_usd()),
        session_limit_usd: budget.session_limit_usd(),
    });
    emit_budget_warning(outcome, warnings, events);
    outcome
}

impl super::Engine<'_> {
    /// The between-steps budget check (never mid-tool — see module docs),
    /// preceded by folding in any spend sub-agents incurred since the last
    /// boundary.
    ///
    /// The fold has to happen *here*, not after the turn: a `task`-style tool
    /// spawns a child from inside `ToolExecutor::execute`, where the engine
    /// already holds `budget` mutably, so the child's cost arrives
    /// out-of-band through [`crate::ports::ToolExecutor::drain_sub_agent_spend_usd`].
    /// Charging it at the next boundary is what keeps a configured `--budget`
    /// a hard ceiling once turns nest — `evaluate()` below is then reading a
    /// guard that already knows what the children spent. Deferring to
    /// end-of-turn instead would let a parent and its children each run to
    /// the cap independently.
    ///
    /// Routed through [`record_settled_cost`] rather than a bare
    /// `record_spend` so child spend moves the HUD and trips observed-mode
    /// warnings exactly like the engine's own calls — money that only
    /// appeared in the total would be money no one saw arrive.
    pub(super) fn check_budget(
        &self,
        budget: &mut BudgetGuard,
        total_cost_usd: f64,
        warnings: &mut BudgetWarnings,
        events: &EventSender,
    ) -> Option<TurnOutcome> {
        let child_spend = self.tools.drain_sub_agent_spend_usd();
        if child_spend > 0.0 {
            record_settled_cost(budget, child_spend, warnings, events);
        }
        check_budget(budget, total_cost_usd, warnings, events)
    }
}

/// The evaluation half, split from the drain above so the pure decision
/// (guard state → abort or not) stays testable without an executor.
fn check_budget(
    budget: &BudgetGuard,
    total_cost_usd: f64,
    warnings: &mut BudgetWarnings,
    events: &EventSender,
) -> Option<TurnOutcome> {
    let outcome = budget.evaluate();
    emit_budget_warning(outcome, warnings, events);
    let BudgetOutcome::AbortTurn {
        axis,
        spent_usd,
        limit_usd,
    } = outcome
    else {
        return None;
    };
    let axis = axis_label(axis);
    let reason =
        format!("budget exceeded: spent ${spent_usd:.4} against a ${limit_usd:.2} {axis} limit");
    let _ = events.send(AgentEvent::Error {
        message: reason.clone(),
        retryable: false,
    });
    Some(TurnOutcome::Aborted {
        reason,
        cost_usd: total_cost_usd,
    })
}
