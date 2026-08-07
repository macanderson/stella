// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Integrity verdicts for `.stella/private/store.db`, and the opt-in
//! quarantine that gives a corrupt one a way out — the two halves
//! `stella doctor` is made of.
//!
//! `corrupt_store_error` — the session path's mapping, which lives here
//! because it shares this module's classifier — already turns "database disk
//! image is malformed" into an actionable sentence at open time. That is a
//! diagnosis the user cannot confirm and a repair they have to invent, which is
//! what the rest of the module fixes: [`Store::integrity_check`] /
//! [`check_workspace_store`] answer "is it really corrupt, and what exactly is
//! wrong", and [`quarantine_corrupt_store`] performs the repair that error now
//! names.
//!
//! # Safety posture
//!
//! - Checking never mutates. The probe opens read-only
//!   (`open_private_sqlite_read_only`) precisely so SQLite cannot recover or
//!   checkpoint a leftover `-wal` into a file the user may still want to
//!   salvage. It also never *creates* a database: a workspace that has never
//!   run a session has nothing to check, and saying so beats materializing an
//!   empty file to prove it.
//! - Repair never deletes. [`quarantine_corrupt_store`] copies out what is
//!   still readable, then RENAMES the corrupt file (and its `-wal`/`-shm`
//!   siblings) to timestamped names beside it. Every byte the user had is still
//!   on disk afterwards; recovering the workspace costs a rename back.
//! - Repair is never automatic. Nothing here runs on the session path — only
//!   `stella doctor --repair`, typed by the user, moves a file.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::{
    Result, Store, StoreError, existing_workspace_private_sqlite_path, open_private_sqlite,
    open_private_sqlite_read_only,
};

/// Does this rusqlite error mean the FILE is unusable (as opposed to the
/// statement being wrong)? `SQLITE_CORRUPT` is a database whose pages no longer
/// hold a valid b-tree; `SQLITE_NOTADB` is a file whose header is not SQLite's
/// at all (truncated to nothing, overwritten by another tool, or an encrypted
/// blob). Shared by `corrupt_store_error` (the session-path message) and
/// `PragmaRun::refuse_if_not_corruption` (the diagnostic verdict), so the
/// classification behind "your store is corrupt" is one definition, not two that
/// can drift.
pub(crate) fn is_sqlite_corruption(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(sqlite, _)
            if matches!(
                sqlite.code,
                rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase
            )
    )
}

/// Give genuine on-disk corruption the same actionable error the schema-version
/// branches of [`Store::migrate`] already produce.
///
/// Without this a malformed `store.db` reaches the caller as the raw rusqlite
/// string ("database disk image is malformed"), which `open_store` prints as
/// `local store unavailable (store: …)` — and then EVERY later session repeats
/// the warning and runs with zero persistence, because nothing ever tells the
/// user to move the file aside. `From<rusqlite::Error>` flattens the error to a
/// `String`, so the code has to be inspected before that conversion happens.
/// Anything that is not corruption is passed through unchanged.
///
/// The message names `stella doctor` because a diagnosis the user cannot
/// confirm and a repair they have to invent are what made this failure a dead
/// end: the rest of this module backs both halves, and this error is the only
/// place most users ever hear about them.
pub(crate) fn corrupt_store_error(error: rusqlite::Error, db_path: Option<&Path>) -> StoreError {
    if !is_sqlite_corruption(&error) {
        return StoreError::from(error);
    }
    let path = db_path.map_or_else(
        || ".stella/private/store.db".to_string(),
        |path| path.display().to_string(),
    );
    StoreError(format!(
        "store.db cannot be read as a SQLite database ({error}), so it is corrupt or \
         was overwritten. Run `stella doctor` to confirm, then `stella doctor --repair` \
         to salvage what is readable and move {path} aside (it is renamed, never \
         deleted) — it holds local telemetry and session replay, never your source."
    ))
}

/// How many problem rows a report keeps. `PRAGMA integrity_check` stops at 100
/// findings by default and a wall of them tells the user nothing the first few
/// don't — but the count is never truncated, only the listing.
const MAX_REPORTED_PROBLEMS: usize = 20;

