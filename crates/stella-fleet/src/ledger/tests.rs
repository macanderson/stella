//! Unit tests for the ledger: schema versioning, orphan reporting, spend and
//! lineage bookkeeping, and the warmth-signal timestamps.
//!
//! Split out of `ledger.rs` once that file reached the line-count ratchet
//! with nothing left to trim. This is the whole `#[cfg(test)] mod tests`
//! block, moved with a `mod tests;` declaration and re-indented one level.
//! No other change.

use super::migrations::{MIGRATION_V1, SCHEMA_VERSION};
use super::*;
use crate::gc::WorktreeActivity;

/// A brand-new ledger enforces the run references, so no new orphan can
/// ever be created.
#[test]
fn a_fresh_ledger_enforces_run_references_and_rejects_an_unknown_run() {
    let ledger = Ledger::open_in_memory().expect("open");
    assert!(ledger.enforces_run_references().expect("ddl"));
    assert!(ledger.orphan_rows().expect("scan").is_empty());

    // Inserting a task for a run that does not exist must now fail.
    let err = ledger.conn.execute(
        "INSERT INTO tasks (run_id, task_id, title, isolation)
         VALUES ('ghost', 't1', 'title', 'shared')",
        [],
    );
    assert!(
        err.is_err(),
        "a fresh ledger must reject a task naming an unknown run"
    );
}

/// The other half of the ruling: a ledger that came up the legacy ladder is
/// left unconstrained — opening it must NOT fail, and must not delete the
/// orphan history it already holds. It is reported instead.
#[test]
fn a_legacy_ledger_keeps_its_orphans_and_reports_them() {
    // Build a v0 file with the unversioned schema, with an orphan row
    // already in it.
    let conn = Connection::open_in_memory().expect("conn");
    conn.execute_batch(MIGRATION_V1).expect("legacy schema");
    conn.execute(
        "INSERT INTO tasks (run_id, task_id, title, isolation)
         VALUES ('deleted-run', 't1', 'orphaned', 'shared')",
        [],
    )
    .expect("orphan row is accepted by the legacy shape");
    // `spend.attempt_id` has always been a real foreign key, so the attempt
    // has to exist — its own `run_id` is the orphaned part.
    conn.execute(
        "INSERT INTO attempts (run_id, task_id, worktree_path, branch, started_at_ms)
         VALUES ('deleted-run', 't1', '/w', 'fleet/t1', 0)",
        [],
    )
    .expect("orphan attempt row");
    let attempt_id: i64 = conn.last_insert_rowid();
    // `commits.attempt_id` has always been a real foreign key too; its
    // denormalized `run_id` is the orphaned part.
    conn.execute(
        "INSERT INTO commits (attempt_id, run_id, task_id, sha, branch, message, timestamp_ms)
         VALUES (?1, 'deleted-run', 't1', 'abc', 'fleet/t1', 'work', 0)",
        params![attempt_id],
    )
    .expect("orphan commit row");
    conn.execute(
        "INSERT INTO spend (run_id, task_id, attempt_id, cost_usd, recorded_at_ms)
         VALUES ('deleted-run', 't1', ?1, 1.0, 0)",
        params![attempt_id],
    )
    .expect("orphan spend row");

    // Now let the ledger take it through migrate() as a deployed file.
    let ledger = Ledger::init(conn).expect("a legacy file with orphans still opens");

    assert!(
        !ledger.enforces_run_references().expect("ddl"),
        "an existing file is left unconstrained on purpose"
    );

    let orphans = ledger.orphan_rows().expect("scan");
    assert_eq!(
        orphans,
        vec![
            OrphanRows {
                table: "tasks",
                column: "run_id",
                count: 1
            },
            OrphanRows {
                table: "attempts",
                column: "run_id",
                count: 1
            },
            OrphanRows {
                table: "commits",
                column: "run_id",
                count: 1
            },
            OrphanRows {
                table: "spend",
                column: "run_id",
                count: 1
            },
        ],
        "every orphan class is reported, none deleted"
    );

    // And the history is still there.
    let surviving: i64 = ledger
        .conn
        .query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))
        .expect("count");
    assert_eq!(surviving, 1, "the orphan row must not have been deleted");
}

