// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella doctor` — named checks over the local state stella itself owns,
//! one pass/fail line each, plus the opt-in repair for the one failure that
//! previously had no way out.
//!
//! Four checks today:
//!
//! - **store integrity** — this workspace's `.stella/private/store.db`. A
//!   corrupt store used to surface as an error at session start and nothing
//!   else (no way to confirm the diagnosis, no way to fix it), so the store's
//!   own [`stella_store::integrity`] answers the first half and `--repair` the
//!   second.
//! - **fleet ledger** — rows in `fleet.db` naming a run that is no longer
//!   recorded. Report-only by design; see [`fleet_ledger_orphans`] for why
//!   `--repair` must not touch them.
//! - **session sidecars** — sidecar directories whose session record is gone;
//!   see [`orphan_session_sidecars`].
//! - **model config** — the model the next run would actually send, and the
//!   endpoint it would send it to; see [`model_config`]. The only check that
//!   is about configuration rather than local state, and the reason is #895:
//!   a default model the provider cannot serve breaks *every* call, and
//!   before this it surfaced only as a mid-task `400`, once per command.
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

/// Entry point for `stella doctor [--repair] [--last-failure]`.
///
/// `model_override` / `base_url_override` are the session flags (`--model` /
/// `STELLA_MODEL`, `--base-url` / `STELLA_BASE_URL`) threaded in so the
/// [`model_config`] check diagnoses the model the NEXT run would use, not a
/// different one — `stella --model x/y doctor` and `stella --model x/y run`
/// must agree about what x/y resolves to.
pub fn run_doctor(
    repair: bool,
    last_failure: bool,
    model_override: Option<&str>,
    base_url_override: Option<&str>,
) -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    if last_failure {
        return print_last_failure(&workspace_root);
    }
    // Install the on-disk catalog so the model check validates against what
    // this machine actually knows, not just the built-in seed. The
    // network-free half on purpose: `stella doctor` promises no provider, no
    // API key, and no network, and a diagnostic that phones home to answer
    // "is my config broken" would be its own bug. Done at the entry point
    // rather than inside the check so the module tests below — which drive
    // `run_doctor_at` against scratch workspaces — never touch the user's
    // real `~/.stella/catalog.db`.
    crate::model_catalog::ensure_catalog_store();
    run_doctor_at(&workspace_root, repair, model_override, base_url_override)
}

/// Print the newest crash dump — `docs/design/diagnostics/diagnostics.md` §7.4.
///
/// The file is content-free by construction (§5.2 makes a record that carries
/// a prompt, a path, or model output a *compile* error), which is what turns
/// "please attach your log" from a request needing a privacy review into one a
/// stranger on the internet can act on immediately. Printing it here rather
/// than only naming it means a user can eyeball what they are about to send,
/// which is the other half of being able to send it comfortably.
fn print_last_failure(workspace_root: &Path) -> Result<(), String> {
    let dir = crate::diag_boot::crash_dir(workspace_root);
    let Some(path) = stella_diag::latest_crash_file(&dir) else {
        return Err(format!(
            "no crash dump in {}. One is written automatically when a run panics or \
             exits non-zero; if the run you are chasing succeeded, there is nothing to show.",
            dir.display()
        ));
    };
    let body = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;

    println!("{}", path.display().to_string().bold());
    println!(
        "{}\n",
        "content-free by construction — safe to attach to a bug report".dimmed()
    );
    print!("{body}");
    Ok(())
}

/// The command with its workspace passed in, so tests can drive a scratch
/// workspace instead of the process's cwd. Note that only the
/// workspace-scoped checks (store integrity, fleet ledger, model config)
/// honour `workspace_root`; the session-sidecar check operates on the
/// machine-wide [`stella_store::SessionRegistry`] default location, which is
/// global by design — so a `--repair` here can reap orphan sidecars
/// regardless of `workspace_root`.
pub(crate) fn run_doctor_at(
    workspace_root: &Path,
    repair: bool,
    model_override: Option<&str>,
    base_url_override: Option<&str>,
) -> Result<(), String> {
    let checks = checks(workspace_root, repair, model_override, base_url_override);
    render(&checks);
    verdict(&checks)
}

