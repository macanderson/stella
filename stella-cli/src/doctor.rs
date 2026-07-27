// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella doctor` — named checks over the local state stella itself owns,
//! one pass/fail line each, plus the opt-in repair for the one failure that
//! previously had no way out.
//!
//! Two checks today:
//!
//! - **store integrity** — this workspace's `.stella/private/store.db`. A
//!   corrupt store used to surface as an error at session start and nothing
//!   else (no way to confirm the diagnosis, no way to fix it), so the store's
//!   own [`stella_store::integrity`] answers the first half and `--repair` the
//!   second.
//! - **fleet ledger** — rows in `fleet.db` naming a run that is no longer
//!   recorded. Report-only by design; see [`fleet_ledger_orphans`] for why
//!   `--repair` must not touch them.
//!
//! The shape is a LIST of named checks rather than a single hard-coded probe
//! because the next environment check (a provider reachable, a `.stella/`
//! whose permissions leak transcripts, a codegraph index older than its
//! workspace) should be one entry in [`checks`] and a renderer that already
//! knows how to print it — not a second command.
//!
//! # Exit codes
//!
//! `0` when every check passed (including a `--repair` that repaired), `1` when
//! any check failed — [`verdict`] is the whole contract, and `main`'s catch-all
//! turns its `Err` into `ExitCode::FAILURE`. Scripts can therefore gate on
//! `stella doctor` the way they gate on `stella tools --validate`.

use std::path::{Path, PathBuf};

use colored::Colorize;
use stella_store::integrity;

use crate::tui;

/// Problem rows printed under a failing check before the rest are summarized.
/// SQLite can emit a hundred findings for one damaged page; the first handful
/// identify the damage and the count carries the scale.
const MAX_PRINTED_PROBLEMS: usize = 5;

/// Whether one named check passed. Deliberately binary: a check that cannot say
/// "this is fine" is a check that failed, and a third "probably" state would
/// only be an invitation to ignore it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckStatus {
    Pass,
    Fail,
}

/// One named check's result, ready to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Check {
    /// Stable, lowercase name — what a user greps for and what a future
    /// `--only <name>` would select on.
    pub(crate) name: &'static str,
    pub(crate) status: CheckStatus,
    /// One line: the verdict, with the file or subject it is about.
    pub(crate) summary: String,
    /// Evidence under the summary — SQLite's problem rows, or what a repair
    /// actually moved.
    pub(crate) details: Vec<String>,
    /// What the user should do about it, printed as `→` lines. Empty on a pass.
    pub(crate) remedy: Vec<String>,
}

impl Check {
    fn pass(name: &'static str, summary: impl Into<String>) -> Self {
        Self {
            name,
            status: CheckStatus::Pass,
            summary: summary.into(),
            details: Vec::new(),
            remedy: Vec::new(),
        }
    }

    fn fail(name: &'static str, summary: impl Into<String>) -> Self {
        Self {
            name,
            status: CheckStatus::Fail,
            summary: summary.into(),
            details: Vec::new(),
            remedy: Vec::new(),
        }
    }

    fn with_details(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }

    fn with_remedy(mut self, remedy: Vec<String>) -> Self {
        self.remedy = remedy;
        self
    }
}

/// Entry point for `stella doctor [--repair]`.
pub fn run_doctor(repair: bool) -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    run_doctor_at(&workspace_root, repair)
}

/// The command with its workspace passed in, so tests can drive a scratch
/// workspace instead of the process's cwd.
pub(crate) fn run_doctor_at(workspace_root: &Path, repair: bool) -> Result<(), String> {
    let checks = checks(workspace_root, repair);
    render(&checks);
    verdict(&checks)
}

/// Every check this build knows how to run, in the order they are reported.
fn checks(workspace_root: &Path, repair: bool) -> Vec<Check> {
    vec![
        store_integrity(workspace_root, repair),
        fleet_ledger_orphans(workspace_root),
    ]
}