/// Which pragma produced a verdict.
///
/// `quick_check` is the same page- and record-level verification as
/// `integrity_check` minus the row-by-row index-vs-table comparison, so it is
/// dramatically cheaper on a large store and still catches every kind of
/// corruption that makes a file unusable. The ladder here runs `quick_check`
/// first and escalates only when it complains, so the healthy path (the common
/// one) stays fast while a real problem still gets the full report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityDepth {
    /// `PRAGMA quick_check` — structure only, no index cross-check.
    Quick,
    /// `PRAGMA integrity_check` — `quick_check` plus index/table agreement.
    Full,
}

impl IntegrityDepth {
    /// The SQLite pragma name, for output that says what was actually run.
    pub fn pragma(self) -> &'static str {
        match self {
            Self::Quick => "quick_check",
            Self::Full => "integrity_check",
        }
    }
}

impl std::fmt::Display for IntegrityDepth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.pragma())
    }
}

/// What an integrity check found — a verdict, not a string to be re-parsed:
/// "healthy" and "corrupt, here is what SQLite said" are different variants,
/// and "the file is not a SQLite database at all" is a third (SQLite reports
/// that as an *error* rather than as problem rows, and flattening it into the
/// corrupt list would lose the distinction that the file cannot even be
/// queried).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntegrityReport {
    /// SQLite verified the file. `depth` is how far the check got before it
    /// could say so.
    Healthy { depth: IntegrityDepth },
    /// The file is a readable SQLite database with structural damage.
    /// `problems` is SQLite's own wording, capped at `MAX_REPORTED_PROBLEMS`
    /// rows, with `total_problems` carrying the untruncated count.
    Corrupt {
        depth: IntegrityDepth,
        problems: Vec<String>,
        total_problems: usize,
    },
    /// The file could not be read as a SQLite database at all (truncated to
    /// nothing, overwritten, or encrypted). `reason` is SQLite's own wording for
    /// why — "file is not a database", "database disk image is malformed" — the
    /// same distinction `corrupt_store_error` classifies on the session path.
    /// Deliberately just the reason, with no advice attached: the caller knows
    /// whether it is a session that should point at `stella doctor` or the doctor
    /// itself, which must not tell a user to run the command they are running.
    Unreadable { reason: String },
}

impl IntegrityReport {
    /// Did the database pass?
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy { .. })
    }

    /// Is this the kind of failure a quarantine can address? True for both
    /// failing variants and false for a healthy one — the gate
    /// `stella doctor --repair` checks before it moves anything, so a store
    /// that failed to open for some OTHER reason (a schema version from a newer
    /// build, a permissions problem) is never mistaken for a corrupt one and
    /// renamed out from under a user who could still read it.
    pub fn is_corruption(&self) -> bool {
        matches!(self, Self::Corrupt { .. } | Self::Unreadable { .. })
    }

    /// The problems to show, in SQLite's own words. Empty for a healthy
    /// database; a single element for an unreadable file.
    pub fn problems(&self) -> &[String] {
        match self {
            Self::Healthy { .. } => &[],
            Self::Corrupt { problems, .. } => problems,
            Self::Unreadable { reason } => std::slice::from_ref(reason),
        }
    }

    /// How many problems SQLite reported, before the listing was capped.
    pub fn total_problems(&self) -> usize {
        match self {
            Self::Healthy { .. } => 0,
            Self::Corrupt { total_problems, .. } => *total_problems,
            Self::Unreadable { .. } => 1,
        }
    }

    /// One line naming the verdict and the check behind it — the summary a
    /// caller puts after the file name.
    pub fn headline(&self) -> String {
        match self {
            Self::Healthy { depth } => format!("ok ({depth})"),
            Self::Corrupt {
                depth,
                total_problems,
                ..
            } => format!(
                "{total_problems} problem{} reported by {depth}",
                if *total_problems == 1 { "" } else { "s" }
            ),
            Self::Unreadable { .. } => "not readable as a SQLite database".to_string(),
        }
    }
}

