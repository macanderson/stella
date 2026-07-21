//! Owner-only local-state filesystem primitives.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::{Result, StoreError};

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
        if metadata.nlink() != 1 {
            return Err(StoreError(format!(
                "private state file {} has {} hard links; refusing ambiguous ownership",
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

/// Platforms without the Unix no-follow/mode-at-create primitives fail closed
/// until an equivalent owner-only implementation exists.
#[cfg(not(unix))]
pub(crate) fn open_private_file(
    path: &Path,
    _options: std::fs::OpenOptions,
) -> Result<std::fs::File> {
    Err(StoreError(format!(
        "secure private file creation is unsupported on this platform: {}",
        path.display()
    )))
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

/// Atomically write an owner-only session registry or snapshot file.
pub(crate) fn write_private_atomic(path: &Path, bytes: &[u8], sync: bool) -> Result<()> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(StoreError(format!(
            "private state target {} is not a regular file",
            path.display()
        )));
    }
    let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp.{}.{}", std::process::id(), sequence));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = open_private_file(&tmp, options)?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|e| StoreError(format!("cannot write {}: {e}", tmp.display())))?;
        if sync {
            file.sync_data()
                .map_err(|e| StoreError(format!("cannot fsync {}: {e}", tmp.display())))?;
        }
        drop(file);
        std::fs::rename(&tmp, path)
            .map_err(|e| StoreError(format!("cannot replace {}: {e}", path.display())))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Pre-create or repair a SQLite main database as an owner-only regular file
/// and return its canonical-parent path.
pub fn prepare_private_sqlite_path(path: &Path) -> Result<PathBuf> {
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
