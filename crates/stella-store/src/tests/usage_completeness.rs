//! Durable execution-accounting completeness witnesses.

use crate::*;

const SINK: &str = "sink_0000000000000000000000000000000000000000000000000000000000000000";

#[test]
fn new_execution_is_pending_and_not_rollupable() {
    let store = Store::in_memory().unwrap();
    let execution = store
        .begin_execution("pipeline", "private", "anthropic", "claude")
        .unwrap();

    assert!(!store.execution_usage_complete(execution).unwrap());
    let status: String = store
        .lock()
        .query_row(
            "SELECT usage_status FROM executions WHERE id = ?1",
            params![execution],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "pending");
    assert!(
        store
            .execution_rollup(execution, std::path::Path::new("/tmp/project"))
            .unwrap()
            .is_none()
    );
}

/// **The #4171 witness**, and the replacement for
/// `incomplete_closeout_is_durable_and_not_rollupable`, which pinned the
/// behaviour this fixes: an execution short by one call rolled up as nothing,
/// so its real spend read as `$0.00` everywhere.
///
/// The flag stays durable and the row now arrives carrying it. `cost_usd` was
/// already the lower bound (`finish_execution_accounted` stores
/// `MAX(reported, RECEIPTS_TOTAL_USD)`), so the gate was withholding a number
/// the store had all along.
#[test]
fn incomplete_closeout_is_durable_and_rolls_up_as_a_floor() {
    let store = Store::in_memory().unwrap();
    let execution = store
        .begin_execution("pipeline", "private", "anthropic", "claude")
        .unwrap();

    store
        .finish_execution_accounted(execution, "aborted", 0.25, false)
        .unwrap();

    assert!(!store.execution_usage_complete(execution).unwrap());
    let rollup = store
        .execution_rollup(execution, std::path::Path::new("/tmp/project"))
        .unwrap()
        .expect("an incomplete execution rolls up as a floor, never as silence");
    assert!(
        !rollup.usage_complete,
        "and says so, or the hub cannot mark the figure"
    );
    assert!(
        (rollup.cost_usd - 0.25).abs() < 1e-9,
        "the receipts prove at least this much: {}",
        rollup.cost_usd
    );
}

#[test]
fn clean_finalization_is_the_only_rollupable_state() {
    let store = Store::in_memory().unwrap();
    let execution = store
        .begin_execution("pipeline", "private", "anthropic", "claude")
        .unwrap();
    store
        .finish_execution_accounted(execution, "completed", 0.25, true)
        .unwrap();

    assert!(store.execution_usage_complete(execution).unwrap());
    let rollup = store
        .execution_rollup(execution, std::path::Path::new("/tmp/project"))
        .unwrap()
        .expect("complete finalized rollup");
    assert!(rollup.usage_complete);
}

#[test]
fn pending_page_skips_incomplete_rows_without_consuming_them() {
    let store = Store::in_memory().unwrap();
    store.begin_enterprise_enrollment(SINK).unwrap();

    let mut incomplete = Vec::new();
    for _ in 0..256 {
        let id = store
            .begin_execution("pipeline", "private", "anthropic", "claude")
            .unwrap();
        store
            .finish_execution_accounted(id, "aborted", 0.25, false)
            .unwrap();
        assert!(
            store
                .mark_enterprise_export_pending(SINK, id)
                .unwrap()
                .is_some()
        );
        incomplete.push(id);
    }
    let complete = store
        .begin_execution("pipeline", "private", "anthropic", "claude")
        .unwrap();
    store
        .finish_execution_accounted(complete, "completed", 0.5, true)
        .unwrap();
    assert!(
        store
            .mark_enterprise_export_pending(SINK, complete)
            .unwrap()
            .is_some()
    );

    let page = store.pending_enterprise_export_page(SINK, None, 1).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].execution_id, complete);
    let retained: i64 = store
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM enterprise_export_ledger WHERE status = 'pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(retained, 257, "incomplete intents remain retryable");
    assert!(incomplete.iter().all(|id| *id < complete));
}