impl Store {
    /// Ask SQLite whether this store's file is structurally sound.
    ///
    /// Runs `PRAGMA quick_check` and escalates to the full
    /// `PRAGMA integrity_check` only when that flags something, so a healthy
    /// store pays for the cheap check and a damaged one still gets the complete
    /// findings (see [`IntegrityDepth`]).
    ///
    /// Returns a verdict, never a bare string: [`IntegrityReport::Healthy`],
    /// [`IntegrityReport::Corrupt`] with SQLite's own problem rows, or
    /// [`IntegrityReport::Unreadable`] when the pragma itself fails with
    /// `SQLITE_CORRUPT`/`SQLITE_NOTADB` — a *diagnosis*, so it comes back as
    /// `Ok(report)`. `Err` is reserved for a check that could not be performed
    /// (a locked or otherwise failing connection), which is a different thing
    /// from a database that failed.
    ///
    /// This is the live-connection door: it reports on the file this `Store`
    /// already has open, so it works for in-memory stores too (nothing to
    /// name, nothing on disk to be wrong). To check a workspace's store
    /// WITHOUT opening it — the `stella doctor` case, where the whole problem
    /// may be that it cannot be opened — use [`check_workspace_store`].
    pub fn integrity_check(&self) -> Result<IntegrityReport> {
        check_connection(&self.lock())
    }
}

/// Check this workspace's `store.db` without opening a [`Store`].
///
/// `Ok(None)` means no store exists yet — a workspace that has never run a
/// session has nothing to check, and this deliberately does not create one to
/// find out. Otherwise the located path is returned with its verdict, so a
/// caller can name the exact file it judged.
///
/// This is the path `stella doctor` takes: [`Store::open`] applies migrations
/// and would itself fail on the corruption being diagnosed, so the check has to
/// stand on its own.
pub fn check_workspace_store(workspace_root: &Path) -> Result<Option<(PathBuf, IntegrityReport)>> {
    let Some(db_path) = existing_workspace_private_sqlite_path(workspace_root, "store.db")? else {
        return Ok(None);
    };
    let report = check_file(&db_path)?;
    Ok(Some((db_path, report)))
}

/// Check one SQLite database file in place, mutating nothing in the healthy
/// case and nothing at all unless a `-wal` forces a session-shaped open.
///
/// The probe is an immutable read (`open_private_sqlite_read_only`): no locks,
/// no `-shm`, not one byte written — a diagnostic must not be the thing that
/// rewrites a file the user may still want to salvage. Immutable reads cannot
/// see a `-wal`'s uncheckpointed pages, so a BAD verdict (or a probe that could
/// not run) escalates to the read-write open a session would perform, which
/// recovers the WAL and verifiers the pair. A GOOD verdict never escalates: a
/// structurally sound main file plus a WAL that a session will replay is not a
/// corrupt store, and re-opening it read-write to say so would be exactly the
/// mutation this avoids.
pub fn check_file(db_path: &Path) -> Result<IntegrityReport> {
    let immutable = open_private_sqlite_read_only(db_path).and_then(|conn| check_connection(&conn));
    if matches!(&immutable, Ok(report) if report.is_healthy()) {
        return immutable;
    }
    if !sibling_with_suffix(db_path, "-wal")?.exists() {
        // Nothing to recover — the immutable verdict is the whole truth.
        return immutable;
    }
    match open_private_sqlite(db_path).and_then(|conn| check_connection(&conn)) {
        Ok(report) => Ok(report),
        Err(error) => Err(match immutable {
            // Both doors failed: lead with the session-shaped attempt (the one
            // that matters) and keep the first failure, since whichever wording
            // the user recognizes is the useful one.
            Err(first) => StoreError(format!(
                "{} (an immutable read-only probe first failed with: {})",
                error.0, first.0
            )),
            Ok(_) => error,
        }),
    }
}

