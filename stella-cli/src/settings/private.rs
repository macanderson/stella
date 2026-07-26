//! Secure persistence for settings files, which may contain API keys.
//!
//! Both scopes are durable writes under the one contract (#617): the bytes go
//! through [`stella_store::durable::write_atomic`] — temp + fsync + rename +
//! fsync of the parent directory — so a crash mid-save can never leave a
//! truncated `settings.json` where a valid one used to be. What differs
//! between the scopes is only the *mode* and how hard the destination is
//! checked, not whether the write is durable.

use std::path::Path;

use stella_store::durable::{
    MODE_PRIVATE, MODE_SHARED, write_atomic, write_atomic_preserving_mode,
};

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

/// Persist rendered settings. `user_private` selects the user scope, whose
/// file can hold credentials and is therefore owner-only in an owner-only
/// directory; the project scope is a committable `.stella/settings.json`.
pub(super) fn write_settings(path: &Path, bytes: &[u8], user_private: bool) -> Result<(), String> {
    if user_private {
        return write_user_settings(path, bytes);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    // Project scope was the last bare `std::fs::write` on a settings path: it
    // truncated the live file before it had the replacement bytes, so a crash
    // in that window left the project with an empty (invalid JSON) settings
    // file and no way to tell what it used to say.
    write_atomic_preserving_mode(path, bytes, MODE_SHARED).map_err(|e| e.to_string())
}

/// The user scope adds what the project scope cannot assume: the parent must
/// be an owner-only directory (created 0700 if absent) and the target must be
/// a regular file, because this one can hold an API key.
///
/// The mode/ownership hardening is unix-only, and on other platforms
/// [`write_atomic`] performs the same temp + fsync + rename without it rather
/// than refusing to save — which is what this function's `#[cfg(not(unix))]`
/// arm used to do, making user settings unwritable on a platform CI never
/// compiles (#617).
fn write_user_settings(path: &Path, bytes: &[u8]) -> Result<(), String> {
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
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        Err(error) => return Err(format!("cannot inspect {}: {error}", parent.display())),
    }
    #[cfg(unix)]
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
    write_atomic(path, bytes, MODE_PRIVATE).map_err(|e| e.to_string())
}
