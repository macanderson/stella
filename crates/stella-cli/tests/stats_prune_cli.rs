//! End-to-end witness for `stella stats prune` (#616): drive the real binary
//! against a real workspace store and a real (isolated) usage hub.
//!
//! `usage.db` has had a retention engine since it shipped; `store.db` — which
//! holds the full prompts, event payloads, tool arguments, and context-block
//! preimages — had none, so every table grew forever. These tests exercise the
//! command: the flags, the replication guard that keeps a prune from
//! destroying un-rolled-up cost data, and the hub cursor repair that keeps
//! replication working *after* a prune. The cascade itself is covered by
//! `stella-store`'s own tests.
//!
//! Every invocation gets its own `STELLA_DATA_DIR`, so the tests never touch
//! (or prune against) the developer's real `~/.stella/usage.db`.

use std::path::Path;
use std::process::{Command, Output};

use stella_store::{Store, TelemetryRow};

fn telemetry(step: u64) -> TelemetryRow {
    TelemetryRow {
        step,
        provider: "anthropic".into(),
        call_role: "worker".into(),
        model: "opus".into(),
        input_tokens: 1_000,
        estimated_input_tokens: 900,
        output_tokens: 100,
        cache_read_tokens: 0,
        cache_miss_tokens: 0,
        cache_write_tokens: 0,
        cost_usd: 0.02,
        duration_ms: 500,
        retries: 0,
        tool_calls: 0,
        usage_complete: true,
    }
}

/// One execution with an event and a telemetry row — enough that pruning it
/// has something to cascade to and something the hub cares about.
fn record_turn(store: &Store, prompt: &str) -> i64 {
    let id = store
        .begin_execution("run", prompt, "anthropic", "opus")
        .expect("execution");
    store
        .record_event(
            id,
            0,
            &stella_protocol::AgentEvent::Text {
                text: format!("worked on {prompt}"),
            },
        )
        .expect("event");
    store
        .record_telemetry(id, &telemetry(0))
        .expect("telemetry");
    id
}

fn store_path(workspace: &Path) -> std::path::PathBuf {
    workspace.join(".stella/private/store.db")
}

/// Backdate executions so the age window can reach them. Done through a direct
/// connection because `started_at` defaults to CURRENT_TIMESTAMP and no public
/// API rewrites it — the store has no "pretend this turn is old" seam.
fn backdate(workspace: &Path, up_to_id: i64) {
    let conn = rusqlite::Connection::open(store_path(workspace)).expect("open store");
    conn.execute(
        "UPDATE executions SET started_at = '2000-01-01 00:00:00' WHERE id <= ?1",
        [up_to_id],
    )
    .expect("backdate");
}

fn count(workspace: &Path, table: &str) -> i64 {
    let conn = rusqlite::Connection::open(store_path(workspace)).expect("open store");
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .expect("count")
}

/// A workspace with three recorded turns, the oldest `backdated_through` of
/// them aged out of any sane retention window. Returns the workspace and the
/// isolated data dir its hub lives in.
fn seeded(backdated_through: i64) -> (tempfile::TempDir, tempfile::TempDir) {
    let workspace = tempfile::tempdir().expect("workspace");
    let data = tempfile::tempdir().expect("data dir");
    {
        let store = Store::open(workspace.path()).expect("store");
        for n in 0..3 {
            record_turn(&store, &format!("turn {n}"));
        }
    }
    backdate(workspace.path(), backdated_through);
    (workspace, data)
}

fn run(workspace: &Path, data: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_stella"))
        .args(args)
        .current_dir(workspace)
        .env("STELLA_DATA_DIR", data)
        .output()
        .expect("run stella")
}