fn task(id: &str) -> Task {
    Task::new(id, format!("title {id}"), "prompt")
}

fn commit(task_id: &str, sha: &str) -> CommitRecord {
    CommitRecord {
        sha: sha.into(),
        branch: format!("fleet/{task_id}"),
        task_id: task_id.into(),
        message: format!("work on {task_id}"),
        timestamp_ms: 1_000,
    }
}

fn seed_run(ledger: &Ledger, run_id: &str) {
    ledger
        .record_run(&RunRecord {
            id: run_id.into(),
            root_task_count: 1,
            created_at_ms: 1,
        })
        .unwrap();
    ledger.record_task(run_id, &task("t1")).unwrap();
}

/// Seed `run1`/`t1` and open one attempt on it.
fn seeded_attempt(ledger: &Ledger) -> AttemptId {
    seed_run(ledger, "run1");
    ledger
        .start_attempt(&AttemptStart {
            run_id: "run1".into(),
            task_id: "t1".into(),
            worktree_path: "/tmp/wt/t1".into(),
            branch: "fleet/t1".into(),
            started_at_ms: 10,
        })
        .unwrap()
}

#[test]
fn open_in_memory_applies_schema_and_is_empty() {
    let ledger = Ledger::open_in_memory().unwrap();
    assert_eq!(ledger.total_spend("run").unwrap(), 0.0);
    assert!(ledger.commits_for_task("run", "t1").unwrap().is_empty());
    assert!(ledger.lineage_children("run").unwrap().is_empty());
}

/// The GC's ledger half: a worktree whose attempt never finished must
/// report as in flight, and a finished one must carry the finish time the
/// `--age` arithmetic reads.
#[test]
fn worktree_activity_separates_in_flight_from_finished() {
    let ledger = Ledger::open_in_memory().unwrap();
    seed_run(&ledger, "run1");
    let live = ledger
        .start_attempt(&AttemptStart {
            run_id: "run1".into(),
            task_id: "t1".into(),
            worktree_path: "/wt/live".into(),
            branch: "fleet/live".into(),
            started_at_ms: 10,
        })
        .unwrap();
    let done = ledger
        .start_attempt(&AttemptStart {
            run_id: "run1".into(),
            task_id: "t1".into(),
            worktree_path: "/wt/done".into(),
            branch: "fleet/done".into(),
            started_at_ms: 11,
        })
        .unwrap();
    ledger
        .finish_attempt(&AttemptFinish {
            attempt_id: done,
            run_id: "run1".into(),
            task_id: "t1".into(),
            finished_at_ms: 42,
            success: true,
            summary: "done".into(),
            commits: vec![],
            cost_usd: 0.0,
            spend_at_ms: 42,
        })
        .unwrap();
    assert!(!ledger.attempt_is_finished(live).unwrap());

    let mut activity = ledger.worktree_activity().unwrap();
    activity.sort_by(|a, b| a.worktree_path.cmp(&b.worktree_path));
    assert_eq!(
        activity,
        vec![
            WorktreeActivity {
                worktree_path: "/wt/done".into(),
                unfinished_attempts: 0,
                last_finished_ms: Some(42),
            },
            WorktreeActivity {
                worktree_path: "/wt/live".into(),
                unfinished_attempts: 1,
                last_finished_ms: None,
            },
        ]
    );
}

