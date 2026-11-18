//! The two files that hold your own MCP servers.
//!
//! `$STELLA_HOME/mcp.toml` holds the servers you want in each repo. It is
//! `~/.stella/mcp.toml` by default. `<workspace>/.stella/mcp.toml` holds the
//! servers of one repo. Stella reads the user file first. Then it lays the
//! repo file on top. So when both files name a server, the repo entry runs.
//!
//! Each other Stella setting works this way too. The repo file is closer to
//! the work. It knows the path a server should open, or the account it
//! should use.
//!
//! Only you write the user file, so it is trusted, like
//! `~/.stella/stella.toml`. The repo file came with a clone. It loads only
//! when you trust the repo (`STELLA_TRUST_PROJECT`). In a repo you have not
//! trusted, your servers start in `$STELLA_HOME`, not in the repo. A runner
//! such as `npx` resolves packages from the directory it starts in, so a
//! repo could otherwise put its own code behind a command from your file.
//! In such a repo, a `--config-overlay` server starts in the folder that
//! holds the overlay file, for the same reason.
//!
//! Both files are read the way every file that can hold a credential is
//! read: no symlink at the end of the path, and a parent directory that only
//! you can write to.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use stella_mcp::{McpConfig, McpServerConfig, McpTransport};

use crate::settings::overlay::Overlay;

/// The servers from both files, merged by name. It also holds a note for
/// each file that was skipped.
#[derive(Debug, Default)]
pub(crate) struct OwnMcpConfig {
    /// The user file's servers, with the repo file's on top, and the
    /// overlay's on top of both.
    pub(crate) config: McpConfig,
    /// One line for each file that is there but was not read. Each line
    /// names its file.
    pub(crate) notices: Vec<String>,
    /// The directory the stdio servers start in. It is `$STELLA_HOME` when
    /// the repo is not trusted, since every server then came from your own
    /// file. It is `None` in a trusted repo, so the servers start where
    /// Stella runs.
    start_dir: Option<PathBuf>,
    /// The servers that start in a directory of their own, by name. In a
    /// repo you have not trusted, each overlay server is here with the
    /// folder that holds the overlay file.
    start_dir_of: BTreeMap<String, PathBuf>,
}

/// The user's MCP server file, `$STELLA_HOME/mcp.toml`.
///
/// It is `None` when there is no stella home. It is also `None` in a unit
/// test that set no home of its own. That way a test never reads the file on
/// your own machine.
pub(crate) fn user_mcp_toml_path() -> Option<PathBuf> {
    crate::paths::user_extension_root().map(|root| root.join("mcp.toml"))
}

/// Read the user file, then the repo file, and merge them by server name.
/// When a name is in both, the repo entry wins.
///
/// A file that is not there adds nothing and says nothing. A file that
/// can't be read, or is not valid, adds no servers. It adds one note that
/// names its path. The other file still loads, so a typo in one file does not
/// stop the servers in the other. When the repo is not trusted, its file is
/// not read at all.
///
/// A repo entry takes the place of the user entry as a whole. That includes
/// `granted` and `candidate_safe`. If the two were merged field by field, a
/// grant in your file could let a command the repo chose run.
///
/// The servers in this launch's `--config-overlay` file go on last
/// (`crate::settings::overlay`).
pub(crate) fn load_own_config(workspace_root: &Path, workspace_trusted: bool) -> OwnMcpConfig {
    load_own_config_with(
        workspace_root,
        workspace_trusted,
        crate::settings::overlay::active(),
    )
}

