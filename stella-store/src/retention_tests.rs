//! Tests for [`crate::retention`].
//!
//! Retention on a source of truth is the one feature where "it deleted
//! slightly too much" is unrecoverable, so most of these assert what
//! **survives** rather than what goes.

use crate::retention::{RetentionPolicy, RetentionReport};
use crate::{AgentEvent, Store, ToolCallRow};
use rusqlite::params;

/// An execution that finished `age_days` ago, with one event and one tool call
/// hanging off it so the cascade has something to remove.
fn aged_execution(store: &Store, prompt: &str, age_days: i64) -> i64 {
    let id = store
        .begin_execution("goal", prompt, "zai", "glm-5.2")
        .unwrap();
    store
        .record_event(
            id,
            0,
            &AgentEvent::Text {
                delta: format!("{prompt} output"),
            },
        )
        .unwrap();
    store
        .record_tool_calls(
            id,
            &[ToolCallRow {
                call_id: format!("call-{prompt}"),
                name: "read_file".into(),
                surface: "native".into(),
                args_json: "{}".into(),
                args_digest: "d0".into(),
                reason: String::new(),
                ok: true,
                error: String::new(),
                bytes_out: 128,
                duration_ms: 3,
            }],
        )
        .unwrap();
    store.finish_execution(id, "success", 0.01).unwrap();
    backdate(store, id, age_days);
    id
}

/// Move an execution's timestamps `age_days` into the past. Direct SQL: the
/// public API always stamps `CURRENT_TIMESTAMP`, and these tests need a
/// deterministic age.
fn backdate(store: &Store, id: i64, age_days: i64) {
    store
        .lock()
        .execute(
            "UPDATE executions \
             SET finished_at = datetime('now', ?1), started_at = datetime('now', ?1) \
             WHERE id = ?2",
            params![format!("-{age_days} days"), id],
        )
        .unwrap();
}

/// An execution left in flight — begun and backdated, never finished.
fn unfinished_execution(store: &Store, prompt: &str, age_days: i64) -> i64 {
    let id = store
        .begin_execution("goal", prompt, "zai", "glm-5.2")
        .unwrap();
    store
        .lock()
        .execute(
            "UPDATE executions SET started_at = datetime('now', ?1) WHERE id = ?2",
            params![format!("-{age_days} days"), id],
        )
        .unwrap();
    id
}

fn mark_export_pending(store: &Store, execution_id: i64) {
    store
        .lock()
        .execute(
            "INSERT INTO enterprise_export_ledger \
             (sink_fingerprint, execution_id, export_nonce, status) \
             VALUES ('sink-a', ?1, 'nonce', 'pending')",
            params![execution_id],
        )
        .unwrap();
}

fn ninety_days() -> RetentionPolicy {
    RetentionPolicy {
        older_than: Some("-90 days".to_string()),
        ..Default::default()
    }
}

// ---- the happy path -----------------------------------------------------

#[test]
fn the_age_cutoff_removes_old_executions_and_cascades_their_rows() {
    let store = Store::in_memory().unwrap();
    aged_execution(&store, "ancient", 200);
    let recent = aged_execution(&store, "recent", 3);

    let report = store.prune(&ninety_days()).unwrap();

    assert_eq!(report.aged_out, 1, "only the 200-day-old turn is past 90d");
    assert!(
        report.child_rows_removed >= 2,
        "its event and tool call must go with it, got {}",
        report.child_rows_removed
    );
    assert_eq!(store.count("executions").unwrap(), 1);
    assert_eq!(
        store.count("events").unwrap(),
        1,
        "the recent turn's event must survive"
    );
    // And the survivor is the right one.
    let surviving: i64 = store
        .lock()
        .query_row("SELECT id FROM executions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(surviving, recent);
}

#[test]
fn a_cascade_leaves_no_orphans_behind() {
    let store = Store::in_memory().unwrap();
    let doomed = aged_execution(&store, "ancient", 200);
    aged_execution(&store, "recent", 1);

    store.prune(&ninety_days()).unwrap();

    // Every execution-scoped table must be free of the pruned id — an orphan
    // pointing at a missing turn reads back as corruption.
    for table in ["events", "tool_calls", "telemetry", "files_touched"] {
        let orphans: i64 = store
            .lock()
            .query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE execution_id = ?1"),
                params![doomed],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0, "`{table}` still holds rows for a pruned turn");
    }
}