#[test]
fn attempt_round_trips_commits_and_spend_atomically() {
    let ledger = Ledger::open_in_memory().unwrap();
    seed_run(&ledger, "run1");

    let attempt_id = ledger
        .start_attempt(&AttemptStart {
            run_id: "run1".into(),
            task_id: "t1".into(),
            worktree_path: "/tmp/wt/t1".into(),
            branch: "fleet/t1".into(),
            started_at_ms: 10,
        })
        .unwrap();
    assert!(!ledger.attempt_is_finished(attempt_id).unwrap());

    ledger
        .finish_attempt(&AttemptFinish {
            attempt_id,
            run_id: "run1".into(),
            task_id: "t1".into(),
            finished_at_ms: 20,
            success: true,
            summary: "done".into(),
            commits: vec![commit("t1", "aaa"), commit("t1", "bbb")],
            cost_usd: 0.25,
            spend_at_ms: 21,
        })
        .unwrap();

    assert!(ledger.attempt_is_finished(attempt_id).unwrap());
    let commits = ledger.commits_for_task("run1", "t1").unwrap();
    assert_eq!(commits.len(), 2);
    assert_eq!(commits[0].sha, "aaa");
    assert_eq!(commits[1].sha, "bbb");
    assert!((ledger.total_spend("run1").unwrap() - 0.25).abs() < 1e-9);
    assert!((ledger.task_spend("run1", "t1").unwrap() - 0.25).abs() < 1e-9);
}

/// `MIGRATION_V4`'s own `CHECK (cost_usd >= 0)` rejects a negative cost
/// even if a caller skips `Fleet::dispatch_claimed`'s Rust-side guard.
/// The rejection happens inside the transaction, so the attempt's own
/// close rolls back too — a rejected finish must not half-apply.
#[test]
fn finish_attempt_rejects_a_negative_cost_at_the_storage_layer() {
    let ledger = Ledger::open_in_memory().unwrap();
    let attempt_id = seeded_attempt(&ledger);
    let err = ledger.finish_attempt(&AttemptFinish {
        attempt_id,
        run_id: "run1".into(),
        task_id: "t1".into(),
        finished_at_ms: 20,
        success: true,
        summary: "done".into(),
        commits: vec![],
        cost_usd: -5.0,
        spend_at_ms: 21,
    });
    assert!(
        err.is_err(),
        "a negative cost_usd must violate the spend table's CHECK constraint"
    );
    assert!(
        !ledger.attempt_is_finished(attempt_id).unwrap(),
        "a rejected finish must roll back the attempt's own close, not half-apply"
    );
    assert_eq!(ledger.total_spend("run1").unwrap(), 0.0);
}

/// A NaN never reaches the `CHECK`: SQLite binds it as NULL, and
/// `cost_usd REAL NOT NULL` rejects that. A direct caller of `Ledger` —
/// it is `pub`, and `Fleet::dispatch_claimed`'s `is_finite()` guard sits
/// one layer above — gets a rejected close, not a poisoned total.
#[test]
fn finish_attempt_with_a_nan_cost_leaves_total_spend_finite() {
    let ledger = Ledger::open_in_memory().unwrap();
    let attempt_id = seeded_attempt(&ledger);
    let err = ledger.finish_attempt(&AttemptFinish {
        attempt_id,
        run_id: "run1".into(),
        task_id: "t1".into(),
        finished_at_ms: 20,
        success: true,
        summary: "done".into(),
        commits: vec![],
        cost_usd: f64::NAN,
        spend_at_ms: 21,
    });

    assert!(
        err.is_err(),
        "a NaN cost_usd binds as NULL and must violate cost_usd REAL NOT NULL"
    );
    assert!(
        !ledger.attempt_is_finished(attempt_id).unwrap(),
        "a rejected finish must roll back the attempt's own close, not half-apply"
    );
    assert!(
        ledger.total_spend("run1").unwrap().is_finite(),
        "a NaN spend row must never make the run's authoritative total unusable"
    );
}