/// [`load_own_config`], with the overlay passed in so a test can supply it.
///
/// The overlay's servers are added to yours. One that has the name of a
/// server you already run takes its place for this process, the way a repo
/// entry takes the place of a user entry. The launcher that named the overlay
/// is trusted like your own file, so its servers start in a repo you have not
/// trusted too.
///
/// In that repo, each overlay server starts in the folder that holds the
/// overlay file. When Stella read the overlay, it checked that no other user
/// can write to that folder. So the repo never picks where a runner such as
/// `npx` looks for packages, even when Stella has no home and `start_dir` is
/// unset.
fn load_own_config_with(
    workspace_root: &Path,
    workspace_trusted: bool,
    overlay: Option<&Overlay>,
) -> OwnMcpConfig {
    let mut own = OwnMcpConfig::default();
    if let Some(path) = user_mcp_toml_path() {
        own.lay_over(&path);
    }
    let workspace = super::mcp_toml_path(workspace_root);
    if workspace_trusted {
        own.lay_over(&workspace);
    } else {
        own.start_dir = crate::paths::user_extension_root();
        if std::fs::symlink_metadata(&workspace).is_ok() {
            own.notices.push(format!(
                "{} was NOT loaded — set STELLA_TRUST_PROJECT=1 to let this repo start its \
                 MCP servers (they run commands / open connections on your machine)",
                workspace.display()
            ));
        }
    }
    if let Some(overlay) = overlay {
        own.add_overlay(overlay, workspace_trusted);
    }
    own
}

impl OwnMcpConfig {
    /// Read one file and put its servers on top of the ones held so far.
    fn lay_over(&mut self, path: &Path) {
        match read_mcp_file(path) {
            Ok(Some(config)) => self.config.servers.extend(config.servers),
            Ok(None) => {}
            Err(reason) => self
                .notices
                .push(format!("{reason} — its MCP servers are not started")),
        }
    }

    /// Put the overlay's servers on top of the ones held so far. In a repo
    /// you have not trusted, each one starts in the overlay file's folder.
    /// The overlay path is absolute, so it always has a parent folder.
    fn add_overlay(&mut self, overlay: &Overlay, workspace_trusted: bool) {
        let start_dir = overlay.path.parent().filter(|_| !workspace_trusted);
        for (name, entry) in &overlay.mcp.servers {
            self.config.servers.insert(name.clone(), entry.clone());
            if let Some(dir) = start_dir {
                self.start_dir_of.insert(name.clone(), dir.to_path_buf());
            }
        }
    }

    /// The servers to connect. A server named in `start_dir_of` starts in
    /// the directory listed there. Every other one starts in `start_dir`.
    pub(crate) fn servers(&self) -> Vec<McpServerConfig> {
        let mut servers = self.config.clone().into_servers();
        for server in &mut servers {
            server.cwd = self
                .start_dir_of
                .get(&server.name)
                .or(self.start_dir.as_ref())
                .cloned();
        }
        servers
    }
}

/// Every server the `stella mcp` commands and the deck's MCP tab can show:
/// the user file's, with the workspace file's on top.
///
/// Unlike [`load_own_config`], this does not ask the trust gate. It starts
/// nothing. It lists what each file says, so you can see and edit a server
/// in a repo you have not trusted. A file that can't be read is an error
/// here, as the workspace file always was for these commands.
pub(crate) fn load_listed_config(workspace_root: &Path) -> Result<McpConfig, String> {
    let mut listed = load_user_config()?;
    listed
        .servers
        .extend(super::load_config(workspace_root)?.servers);
    Ok(listed)
}

/// The servers an OAuth login can reach: each server [`load_listed_config`]
/// lists, with `overlay`'s servers on top.
///
/// A session runs an overlay server in place of a listed one with the same
/// name. So a login reads the overlay too, and gets the URL the session
/// connects to. This only reads. The commands that change a server find its
/// file with [`owning_file`], which reads the two `mcp.toml` files alone, so
/// no command writes to the overlay.
pub(super) fn load_login_config(
    workspace_root: &Path,
    overlay: Option<&McpConfig>,
) -> Result<McpConfig, String> {
    let mut config = load_listed_config(workspace_root)?;
    if let Some(overlay) = overlay {
        config.servers.extend(overlay.servers.clone());
    }
    Ok(config)
}

