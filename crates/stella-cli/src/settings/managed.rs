//! Reads the managed settings file.

use std::path::{Path, PathBuf};

use super::Settings;

impl Settings {
    /// Get the managed telemetry field before dotenv files load.
    pub(crate) fn load_managed_telemetry_snapshot() -> Result<Option<serde_json::Value>, String> {
        let managed = Self::load_managed_scope(&managed_settings_path())?;
        Ok(managed.enterprise_telemetry)
    }

    /// The file [`Self::load`] reads for the managed scope.
    pub fn managed_path() -> PathBuf {
        resolve_managed_path()
    }

    /// Unknown keys in the org-managed settings file. See
    /// [`super::unknown::managed_advisory`].
    pub fn managed_advisory() -> Vec<String> {
        super::unknown::managed_advisory()
    }

    pub(super) fn load_managed_scope(path: &Path) -> Result<Self, String> {
        let contents = match read_managed_settings(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(format!(
                    "cannot securely read managed settings {}: {error}",
                    path.display()
                ));
            }
        };
        let scope: Settings = serde_json::from_str(&contents)
            .map_err(|error| format!("invalid settings file {}: {error}", path.display()))?;
        for (id, entry) in &scope.providers {
            if let Some(stated) = &entry.id
                && stated != id
            {
                return Err(format!(
                    "settings file {}: providers.{id} declares id `{stated}` — the entry's id must match its key",
                    path.display()
                ));
            }
        }
        Ok(scope)
    }
}

pub(super) fn managed_settings_path() -> PathBuf {
    if let Some(path) = std::env::var_os("STELLA_MANAGED_SETTINGS") {
        return PathBuf::from(path);
    }
    if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/stella/settings.json")
    } else {
        PathBuf::from("/etc/stella/settings.json")
    }
}

/// The file the loader reads for the managed scope.
///
/// `STELLA_MANAGED_SETTINGS` picks one file. Otherwise the default TOML path
/// wins if it exists; the JSON path is the fallback. This mirrors
/// `Settings::load_managed_scope_dual`'s own choice, named here in words
/// since that function is private to `merge.rs`.
///
/// A test keeps the two in step: it loads real settings and reads this
/// advisory on the same fixture, so a mismatch fails the test.
pub(super) fn resolve_managed_path() -> PathBuf {
    if std::env::var_os("STELLA_MANAGED_SETTINGS").is_some() {
        return managed_settings_path();
    }
    let toml_path = super::toml_config::managed_toml_path();
    if toml_path.exists() {
        return toml_path;
    }
    managed_settings_path()
}

#[cfg(unix)]
pub(super) fn read_managed_settings(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    let owner = metadata.uid();
    // SAFETY: `geteuid` takes no arguments, touches no pointers, and cannot
    // fail — it is unsafe only because it is an extern fn.
    let euid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.nlink() != 1
        || (owner != euid && owner != 0)
        || metadata.mode() & 0o022 != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "managed settings must be a single-link regular file owned by root or the process user and not group/other writable",
        ));
    }
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    Ok(contents)
}

#[cfg(not(unix))]
pub(super) fn read_managed_settings(_path: &Path) -> std::io::Result<String> {
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "secure managed settings are unsupported on this platform",
    ))
}