/// Report `fleet.db` rows whose run is gone (#617 item 5).
///
/// This is a **report, not a repair**, and `--repair` deliberately does not act
/// on it. The rows could only be removed by deleting fleet history — which
/// tasks ran, what was attempted, what it cost — and nothing reads a row by
/// orphaned run today, so they are inert. A ledger created by a current build
/// constrains these columns and cannot acquire new orphans; an older file was
/// left unconstrained precisely because retrofitting the constraint requires
/// that deletion. So the honest posture is: show the operator what is there and
/// let them decide.
///
/// A `Pass` either way — orphans are not a fault to fix, and failing the
/// command over inert rows on an old ledger would make `stella doctor`
/// unusable on exactly the workspaces that have history worth keeping.
fn fleet_ledger_orphans(workspace_root: &Path) -> Check {
    const NAME: &str = "fleet ledger";

    let db_path = workspace_root.join(".stella/private/fleet.db");
    if !db_path.exists() {
        return Check::pass(
            NAME,
            "no fleet ledger yet — created by the first `stella fleet` run",
        );
    }
    let shown = display_path(workspace_root, &db_path);

    let ledger = match stella_fleet::Ledger::open(&db_path) {
        Ok(ledger) => ledger,
        Err(error) => {
            return Check::fail(NAME, format!("{shown}: could not be opened: {error}"));
        }
    };
    let orphans = match ledger.orphan_rows() {
        Ok(orphans) => orphans,
        Err(error) => {
            return Check::fail(NAME, format!("{shown}: could not be scanned: {error}"));
        }
    };
    let enforced = ledger.enforces_run_references().unwrap_or(false);

    if orphans.is_empty() {
        return Check::pass(
            NAME,
            format!(
                "{shown}: no rows reference a missing run{}",
                if enforced {
                    " (and the schema enforces it)"
                } else {
                    ""
                }
            ),
        );
    }

    let total: i64 = orphans.iter().map(|o| o.count).sum();
    Check::pass(
        NAME,
        format!("{shown}: {total} row(s) reference a run that is no longer recorded"),
    )
    .with_details(
        orphans
            .iter()
            .map(|o| format!("{}.{}: {} row(s)", o.table, o.column, o.count))
            .collect(),
    )
    .with_remedy(vec![
        "nothing to do — these rows are inert (no query resolves a row by its run) and \
             they are kept because removing them would delete fleet history"
            .to_string(),
        "this ledger predates the run-reference constraints; a fleet.db created by this \
             build cannot acquire new orphans"
            .to_string(),
    ])
}

/// The exit-code contract: `Ok(())` (exit 0) when every check passed, `Err`
/// (exit 1, message on stderr) when any failed.
pub(crate) fn verdict(checks: &[Check]) -> Result<(), String> {
    let failed = checks
        .iter()
        .filter(|check| check.status == CheckStatus::Fail)
        .count();
    if failed == 0 {
        return Ok(());
    }
    Err(format!(
        "{failed} of {} doctor check{} failed",
        checks.len(),
        if checks.len() == 1 { "" } else { "s" }
    ))
}

fn render(checks: &[Check]) {
    tui::section_header("Doctor — local state checks");
    for check in checks {
        let mark = match check.status {
            CheckStatus::Pass => "✓".green(),
            CheckStatus::Fail => "✗".red(),
        };
        println!("  {mark} {} — {}", check.name.bold(), check.summary);
        for detail in &check.details {
            // SQLite puts newlines inside a single `integrity_check` row (the
            // "*** in database main ***" banner arrives glued to the finding
            // under it), so each line is indented on its own rather than letting
            // one row break the column.
            for line in detail.lines() {
                println!("      {}", line.dimmed());
            }
        }
        for line in &check.remedy {
            println!("      {} {line}", "→".yellow());
        }
    }
    let passed = checks
        .iter()
        .filter(|check| check.status == CheckStatus::Pass)
        .count();
    let failed = checks.len() - passed;
    println!(
        "\n  {} check{}: {passed} ok, {failed} failed",
        checks.len(),
        if checks.len() == 1 { "" } else { "s" }
    );
}

