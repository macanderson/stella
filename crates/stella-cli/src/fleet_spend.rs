//! What a fleet attempt really cost when its worker never says.
//!
//! `EngineWorker::run` bridges an OS thread back to the async dispatch side
//! over a oneshot. Three things can close that channel with no
//! [`WorkerOutcome`] on it: the worker's runtime failed to start, its setup
//! returned an error, or — the case that costs money — it panicked mid-turn.
//! The dispatch side then has to synthesize an outcome, and the field that
//! matters is `cost_usd`: `Fleet::dispatch` meters it into the parent
//! `BudgetGuard` before stamping anything, precisely because the money is
//! already spent by the time a worker returns. Synthesizing `$0` there
//! under-counts the aggregate `--budget` gate — the one direction that
//! comment calls unsafe (#1216).
//!
//! A dead worker cannot report, but its receipts can. The engine writes a
//! durable `StepUsage` row per landed model call as the turn runs, so the
//! spend a panicking attempt had already settled is sitting in that worker's
//! own store. [`SpendRecovery`] is the handle that survives the thread: the
//! worker publishes it the instant it opens an execution, and
//! [`unreported_outcome`] reads the settled sum back through it.
//!
//! What this recovers is *settled* spend, not *incurred* spend: a provider
//! call that was in flight when the thread died never wrote a receipt and is
//! not counted. That residual is bounded by one call and errs downward, which
//! is the same direction the pre-existing gate already tolerates — and it is
//! an unbounded improvement on `$0`.

use std::sync::{Arc, Mutex};

use stella_fleet::WorkerOutcome;
use stella_store::Store;

/// An attempt's durable accounting handle — the worker's store and the id of
/// the execution row it opened there — as `agent::begin_execution` hands it
/// back. `None` when no execution was opened at all (no store).
pub(crate) type ExecutionHandle = Option<(Arc<Store>, i64)>;

/// The seam a dead worker's spend comes back through.
///
/// Cloned across the worker/dispatch boundary: the worker publishes its
/// [`ExecutionHandle`], and the dispatch side — which outlives the worker's
/// OS thread — reads through it when no outcome ever arrives.
#[derive(Clone, Default)]
pub(crate) struct SpendRecovery(Arc<Mutex<ExecutionHandle>>);

impl SpendRecovery {
    pub(crate) fn publish(&self, execution: &ExecutionHandle) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = execution.clone();
    }

    fn execution(&self) -> ExecutionHandle {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

/// The outcome for an attempt whose worker never returned one.
///
/// `cost_usd` is the attempt's settled spend, never a synthesized `$0` — see
/// the module doc for why that direction matters. The dead worker also never
/// closed its execution row and never will, so this closes it with the same
/// total, marked usage-incomplete: the receipts that landed are durable, the
/// turn they belonged to never finished.
pub(crate) fn unreported_outcome(spend: &SpendRecovery, reason: String) -> WorkerOutcome {
    let mut outcome = WorkerOutcome {
        cost_usd: 0.0,
        commits: Vec::new(),
        summary: reason,
        success: false,
    };
    // No execution was ever opened (no store, or the worker died before it
    // got that far): nothing was spent, and there is no row to close.
    let Some((store, execution_id)) = spend.execution() else {
        return outcome;
    };
    outcome.cost_usd = store
        .execution_settled_cost_usd(execution_id)
        .unwrap_or_default();
    let _ = store.finish_execution_accounted(execution_id, "error", outcome.cost_usd, false);
    if outcome.cost_usd > 0.0 {
        outcome.summary = format!(
            "{} (recovered ${:.4} of settled spend from its receipts)",
            outcome.summary, outcome.cost_usd
        );
    }
    outcome
}

#[cfg(test)]
mod tests {
    use stella_store::TelemetryRow;

    use super::*;

    /// One landed model call's durable receipt, priced at `cost_usd`.
    fn receipt(step: u64, cost_usd: f64) -> TelemetryRow {
        TelemetryRow {
            step,
            provider: "anthropic".into(),
            call_role: "worker".into(),
            model: "test-model".into(),
            input_tokens: 1_000,
            estimated_input_tokens: 900,
            output_tokens: 100,
            cache_read_tokens: 0,
            cache_miss_tokens: 1_000,
            cache_write_tokens: 0,
            cost_usd,
            duration_ms: 10,
            retries: 0,
            tool_calls: 0,
            usage_complete: true,
        }
    }

    /// An execution that has spent real money and never closed — the state a
    /// worker thread's mid-turn panic leaves behind.
    fn spent_attempt(costs: &[f64]) -> (SpendRecovery, Arc<Store>, i64) {
        let store = Arc::new(Store::in_memory().unwrap());
        let execution_id = store
            .begin_execution("fleet", "goal", "anthropic", "test-model")
            .unwrap();
        for (step, cost) in costs.iter().enumerate() {
            store
                .record_telemetry(execution_id, &receipt(step as u64, *cost))
                .unwrap();
        }
        let spend = SpendRecovery::default();
        spend.publish(&Some((store.clone(), execution_id)));
        (spend, store, execution_id)
    }

    /// The witness for #1216's second half. A worker that dies mid-attempt
    /// reports the spend its receipts prove, not `$0` — and `cost_usd` is
    /// exactly what `Fleet::dispatch` meters into the aggregate budget gate.
    #[test]
    fn a_worker_that_never_reports_still_reports_its_settled_spend() {
        let (spend, ..) = spent_attempt(&[0.02, 0.03]);

        let outcome = unreported_outcome(&spend, "worker thread died before reporting".into());

        assert!(!outcome.success);
        assert!(
            (outcome.cost_usd - 0.05).abs() < 1e-9,
            "the attempt settled $0.05 of receipts, got {}",
            outcome.cost_usd
        );
        assert!(
            outcome
                .summary
                .starts_with("worker thread died before reporting"),
            "the named reason survives: {}",
            outcome.summary
        );
        assert!(
            outcome.summary.contains("0.0500"),
            "and the recovery is visible in the report: {}",
            outcome.summary
        );
    }

    /// The row the dead worker left open is closed with that same total, so
    /// the store stops reading `$0` for an attempt that spent money.
    /// Accounting stays marked incomplete: the turn never finished.
    #[test]
    fn the_open_execution_row_is_closed_with_the_recovered_total() {
        let (spend, store, execution_id) = spent_attempt(&[0.25]);

        unreported_outcome(&spend, "worker thread died before reporting".into());

        let closed = store.execution_summary(execution_id).unwrap().unwrap();
        assert!((closed.cost_usd - 0.25).abs() < 1e-9);
        assert!(closed.finished_at.is_some(), "the row is no longer open");
        assert!(
            !store.execution_usage_complete(execution_id).unwrap(),
            "a turn that never finished is not complete accounting"
        );
    }

    /// A worker that died before it ever opened an execution spent nothing,
    /// and says so — recovery reports truth in both directions.
    #[test]
    fn a_worker_that_died_before_spending_reports_zero() {
        let outcome = unreported_outcome(&SpendRecovery::default(), "worker error: git".into());

        assert_eq!(outcome.cost_usd, 0.0);
        assert_eq!(outcome.summary, "worker error: git");
        assert!(outcome.commits.is_empty());
    }
}
