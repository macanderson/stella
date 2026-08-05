//! Owner-only local-state filesystem primitives.
//!
//! Every durable replacement here goes through [`crate::durable::write_atomic`]
//! — the one temp + fsync + rename + fsync-parent implementation (#617). What
//! this module adds on top is *identity*: the no-follow, owner-and-single-link,
//! regular-file validation that says the thing being replaced is the caller's
//! own state and not something planted at that path. That validation is
//! unix-only; the durability is not.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::durable::{MODE_PRIVATE, MODE_SHARED, write_atomic};
use crate::{Result, StoreError};

pub(crate) use crate::durable::sync_directory;

pub const WORKSPACE_PRIVATE_DIR: &str = "private";
pub(crate) const WORKSPACE_GENERATED_IGNORE: &[u8] =
    b"*.db\n*.db-wal\n*.db-shm\nreflections.jsonl\nprivate/\n";

#[cfg(unix)]
fn read_committable_file(path: &Path) -> Result<Option<(Vec<u8>, u32)>> {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let mut file = match options.open(path) {
        Ok(file) => file,
        // Unlinked between the caller's look and this open. Same benign
        // publisher as the `nlink() == 0` arm below, seen one instant earlier.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(StoreError(format!(
                "cannot open committable file {}: {e}",
                path.display()
            )));
        }
    };
    let metadata = file
        .metadata()
        .map_err(|e| StoreError(format!("cannot inspect {}: {e}", path.display())))?;
    // `nlink() == 0` is the one benign member of this family, and it must be
    // split out before the assertion below rather than folded into it: it says
    // the inode we are holding open was unlinked while we held it, which is
    // exactly what a concurrent `write_atomic` rename does to the file it
    // replaces. Nothing planted it — there is nothing left at the path to have
    // planted. An attacker's file always presents at least one link, so
    // conceding this case costs the injection defense nothing, while folding
    // it in reports a fabricated attack on a file that merely lost a race
    // (#617 item 10, and the residual window #1410 closes).
    //
    // Ownership is still asserted first, so a *foreign* unlinked inode is
    // reported rather than quietly retried.
    if metadata.is_file() && metadata.uid() == unsafe { libc::geteuid() } && metadata.nlink() == 0 {
        return Ok(None);
    }
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } || metadata.nlink() != 1
    {
        return Err(StoreError(format!(
            "committable file {} must be an owner-controlled single-link regular file",
            path.display()
        )));
    }
    let mode = metadata.permissions().mode() & 0o7777;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| StoreError(format!("cannot read {}: {e}", path.display())))?;
    Ok(Some((bytes, mode)))
}

/// Read a workspace file that is *meant* to be committed (the generated
/// `.stella/.gitignore`) where there is no ownership model to check.
///
/// It cannot make the owner-and-single-link assertion the unix arm makes, so
/// it does not pretend to: it rejects a symlink and a non-regular file, reads
/// the bytes, and reports the mode the writer will use. Failing closed here
/// instead — which is what this arm used to do — failed
/// [`ensure_workspace_generated_ignore`], and with it every
/// `workspace_private_*` path resolution, so a non-unix build could not open
/// its own store at all (#617).
#[cfg(not(unix))]
fn read_committable_file(path: &Path) -> Result<Option<(Vec<u8>, u32)>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        // `None` carries the same meaning on both arms: no stable file to
        // judge yet, so the caller settles rather than concluding anything.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(StoreError(format!(
                "cannot inspect {}: {e}",
                path.display()
            )));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(StoreError(format!(
            "committable file {} must be a regular file",
            path.display()
        )));
    }
    let bytes = std::fs::read(path)
        .map_err(|e| StoreError(format!("cannot read {}: {e}", path.display())))?;
    Ok(Some((bytes, MODE_SHARED)))
}