// ---- what must never be deleted -----------------------------------------

#[test]
fn an_unfinished_execution_is_never_pruned_even_when_ancient() {
    let store = Store::in_memory().unwrap();
    let in_flight = unfinished_execution(&store, "still running", 500);

    let report = store.prune(&ninety_days()).unwrap();

    assert_eq!(report.aged_out, 0, "an in-flight turn must not be pruned");
    assert_eq!(
        report.protected_unfinished, 1,
        "and must be reported as held"
    );
    assert_eq!(store.count("executions").unwrap(), 1);

    // `force` does not lift this one — it is structural, not policy.
    let forced = RetentionPolicy {
        force: true,
        ..ninety_days()
    };
    store.prune(&forced).unwrap();
    assert_eq!(
        store.count("executions").unwrap(),
        1,
        "--force must not reach an unfinished execution"
    );
    let still_there: i64 = store
        .lock()
        .query_row("SELECT id FROM executions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(still_there, in_flight);
}

#[test]
fn forgotten_tombstones_survive_every_policy() {
    let store = Store::in_memory().unwrap();
    aged_execution(&store, "ancient", 900);
    store
        .lock()
        .execute(
            "INSERT INTO forgotten (surface, item_id, content, reason, forgotten_at) \
             VALUES ('memory', 'm-1', 'the forgotten text', 'user asked', \
                     datetime('now', '-900 days'))",
            [],
        )
        .unwrap();

    let scorched = RetentionPolicy {
        older_than: Some("-1 days".to_string()),
        max_executions: Some(0),
        force: true,
        vacuum: true,
        ..Default::default()
    };
    store.prune(&scorched).unwrap();

    assert_eq!(
        store.count("forgotten").unwrap(),
        1,
        "a tombstone is the record that something was forgotten — dropping it \
         un-forgets that content on the next re-mine"
    );
}

#[test]
fn a_pending_export_holds_an_execution_back_until_forced() {
    let store = Store::in_memory().unwrap();
    let pending = aged_execution(&store, "not yet drained", 200);
    aged_execution(&store, "drained", 200);
    mark_export_pending(&store, pending);

    let report = store.prune(&ninety_days()).unwrap();
    assert_eq!(report.aged_out, 1, "only the drained turn may go");
    assert_eq!(report.protected_pending_export, 1);
    assert_eq!(store.count("executions").unwrap(), 1);

    // `force` is exactly what lifts this guard.
    let report = store
        .prune(&RetentionPolicy {
            force: true,
            ..ninety_days()
        })
        .unwrap();
    assert_eq!(report.aged_out, 1);
    assert_eq!(store.count("executions").unwrap(), 0);
}

#[test]
fn cross_turn_reflections_are_not_owned_by_any_execution_and_survive() {
    let store = Store::in_memory().unwrap();
    let doomed = aged_execution(&store, "ancient", 200);
    store
        .lock()
        .execute(
            "INSERT INTO reflections (execution_id, kind, content, occurred_at) \
             VALUES (NULL, 'insight', 'a cross-turn lesson', 0)",
            [],
        )
        .unwrap();
    store
        .lock()
        .execute(
            "INSERT INTO reflections (execution_id, kind, content, occurred_at) \
             VALUES (?1, 'insight', 'tied to one turn', 0)",
            params![doomed],
        )
        .unwrap();

    store.prune(&ninety_days()).unwrap();

    let remaining: i64 = store
        .lock()
        .query_row("SELECT COUNT(*) FROM reflections", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        remaining, 1,
        "the NULL-execution reflection belongs to no turn and must survive"
    );
    let is_null: i64 = store
        .lock()
        .query_row(
            "SELECT COUNT(*) FROM reflections WHERE execution_id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(is_null, 1);
}

// ---- dry run, ceiling, and the no-op guard ------------------------------

#[test]
fn a_dry_run_reports_exactly_what_a_real_run_would_do_and_deletes_nothing() {
    let store = Store::in_memory().unwrap();
    for i in 0..4 {
        aged_execution(&store, &format!("ancient-{i}"), 200);
    }
    aged_execution(&store, "recent", 2);
    let before = store.count("executions").unwrap();
    let events_before = store.count("events").unwrap();

    let dry = store
        .prune(&RetentionPolicy {
            dry_run: true,
            ..ninety_days()
        })
        .unwrap();

    assert_eq!(
        store.count("executions").unwrap(),
        before,
        "dry run deleted"
    );
    assert_eq!(store.count("events").unwrap(), events_before);
    assert!(!dry.vacuumed, "a dry run must not VACUUM");

    let wet = store.prune(&ninety_days()).unwrap();
    assert_eq!(
        dry.aged_out, wet.aged_out,
        "the dry run's headline number must be the truth"
    );
    assert_eq!(dry.child_rows_removed, wet.child_rows_removed);
    assert_eq!(store.count("executions").unwrap(), 1);
}

#[test]
fn the_row_ceiling_evicts_oldest_first() {
    let store = Store::in_memory().unwrap();
    let mut ids = Vec::new();
    for i in 0..5 {
        // Ages descend, so the lowest id is the oldest.
        ids.push(aged_execution(&store, &format!("turn-{i}"), 50 - i));
    }

    let report = store
        .prune(&RetentionPolicy {
            max_executions: Some(2),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(report.ceiling_evicted, 3);
    assert_eq!(store.count("executions").unwrap(), 2);
    let survivors: Vec<i64> = {
        let conn = store.lock();
        let mut stmt = conn
            .prepare("SELECT id FROM executions ORDER BY id ASC")
            .unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0)).unwrap();
        rows.collect::<std::result::Result<_, _>>().unwrap()
    };
    assert_eq!(
        survivors,
        vec![ids[3], ids[4]],
        "the two newest turns must be the ones kept"
    );
}

#[test]
fn the_ceiling_reports_what_protection_kept_it_from_reaching() {
    let store = Store::in_memory().unwrap();
    for i in 0..3 {
        let id = aged_execution(&store, &format!("turn-{i}"), 10);
        mark_export_pending(&store, id);
    }

    let report = store
        .prune(&RetentionPolicy {
            max_executions: Some(0),
            ..Default::default()
        })
        .unwrap();

    assert_eq!(report.ceiling_evicted, 0);
    assert_eq!(
        report.still_over_ceiling, 3,
        "a ceiling that could not be reached must say so, not silently under-deliver"
    );
    assert_eq!(store.count("executions").unwrap(), 3);
}

#[test]
fn a_policy_that_asks_for_nothing_is_an_error_not_a_silent_noop() {
    let store = Store::in_memory().unwrap();
    let err = store.prune(&RetentionPolicy::default()).unwrap_err();
    assert!(
        err.0.contains("--older-than") && err.0.contains("--max-executions"),
        "the error must name the knobs that would make it do something, got: {}",
        err.0
    );
    assert!(RetentionPolicy::default().is_noop());
}

#[test]
fn an_empty_report_is_empty() {
    let report = RetentionReport::default();
    assert!(report.is_empty());
    assert_eq!(report.executions_removed(), 0);
}

#[test]
fn pruning_an_empty_store_is_harmless() {
    let store = Store::in_memory().unwrap();
    let report = store.prune(&ninety_days()).unwrap();
    assert!(report.is_empty());
    assert_eq!(store.count("executions").unwrap(), 0);
}
