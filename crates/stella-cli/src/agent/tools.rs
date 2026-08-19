//! Custom-tool discovery, the session tool policy, and the witness-artifact
//! identity primitives the surviving wrapper-plugin tamper watch
//! (`crate::wrapper_candidate`) builds on.
//!
//! This module used to also carry the staged pipeline's own workspace ports
//! (repo structure/status, the diagnostic/test/mutation/coverage runners,
//! best-of-N candidate isolation) — that pipeline is gone from this build
//! (#3846), and with it every consumer of those ports.

use super::*;
use stella_plugin::{ArtifactIdentity, ArtifactKind};

/// The single filesystem-isolation seam for developer script-tool discovery.
/// The session stack goes through this report; candidate workspaces still
/// call `discover_in_scopes` directly (see `workspace_ports`) and therefore
/// miss the isolation gate.
#[cfg(test)]
pub(crate) fn custom_tool_report_for_workspace(
    root: &std::path::Path,
) -> stella_tools::custom::DiscoveryReport {
    crate::tool_foundry::adopt::gate_discovery(custom_tool_report_for_scopes(root, true), root)
}

/// Discover only the custom-tool scopes permitted by the current authority.
/// Filesystem-isolated benchmark runs omit both workspace and user-global
/// executable extensions regardless of the ordinary authority policy.
///
/// Installed plugins contribute their `<plugin_dir>/tools` here too (#3380),
/// scanned last so a package never takes a name the user's own manifests
/// defined. Their trust gate is the roster's — an untrusted checkout's
/// plugins are not loaded at all, so its contributed tools are not discovered
/// — and their principal rides on each tool as
/// [`stella_tools::custom::CustomTool::contributed_by`], which
/// `super::tool_stack` turns into the caller the authorization gate sees.
pub(crate) fn custom_tool_report_for_scopes(
    root: &std::path::Path,
    include_workspace: bool,
) -> stella_tools::custom::UngatedDiscovery {
    if crate::settings::filesystem_settings_disabled() {
        stella_tools::custom::UngatedDiscovery::default()
    } else {
        let user_root = crate::paths::user_extension_root();
        stella_tools::custom::discover_with_plugins(
            root,
            user_root.as_deref(),
            include_workspace,
            &crate::plugin_cmd::package::contributed_tool_dirs(root),
        )
    }
}

/// The pipeline's file fingerprint: SHA-256 over the complete bytes. Content
/// hashes are required at the witness authority boundary: size+mtime can be
/// restored after a same-length edit and would incorrectly credit a tampered
/// witness. One definition is shared with candidate snapshots.
pub(crate) fn fs_fingerprint(path: &std::path::Path) -> Option<String> {
    Some(
        OpenedWitnessArtifact::open(path)?
            .identity_for_path(path)?
            .fingerprint,
    )
}

/// Identity for the workspace-relative `rel` under `root`, attesting the
/// location the artifact was actually observed at: `path` carries the opened
/// file's canonical position relative to the canonical root. A witness that
/// was renamed and is still reachable through an aliased lookup (a
/// case-folding filesystem, a symlinked parent directory) therefore reports
/// its real location, which the pipeline's pinned-path equality rejects as
/// tampering. A file whose canonical position cannot be stated inside `root`
/// has no identity at all — fail closed, exactly like a symlink.
pub(crate) fn fs_artifact_identity(
    root: &std::path::Path,
    rel: &str,
) -> Option<ArtifactIdentity> {
    let full = root.join(rel);
    let identity = OpenedWitnessArtifact::open(&full)?.identity_for_path(&full)?;
    Some(ArtifactIdentity {
        path: observed_relative_path(root, &full)?,
        ..identity
    })
}

