// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Closing out the execution a dropped turn leaves behind, and reporting what
//! it cost.
//!
//! Two arms drop a turn future mid-flight — a cancel and a mid-turn `/clear` —
//! and both then owe the store a closed row and the user a number. #2570 fixed
//! the row: `Store::finish_execution_accounted` keeps `executions.cost_usd` at
//! or above the sum of the execution's `telemetry` receipts, so a receipt that
//! lands after the close-out still heals the persisted figure.
//!
//! It did not fix the number the driver holds. The cost handed to the store is
//! `agent::settled_cost_since(dispatch_spend_usd, budget.session_spent_usd())`
//! — an in-memory delta read at the instant the future is dropped, when the
//! expensive roles are still settling. Execution 99 in #2570 reported
//! `$0.8659773` against `$5.28786465` of receipts. So the database was right
//! and the session total the deck kept showing was the pre-cancel prefix, and
//! the bias is one-directional and worst on the runs that cost the most.
//!
//! [`close_dropped_execution`] reads the truth back. The row is re-read after
//! the close-out and the guard's session accumulator is corrected to match, so
//! the deck's own running total stops under-reporting the turn the user just
//! stopped.
//!
//! Not fixed here, and #2807's second half: nothing awaits the forwarder drain
//! before the row closes, so a `StepUsage` can still be priced after
//! `record_execution_end` has run. The read-back heals whatever landed by the
//! time it runs and no more. Draining first must not reintroduce the #2290
//! wedge `detach_event_stream` exists to fix, which is why it is a separate
//! change.
//!
//! Lives here rather than inline in the driver's `TurnEnd` arms: those are in
//! a god file closed to growth.

use super::*;

/// Close the execution a dropped turn left open, correct the guard from the
/// closed row, and surface a failed store write.
///
/// `noun` names the drop in the warning a failed write emits — "cancelled" or
/// "cleared". The stored outcome label is `cancelled` for both, because that
/// is what happened to the turn either way (`session_clear`'s own note).
pub(super) fn close_dropped_execution(
    execution: Option<&(Arc<Store>, i64)>,
    registry: &ToolRegistry,
    noun: &str,
    dispatch_spend_usd: f64,
    budget: &mut BudgetGuard,
    in_tx: &UnboundedSender<Inbound>,
) {
    let dropped_cost = agent::settled_cost_since(dispatch_spend_usd, budget.session_spent_usd());
    let Some((store, id)) = execution else {
        return;
    };
    let recorded =
        agent::record_execution_end(store, *id, registry, "cancelled", dropped_cost, false);
    // After the close-out, because that is when the row carries the floor over
    // both sources. Before it, the receipts the driver missed are exactly the
    // ones not yet folded in.
    if let Ok(Some(summary)) = store.execution_summary(*id) {
        budget.reseed_session_spend(reconciled_session_spend(
            dispatch_spend_usd,
            budget.session_spent_usd(),
            summary.cost_usd,
        ));
    }
    if recorded {
        return;
    }
    let _ = in_tx.send(Inbound::Event {
        agent: LEAD.to_string(),
        event: AgentEvent::Error {
            message: format!("store write failed — this {noun} execution was not recorded"),
            retryable: true,
        },
    });
}

/// The session total after a dropped turn's row has settled: whichever of the
/// two lower bounds is larger.
///
/// `in_memory` is what the guard accumulated, which misses the roles still
/// settling when the future was dropped. `dispatch_spend_usd + row_cost` is
/// what the session had spent before this turn plus what the closed row proves
/// this turn cost. Each can miss what the other saw — the row misses a call
/// whose telemetry write failed, which is what `usage_complete` records — so
/// the larger is the tightest honest answer, exactly as
/// `Store::finish_execution_accounted` argues for the row itself.
///
/// Never lowers the accumulator: spend within one session is monotone, and a
/// correction that could subtract would make a `--spend-limit` gate readable
/// as a lower number than the guard has already enforced against.
pub(super) fn reconciled_session_spend(
    dispatch_spend_usd: f64,
    in_memory: f64,
    row_cost: f64,
) -> f64 {
    in_memory.max(dispatch_spend_usd + row_cost.max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The #2570 shape, in the numbers that issue recorded: the guard saw the
    /// triage+research+plan prefix and the row proves the whole run.
    #[test]
    fn a_row_that_proves_more_than_the_guard_saw_raises_the_total() {
        assert_eq!(
            reconciled_session_spend(10.0, 10.8659773, 5.28786465),
            15.28786465
        );
    }

    /// The other direction is a real case, not a defensive one: a receipt
    /// whose telemetry write failed is spend the guard saw and the row cannot
    /// prove, and `usage_complete` is where that is recorded.
    #[test]
    fn a_guard_that_saw_more_than_the_row_proves_is_kept() {
        assert_eq!(reconciled_session_spend(10.0, 12.0, 1.0), 12.0);
    }

    /// A row read before it settled — or a store that answered nothing useful
    /// — must never lower a session total the guard has already gated on.
    #[test]
    fn the_correction_never_subtracts() {
        assert_eq!(reconciled_session_spend(10.0, 11.0, 0.0), 11.0);
        assert_eq!(reconciled_session_spend(10.0, 11.0, -3.0), 11.0);
    }

    /// **Witness (#2807).** The read-back is wired, not merely available.
    ///
    /// Execution 99's shape against a real store: the guard holds the
    /// pre-cancel prefix, the durable receipts prove several times that, and
    /// the close-out leaves the guard reporting what was actually spent. On
    /// the old driver this arm computed the delta, handed it to the store and
    /// never read anything back, so the guard kept the prefix — which is the
    /// number the deck shows the user at the moment they cancel.
    #[tokio::test]
    async fn the_guard_is_corrected_from_the_row_the_closeout_settled() {
        let root = tempfile::tempdir().expect("workspace");
        let store = Arc::new(Store::open(root.path()).expect("store"));
        let id = store
            .begin_execution("chat", "a prompt", "zai", "glm-5.2")
            .expect("execution");
        // The two expensive roles settled their receipts while the turn future
        // was already being dropped, so the guard below never saw them.
        for (step, cost) in [(1u64, 0.5), (2, 4.0)] {
            store
                .record_telemetry(
                    id,
                    &stella_store::TelemetryRow {
                        step,
                        provider: "zai".into(),
                        call_role: "worker".into(),
                        model: "glm-5.2".into(),
                        input_tokens: 1_000,
                        estimated_input_tokens: 900,
                        output_tokens: 100,
                        cache_read_tokens: 0,
                        cache_miss_tokens: 1_000,
                        cache_write_tokens: 0,
                        cost_usd: cost,
                        duration_ms: 500,
                        retries: 0,
                        tool_calls: 1,
                        usage_complete: true,
                        sub_agent_id: None,
                    },
                )
                .expect("receipt");
        }

        let mut budget = BudgetGuard::new(stella_protocol::BudgetMode::Off, None, None);
        budget.reseed_session_spend(10.25);
        let (in_tx, _in_rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
        let registry = ToolRegistry::new(std::env::temp_dir());

        close_dropped_execution(
            Some(&(Arc::clone(&store), id)),
            &registry,
            "cancelled",
            10.0,
            &mut budget,
            &in_tx,
        );

        assert!(
            (budget.session_spent_usd() - 14.5).abs() < 1e-9,
            "the guard must report the $4.50 the receipts prove, not the $0.25 \
             prefix it accumulated: {}",
            budget.session_spent_usd()
        );
    }
}
