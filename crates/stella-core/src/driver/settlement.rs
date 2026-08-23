use stella_protocol::{AgentEvent, MessageRole};

use super::TurnOutcome;
use crate::TurnState;
use crate::budget::{BudgetAxis, BudgetGuard, BudgetOutcome, DeadlineOutcome};
use crate::event_sender::EventSender;
use crate::step::AbortKind;

/// Which budget axes have already surfaced an observed-mode warning during
/// the current turn. A breach persists across every remaining settled call
/// in the turn, so without this the same warning would repeat on each one;
/// the ledger claims the first warning per axis and swallows the rest.
/// Constructed fresh per turn — `TurnMemos::new`, which every `TurnState`
/// constructor builds — so it resets when a new turn begins.
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

/// `now` is the caller's own boundary clock read — the tick reports the
/// wall-clock axis too (#2240) and this module holds no clock of its own
/// (invariant 2, and `crate::budget`'s module docs).
pub(super) fn record_settled_cost(
    budget: &mut BudgetGuard,
    cost_usd: f64,
    warnings: &mut BudgetWarnings,
    events: &EventSender,
    now: std::time::Instant,
) -> BudgetOutcome {
    let outcome = budget.record_spend(cost_usd);
    let _ = events.send(budget.tick_event(now));
    emit_budget_warning(outcome, warnings, events);
    outcome
}

impl super::Engine<'_> {
    /// The between-steps budget check (never mid-tool — see module docs),
    /// preceded by folding in any spend sub-agents incurred since the last
    /// boundary.
    ///
    /// The fold has to happen *here*, not after the turn: a `delegate`-style tool
    /// spawns a child from inside `ToolExecutor::execute`, where the engine
    /// already holds `budget` mutably, so the child's cost arrives
    /// out-of-band through [`crate::ports::ToolExecutor::drain_sub_agent_spend_usd`].
    /// Charging it at the next boundary is what keeps a configured `--spend-limit`
    /// a hard ceiling once turns nest — `evaluate()` below is then reading a
    /// guard that already knows what the children spent. Deferring to
    /// end-of-turn instead would let a parent and its children each run to
    /// the cap independently.
    ///
    /// Routed through [`record_settled_cost`] rather than a bare
    /// `record_spend` so child spend moves the HUD and trips observed-mode
    /// warnings exactly like the engine's own calls — money that only
    /// appeared in the total would be money no one saw arrive.
    ///
    /// `total_cost_usd` is `&mut` because the drained spend is the turn's
    /// money too: `TurnOutcome`/`AgentEvent::TurnComplete` report the turn total,
    /// and `Complete`'s contract is "a summary of the `StepUsage` events that
    /// preceded it" — a child's `StepUsage` is deliberately forwarded onto
    /// the parent stream, so a total that excluded child spend contradicted
    /// both the event contract and the guard beside it. Added BEFORE the
    /// abort decision below, which reports this same number: when the child's
    /// spend is what trips the cap, the abort must not under-report by
    /// exactly that amount.
    ///
    /// Takes the whole [`TurnState`] rather than three of its fields because
    /// the deadline question needs a fourth — `last_step`, the pace estimate —
    /// and threading each one through by hand is what makes a call site grow
    /// every time this learns to consider something else.
    pub(super) fn check_budget(
        &self,
        state: &mut TurnState,
        events: &EventSender,
    ) -> Option<TurnOutcome> {
        // The one clock read on this path — `crate::budget`'s guard is pure
        // and takes `now` as an owned parameter (module docs), mirroring how
        // `run_step_inner` reads `state.started_at.elapsed()` for the
        // existing per-turn `ContinuationBudget` rather than letting a pure
        // decision function read the clock itself. Read once and shared with
        // the settlement tick below, so the boundary's two answers — what is
        // left on the clock, and whether that is already too little — cannot
        // disagree about when "now" was.
        let now = std::time::Instant::now();
        let child_spend = self.tools.drain_sub_agent_spend_usd();
        if child_spend > 0.0 {
            record_settled_cost(
                &mut state.budget,
                child_spend,
                &mut state.memos.warnings,
                events,
                now,
            );
            state.total_cost_usd += child_spend;
        }
        check_budget(
            &state.budget,
            state.total_cost_usd,
            state.last_step,
            &mut state.memos.warnings,
            events,
            now,
        )
    }
}