/// The canonical position of `full` relative to the canonical `root`, in the
/// repo-relative `/`-separated form the pipeline pins witness paths in.
fn observed_relative_path(root: &std::path::Path, full: &std::path::Path) -> Option<String> {
    let canonical_root = std::fs::canonicalize(root).ok()?;
    let observed = std::fs::canonicalize(full).ok()?;
    let rel = observed.strip_prefix(&canonical_root).ok()?;
    let mut out = String::new();
    for component in rel.components() {
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(component.as_os_str().to_str()?);
    }
    (!out.is_empty()).then_some(out)
}

struct OpenedWitnessArtifact {
    file: std::fs::File,
    metadata: std::fs::Metadata,
}

impl OpenedWitnessArtifact {
    fn open(path: &std::path::Path) -> Option<Self> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_FLAG_OPEN_REPARSE_POINT opens a link/reparse point itself
            // instead of following it. Link count is unavailable through a
            // stable std API, so Windows still fails closed below.
            options.custom_flags(0x0020_0000);
        }
        let file = options.open(path).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.file_type().is_file() || opened_metadata(&metadata).is_none() {
            return None;
        }
        Some(Self { file, metadata })
    }

    fn identity_for_path(mut self, path: &std::path::Path) -> Option<ArtifactIdentity> {
        use std::fmt::Write as _;
        use std::io::Read as _;

        use sha2::{Digest, Sha256};

        let (mode, link_count) = opened_metadata(&self.metadata)?;
        if link_count != 1 || !path_resolves_to_opened_file(path, &self.metadata) {
            return None;
        }
        let mut payload = Vec::new();
        self.file.read_to_end(&mut payload).ok()?;
        let final_metadata = self.file.metadata().ok()?;
        if opened_metadata(&final_metadata) != Some((mode, link_count))
            || !path_resolves_to_opened_file(path, &final_metadata)
        {
            return None;
        }
        let mut hasher = Sha256::new();
        hasher.update(b"regular");
        hasher.update(mode.to_le_bytes());
        hasher.update(link_count.to_le_bytes());
        hasher.update(payload);
        let mut fingerprint = String::from("sha256:");
        for byte in hasher.finalize() {
            write!(&mut fingerprint, "{byte:02x}").ok()?;
        }
        Some(ArtifactIdentity {
            // The observed location is attested by `fs_artifact_identity`,
            // which knows the workspace root. Left empty here, a bare content
            // identity can never satisfy the pipeline's pinned-path equality.
            path: String::new(),
            fingerprint,
            kind: ArtifactKind::Regular,
            mode,
            link_count,
        })
    }
}

#[cfg(unix)]
fn opened_metadata(metadata: &std::fs::Metadata) -> Option<(u32, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.mode(), metadata.nlink()))
}

#[cfg(not(unix))]
fn opened_metadata(_metadata: &std::fs::Metadata) -> Option<(u32, u64)> {
    // Stable Rust does not expose a by-handle link count on Windows. Never
    // manufacture `1`: without proof that no hardlink aliases exist, witness
    // identity is unavailable and acceptance fails closed.
    None
}

#[cfg(unix)]
fn path_resolves_to_opened_file(path: &std::path::Path, opened: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(current) = std::fs::symlink_metadata(path) else {
        return false;
    };
    current.file_type().is_file() && current.dev() == opened.dev() && current.ino() == opened.ino()
}

#[cfg(not(unix))]
fn path_resolves_to_opened_file(_path: &std::path::Path, _opened: &std::fs::Metadata) -> bool {
    false
}

/// The session's tool policy.
///
/// Every session driver wraps its assembled tool stack in
/// [`crate::tool_policy::PolicyToolSet`] with this (via
/// [`super::tool_stack`]), which is what makes a
/// `"tools"` entry cover built-ins, MCP tools, and customer-registered custom
/// tools identically. Resolved once in `Config::load_with_settings` (managed
/// ceiling already folded in), so this is a clone, not a re-derivation — there
/// is no second place that could disagree about what is switched off.
pub(crate) fn session_tool_policy(cfg: &Config) -> stella_tools::policy::ToolPolicy {
    cfg.tool_policy.clone()
}

#[cfg(test)]
mod tests;