/// Create the generated ignore exclusively, or report that someone else won.
///
/// `Ok(true)` means this caller created it; `Ok(false)` means it already
/// existed. The exclusivity is the point: the previous code took
/// `symlink_metadata` → `NotFound` → [`write_atomic`], so concurrent first
/// openers of one workspace all observed no file and all renamed over the
/// target. A rename unlinks the inode it replaces, so a racer that had just
/// opened the winner's file for validation saw `nlink() == 0` and failed
/// closed — `Store::open` reported the generated ignore "must be an
/// owner-controlled single-link regular file" for a file nothing had attacked
/// (#617 item 10).
///
/// `O_NOFOLLOW` alongside `O_EXCL` so this cannot be aimed through a symlink
/// planted in the window, and [`MODE_SHARED`] because the file is meant to be
/// committed — the same mode [`write_atomic`] would have imposed.
#[cfg(unix)]
fn create_generated_ignore_exclusively(path: &Path) -> Result<bool> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(MODE_SHARED)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(WORKSPACE_GENERATED_IGNORE)
                .and_then(|_| file.sync_all())
                .map_err(|e| StoreError(format!("cannot write {}: {e}", path.display())))?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(StoreError(format!(
            "cannot create generated ignore {}: {error}",
            path.display()
        ))),
    }
}

/// Non-unix has no ownership model to race over, so the pre-existing
/// check-then-write is kept as-is rather than pretending to be exclusive.
#[cfg(not(unix))]
fn create_generated_ignore_exclusively(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_atomic(path, WORKSPACE_GENERATED_IGNORE, MODE_SHARED)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// How long a loser waits for the winner's bytes before deciding a zero-length
/// generated ignore is content of its own.
///
/// The window it covers is a single `write_all` of a 47-byte constant, so it
/// shuts in microseconds and the budget is never spent in practice. It is
/// generous anyway because the cost of the two mistakes is nothing alike:
/// waiting a few milliseconds too long is invisible, while giving up too early
/// puts the loser back on the rename path and resurrects the exact failure
/// `create_generated_ignore_exclusively` was written to end.
const IGNORE_SETTLE_ATTEMPTS: u32 = 50;
const IGNORE_SETTLE_PAUSE: std::time::Duration = std::time::Duration::from_millis(1);

pub(crate) fn ensure_workspace_generated_ignore(dot: &Path) -> Result<()> {
    let path = dot.join(".gitignore");
    // Try to be the one that creates it. Whoever wins writes content that
    // already contains `private/`, so every loser falls through to the read
    // below, finds the entry, and returns without writing — no rename races a
    // reader.
    if create_generated_ignore_exclusively(&path)? {
        return Ok(());
    }
    let (mut bytes, mode) = settled_generated_ignore(&path)?;
    if bytes
        .split(|byte| *byte == b'\n')
        .any(|line| line == b"private/")
    {
        return Ok(());
    }
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        bytes.push(b'\n');
    }
    bytes.extend_from_slice(b"private/\n");
    write_atomic(&path, &bytes, mode)
}

