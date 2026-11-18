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
//! overlay's value. That holds for `[self_driving]` and `[issues]` too, key
//! by key. Stella trusts the file like `~/.stella/stella.toml`, since the
//! one who started Stella named it. A repo's file can't drop an overlay
//! hook. The org's limits still cap all the overlay asks for.
//!
//! A file that is missing, can't be read or does not parse stops the launch.
//! The error names the file. Going on without it would run the session as
//! if no overlay had been given.
//!
//! A run that starts another Stella, such as a resumed turn or a
//! self-driving turn, hands it the path and the SHA-256 of the bytes it read.
//! The child refuses a file that has changed since. So every turn of one run
//! uses the same overlay.
//!
//! Stella reads an overlay only on Unix. Elsewhere it can't check who may
//! write the file, so it refuses the flag.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};
use stella_mcp::McpConfig;

use super::Settings;
use super::toml_config::{self, ConfigScope};

/// The flag's name, for messages and for the argv of a resumed run.
pub(crate) const FLAG: &str = "--config-overlay";

/// The hidden flag that hands [`Overlay::sha256`] to a child run.
pub(crate) const SHA256_FLAG: &str = "--config-overlay-sha256";

/// One overlay file, read and parsed.
#[derive(Debug)]
pub(crate) struct Overlay {
    /// The file's full path, from the folder Stella started in. A run you
    /// resume from some other folder still reads the same file.
    pub(crate) path: PathBuf,
    /// The SHA-256 of the bytes Stella read, in lowercase hex.
    pub(crate) sha256: String,
    /// What the file sets, in the shape the scope merge reads.
    pub(crate) settings: Settings,
    /// The servers under `[mcp.servers]`.
    pub(crate) mcp: McpConfig,
    /// The file's `[self_driving]` and `[issues]` tables, as written.
    pub(crate) loop_sections: LoopSections,
}

impl Overlay {
    /// The path and hash a child run is handed.
    pub(crate) fn pin(&self) -> Pin {
        Pin {
            path: self.path.clone(),
            sha256: self.sha256.clone(),
        }
    }
}

/// What a child run needs to read the overlay its parent read, and nothing
/// else. The file's text stays in the file. It can hold a provider key, and
/// a copy on disk would outlive the launcher's cleanup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pin {
    /// The overlay's full path.
    pub(crate) path: PathBuf,
    /// The SHA-256 of the bytes the parent read, in lowercase hex.
    pub(crate) sha256: String,
}

impl Pin {
    /// The flags that hand this overlay to a child `stella`, in argv order.
    pub(crate) fn child_args(&self) -> [OsString; 4] {
        [
            FLAG.into(),
            self.path.clone().into_os_string(),
            SHA256_FLAG.into(),
            self.sha256.clone().into(),
        ]
    }
}

/// The overlay's `[self_driving]` and `[issues]` tables, as the file wrote
/// them.
///
/// The settings merge does not read these two. The self-driving loader and
/// the issue tracker's manifest do, through [`lay_section`]. They stay as
/// written, so a key the file left out reads apart from one it set to the
/// default.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LoopSections {
    /// `[self_driving]`, when the file has it.
    pub(crate) self_driving: Option<toml::Table>,
    /// `[issues]`, when the file has it.
    pub(crate) issues: Option<toml::Table>,
}

impl LoopSections {
    /// The two tables in one `stella.toml` text. Text that is not TOML gives
    /// neither.
    pub(crate) fn of(text: &str) -> Self {
        Self {
            self_driving: table_in(text, "self_driving"),
            issues: table_in(text, "issues"),
        }
    }
}

/// The table `text` holds under `key`, when it parses and has one.
fn table_in(text: &str, key: &str) -> Option<toml::Table> {
    let mut document = toml::from_str::<toml::Table>(text).ok()?;
    match document.remove(key)? {
        toml::Value::Table(table) => Some(table),
        _ => None,
    }
}

/// Lay the overlay's table for section `name` over the workspace's section.
///
/// `typed` is the section as the workspace's loader read it. `written` is
/// the text of that workspace file. It is `None` when there was no file or
/// it did not parse. Each key the overlay sets takes the overlay's value.
/// Where both files hold a table under one key, the two merge the same way.
/// A list is one value, so the overlay's list replaces the workspace's.
/// With no overlay table, `typed` comes back as it was.
pub(crate) fn lay_section<T: DeserializeOwned>(
    name: &str,
    typed: T,
    written: Option<&str>,
    overlay: Option<&toml::Table>,
) -> T {
    let Some(overlay) = overlay else {
        return typed;
    };
    let mut merged = written
        .and_then(|text| table_in(text, name))
        .unwrap_or_default();
    lay_table(&mut merged, overlay);
    match merged.try_into() {
        Ok(section) => section,
        Err(error) => {
            // Two fields that each parse alone can still clash, such as two
            // that the section allows only one of. The overlay wins a clash.
            eprintln!(
                "warning: the {FLAG} file's [{name}] does not combine with the workspace's \
                 ({error}). Stella uses the overlay's [{name}] alone."
            );
            overlay.clone().try_into().unwrap_or(typed)
        }
    }
}

