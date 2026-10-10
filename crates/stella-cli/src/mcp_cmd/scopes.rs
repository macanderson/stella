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
//!
//! Both files are read the way every file that can hold a credential is
//! read: no symlink at the end of the path, and a parent directory that only
//! you can write to.

use std::path::{Path, PathBuf};

use stella_mcp::{McpConfig, McpServerConfig};

/// The servers from both files, merged by name. It also holds a note for
/// each file that was skipped.
#[derive(Debug, Default)]
pub(crate) struct OwnMcpConfig {
    /// The user file's servers, with the repo file's on top.
    pub(crate) config: McpConfig,
    /// One line for each file that is there but was not read. Each line
    /// names its file.
    pub(crate) notices: Vec<String>,
    /// The directory the stdio servers start in. It is `$STELLA_HOME` when
    /// the repo is not trusted, since every server then came from your own
    /// file. It is `None` in a trusted repo, so the servers start where
    /// Stella runs.
    start_dir: Option<PathBuf>,
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
pub(crate) fn load_own_config(workspace_root: &Path, workspace_trusted: bool) -> OwnMcpConfig {
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

    /// The servers to connect, each set to start in the directory chosen
    /// above.
    pub(crate) fn servers(&self) -> Vec<McpServerConfig> {
        let mut servers = self.config.clone().into_servers();
        for server in &mut servers {
            server.cwd.clone_from(&self.start_dir);
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