/// The other half of #4171: the floor has to survive replication, or the
/// report can total it but never mark it.
///
/// `stella usage report` reads the hub's `telemetry` replica, which never
/// filtered on the flag — so the *spend* of an incomplete execution was
/// already counted there. What was missing is the marking: nothing in the hub
/// said the figure was a lower bound, and the `execution_rollup` row (which
/// every per-project total reads) was withheld entirely.
#[test]
fn the_hub_marks_an_incomplete_executions_spend_as_a_floor() {
    let workspace = tempfile::tempdir().unwrap();
    let hub_dir = tempfile::tempdir().unwrap();
    let hub = crate::usage::UsageStore::open_at(&hub_dir.path().join("usage.db")).unwrap();
    let store = Store::in_memory().unwrap();

    let short = store
        .begin_execution("deck", "ship it", "openrouter", "kimi")
        .unwrap();
    store
        .record_telemetry(short, &telemetry_row(1, 2.40))
        .unwrap();
    store
        .finish_execution_accounted(short, "completed", 2.40, false)
        .unwrap();
    let exact = store
        .begin_execution("deck", "ship it again", "openrouter", "kimi")
        .unwrap();
    store
        .record_telemetry(exact, &telemetry_row(1, 1.00))
        .unwrap();
    store
        .finish_execution_accounted(exact, "completed", 1.00, true)
        .unwrap();

    for id in [short, exact] {
        assert!(
            store.sync_to_usage(id, workspace.path(), &hub).unwrap(),
            "execution {id} must reach the hub"
        );
    }

    let rows = hub.global_telemetry_totals(None).unwrap();
    assert_eq!(rows.len(), 1, "one (org, provider, model) line: {rows:?}");
    assert!(
        (rows[0].cost_usd - 3.40).abs() < 1e-9,
        "both executions' spend is counted: {}",
        rows[0].cost_usd
    );
    assert_eq!(
        rows[0].floor_executions, 1,
        "and exactly one of them is a floor: {rows:?}"
    );

    drop(hub);
    let conn = rusqlite::Connection::open(hub_dir.path().join("usage.db")).unwrap();
    let mut stmt = conn
        .prepare("SELECT usage_complete FROM execution_rollup ORDER BY execution_id")
        .unwrap();
    let flags: Vec<i64> = stmt
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(std::result::Result::unwrap)
        .collect();
    assert_eq!(
        flags,
        vec![0, 1],
        "the rollup row exists for both and carries the flag"
    );
}

/// #4485: a telemetry row whose `execution_rollup` row `prune` deleted first
/// must read as **unknown**, never as silently complete.
///
/// `execution_rollup` and `telemetry` age out on predicates they do not
/// share (`crate::usage`'s `prune`; `usage/tool_fold.rs` documents the same
/// asymmetry for #3411): an org-scoped, un-acked `telemetry` row survives an
/// age cutoff its rollup row does not, because only `telemetry` consults the
/// cloud-drain guard. This test skips reproducing that exact age-cutoff
/// timing (env-dependent org registration, multi-connection WAL races) and
/// goes straight to the state it produces — a hub with `telemetry` for an
/// execution but no matching `execution_rollup` row — which is the only
/// thing [`super::global_report`]'s query can see either way.
#[test]
fn a_pruned_rollup_row_reports_as_unknown_never_as_complete() {
    let workspace = tempfile::tempdir().unwrap();
    let hub_dir = tempfile::tempdir().unwrap();
    let hub = crate::usage::UsageStore::open_at(&hub_dir.path().join("usage.db")).unwrap();
    let store = Store::in_memory().unwrap();

    let execution = store
        .begin_execution("deck", "ship it", "openrouter", "kimi")
        .unwrap();
    store
        .record_telemetry(execution, &telemetry_row(1, 2.40))
        .unwrap();
    store
        .finish_execution_accounted(execution, "completed", 2.40, true)
        .unwrap();
    assert!(
        store
            .sync_to_usage(execution, workspace.path(), &hub)
            .unwrap(),
        "execution must reach the hub"
    );

    let before = hub.global_telemetry_totals(None).unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].floor_executions, 0, "synced complete, no floor");
    assert_eq!(
        before[0].unknown_executions, 0,
        "rollup row present, no gap"
    );

    // What `prune`'s age-cutoff branch does to `execution_rollup` on its own
    // predicate (unconditional -- `crate::usage`'s age-cutoff branch --
    // while `telemetry` only ages out when `prunable_predicate` allows it).
    // A second connection is deliberate: it is exactly what a real prune run
    // is -- a write against the same WAL-mode file the open `hub` handle
    // is still reading from.
    {
        let conn = rusqlite::Connection::open(hub_dir.path().join("usage.db")).unwrap();
        let deleted = conn.execute("DELETE FROM execution_rollup", []).unwrap();
        assert_eq!(deleted, 1, "the rollup row for this execution existed");
    }

    let after = hub.global_telemetry_totals(None).unwrap();
    assert_eq!(after.len(), 1, "the telemetry row still reports: {after:?}");
    assert_eq!(
        after[0].floor_executions, 0,
        "no rollup row left to say it IS a floor: {after:?}"
    );
    assert_eq!(
        after[0].unknown_executions, 1,
        "but it must not silently read as complete either: {after:?}"
    );
    assert!(
        (after[0].cost_usd - 2.40).abs() < 1e-9,
        "the spend is still summed from telemetry: {}",
        after[0].cost_usd
    );
}

