//! `~/.stella` — the single user-level stella home, on every platform.
//!
//! Everything user-global (the `usage.db` telemetry hub, `catalog.db`, the
//! session registry, notifications, the enterprise spool, settings, skills,
//! agents, rules, tools, credentials) lives directly under `~/.stella`, the
//! same way Claude Code uses `~/.claude`. No platform data-dir guessing:
//! macOS, Linux, and Windows all resolve to the home directory.
//!
//! Overrides: `STELLA_HOME` moves the whole home; the narrower
//! `STELLA_DATA_DIR` / `STELLA_CONFIG_DIR` overrides still win where they
//! always did (tests, sandboxes, org-managed layouts).
//!
//! [`migrate_legacy_global_dirs`] performs the one-time move from the old
//! split layout (platform data dir + `~/.config/stella`) into `~/.stella`.
//! It is best-effort, idempotent, per-entry (an entry that already exists at
//! the new home is never overwritten), and skipped entirely when any
//! override env var points resolution away from the defaults.

use std::path::PathBuf;
use std::sync::Once;

/// The user's home directory: `HOME`, else `USERPROFILE` (Windows).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// The stella home: `$STELLA_HOME`, else `~/.stella`. `None` only when no
/// home directory is discoverable at all.
pub fn stella_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("STELLA_HOME") {
        return Some(PathBuf::from(dir));
    }
    home_dir().map(|home| home.join(".stella"))
}

/// The pre-`~/.stella` platform data dir (macOS Application Support,
/// `%APPDATA%`, XDG, `~/.local/share`) — where usage.db, catalog.db, the
/// session registry, notifications, and the enterprise spool used to live.
fn legacy_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME") {
        return Some(PathBuf::from(home).join("Library/Application Support/stella"));
    }
    #[cfg(target_os = "windows")]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        return Some(PathBuf::from(appdata).join("stella"));
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        return Some(PathBuf::from(xdg).join("stella"));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share/stella"))
}

/// The pre-`~/.stella` user config dir (`~/.config/stella`) — settings,
/// skills, agents, rules, tools, credentials.
fn legacy_config_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".config").join("stella"))
}

/// The sidecars SQLite keeps beside a database in WAL mode. They are
/// meaningless without their base file, so they never travel alone.
const SQLITE_SIDECARS: [&str; 2] = ["-wal", "-shm"];

/// If `name` is a WAL/SHM sidecar, the database file it belongs to.
fn sqlite_base_of(name: &str) -> Option<&str> {
    SQLITE_SIDECARS
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
}

/// Move every entry of `legacy` that does not already exist under `target`.
/// Prefers `fs::rename` (atomic, same-volume); on a cross-device (`EXDEV`)
/// or other rename failure it falls back to a recursive copy that leaves the
/// legacy original in place — so a failed move never strands the only copy
/// of a user's settings/credentials, and the next run retries. Returns the
/// number of entries it could neither move nor copy, so the caller can
/// surface a partial migration instead of silently reporting an empty home.
///
/// A SQLite database and its `-wal`/`-shm` sidecars are one **family**, not
/// three independent entries (#409). Moving them separately is what the
/// per-entry loop used to do, and it has two failure modes: the family can
/// be split across a crash, and — worse — the `dest.exists()` skip could
/// drop a legacy `-wal` beside a *different*, already-migrated database.
/// Families are therefore prepared and moved as a unit, and an orphan
/// sidecar whose base is absent is left where it is rather than migrated
/// next to a database it does not describe.
fn merge_dir_into(legacy: &std::path::Path, target: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(legacy) else {
        return 0;
    };
    if std::fs::create_dir_all(target).is_err() {
        return 1;
    }
    let names: Vec<std::ffi::OsString> = entries.flatten().map(|e| e.file_name()).collect();
    let present: std::collections::HashSet<&std::ffi::OsStr> =
        names.iter().map(std::ffi::OsString::as_os_str).collect();

    let mut failures = 0u64;
    for name in &names {
        // Sidecars are handled with their base — or, orphaned, not at all.
        if let Some(base) = name.to_str().and_then(sqlite_base_of) {
            if present.contains(std::ffi::OsStr::new(base)) {
                continue;
            }
            // An orphan `-wal`/`-shm` carries nothing recoverable on its
            // own; migrating it could only confuse a database at the target
            // that it has no relationship to.
            continue;
        }

        let sidecars: Vec<std::ffi::OsString> = name
            .to_str()
            .map(|name| {
                SQLITE_SIDECARS
                    .iter()
                    .map(|suffix| std::ffi::OsString::from(format!("{name}{suffix}")))
                    .filter(|sidecar| present.contains(sidecar.as_os_str()))
                    .collect()
            })
            .unwrap_or_default();

        if !sidecars.is_empty() {
            failures += move_sqlite_family(legacy, target, name, &sidecars);
            continue;
        }

        failures += move_entry(&legacy.join(name), &target.join(name));
    }
    failures
}