/// The evaluation half, split from the drain above so the pure decision
/// (guard state → abort or not) stays testable without an executor.
///
/// Checks the task deadline before the dollar axes (#1481): both are
/// safe-boundary aborts checked at the exact same two call sites in
/// `run_step_inner`, so a task already out of wall clock stops here just
/// like an already-over-cap dollar budget does, before the next provider
/// call is dispatched.
fn check_budget(
    budget: &BudgetGuard,
    total_cost_usd: f64,
    last_step: Option<std::time::Duration>,
    warnings: &mut BudgetWarnings,
    events: &EventSender,
    now: std::time::Instant,
) -> Option<TurnOutcome> {
    // The forecast for one more step is what the last one cost. Same rule,
    // same words, as `truncation::ContinuationBudget::affords_another` — and
    // no safety margin invented here either: the caller owns the deadline
    // (`runtime::one_shot_budget_guard` derives it from `--turn-timeout`, which
    // the bench adapter already sets to Harbor's timeout minus a teardown
    // reserve). A margin baked in at this level would be a second, invisible
    // policy on top of the one the operator configured.
    //
    // `None` before any step has been timed, which reads as a zero reserve and
    // so reproduces the reactive check exactly — there is nothing to forecast
    // from yet, and inventing a number would be guessing at the model.
    let reserve = last_step.unwrap_or_default();
    let reason = match budget.check_deadline_with_reserve(now, reserve) {
        DeadlineOutcome::Exceeded { overrun } => Some(format!(
            "task deadline exceeded by {:.1}s — stopping with partial work",
            overrun.as_secs_f64()
        )),
        // Stopping while the deadline is still ahead. Worth its own sentence
        // rather than reusing the overrun one: this is the loop declining to
        // start work it cannot finish, and a reader who sees "exceeded" on a
        // trial that never exceeded anything learns the wrong thing about it.
        DeadlineOutcome::Closing { remaining } => Some(format!(
            "task deadline in {:.1}s, less than the {:.1}s the last step took \
             — stopping with the work already done rather than starting a step \
             that cannot finish",
            remaining.as_secs_f64(),
            reserve.as_secs_f64(),
        )),
        DeadlineOutcome::Continue => None,
    };
    if let Some(reason) = reason {
        let _ = events.send(AgentEvent::Error {
            message: reason.clone(),
            retryable: false,
        });
        return Some(TurnOutcome::Aborted {
            reason,
            kind: AbortKind::DeliberateStop,
            cost_usd: total_cost_usd,
        });
    }

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
        kind: AbortKind::DeliberateStop,
        cost_usd: total_cost_usd,
    })
}

/// The most recent non-empty assistant text in a turn's transcript.
///
/// Used only by the halt path (`TurnHalt`), which ends a turn at a step
/// boundary and therefore has no "final" model text of its own to report. An
/// assistant message that only made tool calls carries empty `content`, so
/// this walks back to the last thing the model actually *said* rather than
/// reporting a blank answer for a turn that did real work.
///
/// `None` when the model has said nothing yet — a turn halted after a first
/// step of pure tool calls — which the caller renders as the halt reason.
pub(super) fn last_assistant_text(state: &crate::step::TurnState) -> Option<String> {
    state
        .messages
        .iter()
        .rev()
        .find(|message| {
            message.role == MessageRole::Assistant && !message.content.trim().is_empty()
        })
        .map(|message| message.content.clone())
}

/// One committed model call, in the shape the metering record needs.
///
/// A struct rather than seven positional parameters: `step`, `duration_ms`
/// and `retries` are all bare integers, and a caller that transposed two of
/// them would compile and mis-meter every call.
pub(super) struct SettledCall<'a> {
    pub(super) step: usize,
    pub(super) role: stella_protocol::event::ModelCallRole,
    /// The provider that actually served the call, never the session default.
    pub(super) provider: &'a str,
    pub(super) result: &'a stella_protocol::CompletionResult,
    pub(super) estimated_input_tokens: u64,
    pub(super) duration_ms: u64,
    pub(super) retries: u32,
}