fn ok(workspace: &Path, data: &Path, args: &[&str]) -> String {
    let output = run(workspace, data, args);
    assert!(
        output.status.success(),
        "stella {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn prune_refuses_a_policy_that_would_do_nothing() {
    let (workspace, data) = seeded(0);
    let output = run(workspace.path(), data.path(), &["stats", "prune"]);
    assert!(
        !output.status.success(),
        "a no-op prune is a usage error, not a silent success"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--older-than") && stderr.contains("--max-rows"),
        "the error names the flags that would make it do something: {stderr}"
    );
}

#[test]
fn prune_spares_executions_whose_telemetry_has_not_reached_the_hub() {
    let (workspace, data) = seeded(2);
    // No `usage sync` has run, so the hub's cursor is 0 and every turn that
    // made a model call is still holding un-rolled-up cost data.
    let out = ok(
        workspace.path(),
        data.path(),
        &["stats", "prune", "--older-than", "1d"],
    );
    assert!(
        out.contains("kept 2 execution(s)"),
        "the guard is reported, not silent: {out}"
    );
    assert!(
        out.contains("stella usage sync"),
        "and it says how to unblock: {out}"
    );
    assert_eq!(count(workspace.path(), "executions"), 3, "nothing dropped");
    assert_eq!(count(workspace.path(), "telemetry"), 3);

    // --force accepts the loss.
    let out = ok(
        workspace.path(),
        data.path(),
        &["stats", "prune", "--older-than", "1d", "--force"],
    );
    assert!(out.contains("removed 2 aged-out execution(s)"), "{out}");
    assert_eq!(count(workspace.path(), "executions"), 1);
}

#[test]
fn a_dry_run_reports_the_same_prune_without_deleting_anything() {
    let (workspace, data) = seeded(2);
    ok(workspace.path(), data.path(), &["usage", "sync"]);

    let out = ok(
        workspace.path(),
        data.path(),
        &["stats", "prune", "--older-than", "1d", "--dry-run"],
    );
    assert!(
        out.contains("would remove 2 aged-out execution(s)"),
        "dry run speaks in the conditional: {out}"
    );
    assert_eq!(count(workspace.path(), "executions"), 3, "nothing deleted");
    assert_eq!(count(workspace.path(), "events"), 3);
    assert_eq!(count(workspace.path(), "telemetry"), 3);
}

#[test]
fn prune_drops_replicated_executions_and_their_dependent_rows() {
    let (workspace, data) = seeded(2);
    ok(workspace.path(), data.path(), &["usage", "sync"]);

    let out = ok(
        workspace.path(),
        data.path(),
        &["stats", "prune", "--older-than", "1d", "--vacuum"],
    );
    assert!(out.contains("removed 2 aged-out execution(s)"), "{out}");
    assert!(
        out.contains("dependent row(s)"),
        "the cascade is reported: {out}"
    );
    assert!(out.contains("vacuumed"), "{out}");
    assert_eq!(count(workspace.path(), "executions"), 1);
    assert_eq!(
        count(workspace.path(), "events"),
        1,
        "events went with them"
    );
    assert_eq!(count(workspace.path(), "telemetry"), 1);
}

#[test]
fn max_rows_evicts_the_oldest_executions_first() {
    let (workspace, data) = seeded(0);
    ok(workspace.path(), data.path(), &["usage", "sync"]);

    let out = ok(
        workspace.path(),
        data.path(),
        &["stats", "prune", "--max-rows", "1"],
    );
    assert!(
        out.contains("removed 2 execution(s) to meet the --max-rows ceiling"),
        "{out}"
    );
    let conn = rusqlite::Connection::open(store_path(workspace.path())).expect("open");
    let survivor: String = conn
        .query_row("SELECT prompt FROM executions", [], |r| r.get(0))
        .expect("one survivor");
    assert_eq!(survivor, "turn 2", "the NEWEST turn is the one kept");
}

/// The repair that makes a prune safe to repeat: `telemetry.rowid` has no
/// `AUTOINCREMENT`, so deleting the top row frees that rowid and SQLite hands
/// it to the very next model call. Left at its old value the hub's replication
/// cursor sits above that reused rowid and the new turn's cost data never
/// replicates — silently, forever. `stats prune` re-anchors the cursor.
#[test]
fn a_prune_that_empties_telemetry_leaves_the_next_turn_replicable() {
    // Every turn aged out, so the prune takes the whole telemetry table with it.
    let (workspace, data) = seeded(3);
    let synced = ok(workspace.path(), data.path(), &["usage", "sync"]);
    assert!(synced.contains("replicated 3"), "{synced}");

    let out = ok(
        workspace.path(),
        data.path(),
        &["stats", "prune", "--older-than", "1d"],
    );
    assert!(out.contains("removed 3 aged-out execution(s)"), "{out}");
    assert!(
        out.contains("re-anchored the usage hub replication cursor to 0"),
        "the cursor repair is reported: {out}"
    );
    assert_eq!(count(workspace.path(), "telemetry"), 0);

    // A new turn reuses rowid 1, under the pre-prune cursor of 3.
    {
        let store = Store::open(workspace.path()).expect("store");
        record_turn(&store, "the turn after the prune");
    }
    let synced = ok(workspace.path(), data.path(), &["usage", "sync"]);
    assert!(
        synced.contains("replicated 1 telemetry row(s)"),
        "a stale cursor would hide the reused rowid forever: {synced}"
    );
}
