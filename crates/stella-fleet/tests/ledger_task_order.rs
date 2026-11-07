//! The order a fleet ledger keeps a run's tasks in.
//!
//! The Observatory names a run after its first task, read by rowid. A repeat
//! write of that task must not move it behind the others.

use stella_fleet::{Ledger, RunRecord, Task};

/// **Witness.** Writing the first task again leaves it first. A ledger that
/// deletes and reinserts the row gives it a new rowid, and the run then takes
/// the second task's name.
#[test]
fn a_repeat_write_keeps_the_first_task_first() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fleet.db");
    let ledger = Ledger::open(&db).unwrap();
    let run = RunRecord {
        id: "run-1".to_string(),
        root_task_count: 2,
        created_at_ms: 0,
    };
    ledger.record_run(&run).unwrap();

    let first = Task::new("t1", "Fix conflicts on PR 123", "prompt");
    let second = Task::new("t2", "Run the tests", "prompt");
    ledger.record_task("run-1", &first).unwrap();
    ledger.record_task("run-1", &second).unwrap();
    ledger.record_task("run-1", &first).unwrap();

    let conn = rusqlite::Connection::open(&db).unwrap();
    let mut stmt = conn
        .prepare("SELECT task_id FROM tasks WHERE run_id = ?1 ORDER BY rowid")
        .unwrap();
    let order: Vec<String> = stmt
        .query_map(["run-1"], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(order, ["t1", "t2"]);
}