/// Move one legacy entry, skipping anything already present at the target.
/// Returns 1 when the entry could be neither renamed nor copied.
fn move_entry(src: &std::path::Path, dest: &std::path::Path) -> u64 {
    if dest.exists() {
        return 0;
    }
    if std::fs::rename(src, dest).is_ok() {
        return 0;
    }
    // Cross-device or racing rename: copy instead, leaving the legacy
    // original untouched. A failed copy removes its own partial output
    // so the retry starts clean.
    if copy_into(src, dest).is_err() {
        let _ = std::fs::remove_dir_all(dest);
        let _ = std::fs::remove_file(dest);
        return 1;
    }
    0
}

/// Migrate a SQLite database together with its live sidecars.
///
/// The database is checkpointed (`wal_checkpoint(TRUNCATE)`) and closed
/// first, so in the common case the `-wal` folds back into the base and
/// there is no live sidecar left to move at all. If another stella process
/// is actively writing the database, the whole family is **deferred** to the
/// next run rather than half-moved — the migration is idempotent and
/// resumable, so deferring costs nothing and a torn family costs data.
fn move_sqlite_family(
    legacy: &std::path::Path,
    target: &std::path::Path,
    name: &std::ffi::OsStr,
    sidecars: &[std::ffi::OsString],
) -> u64 {
    let db = legacy.join(name);
    // Any member already at the target means a previous run (or another
    // install) owns this family there. Never merge into it: a stale legacy
    // `-wal` beside a foreign database is the corruption this guards.
    if target.join(name).exists() || sidecars.iter().any(|s| target.join(s).exists()) {
        return 0;
    }
    if checkpoint_for_move(&db) == Checkpoint::Busy {
        // Live writer — leave every member in place and report it, so the
        // "rerun to retry" advice is the accurate remedy.
        return 1;
    }
    // Move the base first: a crash between renames then leaves the target
    // holding a database with no sidecars (which SQLite opens cleanly)
    // rather than sidecars with no database.
    let failures = move_entry(&db, &target.join(name));
    if failures > 0 {
        return failures;
    }
    sidecars
        .iter()
        // Checkpoint + close usually deletes these; only survivors move.
        .filter(|sidecar| legacy.join(sidecar).exists())
        .map(|sidecar| move_entry(&legacy.join(sidecar), &target.join(sidecar)))
        .sum()
}

/// Whether a database could be quiesced before moving it.
#[derive(Debug, PartialEq)]
enum Checkpoint {
    /// Safe to move — either checkpointed, or not a database we can drive
    /// (in which case the family still moves together, which is what makes
    /// it recoverable).
    Movable,
    /// Another connection holds the database; defer the whole family.
    Busy,
}

/// Fold the WAL back into `db` and close it, so the family reduces to a
/// single closed file before the move.
fn checkpoint_for_move(db: &std::path::Path) -> Checkpoint {
    use rusqlite::{Connection, ErrorCode, OpenFlags};

    // Don't open *through* a symlink — it could point anywhere. Renaming
    // the link itself is still safe, so this is `Movable`, not a failure.
    //
    // Deliberately checked here rather than with `SQLITE_OPEN_NOFOLLOW`:
    // that flag also rejects a symlinked *ancestor*, which is ordinary
    // (`/var` → `/private/var` on macOS), and would defer every family
    // forever for anyone whose home sits behind a link.
    match std::fs::symlink_metadata(db) {
        Ok(meta) if meta.file_type().is_symlink() => return Checkpoint::Movable,
        Ok(_) => {}
        Err(_) => return Checkpoint::Movable,
    }

    // Never CREATE: this path must not conjure a database that wasn't there.
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let Ok(conn) = Connection::open_with_flags(db, flags) else {
        // Cannot even open it — assume someone else can, and defer.
        return Checkpoint::Busy;
    };
    // An exclusive transaction is the precise probe for "another process is
    // using this": it is the one operation that must fail with BUSY while a
    // live writer holds the database.
    if let Err(err) = conn.execute_batch("BEGIN EXCLUSIVE; COMMIT;") {
        if matches!(
            err.sqlite_error_code(),
            Some(ErrorCode::DatabaseBusy) | Some(ErrorCode::DatabaseLocked)
        ) {
            return Checkpoint::Busy;
        }
        // Not a database, or otherwise not drivable by us. Moving the family
        // as a group is still correct — that is the recoverable shape.
        return Checkpoint::Movable;
    }
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
    // Leaving WAL mode retires the sidecars outright, so the family really
    // does reduce to one file. Stella re-enables WAL on its next open, so
    // this is a migration-window state, not a lasting change.
    let _ = conn.pragma_update(None, "journal_mode", "DELETE");
    Checkpoint::Movable
}