/// One paid call at `cost_usd`, with everything else the shape a row needs.
fn telemetry_row(step: u64, cost_usd: f64) -> TelemetryRow {
    TelemetryRow {
        step,
        provider: "openrouter".into(),
        call_role: "worker".into(),
        model: "kimi".into(),
        input_tokens: 100,
        estimated_input_tokens: 100,
        output_tokens: 10,
        cache_read_tokens: 0,
        cache_miss_tokens: 0,
        cache_write_tokens: 0,
        cost_usd,
        duration_ms: 5,
        retries: 0,
        tool_calls: 0,
        usage_complete: true,
        sub_agent_id: None,
    }
}

/// **#4383's witness, at the ledger.** A turn's calls can be grouped by
/// spender, which is what a `(role, model)` census needs and what execution 225
/// of session `ses-1787465453163-60967` could not offer: ninety rows, all
/// `worker`, five of them a parallel delegate fan-out.
///
/// A sub-agent opens no execution row of its own, so both the lead's calls and
/// its delegates' land under one `execution_id`. The column is the only thing
/// that separates them.
#[test]
fn a_turns_calls_can_be_grouped_by_the_sub_agent_that_spent_them() {
    let store = Store::in_memory().unwrap();
    let execution = store
        .begin_execution("run", "research the engine", "openrouter", "kimi")
        .unwrap();
    store
        .record_telemetry(execution, &telemetry_row(1, 0.10))
        .unwrap();
    for (step, agent) in [
        (2, "researcher-0"),
        (3, "researcher-1"),
        (4, "researcher-0"),
    ] {
        store
            .record_telemetry(
                execution,
                &TelemetryRow {
                    sub_agent_id: Some(agent.into()),
                    ..telemetry_row(step, 0.01)
                },
            )
            .unwrap();
    }

    let mut by_spender: Vec<(Option<String>, usize)> = Vec::new();
    for row in store.telemetry_rows_after(0, 100).unwrap() {
        let owner = row.telemetry.sub_agent_id.clone();
        match by_spender.iter_mut().find(|(seen, _)| *seen == owner) {
            Some((_, count)) => *count += 1,
            None => by_spender.push((owner, 1)),
        }
    }
    by_spender.sort();
    assert_eq!(
        by_spender,
        vec![
            (None, 1),
            (Some("researcher-0".into()), 2),
            (Some("researcher-1".into()), 1),
        ],
        "the lead's own call and each delegate's must be separable"
    );
}

#[test]
fn marking_usage_incomplete_is_monotonic() {
    let store = Store::in_memory().unwrap();
    let execution = store
        .begin_execution("pipeline", "private", "anthropic", "claude")
        .unwrap();
    store.mark_execution_usage_incomplete(execution).unwrap();
    store
        .finish_execution(execution, "completed", 0.25)
        .unwrap();
    assert!(!store.execution_usage_complete(execution).unwrap());
}