/// Every check this build knows how to run, in the order they are reported.
fn checks(
    workspace_root: &Path,
    repair: bool,
    model_override: Option<&str>,
    base_url_override: Option<&str>,
) -> Vec<Check> {
    vec![
        store_integrity(workspace_root, repair),
        fleet_ledger_orphans(workspace_root),
        orphan_session_sidecars(&stella_store::SessionRegistry::open_default(), repair),
        model_config(workspace_root, model_override, base_url_override),
    ]
}

/// #895: the model the next run would actually send, checked before it costs
/// a turn. A default slug the provider cannot serve does not break one
/// command — it breaks *every* one, and it used to surface only as a provider
/// `400` in the middle of whatever the user was doing.
///
/// A **Fail** is any finding the catalog can prove offline: an unknown
/// provider, a slug the synced catalog vetoes, an over-qualified
/// `openrouter/openrouter/…`, a de-namespaced `openrouter/auto`, or a
/// `--base-url` pointed at a different provider's host. Every one of them
/// prints the setting, the value, what is wrong, and
/// [`crate::settings_check::model_fix_hints`] — the valid-slug listing, the
/// refresh, and where the default is set. "Invalid model" with no way forward
/// is the failure mode this check exists to replace.
///
/// A **Pass** carries what it could NOT prove: an unseeded provider whose
/// `/models` listing has never been synced has no authoritative slug set, so
/// only the shape was checkable. Overclaiming there would be worse than the
/// silence it replaced.
fn model_config(
    workspace_root: &Path,
    model_override: Option<&str>,
    base_url_override: Option<&str>,
) -> Check {
    const NAME: &str = "model config";

    let report = crate::settings_check::inspect_model_config(
        workspace_root,
        model_override,
        base_url_override,
    );
    if !report.issues.is_empty() {
        return Check::fail(
            NAME,
            format!(
                // Not "model setting(s)": a mis-pointed `--base-url` lands in
                // the same list and is neither a model nor a setting.
                "{} configuration problem(s) that will break model calls",
                report.issues.len()
            ),
        )
        .with_details(
            report
                .issues
                .iter()
                .map(crate::settings_check::SettingsIssue::line)
                .collect(),
        )
        .with_remedy(crate::settings_check::model_fix_hints(
            report.provider.as_deref(),
        ));
    }
    let Some(resolved) = report.resolved else {
        // Nothing configured and no credential to auto-detect from: a fresh
        // install, which is not a fault. `stella auth set` is the next step,
        // and the no-key error already says so the moment a run needs one.
        return Check::pass(
            NAME,
            "no model configured yet — the first run picks the provider whose key it finds",
        );
    };
    let summary = format!("{resolved} — from {}", report.source);
    match report.note {
        Some(note) => Check::pass(NAME, summary).with_details(vec![note]),
        None => Check::pass(NAME, summary),
    }
}