/// Recursively copy a file or directory tree `src` → `dest`. Used only as
/// the cross-device fallback in [`merge_dir_into`].
fn copy_into(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(src)?;
    if meta.is_dir() {
        std::fs::create_dir_all(dest)?;
        for entry in std::fs::read_dir(src)?.flatten() {
            copy_into(&entry.path(), &dest.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(src, dest).map(|_| ())
    }
}

/// One-time (per process), best-effort migration of the legacy split layout
/// into `~/.stella`. A no-op when `STELLA_HOME`, `STELLA_DATA_DIR`, or
/// `STELLA_CONFIG_DIR` is set (resolution isn't pointing at the defaults, so
/// moving real user data would be wrong), or when there is nothing to move.
pub fn migrate_legacy_global_dirs() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        for var in ["STELLA_HOME", "STELLA_DATA_DIR", "STELLA_CONFIG_DIR"] {
            if std::env::var_os(var).is_some() {
                return;
            }
        }
        let Some(target) = stella_home() else {
            return;
        };
        let mut failures = 0u64;
        for legacy in [legacy_data_dir(), legacy_config_dir()]
            .into_iter()
            .flatten()
        {
            if legacy != target && legacy.is_dir() {
                failures += merge_dir_into(&legacy, &target);
            }
        }
        if failures > 0 {
            // Never fail a turn, but don't let a partial migration look like
            // a clean empty home — the legacy originals are still in place.
            eprintln!(
                "stella: could not migrate {failures} legacy config/data \
                 entr{} into {} — the originals were left untouched; rerun to retry",
                if failures == 1 { "y" } else { "ies" },
                target.display()
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_moves_only_missing_entries_and_keeps_legacy_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy");
        let target = tmp.path().join("home/.stella");
        std::fs::create_dir_all(legacy.join("sessions")).unwrap();
        std::fs::write(legacy.join("usage.db"), b"legacy-usage").unwrap();
        std::fs::write(legacy.join("installation-id"), b"legacy-id").unwrap();
        std::fs::write(legacy.join("sessions/one.json"), b"{}").unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("installation-id"), b"new-id").unwrap();

        assert_eq!(
            merge_dir_into(&legacy, &target),
            0,
            "clean move, no failures"
        );

        assert_eq!(
            std::fs::read(target.join("usage.db")).unwrap(),
            b"legacy-usage",
            "missing entries move over"
        );
        assert_eq!(
            std::fs::read(target.join("sessions/one.json")).unwrap(),
            b"{}",
            "directories move wholesale"
        );
        assert_eq!(
            std::fs::read(target.join("installation-id")).unwrap(),
            b"new-id",
            "existing entries are never overwritten"
        );
        assert!(
            std::fs::read(legacy.join("installation-id")).unwrap() == b"legacy-id",
            "skipped entries stay in the legacy dir"
        );
        assert!(legacy.exists(), "legacy dir itself is preserved");

        // Idempotent: a second merge changes nothing.
        merge_dir_into(&legacy, &target);
        assert_eq!(
            std::fs::read(target.join("installation-id")).unwrap(),
            b"new-id"
        );
    }

    /// A WAL-mode database at `path` holding one row, closed cleanly.
    fn seed_db(path: &std::path::Path, value: &str) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.execute_batch("CREATE TABLE IF NOT EXISTS t (v TEXT);")
            .unwrap();
        conn.execute("INSERT INTO t (v) VALUES (?1)", [value])
            .unwrap();
    }

    fn read_back(path: &std::path::Path) -> Vec<String> {
        let conn = rusqlite::Connection::open(path).unwrap();
        let mut stmt = conn.prepare("SELECT v FROM t ORDER BY v").unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// Witness for #409: a database and its sidecars are one unit. The
    /// family must arrive intact and must not leave sidecars stranded in
    /// the legacy dir.
    #[test]
    fn a_sqlite_family_migrates_as_one_unit_with_its_data() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy");
        let target = tmp.path().join("home/.stella");
        std::fs::create_dir_all(&legacy).unwrap();
        seed_db(&legacy.join("usage.db"), "kept");
        // The shape a crash leaves behind: sidecars still on disk beside
        // the base at migration time.
        std::fs::write(legacy.join("usage.db-wal"), b"").unwrap();
        std::fs::write(legacy.join("usage.db-shm"), b"").unwrap();

        assert_eq!(merge_dir_into(&legacy, &target), 0, "a closed family moves");

        assert_eq!(
            read_back(&target.join("usage.db")),
            vec!["kept".to_string()],
            "zero data loss: the migrated database still answers for its rows"
        );
        for sidecar in ["usage.db-wal", "usage.db-shm"] {
            assert!(
                !legacy.join(sidecar).exists(),
                "{sidecar} must not be stranded in the legacy dir"
            );
        }
        assert!(
            !legacy.join("usage.db").exists(),
            "the base moved, so the family is gone from legacy"
        );
    }

    /// Witness for #409's sharpest edge: the per-entry loop could skip an
    /// already-migrated `usage.db` and then move the legacy `-wal` in
    /// beside it — a stale WAL describing a database it has no relationship
    /// to. The family must be skipped whole.
    #[test]
    fn a_legacy_sidecar_never_lands_beside_a_foreign_database() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy");
        let target = tmp.path().join("home/.stella");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        seed_db(&legacy.join("usage.db"), "legacy");
        std::fs::write(legacy.join("usage.db-wal"), b"stale-wal").unwrap();
        seed_db(&target.join("usage.db"), "already-migrated");

        assert_eq!(merge_dir_into(&legacy, &target), 0);

        assert!(
            !target.join("usage.db-wal").exists(),
            "a legacy WAL must never be dropped beside a database it does not describe"
        );
        assert_eq!(
            read_back(&target.join("usage.db")),
            vec!["already-migrated".to_string()],
            "the already-migrated database is untouched"
        );
        assert!(
            legacy.join("usage.db").exists() && legacy.join("usage.db-wal").exists(),
            "the skipped family stays whole in the legacy dir"
        );
    }

    /// An orphan sidecar describes nothing; migrating it could only
    /// confuse the target. It stays put.
    #[test]
    fn an_orphan_sidecar_is_left_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy");
        let target = tmp.path().join("home/.stella");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("usage.db-wal"), b"orphan").unwrap();

        assert_eq!(merge_dir_into(&legacy, &target), 0);

        assert!(
            !target.join("usage.db-wal").exists(),
            "orphan does not move"
        );
        assert!(legacy.join("usage.db-wal").exists(), "orphan stays put");
    }

    /// The one genuinely unhandled window in #409: another stella process
    /// actively writing a legacy database *during* the first migration.
    /// The family must be deferred whole, never half-moved.
    #[test]
    fn a_live_writer_defers_the_whole_family() {
        let tmp = tempfile::tempdir().unwrap();
        let legacy = tmp.path().join("legacy");
        let target = tmp.path().join("home/.stella");
        std::fs::create_dir_all(&legacy).unwrap();
        let db = legacy.join("usage.db");
        seed_db(&db, "in-flight");

        // A live writer: an open connection holding the write lock, exactly
        // as a concurrent stella process would.
        let writer = rusqlite::Connection::open(&db).unwrap();
        writer.pragma_update(None, "journal_mode", "WAL").unwrap();
        writer.execute_batch("BEGIN EXCLUSIVE;").unwrap();
        assert!(
            legacy.join("usage.db-shm").exists(),
            "precondition: the live writer has real sidecars on disk"
        );

        assert_eq!(
            merge_dir_into(&legacy, &target),
            1,
            "a busy family is reported, so `rerun to retry` is accurate advice"
        );
        assert!(
            !target.join("usage.db").exists(),
            "nothing moved: a half-moved family is the data loss this prevents"
        );
        assert!(
            db.exists(),
            "the database stays where the live writer left it"
        );

        writer.execute_batch("COMMIT;").unwrap();
        drop(writer);

        // Resumable: once the writer is gone, the next run migrates it.
        assert_eq!(merge_dir_into(&legacy, &target), 0, "the retry succeeds");
        assert_eq!(
            read_back(&target.join("usage.db")),
            vec!["in-flight".to_string()],
            "deferring cost nothing: the data arrives intact on the next run"
        );
    }

    #[test]
    fn merge_with_missing_legacy_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join(".stella");
        assert_eq!(
            merge_dir_into(&tmp.path().join("does-not-exist"), &target),
            0
        );
        assert!(!target.exists(), "no legacy → nothing created");
    }

    #[test]
    fn copy_into_clones_a_nested_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir_all(src.join("skills/a")).unwrap();
        std::fs::write(src.join("settings.json"), b"{}").unwrap();
        std::fs::write(src.join("skills/a/SKILL.md"), b"# a").unwrap();
        let dest = tmp.path().join("dest");

        copy_into(&src, &dest).unwrap();

        assert_eq!(std::fs::read(dest.join("settings.json")).unwrap(), b"{}");
        assert_eq!(
            std::fs::read(dest.join("skills/a/SKILL.md")).unwrap(),
            b"# a"
        );
        assert!(src.join("settings.json").exists(), "source is left intact");
    }

    #[test]
    fn stella_home_honors_the_override() {
        // SAFETY: single-threaded test; set and read one process env var.
        unsafe {
            std::env::set_var("STELLA_HOME", "/tmp/stella-home-test");
        }
        assert_eq!(stella_home(), Some(PathBuf::from("/tmp/stella-home-test")));
        unsafe {
            std::env::remove_var("STELLA_HOME");
        }
    }
}