/// The generated ignore's contents once no publisher is still mid-flight.
///
/// Losing the exclusive create says a file exists; it does *not* say the
/// winner has written to it yet. Between `create_new` returning and the
/// winner's `write_all` landing, the path holds a zero-length file — and a
/// loser that read it went on to treat it as a real ignore missing `private/`
/// and repaired it with [`write_atomic`], whose rename unlinks the winner's
/// inode. Any third opener holding that inode open for validation then saw
/// `nlink() == 0` and reported the generated ignore as something planted.
///
/// That is the residual of #617 item 10 (#1410): the exclusive create removed
/// the *common* rename-races-a-reader path, and this closes the window it
/// left. Waiting is the whole fix — the winner's bytes contain `private/` by
/// construction, so a loser that waits has nothing left to write.
///
/// A file that is still empty after the budget is a real empty file that
/// nobody is writing, and it is repaired exactly as before. Skipping it
/// instead would be a permanent skip, not a wait: the next open would find the
/// same empty file and skip again, and `private/` would never be added.
fn settled_generated_ignore(path: &Path) -> Result<(Vec<u8>, u32)> {
    let mut last = None;
    for attempt in 0..IGNORE_SETTLE_ATTEMPTS {
        match read_committable_file(path)? {
            // Settled and non-empty: the common case, and the only one that
            // can be answered on the first look.
            Some((bytes, mode)) if !bytes.is_empty() => return Ok((bytes, mode)),
            // Present but empty — a winner mid-write, or a genuinely empty
            // file. Indistinguishable in this instant, so keep it and look
            // again; whichever it is declares itself by whether it changes.
            Some(empty) => last = Some(empty),
            // Unlinked under us mid-publication. Nothing to keep: the inode we
            // looked at is gone and the replacement is not visible yet.
            None => {}
        }
        if attempt + 1 < IGNORE_SETTLE_ATTEMPTS {
            std::thread::sleep(IGNORE_SETTLE_PAUSE);
        }
    }
    // The budget is spent. An empty file we saw the whole way through is one
    // nobody is publishing, so it is ours to repair.
    last.ok_or_else(|| {
        StoreError(format!(
            "generated ignore {} never settled: it was still being replaced \
             after {IGNORE_SETTLE_ATTEMPTS} attempts",
            path.display()
        ))
    })
}

pub(crate) fn ensure_workspace_state_dir(workspace_root: &Path) -> Result<(PathBuf, bool)> {
    let dir = workspace_root.join(".stella");
    let created = match std::fs::symlink_metadata(&dir) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(StoreError(format!(
                    "workspace state path {} is not a real directory",
                    dir.display()
                )));
            }
            false
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(&dir)
                .map_err(|e| StoreError(format!("cannot create {}: {e}", dir.display())))?;
            true
        }
        Err(error) => {
            return Err(StoreError(format!(
                "cannot inspect workspace state directory {}: {error}",
                dir.display()
            )));
        }
    };
    Ok((dir, created))
}

fn validate_state_name(name: &str) -> Result<()> {
    let mut components = Path::new(name).components();
    if !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(StoreError(format!(
            "private state name must be one filename, got {name:?}"
        )));
    }
    Ok(())
}

fn path_entry_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(StoreError(format!(
            "cannot inspect {}: {error}",
            path.display()
        ))),
    }
}