/// The store-integrity check, and — under `--repair` — the quarantine.
///
/// `--repair` acts only on a verdict of genuine corruption
/// ([`integrity::IntegrityReport::is_corruption`]). A store that merely failed to
/// be *checked* (a locked file, a permissions problem, a `.stella/` that is not
/// ours) is reported and left exactly where it is: moving a database because
/// the diagnosis was inconclusive is how a repair tool loses somebody's data.
fn store_integrity(workspace_root: &Path, repair: bool) -> Check {
    const NAME: &str = "store integrity";

    let located = match integrity::check_workspace_store(workspace_root) {
        Ok(located) => located,
        Err(error) => {
            return Check::fail(NAME, format!("could not be checked: {error}")).with_remedy(vec![
                "the store was NOT touched — resolve the error above and re-run `stella doctor`"
                    .to_string(),
            ]);
        }
    };
    let Some((db_path, report)) = located else {
        return Check::pass(
            NAME,
            "no store yet — .stella/private/store.db is created by the first session",
        );
    };
    let shown = display_path(workspace_root, &db_path);

    if report.is_healthy() {
        let mut check = Check::pass(NAME, format!("{shown}: {}", report.headline()));
        if repair {
            check = check.with_details(vec![
                "--repair had nothing to do: the database passed".to_string(),
            ]);
        }
        return check;
    }

    let mut details: Vec<String> = report
        .problems()
        .iter()
        .take(MAX_PRINTED_PROBLEMS)
        .cloned()
        .collect();
    // `saturating_sub` on purpose: the count comes from the store and the
    // listing from this function, and a diagnostic must never be the thing that
    // panics on a workspace that is already in trouble.
    let undisplayed = report.total_problems().saturating_sub(details.len());
    if undisplayed > 0 {
        details.push(format!("… and {undisplayed} more"));
    }

    if !repair {
        return Check::fail(NAME, format!("{shown}: {}", report.headline()))
            .with_details(details)
            .with_remedy(repair_advice(&shown));
    }

    if !report.is_corruption() {
        // Unreachable today (a non-healthy report is always a corruption
        // verdict), but the gate is the point: `--repair` moves files only on a
        // corruption verdict, so a future check state can never be quarantined
        // by accident.
        return Check::fail(NAME, format!("{shown}: {}", report.headline()))
            .with_details(details)
            .with_remedy(vec![
                "--repair only acts on a corrupt database; this failure is something else"
                    .to_string(),
            ]);
    }

    match integrity::quarantine_corrupt_store(&db_path) {
        Ok(quarantine) => {
            let mut done = vec![format!("was: {}", report.headline())];
            for (from, to) in &quarantine.moved {
                done.push(format!(
                    "moved {} → {}",
                    display_path(workspace_root, from),
                    display_path(workspace_root, to)
                ));
            }
            match (&quarantine.salvaged, &quarantine.salvage_error) {
                (Some(salvaged), _) => done.push(format!(
                    "salvaged what was readable → {}",
                    display_path(workspace_root, salvaged)
                )),
                (None, Some(error)) => {
                    done.push(format!("nothing could be salvaged ({error})"));
                }
                (None, None) => {}
            }
            done.push("nothing was deleted; the next session starts a fresh store".to_string());
            let mut check = Check::pass(NAME, format!("{shown}: quarantined")).with_details(done);
            if quarantine.salvage_error.is_some() {
                check = check.with_remedy(vec![manual_recover_advice(
                    &quarantine
                        .moved
                        .first()
                        .map(|(_, to)| display_path(workspace_root, to))
                        .unwrap_or_else(|| shown.clone()),
                )]);
            }
            check
        }
        Err(error) => Check::fail(NAME, format!("{shown}: repair failed: {error}"))
            .with_details(details)
            .with_remedy(repair_advice(&shown)),
    }
}

/// What to tell a user sitting in front of a corrupt store they have not asked
/// to repair yet. The manual procedure is spelled out on purpose: `--repair` is
/// the convenient path, not the only one, and a user who would rather drive
/// `sqlite3` themselves should not have to guess the incantation.
fn repair_advice(shown: &str) -> Vec<String> {
    vec![
        "`stella doctor --repair` moves it aside (renamed, never deleted) and copies out \
         whatever is still readable"
            .to_string(),
        manual_recover_advice(shown),
        "the store holds local telemetry and session replay only — never your source".to_string(),
    ]
}

fn manual_recover_advice(shown: &str) -> String {
    format!("by hand: sqlite3 {shown} \".recover\" | sqlite3 {shown}.recovered")
}