/// Lay `overlay` over `base`, key by key. A key the overlay sets takes its
/// value. Where both hold a table, the two merge the same way.
pub(crate) fn lay_table(base: &mut toml::Table, overlay: &toml::Table) {
    for (key, value) in overlay {
        if let toml::Value::Table(over) = value
            && let Some(toml::Value::Table(inner)) = base.get_mut(key)
        {
            lay_table(inner, over);
            continue;
        }
        base.insert(key.clone(), value.clone());
    }
}

/// Set once by [`install`]. Tests never set it, so a test that loads
/// settings sees no overlay. Tests call [`read`] and pass the result in.
static OVERLAY: OnceLock<Overlay> = OnceLock::new();

/// Read the overlay at `path` before any settings load, and keep it until
/// Stella exits. `None` sets nothing. A later call does nothing, so the
/// overlay can't change once a session starts.
///
/// `sha256` comes from [`SHA256_FLAG`], which a parent run passes to each
/// child. The read then refuses a file whose bytes hash to anything else.
///
/// `STELLA_NO_SETTINGS=1` says no config file is read, so it refuses the
/// flag. To honor one would break the other with no word.
pub(crate) fn install(path: Option<&Path>, sha256: Option<&str>) -> Result<(), String> {
    let Some(path) = path else {
        return match sha256 {
            None => Ok(()),
            Some(_) => Err(format!(
                "{SHA256_FLAG} needs {FLAG}, which names the file to check."
            )),
        };
    };
    if super::filesystem_settings_disabled() {
        return Err(format!(
            "{FLAG} {} cannot be used with STELLA_NO_SETTINGS=1, which reads no config \
             file at all. Drop one of the two.",
            path.display()
        ));
    }
    let overlay = read(path, sha256)?;
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

/// The overlay's `[self_driving]` and `[issues]`, for the loaders that read
/// those two sections.
pub(crate) fn active_loop_sections() -> Option<&'static LoopSections> {
    active().map(|overlay| &overlay.loop_sections)
}

/// Read and parse one overlay file. Each error names the file.
///
/// The read refuses a symlink as the file. It refuses a parent folder that
/// some other user can write to. The file can hold a key and hooks that run
/// commands. So it gets the same check as `~/.stella/mcp.toml` and the
/// credentials file.
///
/// With `sha256` given, the read also refuses a file whose bytes hash to
/// anything else. A child run gets the hash its parent read. The launcher
/// may rewrite the file between two turns, or delete it. Each turn then
/// runs with the overlay the run began with, or stops.
pub(crate) fn read(path: &Path, sha256: Option<&str>) -> Result<Overlay, String> {
    let path = std::path::absolute(path)
        .map_err(|error| format!("cannot read {FLAG} {}: {error}", path.display()))?;
    let text = read_text(&path)?;
    let digest = sha256_hex(text.as_bytes());
    if sha256.is_some_and(|expected| !expected.eq_ignore_ascii_case(&digest)) {
        return Err(format!(
            "the {FLAG} file {} changed after the run that started this one read it. \
             Stella stops so this turn can't run with other settings. Start a new run to \
             use the file as it is now.",
            path.display()
        ));
    }
    let loaded = toml_config::lower_toml_scope(&text, &path, ConfigScope::Overlay)
        .map_err(|error| format!("the {FLAG} file is not valid: {error}"))?;
    for warning in &loaded.warnings {
        eprintln!("{warning}");
    }
    // The unknown-key scan reads the text the hash and the parse read.
    // Opening the path again could see a file swapped in after this read.
    let unknown_keys = super::unknown::unknown_toml_keys_in_text(&text);
    for line in super::unknown::notices(&path.display().to_string(), unknown_keys) {
        eprintln!("{line}");
    }
    Ok(Overlay {
        loop_sections: LoopSections::of(&text),
        path,
        sha256: digest,
        settings: loaded.settings,
        mcp: loaded.mcp,
    })
}

/// The file's text, read with the owner and symlink checks.
#[cfg(unix)]
fn read_text(path: &Path) -> Result<String, String> {
    stella_store::read_sensitive_file_to_string(path)
        .map_err(|error| format!("cannot read {FLAG} {}: {error}", path.display()))
}

/// Off Unix, Stella can't tell who may write the file or its folder. Another
/// user who can swap the file could add hooks or MCP commands that run in
/// this process. So the read stops here.
#[cfg(not(unix))]
fn read_text(path: &Path) -> Result<String, String> {
    Err(format!(
        "cannot read {FLAG} {}: on this system Stella cannot check who may write to the \
         file or its folder, so it does not read overlays here. Use {FLAG} on macOS or \
         Linux.",
        path.display()
    ))
}

/// The SHA-256 of `bytes`, in lowercase hex.
fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[cfg(test)]
mod tests;
