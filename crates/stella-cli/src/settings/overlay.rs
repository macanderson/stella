//! `--config-overlay <FILE>` (`STELLA_CONFIG_OVERLAY`): one more `stella.toml`
//! for this process only.
//!
//! A tool that starts Stella for a person can give one launch its own hooks,
//! endpoint, key and MCP servers. It does not touch that person's files.
//! Stella reads the file once, at start. Each load lays it over the user,
//! managed and project files. Nothing writes it back.
//!
//! Hooks and MCP servers add to the person's own. An overlay server takes
//! the place of one with the same name. Every other field takes the
//! overlay's value. Stella trusts the file like `~/.stella/stella.toml`,
//! since the one who started Stella named it. A repo's file can't drop an
//! overlay hook. The org's limits still cap all the overlay asks for.
//!
//! A file that is missing, can't be read or does not parse stops the launch.
//! The error names the file. Going on without it would run the session as
//! if no overlay had been given.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use stella_mcp::McpConfig;

use super::Settings;
use super::toml_config::{self, ConfigScope};

/// The flag's name, for messages and for the argv of a resumed run.
pub(crate) const FLAG: &str = "--config-overlay";

/// One overlay file, read and parsed.
#[derive(Debug)]
pub(crate) struct Overlay {
    /// The file's full path, from the folder Stella started in. A run you
    /// resume from some other folder still reads the same file.
    pub(crate) path: PathBuf,
    /// What the file sets, in the shape the scope merge reads.
    pub(crate) settings: Settings,
    /// The servers under `[mcp.servers]`.
    pub(crate) mcp: McpConfig,
}

/// Set once by [`install`]. Tests never set it, so a test that loads
/// settings sees no overlay. Tests call [`read`] and pass the result in.
static OVERLAY: OnceLock<Overlay> = OnceLock::new();

/// Read the overlay at `path` before any settings load, and keep it until
/// Stella exits. `None` sets nothing. A later call does nothing, so the
/// overlay can't change once a session starts.
///
/// `STELLA_NO_SETTINGS=1` says no config file is read, so it refuses the
/// flag. To honor one would break the other with no word.
pub(crate) fn install(path: Option<&Path>) -> Result<(), String> {
    let Some(path) = path else {
        return Ok(());
    };
    if super::filesystem_settings_disabled() {
        return Err(format!(
            "{FLAG} {} cannot be used with STELLA_NO_SETTINGS=1, which reads no config \
             file at all. Drop one of the two.",
            path.display()
        ));
    }
    let overlay = read(path)?;
    for line in super::unknown::notices(
        &overlay.path.display().to_string(),
        super::unknown::unknown_toml_keys_in(&overlay.path),
    ) {
        eprintln!("{line}");
    }
    let _ = OVERLAY.set(overlay);
    Ok(())
}

/// The overlay this process started with.
pub(crate) fn active() -> Option<&'static Overlay> {
    OVERLAY.get()
}

/// The overlay's settings, for the scope merge.
pub(crate) fn active_settings() -> Option<&'static Settings> {
    active().map(|overlay| &overlay.settings)
}

/// The overlay's MCP servers, for the server list a session starts.
pub(crate) fn active_mcp() -> Option<&'static McpConfig> {
    active().map(|overlay| &overlay.mcp)
}

/// Read and parse one overlay file. Each error names the file.
///
/// The read refuses a symlink as the file. It refuses a parent folder that
/// some other user can write to. The file can hold a key and hooks that run
/// commands. So it gets the same check as `~/.stella/mcp.toml` and the
/// credentials file.
pub(crate) fn read(path: &Path) -> Result<Overlay, String> {
    let path = std::path::absolute(path)
        .map_err(|error| format!("cannot read {FLAG} {}: {error}", path.display()))?;
    let text = stella_store::read_sensitive_file_to_string(&path)
        .map_err(|error| format!("cannot read {FLAG} {}: {error}", path.display()))?;
    let loaded = toml_config::lower_toml_scope(&text, &path, ConfigScope::Overlay)
        .map_err(|error| format!("the {FLAG} file is not valid: {error}"))?;
    for warning in &loaded.warnings {
        eprintln!("{warning}");
    }
    Ok(Overlay {
        path,
        settings: loaded.settings,
        mcp: loaded.mcp,
    })
}

#[cfg(test)]
mod tests;