/// The pragma ladder, against an already-open connection.
fn check_connection(conn: &Connection) -> Result<IntegrityReport> {
    let quick = pragma_run(conn, IntegrityDepth::Quick);
    quick.refuse_if_not_corruption()?;
    if quick.stopped_by.is_none() && quick.problems.is_empty() {
        return Ok(IntegrityReport::Healthy {
            depth: IntegrityDepth::Quick,
        });
    }

    // Something is wrong. The full check enumerates more (indexes against their
    // tables) and, when the damage aborts the walk partway, still hands back
    // everything it named before giving up — which is the difference between
    // "your database is broken" and "index forgotten_by_surface disagrees with
    // its table, page 62 is unreadable".
    let full = pragma_run(conn, IntegrityDepth::Full);
    full.refuse_if_not_corruption()?;

    // `quick_check`'s checks are a strict subset of `integrity_check`'s, so a
    // silent full pass after a dirty quick one should be impossible — if SQLite
    // ever does that, keep the findings we actually have rather than reporting a
    // clean bill of health nobody gave us.
    let (depth, mut run) = if full.problems.is_empty() {
        (IntegrityDepth::Quick, quick)
    } else {
        (IntegrityDepth::Full, full)
    };
    if run.problems.is_empty() {
        // The check aborted before naming anything: the file cannot be walked at
        // all, which is a different verdict from a walkable file with damage.
        return Ok(IntegrityReport::Unreadable {
            reason: run.stopped_by.map_or_else(
                || "no reason reported".to_string(),
                |error| error.to_string(),
            ),
        });
    }
    if let Some(error) = run.stopped_by {
        // The listing is incomplete and the user must know that, or a short list
        // reads as the full extent of the damage.
        run.problems.push(format!("{depth} stopped early: {error}"));
    }
    let total_problems = run.problems.len();
    Ok(IntegrityReport::Corrupt {
        depth,
        problems: run
            .problems
            .into_iter()
            .take(MAX_REPORTED_PROBLEMS)
            .collect(),
        total_problems,
    })
}

/// One pragma's output: the problem rows it named (an `ok` row is not a problem)
/// and the failure that ended it, if any.
struct PragmaRun {
    problems: Vec<String>,
    stopped_by: Option<rusqlite::Error>,
}

impl PragmaRun {
    /// A pragma that failed for a reason OTHER than corruption did not judge the
    /// database — the check could not be performed, and that propagates as an
    /// error rather than masquerading as a verdict. (`corrupt_store_error` makes
    /// the same distinction on the session path, through the same predicate.)
    fn refuse_if_not_corruption(&self) -> Result<()> {
        match &self.stopped_by {
            Some(error) if !is_sqlite_corruption(error) => Err(StoreError(error.to_string())),
            _ => Ok(()),
        }
    }
}

/// Run one integrity pragma, keeping the rows it produced even when it aborts
/// mid-walk — SQLite names what it found and *then* raises `SQLITE_CORRUPT`, so
/// collecting into a `Result` would throw away the entire diagnosis.
///
/// The pragma name comes from [`IntegrityDepth`], never from a caller, so the
/// interpolation cannot be an injection site (pragma names cannot be bound as
/// parameters).
fn pragma_run(conn: &Connection, depth: IntegrityDepth) -> PragmaRun {
    let mut problems = Vec::new();
    let mut statement = match conn.prepare(&format!("PRAGMA {}", depth.pragma())) {
        Ok(statement) => statement,
        Err(error) => {
            return PragmaRun {
                problems,
                stopped_by: Some(error),
            };
        }
    };
    let mut rows = match statement.query([]) {
        Ok(rows) => rows,
        Err(error) => {
            return PragmaRun {
                problems,
                stopped_by: Some(error),
            };
        }
    };
    loop {
        let stopped_by = match rows.next() {
            // SQLite says "sound" with a single `ok` row; anything else is a
            // finding.
            Ok(Some(row)) => match row.get::<_, String>(0) {
                Ok(text) => {
                    if !text.eq_ignore_ascii_case("ok") {
                        problems.push(text);
                    }
                    continue;
                }
                Err(error) => Some(error),
            },
            Ok(None) => None,
            Err(error) => Some(error),
        };
        return PragmaRun {
            problems,
            stopped_by,
        };
    }
}