#[cfg(unix)]
fn validate_safe_legacy(path: &Path, directory: bool) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = std::fs::symlink_metadata(path).map_err(|e| {
        StoreError(format!(
            "cannot inspect legacy state {}: {e}",
            path.display()
        ))
    })?;
    let expected_type = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file() && metadata.nlink() == 1
    };
    // A regular file inside an owner-only directory is unreachable by other
    // users even if its historical mode is permissive; migration immediately
    // repairs it to 0600 in the new private directory. The parent itself must
    // be owner-only before any legacy path is trusted.
    let owner_only = metadata.uid() == unsafe { libc::geteuid() }
        && (!directory || metadata.permissions().mode() & 0o077 == 0);
    if metadata.file_type().is_symlink() || !expected_type || !owner_only {
        return Err(StoreError(format!(
            "legacy private state {} is not owner-only; left untouched. Restrict its parent to \
             0700 and file to 0600, then retry, or move it aside to create fresh private state",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_safe_legacy(path: &Path, _directory: bool) -> Result<()> {
    Err(StoreError(format!(
        "legacy private state migration is unsupported on this platform; left untouched: {}",
        path.display()
    )))
}

fn migrate_legacy_files(dot: &Path, private: &Path, names: &[String]) -> Result<()> {
    let mut existing: Vec<&String> = Vec::new();
    for name in names {
        if path_entry_exists(&dot.join(name))? {
            existing.push(name);
        }
    }
    if existing.is_empty() {
        return Ok(());
    }
    validate_safe_legacy(dot, true)?;
    for name in &existing {
        let legacy = dot.join(name.as_str());
        validate_safe_legacy(&legacy, false)?;
        if path_entry_exists(&private.join(name.as_str()))? {
            return Err(StoreError(format!(
                "both legacy {} and private {} exist; refusing to choose or overwrite either",
                legacy.display(),
                private.join(name.as_str()).display()
            )));
        }
    }
    ensure_private_dir(private)?;
    for name in existing {
        let legacy = dot.join(name.as_str());
        let target = private.join(name.as_str());
        std::fs::rename(&legacy, &target).map_err(|e| {
            StoreError(format!(
                "cannot migrate legacy private state {} to {}: {e}",
                legacy.display(),
                target.display()
            ))
        })?;
    }
    sync_directory(private)?;
    sync_directory(dot)?;
    Ok(())
}

/// Resolve a workspace-private artifact beneath the owner-only state child,
/// migrating an owner-safe legacy file from the mixed `.stella` directory.
pub fn workspace_private_state_path(workspace_root: &Path, name: &str) -> Result<PathBuf> {
    validate_state_name(name)?;
    let (dot, _) = ensure_workspace_state_dir(workspace_root)?;
    let private = dot.join(WORKSPACE_PRIVATE_DIR);
    ensure_workspace_generated_ignore(&dot)?;
    migrate_legacy_files(&dot, &private, &[name.to_string()])?;
    ensure_private_dir(&private)?;
    Ok(private.join(name))
}

/// Append one line to a workspace-private log through the same no-follow,
/// owner-only file primitive used by session state.
pub fn append_workspace_private_line(
    workspace_root: &Path,
    name: &str,
    line: &str,
) -> Result<PathBuf> {
    use std::io::Write as _;

    let path = workspace_private_state_path(workspace_root, name)?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    let mut file = open_private_file(&path, options)?;
    writeln!(file, "{line}")
        .map_err(|e| StoreError(format!("cannot append {}: {e}", path.display())))?;
    Ok(path)
}

/// Resolve a workspace-private SQLite family. A closed, single-file legacy DB
/// can migrate atomically. Live WAL/SHM families fail closed and stay put.
pub fn workspace_private_sqlite_path(workspace_root: &Path, name: &str) -> Result<PathBuf> {
    validate_state_name(name)?;
    let (dot, _) = ensure_workspace_state_dir(workspace_root)?;
    let private = dot.join(WORKSPACE_PRIVATE_DIR);
    ensure_workspace_generated_ignore(&dot)?;
    let sidecars = [format!("{name}-wal"), format!("{name}-shm")];
    let mut existing_sidecars = Vec::new();
    for sidecar in &sidecars {
        if path_entry_exists(&dot.join(sidecar))? {
            existing_sidecars.push(sidecar);
        }
    }
    if !existing_sidecars.is_empty() {
        validate_safe_legacy(&dot, true)?;
        for sidecar in &existing_sidecars {
            validate_safe_legacy(&dot.join(sidecar.as_str()), false)?;
        }
        if path_entry_exists(&dot.join(name))? {
            validate_safe_legacy(&dot.join(name), false)?;
        }
        return Err(StoreError(format!(
            "legacy SQLite state for {} has active WAL/SHM sidecars and was left untouched; \
             close and checkpoint the database, then retry migration into {}",
            dot.join(name).display(),
            private.display()
        )));
    }
    migrate_legacy_files(&dot, &private, &[name.to_string()])?;
    ensure_private_dir(&private)?;
    prepare_private_sqlite_path(&private.join(name))
}

/// Locate an existing workspace-private artifact without creating an empty
/// file. Owner-safe legacy files are migrated; unsafe legacy files error.
pub fn existing_workspace_private_state_path(
    workspace_root: &Path,
    name: &str,
) -> Result<Option<PathBuf>> {
    validate_state_name(name)?;
    let dot = workspace_root.join(".stella");
    if !path_entry_exists(&dot)? {
        return Ok(None);
    }
    ensure_workspace_state_dir(workspace_root)?;
    let private = dot.join(WORKSPACE_PRIVATE_DIR);
    let target = private.join(name);
    if path_entry_exists(&target)? {
        ensure_private_dir(&private)?;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        drop(open_private_file(&target, options)?);
        return Ok(Some(target));
    }
    let legacy = dot.join(name);
    if !path_entry_exists(&legacy)? {
        return Ok(None);
    }
    workspace_private_state_path(workspace_root, name).map(Some)
}

/// Locate an existing workspace-private SQLite database without creating an
/// empty one. A safe closed legacy database migrates atomically; a legacy
/// database with live sidecars fails closed.
pub fn existing_workspace_private_sqlite_path(
    workspace_root: &Path,
    name: &str,
) -> Result<Option<PathBuf>> {
    validate_state_name(name)?;
    let dot = workspace_root.join(".stella");
    if !path_entry_exists(&dot)? {
        return Ok(None);
    }
    ensure_workspace_state_dir(workspace_root)?;
    let target = dot.join(WORKSPACE_PRIVATE_DIR).join(name);
    let legacy_family_exists = path_entry_exists(&dot.join(name))?
        || path_entry_exists(&dot.join(format!("{name}-wal")))?
        || path_entry_exists(&dot.join(format!("{name}-shm")))?;
    if !path_entry_exists(&target)? && !legacy_family_exists {
        return Ok(None);
    }
    workspace_private_sqlite_path(workspace_root, name).map(Some)
}

/// Create or validate a directory that contains only private local state.
pub(crate) fn ensure_private_dir(dir: &Path) -> Result<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(StoreError(format!(
                    "private state directory {} is not a real directory",
                    dir.display()
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(dir)
                .map_err(|e| StoreError(format!("cannot create {}: {e}", dir.display())))?;
        }
        Err(error) => {
            return Err(StoreError(format!(
                "cannot inspect private state directory {}: {error}",
                dir.display()
            )));
        }
    }
    let metadata = std::fs::symlink_metadata(dir)
        .map_err(|e| StoreError(format!("cannot inspect {}: {e}", dir.display())))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StoreError(format!(
            "private state directory {} changed while opening",
            dir.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
            StoreError(format!(
                "cannot restrict private directory {}: {e}",
                dir.display()
            ))
        })?;
    }
    Ok(())
}

/// Open a private regular file without following a terminal symlink.
#[cfg(unix)]
pub(crate) fn open_private_file(
    path: &Path,
    mut options: std::fs::OpenOptions,
) -> Result<std::fs::File> {
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(path)
        .map_err(|e| StoreError(format!("cannot open private file {}: {e}", path.display())))?;
    let metadata = file.metadata().map_err(|e| {
        StoreError(format!(
            "cannot inspect private file {}: {e}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(StoreError(format!(
            "private state path {} is not a regular file",
            path.display()
        )));
    }
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.nlink() != 1 {
            return Err(StoreError(format!(
                "private state file {} is not an owner-controlled single-link file (links: {}); \
                 refusing ambiguous ownership",
                path.display(),
                metadata.nlink()
            )));
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| {
                StoreError(format!(
                    "cannot restrict private file {}: {e}",
                    path.display()
                ))
            })?;
    }
    Ok(file)
}

/// Open a private regular file where the Unix no-follow/mode-at-create
/// primitives do not exist.
///
/// The regular-file check still runs — that one needs no ownership model —
/// but there is no `O_NOFOLLOW`, no uid comparison, no link count, and no
/// 0600 to enforce; the file inherits the parent directory's ACL. Failing
/// closed here (this arm's previous behaviour) meant a non-unix build could
/// not create `.stella/private/`, its SQLite stores, the session registry or
/// the reflections log — it could not run (#617). The hardening is a unix
/// bonus, not the price of admission.
#[cfg(not(unix))]
pub(crate) fn open_private_file(
    path: &Path,
    options: std::fs::OpenOptions,
) -> Result<std::fs::File> {
    let file = options
        .open(path)
        .map_err(|e| StoreError(format!("cannot open private file {}: {e}", path.display())))?;
    let metadata = file.metadata().map_err(|e| {
        StoreError(format!(
            "cannot inspect private file {}: {e}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(StoreError(format!(
            "private state path {} is not a regular file",
            path.display()
        )));
    }
    Ok(file)
}

pub(crate) fn read_private_to_string(path: &Path) -> Result<String> {
    use std::io::Read as _;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    let mut file = open_private_file(path, options)?;
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|e| StoreError(format!("cannot read {}: {e}", path.display())))?;
    Ok(text)
}

#[cfg(unix)]
fn validate_owner_controlled_parent(path: &Path) -> Result<&Path> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let parent = path
        .parent()
        .ok_or_else(|| StoreError(format!("private file {} has no parent", path.display())))?;
    let metadata = std::fs::symlink_metadata(parent)
        .map_err(|e| StoreError(format!("cannot inspect {}: {e}", parent.display())))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o022 != 0
    {
        return Err(StoreError(format!(
            "sensitive file parent {} must be a real owner-controlled directory with no \
             group/other write permission",
            parent.display()
        )));
    }
    Ok(parent)
}

