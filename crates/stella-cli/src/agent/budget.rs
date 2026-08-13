// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Session budget helpers: the `--spend-limit` guard's construction, the
//! headroom read the deck and fleet surfaces report, and the post-turn
//! reflection settlement. Split out of `agent.rs` (the `driver/settlement.rs`
//! pattern) — the parent is a grandfathered god file closed to growth, and
//! these three are the self-contained budget seam.

use stella_core::BudgetGuard;
use stella_protocol::AgentEvent;

use crate::memory::ReflectionReport;

/// Construct the budget guard from `--spend-limit`. The limit lands on the
/// session axis, capping cumulative spend across every turn and goal round
/// of the run — `begin_turn` resets only the turn-scoped counter. The turn
/// axis is left unset on purpose: turn spend is a reset-to-zero subset of
/// session spend, so an equal turn limit could never trip first and would
/// only mislabel a session breach as a turn one. No limit at all still
/// meters spend (`BudgetMode::Observed`) so the cost summary and
/// `BudgetTick` events stay meaningful even when nothing is enforced.
pub(crate) fn build_budget_guard(budget_limit: Option<f64>) -> BudgetGuard {
    stella_runtime::budget_guard(budget_limit)
}

/// Headroom before the next configured cap trips, in USD — the smaller of
/// the turn- and session-axis remainders when both are set, so the reported
/// value is always the binding constraint. `None` when neither axis has a
/// limit.
pub(crate) fn remaining_budget(guard: &BudgetGuard) -> Option<f64> {
    let turn = guard
        .turn_limit_usd()
        .map(|limit| (limit - guard.spent_usd()).max(0.0));
    let session = guard
        .session_limit_usd()
        .map(|limit| (limit - guard.session_spent_usd()).max(0.0));
    match (turn, session) {
        (Some(turn), Some(session)) => Some(turn.min(session)),
        (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
        (None, None) => None,
    }
}

pub(crate) fn settle_reflection_budget(report: &mut ReflectionReport, guard: &mut BudgetGuard) {
    let had_accounting = report.events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::StepUsage { .. }
                | AgentEvent::UsageIncomplete { .. }
                | AgentEvent::BudgetTick { .. }
        )
    });
    report
        .events
        .retain(|event| !matches!(event, AgentEvent::BudgetTick { .. }));
    if report.cost_usd > 0.0 {
        let _ = guard.record_spend(report.cost_usd);
    }
    if had_accounting {
        // Through the guard's own constructor, not a literal: the session axis
        // was hardcoded `None` here while this very guard tracked one, and the
        // wall-clock axis (#2240) would have gone missing the same way.
        report
            .events
            .push(guard.tick_event(std::time::Instant::now()));
    }
}