/// Workspace-relative when it can be (the paths the store returns are
/// canonical, so a `/var` → `/private/var` symlinked workspace still prints
/// something the user recognizes), absolute otherwise.
fn display_path(workspace_root: &Path, path: &Path) -> String {
    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let relative = path
        .strip_prefix(&canonical_root)
        .or_else(|_| path.strip_prefix(workspace_root))
        .map(PathBuf::from);
    relative
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use stella_store::Store;

    use super::*;

    /// A workspace with a real, cleanly closed store holding one execution.
    fn workspace_with_store() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("store");
        let id = store
            .begin_execution("run", "keep the lights on", "zai", "glm-5.2")
            .expect("execution");
        store
            .finish_execution(id, "completed", 0.25)
            .expect("finish");
        drop(store);
        dir
    }

    fn store_db(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join(".stella/private/store.db")
    }

    /// Overwrite the SQLite header — the "something else wrote over it" shape.
    fn corrupt(db_path: &Path) {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(db_path)
            .expect("open db");
        file.write_all(&[0x7f; 128]).expect("write garbage");
        file.sync_all().expect("sync");
    }

    #[test]
    fn doctor_passes_on_a_healthy_workspace_store() {
        let dir = workspace_with_store();
        assert_eq!(run_doctor_at(dir.path(), false), Ok(()));

        let checks = checks(dir.path(), false);
        assert_eq!(
            checks.iter().map(|c| c.name).collect::<Vec<_>>(),
            vec!["store integrity", "fleet ledger"],
            "the shipped checks, in report order: {checks:?}"
        );
        assert!(
            checks.iter().all(|c| c.status == CheckStatus::Pass),
            "every check passes on a healthy workspace: {checks:?}"
        );
        assert!(
            checks[0].summary.contains("quick_check"),
            "the pass says what was run: {}",
            checks[0].summary
        );
        assert!(checks[0].remedy.is_empty(), "a pass advises nothing");
        // No fleet run has happened in this workspace, so the ledger check has
        // nothing to open and must say so rather than inventing a file.
        assert!(
            checks[1].summary.contains("no fleet ledger yet"),
            "the ledger check names the absence: {}",
            checks[1].summary
        );
    }

    /// #617 item 5: an old ledger's orphan rows are surfaced, and `--repair`
    /// leaves them alone — removing them would delete fleet history.
    #[test]
    fn doctor_reports_fleet_ledger_orphans_without_repairing_them() {
        let dir = workspace_with_store();
        let db = dir.path().join(".stella/private/fleet.db");

        // A ledger in the legacy (unconstrained) shape holding an orphan row.
        {
            let ledger = stella_fleet::Ledger::open(&db).expect("open ledger");
            let _ = ledger;
        }
        let conn = rusqlite::Connection::open(&db).expect("raw open");
        conn.execute_batch(
            "DROP TABLE tasks;
             CREATE TABLE tasks (
                 run_id    TEXT NOT NULL,
                 task_id   TEXT NOT NULL,
                 title     TEXT NOT NULL,
                 isolation TEXT NOT NULL,
                 PRIMARY KEY (run_id, task_id)
             );
             INSERT INTO tasks (run_id, task_id, title, isolation)
                 VALUES ('deleted-run', 't1', 'orphaned', 'shared');",
        )
        .expect("legacy shape with an orphan");
        drop(conn);

        for repair in [false, true] {
            let checks = checks(dir.path(), repair);
            let ledger_check = checks
                .iter()
                .find(|c| c.name == "fleet ledger")
                .expect("the ledger check runs");
            assert_eq!(
                ledger_check.status,
                CheckStatus::Pass,
                "inert orphans must not fail the command (repair={repair})"
            );
            assert!(
                ledger_check
                    .details
                    .iter()
                    .any(|d| d.contains("tasks.run_id")),
                "the orphan class is named: {:?}",
                ledger_check.details
            );
        }

        // The row is still there after `--repair`.
        let conn = rusqlite::Connection::open(&db).expect("raw reopen");
        let surviving: i64 = conn
            .query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))
            .expect("count");
        assert_eq!(surviving, 1, "--repair must not delete fleet history");
    }

    #[test]
    fn doctor_passes_on_a_workspace_that_has_never_run_a_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(run_doctor_at(dir.path(), false), Ok(()));
        assert!(
            !store_db(&dir).exists(),
            "checking must not create the store it did not find"
        );
    }

    #[test]
    fn doctor_reports_a_corrupted_store_and_leaves_it_alone() {
        let dir = workspace_with_store();
        let db_path = store_db(&dir);
        corrupt(&db_path);

        let checks = checks(dir.path(), false);
        assert_eq!(checks[0].status, CheckStatus::Fail, "{checks:?}");
        assert!(
            checks[0].summary.contains(".stella/private/store.db"),
            "the failure names the file: {}",
            checks[0].summary
        );
        assert!(
            checks[0]
                .details
                .iter()
                .any(|detail| detail.contains("not a database")),
            "SQLite's own words are shown as the evidence: {:?}",
            checks[0].details
        );
        assert!(
            !checks[0]
                .details
                .iter()
                .any(|detail| detail.contains("stella doctor")),
            "the doctor must not tell a user to run the doctor: {:?}",
            checks[0].details
        );
        assert!(
            checks[0].remedy.iter().any(|r| r.contains("--repair"))
                && checks[0].remedy.iter().any(|r| r.contains(".recover")),
            "both the offered and the manual repair are surfaced: {:?}",
            checks[0].remedy
        );
        assert!(
            db_path.exists(),
            "a check without --repair moves nothing: {}",
            db_path.display()
        );
    }

    /// The exit-code contract, at both ends of the same corrupt store.
    #[test]
    fn doctor_exits_nonzero_on_corruption_and_zero_after_a_repair() {
        let dir = workspace_with_store();
        corrupt(&store_db(&dir));

        let failure = run_doctor_at(dir.path(), false)
            .expect_err("a corrupt store must not exit 0 — scripts gate on this");
        assert!(failure.contains("failed"), "{failure}");

        // `verdict` is the mapping `main` turns into ExitCode: 1 on any failure.
        assert!(verdict(&checks(dir.path(), false)).is_err());

        // And --repair resolves it, so the same command now exits 0.
        assert_eq!(run_doctor_at(dir.path(), true), Ok(()));
        assert_eq!(run_doctor_at(dir.path(), false), Ok(()));
    }

    #[test]
    fn doctor_repair_quarantines_the_corrupt_store_without_deleting_it() {
        let dir = workspace_with_store();
        let db_path = store_db(&dir);
        corrupt(&db_path);
        let corrupt_bytes = std::fs::read(&db_path).expect("read corrupt db");

        let checks = checks(dir.path(), true);
        assert_eq!(checks[0].status, CheckStatus::Pass, "{checks:?}");
        assert!(
            checks[0].summary.contains("quarantined"),
            "the pass says what it did: {}",
            checks[0].summary
        );
        assert!(
            checks[0].details.iter().any(|d| d.contains("moved")),
            "the output names the rename: {:?}",
            checks[0].details
        );
        assert!(
            checks[0]
                .details
                .iter()
                .any(|d| d.contains("nothing was deleted")),
            "the safety property is stated to the user: {:?}",
            checks[0].details
        );

        assert!(!db_path.exists(), "the corrupt file is out of the way");
        let backup = std::fs::read_dir(db_path.parent().expect("private dir"))
            .expect("read private dir")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("store.db.corrupt-"))
            })
            .expect("a timestamped backup exists");
        assert_eq!(
            std::fs::read(&backup).expect("read backup"),
            corrupt_bytes,
            "the backup is the original bytes"
        );

        // The workspace works again.
        let store = Store::open(dir.path()).expect("a fresh store opens");
        assert!(store.integrity_check().expect("check").is_healthy());
    }

    #[test]
    fn doctor_repair_on_a_healthy_store_moves_nothing() {
        let dir = workspace_with_store();
        let db_path = store_db(&dir);
        let before = std::fs::read(&db_path).expect("read db");

        let checks = checks(dir.path(), true);
        assert_eq!(checks[0].status, CheckStatus::Pass);
        assert!(
            checks[0]
                .details
                .iter()
                .any(|d| d.contains("nothing to do")),
            "a healthy store is told it was left alone: {:?}",
            checks[0].details
        );
        assert_eq!(
            std::fs::read(&db_path).expect("read db"),
            before,
            "--repair is not a licence to touch a healthy database"
        );
    }
}
