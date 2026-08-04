//! **The** durability contract for everything Stella writes to disk, in one
//! place (#617).
//!
//! Before this module the tree carried five hand-rolled copies of
//! temp-then-rename — one per crate, each with a slightly different set of
//! omissions (a missing directory `fsync` here, a fixed temp name there, a
//! non-unix arm that failed closed somewhere else). This is the single
//! implementation they all call now. Adding a sixth copy is the thing this
//! module exists to prevent.
//!
//! # The contract
//!
//! A **durable write** of Stella's own state — `.stella/` files, the media
//! manifest, `stella.storage.toml`, the session registry, settings —
//! is [`write_atomic`], and means all of:
//!
//! 1. the replacement bytes go to a **sibling temp file** whose name carries
//!    the writer's pid and a per-process counter, so two processes and two
//!    threads can never collide on one temp path;
//! 2. the temp file is `fsync`ed **before** the rename, so the rename cannot
//!    publish bytes that are still only in the page cache;
//! 3. the temp file is `rename`d over the target — the replacement is atomic
//!    for any concurrent reader, which either sees the whole old file or the
//!    whole new one;
//! 4. the **parent directory** is `fsync`ed after the rename, because a
//!    rename is only durable once the directory entry that records it has
//!    itself reached the disk;
//! 5. on any failure the temp file is removed, so a failed write never
//!    litters the user's tree and a partially written temp is never left
//!    where a later reader could mistake it for state.
//!
//! What this does **not** promise: serialization between writers. A
//! read-modify-write (the media manifest, `stella.storage.toml`) still needs
//! its own lock around the read and the write — atomicity of the replacement
//! does not stop two racers from both reading the same pre-image and one
//! losing. Callers that need that take an advisory lock and then call in
//! here.
//!
//! # Platform asymmetry — what is guaranteed where
//!
//! The five properties above hold on **every** platform except that (4), the
//! directory `fsync`, is unix-only: Windows has no handle you can open on a
//! directory and flush, so the rename's durability there is whatever NTFS's
//! own metadata journal gives you. That is a real, documented difference in
//! the guarantee, not a bug to be fixed here.
//!
//! On unix the write is additionally **hardened**: the temp is created
//! `O_NOFOLLOW | O_CLOEXEC` with `O_EXCL`, its mode is set explicitly after
//! the bytes are written (so a set-user-ID/set-group-ID bit survives the
//! write that POSIX would otherwise clear), and the file is confirmed to be a
//! regular file before anything is written to it. None of those primitives
//! exist on non-unix, so the `#[cfg(not(unix))]` arm does temp + fsync +
//! rename **without** them.
//!
//! It does not *fail* for lack of them. That is the ruling this module
//! encodes: before #617, several of these write paths were `#[cfg(unix)]`
//! with a non-unix arm that returned "unsupported on this platform", which
//! means a Windows build could not write its own session registry, settings
//! or workspace `.gitignore` at all. A platform that cannot offer the
//! hardening still gets the durability; refusing to write state is a strictly
//! worse outcome than writing it without an owner check, and the mode
//! argument is simply ignored where the OS has no mode.
//!
//! # Schema versioning
//!
//! The same file also owns the *other* half of the contract:
//! [`ensure_converged_schema_version`], the one policy for stamping and
//! checking `PRAGMA user_version` on the sidecar SQLite stores whose DDL is
//! idempotent convergence rather than a stepwise migration ladder.

use std::path::Path;

use rusqlite::Connection;

use crate::{Result, StoreError};

/// Default mode for a durable file that is not owner-private: readable by
/// anyone who can read the workspace, writable only by its owner.
pub const MODE_SHARED: u32 = 0o644;

/// Mode for owner-only local state — everything under `.stella/private/` and
/// every file that can carry a credential.
pub const MODE_PRIVATE: u32 = 0o600;