/// Without POSIX ownership and mode bits there is no owner-controlled
/// assertion to make, so this checks the one thing that is checkable — the
/// parent is a real directory, not a symlink pointing somewhere else — and
/// lets the write proceed. The platform's own ACLs, inherited from the parent
/// the user chose, are the access control here.
///
/// The alternative, which this arm used to do, was to refuse every sensitive
/// write on non-unix: no credentials file, no MCP OAuth tokens, no user
/// settings. Refusing to store state is not a stronger security posture than
/// storing it under the OS's own permissions — it just moves the secret
/// somewhere Stella cannot protect at all (#617).
#[cfg(not(unix))]
fn validate_owner_controlled_parent(path: &Path) -> Result<&Path> {
    let parent = path
        .parent()
        .ok_or_else(|| StoreError(format!("private file {} has no parent", path.display())))?;
    let metadata = std::fs::symlink_metadata(parent)
        .map_err(|e| StoreError(format!("cannot inspect {}: {e}", parent.display())))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StoreError(format!(
            "sensitive file parent {} must be a real directory",
            parent.display()
        )));
    }
    Ok(parent)
}

/// Atomically replace a sensitive file in an existing owner-controlled
/// directory without following its terminal path.
pub fn write_sensitive_file_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    validate_owner_controlled_parent(path)?;
    write_private_atomic(path, bytes)
}