/// Emit the metering record for one committed call.
///
/// Lives here rather than at the call site because `driver.rs` is a
/// grandfathered god file closed to growth (AGENTS.md § "God files"), and
/// because this is settlement: the event carries the same figures the budget
/// settles on, at the same no-await boundary.
pub(super) fn emit_step_usage(events: &EventSender, call: SettledCall<'_>) {
    let result = call.result;
    let _ = events.send(AgentEvent::StepUsage {
        step: call.step,
        role: call.role,
        provider: call.provider.to_string(),
        upstream_provider: result.upstream_provider.clone(),
        // The engine's own step already streams its answer as a `Text`
        // event; duplicating it here would double the transcript.
        output_text: None,
        model: result.model.clone(),
        input_tokens: result.usage.input_tokens,
        output_tokens: result.usage.output_tokens,
        cached_input_tokens: result.usage.cached_input_tokens,
        cache_write_tokens: result.usage.cache_write_tokens,
        reasoning_tokens: result.usage.reasoning_tokens,
        estimated_input_tokens: call.estimated_input_tokens,
        cost_usd: result.cost_usd,
        duration_ms: call.duration_ms,
        retries: call.retries,
        tool_calls: result.tool_calls.len(),
        complete: result.usage.is_complete(),
        // The provider's own stop reason, forwarded rather than inferred:
        // `Length` here is the only truthful "this step hit the output
        // ceiling" signal any consumer gets.
        finish_reason: result.finish_reason,
        // The lead's own call. A delegate's is stamped where the delegate is
        // known — `subagent::child_sender`, the one place that can say which
        // of several concurrent children an event belongs to (#4383).
        sub_agent_id: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use stella_protocol::BudgetMode;

    /// The pure decision, exercised without an executor — which is the reason
    /// `check_budget` was split out of the `&self` wrapper in the first place.
    fn decide(
        deadline_in: Option<Duration>,
        last_step: Option<Duration>,
    ) -> (Option<TurnOutcome>, Vec<String>) {
        let now = std::time::Instant::now();
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        if let Some(offset) = deadline_in {
            budget.set_task_deadline(Some(now + offset));
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        let mut warnings = BudgetWarnings::default();
        let outcome = check_budget(&budget, 0.0, last_step, &mut warnings, &events, now);
        let mut messages = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::Error { message, .. } = event {
                messages.push(message);
            }
        }
        (outcome, messages)
    }

    #[test]
    fn a_step_that_cannot_finish_is_not_started() {
        // #2278's witness. 20s of wall clock left and the last step took 30s:
        // starting another buys a harness SIGKILL with the work discarded,
        // which is what `sqlite-with-gcov__egbgJmd` bought at step 77.
        let (outcome, messages) =
            decide(Some(Duration::from_secs(20)), Some(Duration::from_secs(30)));

        let Some(TurnOutcome::Aborted { reason, kind, .. }) = outcome else {
            panic!("expected an anticipatory stop, got {outcome:?}");
        };
        assert_eq!(
            kind,
            AbortKind::DeliberateStop,
            "stopping early is the engine's own policy, not a crash"
        );
        assert!(
            reason.contains("cannot finish") && reason.contains("20.0s"),
            "the reason must say it stopped BEFORE the deadline, and by how \
             much — a reader told 'exceeded' about a trial that never exceeded \
             anything learns the wrong thing: {reason}"
        );
        assert!(
            !reason.contains("exceeded"),
            "this trial did not exceed its deadline: {reason}"
        );
        assert_eq!(messages, vec![reason], "and it is announced once");
    }

    #[test]
    fn room_for_another_step_starts_it() {
        let (outcome, messages) = decide(
            Some(Duration::from_secs(300)),
            Some(Duration::from_secs(30)),
        );
        assert!(outcome.is_none(), "300s left, 30s steps: keep working");
        assert!(messages.is_empty(), "and say nothing");
    }

    #[test]
    fn before_the_first_timed_step_the_check_stays_reactive() {
        // No pace to forecast from yet. Inventing one would be guessing at the
        // model, so an unmeasured turn behaves exactly as it did before #2278.
        let (outcome, _) = decide(Some(Duration::from_millis(1)), None);
        assert!(
            outcome.is_none(),
            "a 1ms-from-deadline turn with no measured step must not stop early"
        );
    }

    #[test]
    fn a_crossed_deadline_still_reports_the_overrun() {
        // The anticipatory rung must not swallow the hard one.
        let (outcome, _) = decide(None, Some(Duration::from_secs(30)));
        assert!(outcome.is_none(), "no deadline configured: never stops");

        let now = std::time::Instant::now();
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        budget.set_task_deadline(Some(now - Duration::from_secs(2)));
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut warnings = BudgetWarnings::default();
        let outcome = check_budget(
            &budget,
            0.0,
            Some(Duration::from_secs(600)),
            &mut warnings,
            &EventSender::new(tx),
            now,
        );
        let Some(TurnOutcome::Aborted { reason, .. }) = outcome else {
            panic!("expected the overrun stop");
        };
        assert!(reason.contains("exceeded by 2.0s"), "{reason}");
    }
}