/// What [`quarantine_corrupt_store`] did, so a caller can tell the user where
/// their bytes went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreQuarantine {
    /// Every rename performed, `(from, to)`, in order: the database first, then
    /// each `-wal`/`-shm` sibling. Nothing is ever deleted, so this list is the
    /// complete record of how to put the workspace back (`mv` each pair the
    /// other way).
    pub moved: Vec<(PathBuf, PathBuf)>,
    /// A new database holding everything SQLite could still copy out of the
    /// quarantined one, or `None` when nothing could be salvaged.
    pub salvaged: Option<PathBuf>,
    /// Why the salvage attempt came back empty — `None` when it worked. A
    /// failure here is expected for a badly damaged file and is never fatal:
    /// the quarantine has already put the workspace back in working order, and
    /// the evidence is still on disk for a manual `.recover`.
    pub salvage_error: Option<String>,
}

/// Move a corrupt store aside, then salvage what is still readable from it.
///
/// In order:
/// 1. Rename the database to `store.db.corrupt-<epoch>`, then its `-wal`/`-shm`
///    siblings to matching names. The sidecars MUST travel with it: a stale WAL
///    left beside the fresh database the next session creates is how a
///    successful repair would turn into new corruption. Renames come first
///    because they are the part that has to work — after this step the
///    workspace is usable again even if everything below fails.
/// 2. `VACUUM INTO` a new `store.db.salvaged-<epoch>` — a page-by-page copy of
///    everything SQLite can still read out of the quarantined file. Localized
///    damage (a torn index, an unreadable page in one table) often salvages
///    nearly everything; a shredded header salvages nothing, and that is
///    recorded in [`StoreQuarantine::salvage_error`] rather than failing the
///    repair. The read is immutable, so the quarantined evidence is never
///    modified by the attempt to read it.
///
/// Nothing is deleted and nothing is overwritten — every target name is made
/// unique first — so the whole operation is undoable with `mv`, and the caller
/// is expected to print the paths. Uncheckpointed pages in a quarantined
/// `-wal` do not reach the salvaged copy (SQLite only replays a WAL sitting at
/// its database's exact name); renaming the quarantined pair back to
/// `store.db`/`store.db-wal` hands them to SQLite's own recovery, which is why
/// the sidecar is preserved rather than consumed.
///
/// Call this ONLY on a file [`check_file`] judged
/// [`IntegrityReport::is_corruption`]. It does not re-check — the caller just
/// did, and re-running the pragma on a damaged file can be slow — so it will
/// faithfully quarantine a healthy database if asked to. The gate lives at the
/// call site (`stella doctor --repair`), and that is deliberate: "is this
/// corrupt" and "may I move it" are different questions.
pub fn quarantine_corrupt_store(db_path: &Path) -> Result<StoreQuarantine> {
    let metadata = std::fs::symlink_metadata(db_path)
        .map_err(|e| StoreError(format!("cannot inspect {}: {e}", db_path.display())))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(StoreError(format!(
            "{} is not a regular file — refusing to quarantine an ambiguous path",
            db_path.display()
        )));
    }
    let stamp = epoch_secs();

    let mut moved = Vec::new();
    let quarantined = unique_sibling(db_path, &format!("corrupt-{stamp}"))?;
    rename(db_path, &quarantined)?;
    moved.push((db_path.to_path_buf(), quarantined.clone()));
    for suffix in ["-wal", "-shm"] {
        let sidecar = sibling_with_suffix(db_path, suffix)?;
        if !sidecar.exists() {
            continue;
        }
        let target = unique_sibling(&sidecar, &format!("corrupt-{stamp}"))?;
        rename(&sidecar, &target)?;
        moved.push((sidecar, target));
    }

    // Salvage from the quarantined copy, into a name derived from the ORIGINAL
    // (so the user sees `store.db.salvaged-…`, not a name-of-a-name). Reading
    // the moved file rather than the live path is what keeps SQLite away from
    // the user's sidecars: the names it would create, consult, or delete now
    // belong to the quarantined file, not to anything that was here before.
    let (salvaged, salvage_error) = salvage(&quarantined, db_path, &stamp);
    Ok(StoreQuarantine {
        moved,
        salvaged,
        salvage_error,
    })
}

