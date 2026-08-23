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
    }
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