/// The servers the deck's MCP tab shows, with what it needs to know about
/// the overlay's.
#[derive(Debug, Default)]
pub(crate) struct TabConfig {
    /// One entry per row. A name a `mcp.toml` holds shows that file's entry,
    /// even when the overlay replaces it for the session. A name only the
    /// overlay holds shows the overlay's entry.
    pub(crate) rows: McpConfig,
    /// The names only the overlay holds. Stella never writes the overlay, so
    /// the tab refuses each key that would change one of these on disk.
    pub(crate) overlay_only: BTreeSet<String>,
    /// The entry an OAuth login reaches for each name, as
    /// [`load_login_config`] reads it.
    login: McpConfig,
}

impl TabConfig {
    /// Whether an OAuth login applies to `name`. It does when the entry the
    /// session runs under that name is an http server. For a shared name that
    /// is the overlay's entry, not the row's.
    pub(crate) fn has_oauth(&self, name: &str) -> bool {
        matches!(self.login.get(name), Some(McpTransport::Http { .. }))
    }
}

/// The deck's MCP tab: [`load_tab_config_with`] with this launch's overlay.
pub(crate) fn load_tab_config(workspace_root: &Path) -> Result<TabConfig, String> {
    load_tab_config_with(workspace_root, crate::settings::overlay::active_mcp())
}

/// The rows of the deck's MCP tab: each server [`load_listed_config`] lists,
/// then each server only `overlay` names.
///
/// An overlay server can need an OAuth login, and the tab's `o` key is how
/// the deck starts one, so the server needs a row. Each read here is the
/// same read [`load_login_config`] makes.
pub(crate) fn load_tab_config_with(
    workspace_root: &Path,
    overlay: Option<&McpConfig>,
) -> Result<TabConfig, String> {
    let listed = load_listed_config(workspace_root)?;
    let mut tab = TabConfig {
        rows: listed.clone(),
        overlay_only: BTreeSet::new(),
        login: listed,
    };
    for (name, entry) in overlay.into_iter().flat_map(|config| &config.servers) {
        if !tab.rows.servers.contains_key(name) {
            tab.rows.servers.insert(name.clone(), entry.clone());
            tab.overlay_only.insert(name.clone());
        }
        tab.login.servers.insert(name.clone(), entry.clone());
    }
    Ok(tab)
}

/// The servers in `$STELLA_HOME/mcp.toml`. There are none when there is no
/// stella home or no file.
fn load_user_config() -> Result<McpConfig, String> {
    match user_mcp_toml_path() {
        Some(path) => Ok(read_mcp_file(&path)?.unwrap_or_default()),
        None => Ok(McpConfig::default()),
    }
}

/// The file that holds the server named `name`, so a change to that server
/// is written where the server comes from. When both files name it, the
/// workspace entry is the one used, so the workspace file holds it. It is
/// `None` when neither file names it.
pub(crate) fn owning_file(workspace_root: &Path, name: &str) -> Result<Option<PathBuf>, String> {
    if super::load_config(workspace_root)?.get(name).is_some() {
        return Ok(Some(super::mcp_toml_path(workspace_root)));
    }
    match user_mcp_toml_path() {
        Some(path) if load_user_config()?.get(name).is_some() => Ok(Some(path)),
        _ => Ok(None),
    }
}

/// Read one `mcp.toml`. It is `Ok(None)` when the file is not there. The
/// error names the path when the file can't be read, is a symlink, sits in a
/// directory another user can write to, or is not valid.
pub(super) fn read_mcp_file(path: &Path) -> Result<Option<McpConfig>, String> {
    let text = match stella_store::read_sensitive_file_to_string(path) {
        Ok(text) => text,
        // The parent check runs first, so a missing `~/.stella` fails it.
        // Ask the file itself whether it is there.
        Err(_)
            if matches!(
                std::fs::symlink_metadata(path),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    McpConfig::from_toml_str(&text)
        .map(Some)
        .map_err(|error| format!("{} is invalid: {error}", path.display()))
}

#[cfg(test)]
mod tests;