/// `VACUUM INTO` a fresh database from `source`, named beside `original`.
/// Best-effort by construction: the returned error string is a report, not a
/// failure. The source is opened immutably — the evidence a user may still want
/// to `.recover` by hand is not modified by the attempt to copy it.
fn salvage(source: &Path, original: &Path, stamp: &str) -> (Option<PathBuf>, Option<String>) {
    let target = match unique_sibling(original, &format!("salvaged-{stamp}")) {
        Ok(target) => target,
        Err(error) => return (None, Some(error.0)),
    };
    // `VACUUM INTO` takes an expression, so the path binds as a parameter — no
    // quoting of a path that may contain apostrophes.
    let Some(target_str) = target.to_str() else {
        return (
            None,
            Some(format!(
                "salvage target {} is not valid UTF-8",
                target.display()
            )),
        );
    };
    let attempt = open_private_sqlite_read_only(source).and_then(|conn| {
        conn.execute("VACUUM INTO ?1", [target_str])
            .map_err(|e| StoreError(format!("VACUUM INTO failed: {e}")))
    });
    match attempt {
        Ok(_) => {
            // The salvaged copy holds the same transcripts the original did, so
            // it gets the same owner-only mode the store is created with. A mode
            // that cannot be set is a leak, not a cosmetic problem: drop the
            // copy and report it.
            if let Err(error) = restrict_to_owner(&target) {
                let _ = std::fs::remove_file(&target);
                return (None, Some(error.0));
            }
            (Some(target), None)
        }
        Err(error) => {
            // A failed VACUUM can leave a partial file behind; it is this
            // function's own output path, never anything the user had.
            let _ = std::fs::remove_file(&target);
            (None, Some(error.0))
        }
    }
}

#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| StoreError(format!("cannot restrict {}: {e}", path.display())))
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> Result<()> {
    Ok(())
}

fn rename(from: &Path, to: &Path) -> Result<()> {
    std::fs::rename(from, to).map_err(|e| {
        StoreError(format!(
            "cannot move {} to {}: {e}",
            from.display(),
            to.display()
        ))
    })
}

/// `store.db` + `-wal` → `store.db-wal`, in the same directory.
fn sibling_with_suffix(db_path: &Path, suffix: &str) -> Result<PathBuf> {
    let name = db_path
        .file_name()
        .ok_or_else(|| StoreError(format!("{} has no filename", db_path.display())))?;
    let mut with_suffix = name.to_os_string();
    with_suffix.push(suffix);
    Ok(db_path.with_file_name(with_suffix))
}

/// `<name>.<tag>`, or `<name>.<tag>.2`, `.3`, … if that is taken. A quarantine
/// that overwrote an earlier quarantine would destroy exactly the bytes this
/// whole path exists to preserve, so a name is only returned when nothing is
/// there.
fn unique_sibling(path: &Path, tag: &str) -> Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| StoreError(format!("{} has no filename", path.display())))?;
    for attempt in 1..=64u32 {
        let mut candidate = name.to_os_string();
        candidate.push(".");
        candidate.push(tag);
        if attempt > 1 {
            candidate.push(format!(".{attempt}"));
        }
        let candidate = path.with_file_name(candidate);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(StoreError(format!(
        "cannot find a free `{tag}` name beside {} after 64 tries",
        path.display()
    )))
}

