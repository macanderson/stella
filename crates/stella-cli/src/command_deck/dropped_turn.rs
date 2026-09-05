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
//! That read-back heals whatever the forwarder had already persisted and no
//! more, which left #4853: a `StepUsage` still sitting in the turn's channel at
//! the instant of the cancel is priced *after* `record_execution_end` closed
//! the row, so there is nothing to read back yet. [`close_dropped_turn`] waits
//! the forwarder out first, through
//! [`super::forwarder::drain_dropped_stream`], and only then closes. The two
//! halves are ordered rather than merged because each answers a different
//! question: the drain decides what the row *contains*, the read-back decides
//! what the guard *reports*.
//!
//! Lives here rather than inline in the driver's `TurnEnd` arms: those are in
//! a god file closed to growth.

use stella_core::BudgetGuard;

use super::*;

/// Wait out the dropped turn's forwarder, then close its execution.
///
/// The order is the fix (#4853). `close_dropped_execution` alone reports what
/// the store already knew at the instant the future was dropped; the events
/// still in the turn's channel are exactly the ones the driver's own running
/// total is missing, and they are priced by the forwarder rather than by
/// anything the driver holds. Draining first is what makes the row — and so
/// the read-back below — include them.
pub(super) async fn close_dropped_turn(
    drain: &forwarder::ForwarderSlot,
    execution: Option<&(Arc<Store>, i64)>,
    registry: &ToolRegistry,
    noun: &str,
    dispatch_spend_usd: f64,
    budget: &mut BudgetGuard,
    in_tx: &UnboundedSender<Inbound>,
) {
    forwarder::drain_dropped_stream(registry, drain).await;
    close_dropped_execution(execution, registry, noun, dispatch_spend_usd, budget, in_tx);
}

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
    let outcome =
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
    // `audit_complete` is false for every cancel — the provider-side usage
    // envelope is unknowable, not that anything failed to write. Warn on
    // `write_ok` alone, or this fires on every single cancel.
    if outcome.write_ok {
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
                        stream_seq: step,
                        turn_instance: None,
                        engine_step: None,
                        call_seq: None,
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

    /// A cancelled execution's usage envelope is unknowable, not unwritten —
    /// closing one out against a real store where every write succeeds must
    /// not tell the user their work was lost.
    #[tokio::test]
    async fn a_clean_cancel_warns_nobody() {
        let root = tempfile::tempdir().expect("workspace");
        let store = Arc::new(Store::open(root.path()).expect("store"));
        let id = store
            .begin_execution("chat", "a prompt", "zai", "glm-5.2")
            .expect("execution");

        let mut budget = BudgetGuard::new(stella_protocol::BudgetMode::Off, None, None);
        let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
        let registry = ToolRegistry::new(std::env::temp_dir());

        close_dropped_execution(
            Some(&(Arc::clone(&store), id)),
            &registry,
            "cancelled",
            0.0,
            &mut budget,
            &in_tx,
        );
        drop(in_tx);

        let mut events = Vec::new();
        while let Some(inbound) = in_rx.recv().await {
            events.push(inbound);
        }
        assert!(
            events.is_empty(),
            "every write succeeded; the deck must not tell the user otherwise: {events:?}"
        );
    }

    /// One priced `StepUsage`, as the engine would emit it.
    fn priced_step_usage(cost_usd: f64) -> AgentEvent {
        AgentEvent::StepUsage {
            step: 0,
            turn_instance: Some(0),
            call_seq: Some(0),
            role: stella_protocol::ModelCallRole::Worker,
            provider: "zai".into(),
            upstream_provider: None,
            output_text: None,
            model: "glm-5.2".into(),
            input_tokens: 12_000,
            output_tokens: 450,
            cached_input_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: None,
            estimated_input_tokens: 12_000,
            cost_usd,
            duration_ms: 1_830,
            retries: 0,
            tool_calls: 0,
            complete: true,
            finish_reason: None,
            effort: None,
            max_output_tokens: None,
            temperature: None,
            params: None,
            sub_agent_id: None,
            task_id: None,
        }
    }

    /// A cancelled turn's live lane: a forwarder parked in the slot with one
    /// priced `StepUsage` still unread in its channel, and the turn's own `tx`
    /// already gone the way the dropped future takes it.
    ///
    /// The registry keeps an `EventSender` clone, which is what makes the
    /// channel outlive `drop(tx)` — the #2290 condition, and the reason the
    /// drain has to detach before it awaits.
    fn cancelled_lane(
        registry: &ToolRegistry,
        store: &Arc<Store>,
        id: i64,
        cost_usd: f64,
    ) -> (
        forwarder::ForwarderSlot,
        tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    ) {
        let (in_tx, in_rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let forwarder = spawn_forwarder(
            rx,
            Some((Arc::clone(store), id)),
            crate::cache_insight::InsightScope {
                provider_id: "zai".into(),
                cache_ttl: stella_model::CacheTtl::default(),
                opens_execute_stage: true,
            },
            in_tx,
            LEAD.to_string(),
            None,
            SharedRevisions::default(),
        );
        registry.attach_events(stella_core::EventSender::new(tx.clone()));
        tx.send(priced_step_usage(cost_usd)).expect("in flight");
        // The turn future is dropped: its own sender goes with it, the
        // registry's clone does not, and the event is still in the channel.
        drop(tx);
        let slot = forwarder::forwarder_slot();
        *slot.lock().expect("slot") = Some(forwarder);
        (slot, in_rx)
    }

    /// **Witness (#4853).** A `StepUsage` still in flight when the user
    /// cancels is priced before the row closes, not after it.
    ///
    /// Nothing awaits between the send and the close-out, so on a
    /// current-thread runtime the forwarder cannot have run: the receipt is
    /// unwritten unless something drains the stream first. #2807's read-back
    /// heals only what the store already holds, so without the drain it reads
    /// back a row that knows nothing about this call and the deck reports the
    /// pre-cancel prefix — the same one-directional under-report as #2570, on
    /// exactly the turn the user is looking at.
    #[tokio::test]
    async fn a_usage_event_still_in_flight_at_the_cancel_is_priced_into_the_closed_row() {
        let root = tempfile::tempdir().expect("workspace");
        let store = Arc::new(Store::open(root.path()).expect("store"));
        let id = store
            .begin_execution("chat", "a prompt", "zai", "glm-5.2")
            .expect("execution");
        let registry = ToolRegistry::new(std::env::temp_dir());
        let (drain, _in_rx) = cancelled_lane(&registry, &store, id, 3.75);

        let mut budget = BudgetGuard::new(stella_protocol::BudgetMode::Off, None, None);
        budget.reseed_session_spend(10.0);
        let (in_tx, _tx_rx) = tokio::sync::mpsc::unbounded_channel::<Inbound>();

        close_dropped_turn(
            &drain,
            Some(&(Arc::clone(&store), id)),
            &registry,
            "cancelled",
            10.0,
            &mut budget,
            &in_tx,
        )
        .await;

        let summary = store
            .execution_summary(id)
            .expect("readable")
            .expect("a closed row");
        assert!(
            (summary.cost_usd - 3.75).abs() < 1e-9,
            "the row must carry the receipt the drain settled, not $0: {}",
            summary.cost_usd
        );
        assert!(
            (budget.session_spent_usd() - 13.75).abs() < 1e-9,
            "the deck must report the $3.75 this turn actually cost, not the \
             $0 prefix the guard held when the future was dropped: {}",
            budget.session_spent_usd()
        );
    }
}
