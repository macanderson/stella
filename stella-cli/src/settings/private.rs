//! Secure persistence for user-scope settings, which may contain API keys.

use std::path::Path;

pub(super) fn reject_symlink(path: &Path) -> Result<(), String> {
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        return Err(format!(
            "refusing to save settings through symlink {}",
            path.display()
        ));
    }
    Ok(())
}

pub(super) fn write_settings(path: &Path, bytes: &[u8], user_private: bool) -> Result<(), String> {
    if user_private {
        return write_user_settings(path, bytes);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    write_atomic(path, bytes)
}

/// Replace `path` with `bytes` through a temp file and a rename, so a reader
/// sees either the whole old file or the whole new one and never a torn
/// prefix.
///
/// `std::fs::write` truncates in place: a crash, a full disk, or a killed
/// process partway through left project settings half-written, and settings
/// that no longer parse take every configured provider, tool toggle and
/// permission rule down with them until someone hand-repairs the file.
///
/// This is deliberately NOT [`write_user_settings`], which additionally
/// enforces 0600 on the file, 0700 on its parent, and `O_NOFOLLOW`, because it
/// holds API keys and is Unix-only for exactly that reason. Project settings
/// are shared, version-controlled, non-secret files: they need durability, not
/// confinement, and they must keep working on every platform — so this path
/// uses only portable std calls.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    // Same directory as the target: `rename` is only atomic within one
    // filesystem, and a temp dir may well be on another.
    let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp.{}.{}", std::process::id(), sequence));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        // Durability before visibility: without the fsync the rename can be
        // ordered ahead of the data, leaving a file that exists but is empty.
        file.sync_data()
            .map_err(|e| format!("cannot fsync {}: {e}", tmp.display()))?;
        drop(file);
        std::fs::rename(&tmp, path).map_err(|e| format!("cannot replace {}: {e}", path.display()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(unix)]
fn write_user_settings(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

    let parent = path
        .parent()
        .ok_or_else(|| format!("settings path {} has no parent", path.display()))?;
    match std::fs::symlink_metadata(parent) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(format!("{} is not a private directory", parent.display()));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
            builder
                .create(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        Err(error) => return Err(format!("cannot inspect {}: {error}", parent.display())),
    }
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot restrict {}: {e}", parent.display()))?;
    }
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(format!("{} is not a regular settings file", path.display()));
    }

    let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp.{}.{}", std::process::id(), sequence));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options
        .open(&tmp)
        .map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        file.sync_data()
            .map_err(|e| format!("cannot fsync {}: {e}", tmp.display()))?;
        drop(file);
        std::fs::rename(&tmp, path)
            .map_err(|e| format!("cannot replace {}: {e}", path.display()))?;
        std::fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| format!("cannot fsync directory {}: {e}", parent.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(not(unix))]
fn write_user_settings(path: &Path, _bytes: &[u8]) -> Result<(), String> {
    Err(format!(
        "secure user settings persistence is unsupported on this platform: {}",
        path.display()
    ))
}
