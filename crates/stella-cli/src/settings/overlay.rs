//! `--config-overlay <FILE>` (`STELLA_CONFIG_OVERLAY`): one more `stella.toml`
//! for this process only.
//!
//! A launcher that starts Stella for someone can hand one launch its hooks,
//! its provider endpoint and key, and its MCP servers without editing that
//! person's files. The file is read once, at startup, and laid over the user,
//! managed and project scopes on every load. Nothing writes it back to disk.
//!
//! Hooks and MCP servers add to the person's own, and an overlay server
//! replaces one of the same name. Every other field takes the overlay's value.
//! The overlay is trusted like `~/.stella/stella.toml`, because whoever started
//! the process named it. A repository's file cannot remove an overlay hook,
//! and the org-managed ceilings still cap everything the overlay asks for.
//!
//! A file that is missing, unreadable or invalid stops the launch with an
//! error that names it. Going on without it would run a session the launcher
//! meant to configure as if no overlay had been given.

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
    /// The file, made absolute against the directory Stella started in, so a
    /// resumed run started from another directory reads the same file.
    pub(crate) path: PathBuf,
    /// What the file sets, in the shape the scope merge reads.
    pub(crate) settings: Settings,
    /// The servers under `[mcp.servers]`.
    pub(crate) mcp: McpConfig,
}

/// Set once by [`install`]. Tests never set it, so every test that loads
/// settings sees no overlay; they call [`read`] and pass the result in.
static OVERLAY: OnceLock<Overlay> = OnceLock::new();

/// Read the overlay `path` names before anything loads settings, and keep it
/// for the rest of the process. `None` installs nothing. A later call is
/// ignored, so nothing after startup can change the overlay a session runs
/// with.
///
/// Refused under `STELLA_NO_SETTINGS=1`, which promises that no config file is
/// read. Honoring one of the two asks would silently break the other.
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

/// Read and parse one overlay file. Every error names the file.
///
/// The read refuses a symlink at the end of the path and a parent directory
/// that another user can write to. The file can carry a provider key and
/// hooks that run commands, so it gets the same check as `~/.stella/mcp.toml`
/// and the credentials file.
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