/// Read a sensitive regular file without following its terminal path.
pub fn read_sensitive_file_to_string(path: &Path) -> Result<String> {
    validate_owner_controlled_parent(path)?;
    read_private_to_string(path)
}

/// Atomically write an owner-only session registry or snapshot file.
///
/// The target's own type is checked here rather than in
/// [`crate::durable::write_atomic`]: replacing a symlink or a directory that
/// turned up where private state belongs is a *security* refusal with its own
/// wording, not a durability failure. There is no longer a "skip the fsync"
/// variant — every caller passed `sync = true`, and the contract (#617) is
/// that a durable write is fsynced.
pub(crate) fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(StoreError(format!(
            "private state target {} is not a regular file",
            path.display()
        )));
    }
    write_atomic(path, bytes, MODE_PRIVATE)
}

/// Pre-create or repair a SQLite main database as an owner-only regular file
/// and return its canonical-parent path.
pub(crate) fn prepare_private_sqlite_path(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| StoreError(format!("database path {} has no parent", path.display())))?
        .canonicalize()
        .map_err(|e| StoreError(format!("cannot canonicalize {}: {e}", path.display())))?;
    let name = path
        .file_name()
        .ok_or_else(|| StoreError(format!("database path {} has no filename", path.display())))?;
    let path = parent.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    drop(open_private_file(&path, options)?);
    Ok(path)
}