/// Seconds since the epoch, as a sortable filename stamp. A clock before the
/// epoch degrades to `0`; [`unique_sibling`] keeps that from colliding.
fn epoch_secs() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::AgentEvent;

    /// A workspace with a real, cleanly closed `store.db` holding one
    /// execution's worth of state — closed so no `-wal` is in play unless a
    /// test arranges one.
    fn healthy_workspace() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path()).expect("store");
        let id = store
            .begin_execution("run", "keep the lights on", "zai", "glm-5.2")
            .expect("execution");
        store
            .record_event(
                id,
                0,
                &AgentEvent::Text {
                    text: "recoverable transcript".into(),
                },
            )
            .expect("event");
        store
            .finish_execution(id, "completed", 0.5)
            .expect("finish");
        drop(store);
        let db_path = dir.path().join(".stella/private/store.db");
        assert!(db_path.is_file(), "fixture store exists");
        (dir, db_path)
    }

    /// Overwrite the header so the file is not a SQLite database at all — the
    /// "something else wrote over it" corruption.
    fn shred_header(db_path: &Path) {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(db_path)
            .expect("open db for corruption");
        file.seek(SeekFrom::Start(0)).expect("seek");
        file.write_all(&[0x7f; 128]).expect("write garbage");
        file.sync_all().expect("sync");
    }

    #[test]
    fn healthy_store_reports_clean() {
        let (dir, db_path) = healthy_workspace();

        // The live-connection door.
        let store = Store::open(dir.path()).expect("reopen");
        let report = store.integrity_check().expect("check");
        assert_eq!(
            report,
            IntegrityReport::Healthy {
                depth: IntegrityDepth::Quick
            },
            "a sound store passes on the cheap check alone"
        );
        assert!(report.problems().is_empty());
        assert!(!report.is_corruption());
        assert_eq!(report.headline(), "ok (quick_check)");
        drop(store);

        // The file door `stella doctor` uses. The located path is canonical
        // (`/private/var/…` on macOS for a `/var/…` tempdir), so compare what
        // both sides agree on.
        let (checked, report) = check_workspace_store(dir.path())
            .expect("check")
            .expect("store exists");
        assert_eq!(
            checked.canonicalize().expect("canonical located path"),
            db_path.canonicalize().expect("canonical fixture path")
        );
        assert!(report.is_healthy());
    }

    #[test]
    fn in_memory_store_has_nothing_to_be_wrong_with() {
        let store = Store::in_memory().expect("in-memory store");
        assert!(store.integrity_check().expect("check").is_healthy());
    }

    #[test]
    fn a_workspace_with_no_store_is_not_a_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            check_workspace_store(dir.path()).expect("check"),
            None,
            "a workspace that never ran a session has no store to judge — and \
             checking must not create one"
        );
        assert!(!dir.path().join(".stella/private/store.db").exists());
    }

    #[test]
    fn garbage_over_the_header_is_reported_unreadable() {
        let (dir, db_path) = healthy_workspace();
        shred_header(&db_path);

        let report = check_file(&db_path).expect("a corrupt file is a verdict, not an error");
        assert!(
            matches!(report, IntegrityReport::Unreadable { .. }),
            "an overwritten header is not queryable at all: {report:?}"
        );
        assert!(report.is_corruption());
        assert!(!report.is_healthy());
        let reason = &report.problems()[0];
        assert!(
            reason.contains("not a database"),
            "the verdict carries SQLite's own wording, with no advice glued on: {reason}"
        );

        // The check left the file exactly as it found it.
        let after = std::fs::read(&db_path).expect("read back");
        assert_eq!(&after[..128], &[0x7f; 128], "the probe mutated nothing");

        // The session path classifies the same failure through the same
        // predicate, and that is where the advice belongs: a turn that cannot
        // open the store is the moment a user needs to hear about the tooling.
        let error = match Store::open(dir.path()) {
            Ok(_) => panic!("a corrupt store must not open"),
            Err(error) => error.to_string(),
        };
        assert!(
            error.contains("stella doctor") && error.contains("store.db"),
            "the open-time error points at the tooling and names the file: {error}"
        );
    }

    #[test]
    fn a_zeroed_interior_page_is_reported_as_corrupt_with_problems() {
        let (dir, db_path) = healthy_workspace();
        // Enough rows to push the events table past page 2, so zeroing an
        // interior page damages a b-tree while page 1's header stays valid —
        // the "still a database, but broken" case.
        {
            let store = Store::open(dir.path()).expect("store");
            let id = store
                .begin_execution("run", "fill some pages", "zai", "glm-5.2")
                .expect("execution");
            for seq in 0..400u64 {
                store
                    .record_event(
                        id,
                        seq,
                        &AgentEvent::Text {
                            text: format!("{seq} {}", "padding ".repeat(24)),
                        },
                    )
                    .expect("event");
            }
            drop(store);
        }
        let mut bytes = std::fs::read(&db_path).expect("read db");
        // Bytes 16..18 of the header are the page size (1 encodes 65536).
        let page_size = match u32::from(u16::from_be_bytes([bytes[16], bytes[17]])) {
            1 => 65_536usize,
            size => size as usize,
        };
        assert!(bytes.len() > page_size * 6, "fixture spans several pages");
        // Zero pages 4..7 (0-indexed 3..6) — past the schema, inside the data.
        for page in 3..6 {
            let start = page * page_size;
            bytes[start..start + page_size].fill(0);
        }
        std::fs::write(&db_path, &bytes).expect("write damaged db");

        let report = check_file(&db_path).expect("a damaged file is a verdict, not an error");
        assert!(!report.is_healthy(), "damage must not read as healthy");
        assert!(report.is_corruption());
        assert!(
            !report.problems().is_empty(),
            "the verdict carries what SQLite said: {report:?}"
        );
        assert!(
            report.headline().contains("problem") || report.headline().contains("not readable"),
            "the headline states the verdict: {}",
            report.headline()
        );
    }

    #[test]
    fn quarantine_moves_the_corrupt_store_aside_and_deletes_nothing() {
        let (dir, db_path) = healthy_workspace();
        shred_header(&db_path);
        let corrupt_bytes = std::fs::read(&db_path).expect("read corrupt db");
        // A stale WAL sibling must travel with the database it belongs to.
        let wal = dir.path().join(".stella/private/store.db-wal");
        std::fs::write(&wal, b"stale wal").expect("write wal");

        let quarantine = quarantine_corrupt_store(&db_path).expect("quarantine");

        assert!(!db_path.exists(), "the corrupt file is out of the way");
        assert!(!wal.exists(), "its stale WAL went with it");
        assert_eq!(quarantine.moved.len(), 2, "{quarantine:?}");
        let (from, to) = &quarantine.moved[0];
        assert_eq!(from, &db_path);
        assert_eq!(
            std::fs::read(to).expect("read backup"),
            corrupt_bytes,
            "the backup is the original bytes, untouched"
        );
        assert!(
            to.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("store.db.corrupt-")),
            "the backup name says what it is: {to:?}"
        );

        // The whole point: the workspace works again.
        let store = Store::open(dir.path()).expect("a fresh store opens");
        assert!(store.integrity_check().expect("check").is_healthy());
    }

    #[test]
    fn quarantine_salvages_readable_content_into_a_new_owner_only_database() {
        // Salvage is exercised on an intact file: `VACUUM INTO` on a shredded
        // header can only fail, so proving the copy actually carries the user's
        // rows over needs a database SQLite can still read. Localized damage
        // behaves the same way — that is why the step exists at all.
        let (_dir, db_path) = healthy_workspace();

        let quarantine = quarantine_corrupt_store(&db_path).expect("quarantine");
        assert_eq!(
            quarantine.salvage_error, None,
            "a readable file salvages: {quarantine:?}"
        );
        let salvaged = quarantine.salvaged.expect("salvaged copy");
        assert!(
            salvaged
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("store.db.salvaged-")),
            "the copy is named for what it is: {salvaged:?}"
        );
        let conn = open_private_sqlite_read_only(&salvaged).expect("open salvaged copy");
        let events: i64 = conn
            .query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0))
            .expect("count events");
        assert_eq!(events, 1, "the transcript survived into the salvaged copy");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::symlink_metadata(&salvaged)
                .expect("stat salvaged")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "the copy holds transcripts: owner-only");
        }
    }

    #[test]
    fn quarantine_never_overwrites_an_earlier_quarantine() {
        let (dir, db_path) = healthy_workspace();
        shred_header(&db_path);
        let first = quarantine_corrupt_store(&db_path).expect("first quarantine");

        // Same second, same stamp: a second round must pick a fresh name.
        Store::open(dir.path()).expect("fresh store");
        shred_header(&db_path);
        let second = quarantine_corrupt_store(&db_path).expect("second quarantine");

        let first_backup = &first.moved[0].1;
        let second_backup = &second.moved[0].1;
        assert_ne!(first_backup, second_backup);
        assert!(first_backup.exists(), "the older backup is still there");
        assert!(second_backup.exists());
    }
}