/// A second `finish_attempt` for the SAME attempt must not double the
/// ledger's commits or spend — a retried dispatch can settle twice, and
/// the `WHERE finished_at_ms IS NULL` guard on the first close makes the
/// second call a no-op.
#[test]
fn a_second_finish_attempt_for_the_same_attempt_does_not_double_spend() {
    let ledger = Ledger::open_in_memory().unwrap();
    let attempt_id = seeded_attempt(&ledger);
    let finish = AttemptFinish {
        attempt_id,
        run_id: "run1".into(),
        task_id: "t1".into(),
        finished_at_ms: 20,
        success: true,
        summary: "done".into(),
        commits: vec![commit("t1", "aaa")],
        cost_usd: 0.5,
        spend_at_ms: 21,
    };

    ledger.finish_attempt(&finish).unwrap();
    // A retried close for the same attempt, real spend unchanged.
    ledger.finish_attempt(&finish).unwrap();

    assert!(
        (ledger.total_spend("run1").unwrap() - 0.5).abs() < 1e-9,
        "spend must not double on a repeated finish_attempt"
    );
    assert_eq!(
        ledger.commits_for_task("run1", "t1").unwrap().len(),
        1,
        "commits must not double on a repeated finish_attempt"
    );
}

#[test]
fn spend_sums_across_multiple_attempts_of_a_run() {
    let ledger = Ledger::open_in_memory().unwrap();
    seed_run(&ledger, "run1");
    ledger.record_task("run1", &task("t2")).unwrap();

    for (task_id, cost) in [("t1", 0.1), ("t2", 0.4)] {
        let attempt_id = ledger
            .start_attempt(&AttemptStart {
                run_id: "run1".into(),
                task_id: task_id.into(),
                worktree_path: format!("/tmp/{task_id}"),
                branch: format!("fleet/{task_id}"),
                started_at_ms: 1,
            })
            .unwrap();
        ledger
            .finish_attempt(&AttemptFinish {
                attempt_id,
                run_id: "run1".into(),
                task_id: task_id.into(),
                finished_at_ms: 2,
                success: true,
                summary: "ok".into(),
                commits: vec![],
                cost_usd: cost,
                spend_at_ms: 3,
            })
            .unwrap();
    }
    assert!((ledger.total_spend("run1").unwrap() - 0.5).abs() < 1e-9);
    assert!((ledger.task_spend("run1", "t1").unwrap() - 0.1).abs() < 1e-9);
    assert!((ledger.task_spend("run1", "t2").unwrap() - 0.4).abs() < 1e-9);
}

#[test]
fn retries_of_a_task_show_up_as_multiple_attempts() {
    let ledger = Ledger::open_in_memory().unwrap();
    seed_run(&ledger, "run1");
    for started in [1, 5] {
        ledger
            .start_attempt(&AttemptStart {
                run_id: "run1".into(),
                task_id: "t1".into(),
                worktree_path: "/tmp/t1".into(),
                branch: "fleet/t1".into(),
                started_at_ms: started,
            })
            .unwrap();
    }
    assert_eq!(ledger.attempt_count("run1", "t1").unwrap(), 2);
}

#[test]
fn lineage_records_parent_run_to_child_tasks() {
    let ledger = Ledger::open_in_memory().unwrap();
    seed_run(&ledger, "parent-run");
    ledger.record_lineage("parent-run", "t-child-b", 1).unwrap();
    ledger.record_lineage("parent-run", "t-child-a", 2).unwrap();
    assert_eq!(
        ledger.lineage_children("parent-run").unwrap(),
        vec!["t-child-a".to_string(), "t-child-b".to_string()]
    );
    assert!(ledger.lineage_children("other-run").unwrap().is_empty());
}

#[test]
fn re_dispatching_a_task_does_not_duplicate_its_lineage_edge() {
    // Restart is "the caller re-dispatching the same Task", and dispatch
    // stamps lineage once per attempt — but an edge is a fact about the
    // graph, not an attempt count (`attempts` already records retries).
    let ledger = Ledger::open_in_memory().unwrap();
    seed_run(&ledger, "run1");
    ledger.record_lineage("run1", "t1", 10).unwrap();
    ledger.record_lineage("run1", "t1", 40).unwrap();

    assert_eq!(
        ledger.lineage_children("run1").unwrap(),
        vec!["t1".to_string()],
        "a re-dispatch does not return the child twice"
    );
    let recorded: i64 = ledger
        .conn
        .query_row("SELECT recorded_at_ms FROM lineage", [], |r| r.get(0))
        .unwrap();
    assert_eq!(recorded, 10, "the first dispatch's timestamp is kept");
}