/// Atomically replace `path` with `bytes`, durably.
///
/// See the module docs for the exact guarantee and its one platform
/// asymmetry. `mode` is the permission the *target* ends up with on unix and
/// is ignored elsewhere; pass [`MODE_PRIVATE`] for owner-only state and
/// [`MODE_SHARED`] for a file meant to be readable (a generated `.gitignore`,
/// the storage manifest).
///
/// The target's own type is not inspected: callers that must refuse to
/// replace, say, a symlink or a directory check that first — they have the
/// better error message for it, and adding a stat here would only widen the
/// race, not close it.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    let temp = temp_sibling(path)?;
    let result = write_temp_then_rename(&temp, path, bytes, mode);
    if result.is_err() {
        // Never leave a partially written temp beside the target: it is not
        // state, and a later reader (or a glob) must not be able to find it.
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// [`write_atomic`], but carrying the target's existing mode forward instead
/// of imposing one; `fallback` is used only when there is no target yet.
///
/// The distinction is the whole reason `mode` is a parameter rather than a
/// constant. Rename-over-target replaces the inode, so the replacement's mode
/// is whatever the writer chose — and for two different classes of file the
/// right choice is opposite:
///
/// - **Owner-only state** (`.stella/private/`, credentials, settings that can
///   hold an API key) uses [`write_atomic`] with [`MODE_PRIVATE`], which
///   *enforces* 0600 on every write. Finding a widened mode there is a fault
///   to repair, not a preference to respect.
/// - **A file in the user's own tree** (`stella.storage.toml`, project
///   `.stella/settings.json`, the generated `.gitignore`) uses this one. The
///   user may have chmod'd it deliberately, or it may be group-writable
///   because their repository is; silently resetting it to 0644 on the next
///   agent write is exactly the inode-replacement damage #617 ruled against.
pub fn write_atomic_preserving_mode(path: &Path, bytes: &[u8], fallback: u32) -> Result<()> {
    write_atomic(path, bytes, existing_mode(path).unwrap_or(fallback))
}

/// The mode of an existing regular file at `path`. `None` when there is no
/// file, it is not a regular one, or the platform has no modes.
#[cfg(unix)]
fn existing_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt as _;
    let metadata = std::fs::symlink_metadata(path).ok()?;
    metadata
        .is_file()
        .then(|| metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn existing_mode(_path: &Path) -> Option<u32> {
    None
}

/// `fsync` a directory so a rename inside it is durable, not just the bytes
/// it published. A no-op on platforms with no directory handle to flush.
pub fn sync_directory(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let file = std::fs::File::open(dir).map_err(|e| {
            StoreError(format!(
                "cannot open directory {} for fsync: {e}",
                dir.display()
            ))
        })?;
        file.sync_all()
            .map_err(|e| StoreError(format!("cannot fsync directory {}: {e}", dir.display())))?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}

/// A sibling temp path carrying pid + a per-process counter.
///
/// Both halves matter: the pid separates two processes writing one workspace,
/// the counter separates two threads (or two sequential writes) inside one.
/// A fixed `.tmp` name — what three of the copies this module replaced used —
/// lets either pair interleave into one file and publish a spliced document.
fn temp_sibling(path: &Path) -> Result<std::path::PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    let name = path
        .file_name()
        .ok_or_else(|| StoreError(format!("durable path {} has no filename", path.display())))?;
    let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let mut temp_name = name.to_os_string();
    temp_name.push(format!(".stella-tmp.{}.{sequence}", std::process::id()));
    Ok(path.with_file_name(temp_name))
}

#[cfg(unix)]
fn write_temp_then_rename(temp: &Path, path: &Path, bytes: &[u8], mode: u32) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = options
        .open(temp)
        .map_err(|e| StoreError(format!("cannot create {}: {e}", temp.display())))?;
    let metadata = file
        .metadata()
        .map_err(|e| StoreError(format!("cannot inspect {}: {e}", temp.display())))?;
    if !metadata.is_file() {
        return Err(StoreError(format!(
            "temporary path {} is not a regular file",
            temp.display()
        )));
    }
    file.write_all(bytes)
        .map_err(|e| StoreError(format!("cannot write {}: {e}", temp.display())))?;
    // After the write, never before: POSIX clears set-user-ID/set-group-ID on
    // write, so a mode applied first loses exactly the bits hardest to notice
    // missing. Before the fsync, so the metadata change is covered by it.
    // `set_permissions` rather than the `.mode()` above alone, because that
    // one is masked by the umask.
    file.set_permissions(std::fs::Permissions::from_mode(mode))
        .map_err(|e| StoreError(format!("cannot set mode on {}: {e}", temp.display())))?;
    file.sync_all()
        .map_err(|e| StoreError(format!("cannot fsync {}: {e}", temp.display())))?;
    drop(file);
    std::fs::rename(temp, path)
        .map_err(|e| StoreError(format!("cannot replace {}: {e}", path.display())))?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

/// Temp + fsync + rename with none of the unix-only hardening: there is no
/// mode to apply, no `O_NOFOLLOW` to open with, and no directory handle to
/// fsync. `mode` is accepted and ignored so the call site reads identically
/// on every platform.
#[cfg(not(unix))]
fn write_temp_then_rename(temp: &Path, path: &Path, bytes: &[u8], _mode: u32) -> Result<()> {
    use std::io::Write as _;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temp)
        .map_err(|e| StoreError(format!("cannot create {}: {e}", temp.display())))?;
    file.write_all(bytes)
        .map_err(|e| StoreError(format!("cannot write {}: {e}", temp.display())))?;
    file.sync_all()
        .map_err(|e| StoreError(format!("cannot fsync {}: {e}", temp.display())))?;
    drop(file);
    // `std::fs::rename` is MoveFileEx(MOVEFILE_REPLACE_EXISTING) here, which
    // does replace an existing destination — the one thing the naive
    // "rename never overwrites on Windows" folklore gets wrong.
    std::fs::rename(temp, path)
        .map_err(|e| StoreError(format!("cannot replace {}: {e}", path.display())))?;
    Ok(())
}