pub(crate) fn open_private_sqlite(path: &Path) -> Result<Connection> {
    let path = prepare_private_sqlite_path(path)?;
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
        | rusqlite::OpenFlags::SQLITE_OPEN_URI
        | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW;
    Connection::open_with_flags(path, flags).map_err(Into::into)
}

/// Open an EXISTING private SQLite database for reading only, creating nothing
/// and writing nothing.
///
/// The diagnostic counterpart to [`open_private_sqlite`], for
/// [`crate::integrity`]: a read-write open lets SQLite recover and checkpoint a
/// leftover `-wal` on the spot, which is right for a session and wrong for a
/// check — a corrupt file the user may still want to salvage must not be
/// rewritten by the command that merely inspects it.
///
/// `immutable=1` rather than a plain `SQLITE_OPEN_READ_ONLY`, because the store
/// is a WAL database and a read-only connection to one needs to create the
/// `-shm` file it is not allowed to create (SQLite: "it is not possible to open
/// read-only WAL databases"), so a plain read-only open fails with
/// `SQLITE_CANTOPEN` on a perfectly healthy store. Immutable skips locking and
/// shared memory entirely and reads the main database file as it stands — no
/// byte of the user's state can move. The trade-off is that a `-wal`'s
/// uncheckpointed pages are invisible, which [`crate::integrity::check_file`]
/// handles by escalating a bad verdict (never a good one) to a session-shaped
/// open when a `-wal` exists.
///
/// The same no-follow, owner-and-single-link, regular-file validation as the
/// read-write path runs first, through [`open_private_file`]. It deliberately
/// does not reuse [`prepare_private_sqlite_path`]: that opens `create(true)` and
/// would materialize an empty database instead of reporting an absent one.
///
/// `SQLITE_OPEN_NOFOLLOW` is absent — unlike [`open_private_sqlite`], which
/// keeps it — because SQLite refuses the combination: a read-only open carrying
/// that flag fails with `SQLITE_CANTOPEN` for every path, symlink or not. The
/// symlink guard is therefore [`open_private_file`]'s `O_NOFOLLOW` on the line
/// above, which validates the same final component microseconds earlier; the
/// residual window is only exploitable by someone who can already write inside
/// the 0700 `private/` directory, i.e. the owner.
pub(crate) fn open_private_sqlite_read_only(path: &Path) -> Result<Connection> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    drop(open_private_file(path, options)?);
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
        | rusqlite::OpenFlags::SQLITE_OPEN_URI;
    Connection::open_with_flags(immutable_uri(path)?, flags).map_err(Into::into)
}

/// `file:`-URI form of `path` with `immutable=1`.
///
/// Only `%`, `?` and `#` are escaped — the three characters SQLite's URI parser
/// gives meaning to (percent-decoding, query, fragment). Everything else,
/// spaces and non-ASCII included, is passed through as SQLite expects.
fn immutable_uri(path: &Path) -> Result<String> {
    let absolute = std::path::absolute(path)
        .map_err(|e| StoreError(format!("cannot absolutize {}: {e}", path.display())))?;
    let text = absolute.to_str().ok_or_else(|| {
        StoreError(format!(
            "cannot read {} through a SQLite URI: the path is not valid UTF-8",
            absolute.display()
        ))
    })?;
    let mut uri = String::with_capacity(text.len() + 20);
    uri.push_str("file:");
    for character in text.chars() {
        match character {
            '%' => uri.push_str("%25"),
            '?' => uri.push_str("%3F"),
            '#' => uri.push_str("%23"),
            other => uri.push(other),
        }
    }
    uri.push_str("?immutable=1");
    Ok(uri)
}

#[cfg(test)]
mod tests;