#[test]
fn record_run_refreshes_the_task_count_but_never_the_creation_time() {
    // `Fleet::new` stamps the creation time with a 0 task count; the
    // later `run_plan` fills the count in. That second write must not
    // rewrite the run's recorded creation time.
    let ledger = Ledger::open_in_memory().unwrap();
    for (root_task_count, created_at_ms) in [(0, 100), (3, 900)] {
        ledger
            .record_run(&RunRecord {
                id: "run1".into(),
                root_task_count,
                created_at_ms,
            })
            .unwrap();
    }
    let (count, created): (u32, i64) = ledger
        .conn
        .query_row(
            "SELECT root_task_count, created_at_ms FROM runs WHERE id = ?1",
            params!["run1"],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(count, 3, "the root task count is refreshed");
    assert_eq!(created, 100, "creation time is write-once");
}

// schema versioning

fn user_version(conn: &Connection) -> i64 {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn a_fresh_ledger_is_stamped_at_the_current_schema_version() {
    let ledger = Ledger::open_in_memory().unwrap();
    assert_eq!(user_version(&ledger.conn), SCHEMA_VERSION);
}

#[test]
fn an_unversioned_ledger_migrates_in_place_without_losing_data() {
    // A `fleet.db` written before `user_version` was stamped: the schema
    // as it originally shipped, real rows, and the duplicate lineage edge
    // a re-dispatch left behind.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy-fleet.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(MIGRATION_V1).unwrap();
        assert_eq!(user_version(&conn), 0, "the legacy file is unversioned");
        conn.execute(
            "INSERT INTO runs (id, root_task_count, created_at_ms) VALUES ('run1', 2, 7)",
            [],
        )
        .unwrap();
        for at in [10_i64, 40] {
            conn.execute(
                "INSERT INTO lineage (parent_run_id, child_task_id, recorded_at_ms) \
                 VALUES ('run1', 't1', ?1)",
                params![at],
            )
            .unwrap();
        }
    }

    let ledger = Ledger::open(&path).unwrap();
    assert_eq!(user_version(&ledger.conn), SCHEMA_VERSION);
    // Pre-existing rows survived, and the duplicate edge collapsed onto
    // the earliest timestamp.
    let created: i64 = ledger
        .conn
        .query_row(
            "SELECT created_at_ms FROM runs WHERE id = 'run1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(created, 7);
    assert_eq!(
        ledger.lineage_children("run1").unwrap(),
        vec!["t1".to_string()]
    );
    let recorded: i64 = ledger
        .conn
        .query_row("SELECT recorded_at_ms FROM lineage", [], |r| r.get(0))
        .unwrap();
    assert_eq!(recorded, 10);

    // And the migrated file still takes writes on the new schema.
    ledger.record_lineage("run1", "t1", 90).unwrap();
    assert_eq!(ledger.lineage_children("run1").unwrap().len(), 1);
}

#[test]
fn a_migrated_ledger_is_not_re_migrated_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fleet.db");
    {
        let ledger = Ledger::open(&path).unwrap();
        seed_run(&ledger, "run1");
        ledger.record_lineage("run1", "t1", 5).unwrap();
    }
    let reopened = Ledger::open(&path).unwrap();
    assert_eq!(user_version(&reopened.conn), SCHEMA_VERSION);
    assert_eq!(
        reopened.lineage_children("run1").unwrap(),
        vec!["t1".to_string()]
    );
}

// the warmth-signal reads

/// Record one finished attempt for `task_id` in `run_id`, minimal shape.
fn finished_attempt(ledger: &Ledger, run_id: &str, task_id: &str, finished_at_ms: u64) {
    let attempt_id = ledger
        .start_attempt(&AttemptStart {
            run_id: run_id.into(),
            task_id: task_id.into(),
            worktree_path: format!("/tmp/{task_id}"),
            branch: format!("fleet/{task_id}"),
            started_at_ms: finished_at_ms.saturating_sub(1_000),
        })
        .unwrap();
    ledger
        .finish_attempt(&AttemptFinish {
            attempt_id,
            run_id: run_id.into(),
            task_id: task_id.into(),
            finished_at_ms,
            success: true,
            summary: "ok".into(),
            commits: vec![],
            cost_usd: 0.0,
            spend_at_ms: finished_at_ms,
        })
        .unwrap();
}

#[test]
fn last_attempt_finish_is_per_task_and_spans_runs() {
    let ledger = Ledger::open_in_memory().unwrap();
    seed_run(&ledger, "run1");
    seed_run(&ledger, "run2");
    // The same task id across two runs — a plan re-run — plus a sibling.
    finished_attempt(&ledger, "run1", "t1", 10_000);
    finished_attempt(&ledger, "run2", "t1", 50_000);
    finished_attempt(&ledger, "run1", "t2", 30_000);

    assert_eq!(
        ledger.last_attempt_finish_ms("t1").unwrap(),
        Some(50_000),
        "the LATEST finish across runs wins"
    );
    assert_eq!(ledger.last_attempt_finish_ms("t2").unwrap(), Some(30_000));
    assert_eq!(
        ledger.last_attempt_finish_ms("never-ran").unwrap(),
        None,
        "a task with no history carries no per-task signal"
    );
    // The shared-prefix timestamp: the newest finish over ALL tasks.
    assert_eq!(ledger.latest_attempt_finish_ms().unwrap(), Some(50_000));
}

#[test]
fn unfinished_attempts_carry_no_warmth_signal() {
    let ledger = Ledger::open_in_memory().unwrap();
    seed_run(&ledger, "run1");
    // Opened but never stamped — a crash, or a worker still in flight.
    ledger
        .start_attempt(&AttemptStart {
            run_id: "run1".into(),
            task_id: "t1".into(),
            worktree_path: "/tmp/t1".into(),
            branch: "fleet/t1".into(),
            started_at_ms: 5,
        })
        .unwrap();
    assert_eq!(ledger.last_attempt_finish_ms("t1").unwrap(), None);
    assert_eq!(ledger.latest_attempt_finish_ms().unwrap(), None);
}

#[test]
fn commit_record_json_roundtrips() {
    let c = commit("t1", "deadbeef");
    let back: CommitRecord = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
    assert_eq!(back, c);
}

#[test]
fn ledger_persists_to_a_file_and_reopens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ledger.db");
    {
        let ledger = Ledger::open(&path).unwrap();
        seed_run(&ledger, "run1");
        let attempt_id = ledger
            .start_attempt(&AttemptStart {
                run_id: "run1".into(),
                task_id: "t1".into(),
                worktree_path: "/tmp/t1".into(),
                branch: "fleet/t1".into(),
                started_at_ms: 1,
            })
            .unwrap();
        ledger
            .finish_attempt(&AttemptFinish {
                attempt_id,
                run_id: "run1".into(),
                task_id: "t1".into(),
                finished_at_ms: 2,
                success: true,
                summary: "ok".into(),
                commits: vec![commit("t1", "abc")],
                cost_usd: 0.5,
                spend_at_ms: 3,
            })
            .unwrap();
    }
    // Reopen the same file: the schema and data survive.
    let reopened = Ledger::open(&path).unwrap();
    assert_eq!(reopened.commits_for_task("run1", "t1").unwrap().len(), 1);
    assert!((reopened.total_spend("run1").unwrap() - 0.5).abs() < 1e-9);
}
