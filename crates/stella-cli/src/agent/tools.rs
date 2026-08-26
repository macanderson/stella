//! Custom-tool discovery, the session tool policy, and the witness-artifact
//! identity primitives the surviving wrapper-plugin tamper watch
//! (`crate::wrapper_candidate`) builds on.
//!
//! This module used to also carry the staged pipeline's own workspace ports
//! (repo structure/status, the diagnostic/test/mutation/coverage runners,
//! best-of-N candidate isolation) — that pipeline is gone from this build
//! (#3865), and with it every consumer of those ports.

use super::*;
use stella_plugin::{ArtifactIdentity, ArtifactKind};

/// The single filesystem-isolation seam for developer script-tool discovery.
/// The session stack goes through this report, and since #3865 deleted the
/// candidate-workspace tool chain there is no second path around it: every
/// remaining `discover_in_scopes` caller lives inside `stella_tools::custom`
/// itself.
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

/// Identity for the workspace-relative `rel` inside the tree `root` holds open,
/// attesting the location the artifact was actually observed at.
///
/// `root` is a **held directory descriptor**, not a path, and that is the whole
/// safety argument (#3483). The name is resolved exactly once, by the walk that
/// opens the file: every interior component is opened `O_NOFOLLOW` from the
/// descriptor above it, `..` pops that stack rather than asking the kernel, and
/// the leaf is opened without following a link there either. There is no path
/// left over for anything to re-point between the check and the use, and the
/// root itself cannot be swapped — a descriptor keeps naming the directory it
/// was opened on however the directory is renamed afterwards.
///
/// [`ConfinedEntry::resolved`] is that same walk's record of where it landed,
/// so a witness that was renamed and is still reachable at its pinned path
/// through an aliased lookup (a symlinked parent directory) reports its real
/// location, which the tamper watch's pinned-path equality rejects. An artifact
/// whose location cannot be stated, or whose name stops meaning this file while
/// it is being read, has no identity at all — fail closed, exactly like a
/// symlink.
pub(crate) fn fs_artifact_identity(
    root: &stella_tools::rootfd::RootHandle,
    rel: &str,
) -> Option<ArtifactIdentity> {
    let entry = root.open_entry(rel).ok()?;
    let identity = witness_identity(&entry)?;
    Some(ArtifactIdentity {
        path: entry.resolved().to_string(),
        ..identity
    })
}

/// The content half of an artifact's identity, read from an already-open
/// confined entry.
///
/// Bracketed by [`ConfinedEntry::still_named`] on both sides of the read: the
/// name this descriptor was opened by must still mean this file before the
/// bytes are hashed and after, or the fingerprint describes something that name
/// no longer refers to. Both questions go through the directory descriptor the
/// walk already holds, so neither is a fresh resolution of `rel`.
fn witness_identity(entry: &stella_tools::rootfd::ConfinedEntry) -> Option<ArtifactIdentity> {
    use std::fmt::Write as _;
    use std::io::Read as _;

    use sha2::{Digest, Sha256};

    let metadata = entry.file.metadata().ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    let (mode, link_count) = opened_metadata(&metadata)?;
    if link_count != 1 || !entry.still_named() {
        return None;
    }
    let mut payload = Vec::new();
    (&entry.file).read_to_end(&mut payload).ok()?;
    let final_metadata = entry.file.metadata().ok()?;
    if opened_metadata(&final_metadata) != Some((mode, link_count)) || !entry.still_named() {
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
        // The observed location is attested by `fs_artifact_identity`, which
        // holds the walk that found it. Left empty here, a bare content
        // identity can never satisfy the tamper watch's pinned-path equality.
        path: String::new(),
        fingerprint,
        kind: ArtifactKind::Regular,
        mode,
        link_count,
    })
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