/// Stamp and check `PRAGMA user_version` on a store whose DDL is idempotent
/// convergence (`CREATE … IF NOT EXISTS`, replayed on every open) rather than
/// a stepwise migration ladder.
///
/// Call it **after** the DDL batch, with the tables that batch is supposed to
/// have produced. The policy it implements, per store (#617):
///
/// - **`user_version` > `current`** — the file was written by a newer stella
///   than this binary. Fail closed with a message that says so. Opening it
///   as-is would let old code write into a shape whose invariants it does not
///   know, which is how a store gets silently downgraded.
/// - **`user_version` < 0** — the header is not one of ours (or was
///   overwritten). Fail closed rather than treat it as version 0 and stamp
///   over someone else's file.
/// - **`user_version` == 0 and every expected table is present** — a store
///   already in the field, from before this store carried a stamp. It is
///   **stamped to `current`, never rebuilt**: the DDL that just replayed is
///   the whole of this store's schema, so the shape genuinely matches and
///   destroying the user's rows to "start clean" would be a data loss with no
///   upside. This is also the fresh-file path — a store the DDL just created
///   is indistinguishable from a converged one, and wants the same stamp.
/// - **`user_version` == 0 and a table is missing** — the DDL did not produce
///   what it claims. Fail closed rather than stamp a version onto a shape
///   that does not match it; a stamped lie is worse than no stamp, because
///   every future migration would trust it.
/// - **0 < `user_version` < `current`** — an older stamp on a schema that has
///   only ever grown additively; the replayed DDL has already converged it,
///   so it is stamped forward.
///
/// This is deliberately **not** for stores with a real migration ladder
/// (`store.db`, `context.db`, `fleet.db`): those must run their numbered
/// steps, and a helper that stamps forward unconditionally would skip them.
pub fn ensure_converged_schema_version(
    conn: &Connection,
    store: &str,
    current: i64,
    expected_tables: &[&str],
) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version < 0 {
        return Err(StoreError(format!(
            "{store} carries a negative schema version ({version}), so it is not a stella store \
             or its header was overwritten. Move the file aside and reopen to start a fresh one."
        )));
    }
    if version > current {
        return Err(StoreError(format!(
            "{store} is at schema version {version}, but this build only knows {current} — your \
             stella binary is older than the workspace, not the other way round. Upgrade stella \
             (brew upgrade stella, install.sh, or a newer release build) and reopen."
        )));
    }
    if version == current {
        return Ok(());
    }
    for table in expected_tables {
        let present: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )?;
        if present == 0 {
            return Err(StoreError(format!(
                "{store} is missing the table {table} that its schema declares, so it cannot be \
                 stamped at version {current}. Move the file aside and reopen to rebuild it."
            )));
        }
    }
    conn.pragma_update(None, "user_version", current)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "stella-durable-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn strays(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .expect("read dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("stella-tmp"))
            .collect()
    }

    #[test]
    fn a_successful_write_publishes_the_bytes_and_leaves_no_temp() {
        let dir = temp_dir();
        let path = dir.join("state.json");
        write_atomic(&path, b"{\"a\":1}", MODE_PRIVATE).expect("write");
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"a\":1}");
        assert!(
            strays(&dir).is_empty(),
            "temp left behind: {:?}",
            strays(&dir)
        );
    }

    /// The failure has to happen *after* the temp exists or it proves
    /// nothing about cleanup. Renaming over a non-empty directory is a
    /// deterministic post-temp failure needing no injection seam: the temp is
    /// created, written and fsynced, and only the rename fails.
    #[test]
    fn a_failed_rename_removes_the_temp_and_leaves_the_target_untouched() {
        let dir = temp_dir();
        let target = dir.join("occupied");
        std::fs::create_dir(&target).expect("create dir");
        std::fs::write(target.join("child"), b"resident").expect("child");

        let error = write_atomic(&target, b"replacement", MODE_PRIVATE)
            .expect_err("renaming over a non-empty directory must fail");
        assert!(
            error.0.contains("cannot replace"),
            "unexpected: {}",
            error.0
        );
        assert!(
            strays(&dir).is_empty(),
            "a failed write left a temp behind: {:?}",
            strays(&dir)
        );
        assert_eq!(
            std::fs::read(target.join("child")).unwrap(),
            b"resident",
            "the target must be untouched by a failed replace"
        );
    }

    /// A crashed writer's leftover temp is inert: it is not the target, and
    /// the next successful write neither reads it nor publishes it.
    #[test]
    fn a_stale_temp_never_becomes_the_target() {
        let dir = temp_dir();
        let path = dir.join("manifest.json");
        write_atomic(&path, b"good", MODE_PRIVATE).expect("first write");
        let orphan = dir.join("manifest.json.stella-tmp.99999.7");
        std::fs::write(&orphan, b"half-writ").expect("orphan");

        assert_eq!(std::fs::read(&path).unwrap(), b"good");
        write_atomic(&path, b"better", MODE_PRIVATE).expect("second write");
        assert_eq!(std::fs::read(&path).unwrap(), b"better");
        assert_eq!(
            std::fs::read(&orphan).unwrap(),
            b"half-writ",
            "an unrelated writer's temp must not be consumed or renamed"
        );
    }

    /// A rename swaps the inode, so the user's chosen mode is only preserved
    /// if the writer deliberately carries it. A group-writable file in a
    /// shared repository must not come back 0644 after an agent write.
    #[cfg(unix)]
    #[test]
    fn preserving_mode_carries_the_targets_existing_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir();
        let path = dir.join("stella.storage.toml");
        std::fs::write(&path, b"version = 1\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o664)).unwrap();

        write_atomic_preserving_mode(&path, b"version = 2\n", MODE_SHARED).expect("write");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o664, "the user's mode must survive the replace");
        assert_eq!(std::fs::read(&path).unwrap(), b"version = 2\n");
    }

    #[cfg(unix)]
    #[test]
    fn preserving_mode_falls_back_when_there_is_no_target() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir();
        let path = dir.join("fresh.toml");
        write_atomic_preserving_mode(&path, b"x", MODE_PRIVATE).expect("write");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, MODE_PRIVATE);
    }

    #[cfg(unix)]
    #[test]
    fn the_requested_mode_survives_the_umask() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir();
        let path = dir.join("credentials.toml");
        write_atomic(&path, b"key = 1", MODE_PRIVATE).expect("write");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, MODE_PRIVATE, "0600 must not be widened by the umask");
    }

    fn converged(conn: &Connection) {
        conn.execute_batch("CREATE TABLE IF NOT EXISTS thing (id INTEGER PRIMARY KEY);")
            .expect("ddl");
    }

    fn version(conn: &Connection) -> i64 {
        conn.query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read version")
    }

    #[test]
    fn a_fresh_store_is_stamped_and_a_reopen_preserves_it() {
        let conn = Connection::open_in_memory().expect("open");
        converged(&conn);
        ensure_converged_schema_version(&conn, "test.db", 3, &["thing"]).expect("stamp");
        assert_eq!(version(&conn), 3);

        ensure_converged_schema_version(&conn, "test.db", 3, &["thing"]).expect("reopen");
        assert_eq!(version(&conn), 3, "a matching version is accepted as-is");
    }

    /// The in-the-field case: a store written before this store carried a
    /// stamp. Its rows must survive; only the header changes.
    #[test]
    fn an_unstamped_store_that_matches_is_stamped_not_rebuilt() {
        let conn = Connection::open_in_memory().expect("open");
        converged(&conn);
        conn.execute("INSERT INTO thing (id) VALUES (1)", [])
            .expect("row");
        assert_eq!(version(&conn), 0);

        ensure_converged_schema_version(&conn, "test.db", 2, &["thing"]).expect("stamp");

        assert_eq!(version(&conn), 2);
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM thing", [], |row| row.get(0))
            .expect("count");
        assert_eq!(rows, 1, "field data must survive the stamp");
    }

    #[test]
    fn a_store_from_the_future_is_refused() {
        let conn = Connection::open_in_memory().expect("open");
        converged(&conn);
        conn.pragma_update(None, "user_version", 9i64).expect("set");

        let error = ensure_converged_schema_version(&conn, "test.db", 2, &["thing"])
            .expect_err("a newer store must fail closed");
        assert!(error.0.contains("schema version 9"), "{}", error.0);
        assert_eq!(version(&conn), 9, "the future stamp must not be downgraded");
    }

    #[test]
    fn a_store_missing_a_declared_table_is_not_stamped() {
        let conn = Connection::open_in_memory().expect("open");
        let error = ensure_converged_schema_version(&conn, "test.db", 1, &["thing"])
            .expect_err("an unconverged store must fail closed");
        assert!(error.0.contains("missing the table thing"), "{}", error.0);
        assert_eq!(version(&conn), 0, "no version is stamped onto a bad shape");
    }

    #[test]
    fn a_negative_stamp_is_refused() {
        let conn = Connection::open_in_memory().expect("open");
        converged(&conn);
        conn.pragma_update(None, "user_version", -4i64)
            .expect("set");
        let error = ensure_converged_schema_version(&conn, "test.db", 1, &["thing"])
            .expect_err("a negative stamp must fail closed");
        assert!(error.0.contains("negative schema version"), "{}", error.0);
    }
}