/// Report — and with `--repair`, reap — session sidecar directories whose
/// `.json` record is gone (#617's last open sweep, wired here to match the
/// `fleet_ledger_orphans` precedent: `SessionRegistry::prune` is age-gated on
/// a record an orphan doesn't have, so the sweep belongs to an explicit
/// operator action, not a background loop).
///
/// The safety property lives in the registry, not here:
/// `prune_orphan_sidecars` sweeps only `sidecar_names − record_names`, and a
/// record that exists but cannot be parsed is *damaged*, never an orphan —
/// so a power-cut session's conversation history is unreachable by this
/// sweep. Damaged records are surfaced for visibility and deliberately have
/// no repair.
fn orphan_session_sidecars(registry: &stella_store::SessionRegistry, repair: bool) -> Check {
    const NAME: &str = "session sidecars";

    let scan = registry.scan();
    let damaged_note = (!scan.damaged.is_empty()).then(|| {
        format!(
            "{} damaged record(s) left untouched — damaged is not orphaned",
            scan.damaged.len()
        )
    });
    if scan.orphan_sidecars.is_empty() {
        return Check::pass(
            NAME,
            match damaged_note {
                Some(note) => format!("no orphan sidecars ({note})"),
                None => "every sidecar directory has its session record".to_string(),
            },
        );
    }
    if repair {
        return match registry.prune_orphan_sidecars() {
            Ok(removed) => Check::pass(
                NAME,
                format!("reaped {} orphan sidecar director(ies)", removed.len()),
            )
            .with_details(removed),
            Err(error) => Check::fail(NAME, format!("orphan sweep failed: {error}")),
        };
    }
    Check::pass(
        NAME,
        format!(
            "{} sidecar director(ies) have no session record",
            scan.orphan_sidecars.len()
        ),
    )
    .with_details(scan.orphan_sidecars.clone())
    .with_remedy(vec![
        "run `stella doctor --repair` to remove them — an orphan holds no session record, \
         only a directory a deleted record left behind"
            .to_string(),
    ])
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
        assert_eq!(run_doctor_at(dir.path(), false, None, None), Ok(()));

        let checks = checks(dir.path(), false, None, None);
        assert_eq!(
            checks.iter().map(|c| c.name).collect::<Vec<_>>(),
            vec![
                "store integrity",
                "fleet ledger",
                "session sidecars",
                "model config"
            ],
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

    /// #617's last open sweep: an orphan sidecar is reported without
    /// `--repair` and reaped with it, while a damaged record's sidecar is
    /// never touched.
    #[test]
    fn doctor_reaps_orphan_sidecars_only_under_repair() {
        let dir = tempfile::tempdir().unwrap();
        let registry = stella_store::SessionRegistry::open(dir.path());
        // An orphan: a sidecar directory with no record.
        std::fs::create_dir_all(dir.path().join("sess-orphan")).unwrap();
        std::fs::write(dir.path().join("sess-orphan/journal.jsonl"), "{}\n").unwrap();
        // A damaged record: unparsable JSON, sidecar present.
        std::fs::write(dir.path().join("sess-damaged.json"), "{ not json").unwrap();
        std::fs::create_dir_all(dir.path().join("sess-damaged")).unwrap();

        let report = super::orphan_session_sidecars(&registry, false);
        assert_eq!(report.status, super::CheckStatus::Pass);
        assert!(report.summary.contains("1 sidecar"), "{}", report.summary);
        assert!(dir.path().join("sess-orphan").exists());

        let repaired = super::orphan_session_sidecars(&registry, true);
        assert_eq!(repaired.status, super::CheckStatus::Pass);
        assert!(!dir.path().join("sess-orphan").exists(), "orphan reaped");
        assert!(
            dir.path().join("sess-damaged").exists(),
            "a damaged record's sidecar is never an orphan"
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
            let checks = checks(dir.path(), repair, None, None);
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
        assert_eq!(run_doctor_at(dir.path(), false, None, None), Ok(()));
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

        let checks = checks(dir.path(), false, None, None);
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

        let failure = run_doctor_at(dir.path(), false, None, None)
            .expect_err("a corrupt store must not exit 0 — scripts gate on this");
        assert!(failure.contains("failed"), "{failure}");

        // `verdict` is the mapping `main` turns into ExitCode: 1 on any failure.
        assert!(verdict(&checks(dir.path(), false, None, None)).is_err());

        // And --repair resolves it, so the same command now exits 0.
        assert_eq!(run_doctor_at(dir.path(), true, None, None), Ok(()));
        assert_eq!(run_doctor_at(dir.path(), false, None, None), Ok(()));
    }

    #[test]
    fn doctor_repair_quarantines_the_corrupt_store_without_deleting_it() {
        let dir = workspace_with_store();
        let db_path = store_db(&dir);
        corrupt(&db_path);
        let corrupt_bytes = std::fs::read(&db_path).expect("read corrupt db");

        let checks = checks(dir.path(), true, None, None);
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

        let checks = checks(dir.path(), true, None, None);
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
