// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella-tools` — the built-in tool set the agent loop calls.
//!
//! Every tool implements [`registry::Tool`], takes a JSON input from the model,
//! and returns a [`stella_protocol::ToolOutput`] (success or a typed, named
//! error — never a bare string).
//!
//! The dispatchable surface is deliberately small, and every name in it earns
//! its place by being something an agent cannot do any other way:
//!
//! - the **working surface** — one shell ([`bash`]), the file CRUD quartet
//!   ([`read`] / [`mod@write`] / [`edit`] / [`delete`]), and one unified code
//!   search ([`search`], lexical and semantic in a single `query` parameter);
//! - the **coordination surface** — the sub-agent spawn tool (`delegate`), the
//!   session task board (`task_create` / `task_list` / `task_start` /
//!   `task_complete` / `task_cancel` / `task_assign`), the session scratch
//!   state plane (`save_state` / `get_state` / `list_state` / `delete_state`),
//!   and the environment report (`get_environment`).
//!
//! That is the whole built-in set. Every *other* capability reaches the model
//! as a developer-defined custom script tool ([`custom`]), an MCP tool
//! (`stella-mcp`), or a CLI session-layer tool — never as a built-in here.
//! The working surface is deliberately one tool per job: one shell rather than
//! a family of structured runners, one search rather than a `grep`/`glob`/
//! `graph_query` triple the model has to choose between (invariant #9 governs
//! the split, not the count).
//!
//! Beside the tools, this crate owns the session tool *mechanisms*: the
//! registry and its dispatch gates ([`registry`]), the custom-tool loader and
//! its execution contract ([`custom`], [`exec`], [`subprocess_env`]), the
//! tool-foundry authorship/adoption plane ([`foundry_author`],
//! [`foundry_gate`], [`foundry_witness`]), skill tool grants
//! ([`skill_grant`]), operator tool policy ([`policy`]), and the extension
//! hook runner/bridge ([`hook_runner`], [`hook_bridge`]).

pub mod agent_use;
pub mod bash;
pub mod batch;
pub mod catalog;
pub mod contracts;
pub mod ctx;
pub mod custom;
pub mod delete;
pub mod durable_write;
pub mod edit;
pub mod environment;
pub mod exec;
pub mod foundry_author;
pub mod foundry_gate;
pub mod foundry_witness;
pub mod gated;
pub mod hook_bridge;
pub mod hook_runner;
pub mod input;
pub mod policy;
pub mod read;
pub mod registry;
pub mod rootfd;
pub mod scratch;
pub mod search;
pub mod skill_grant;
pub mod staleness;
pub mod subagent;
/// Shared environment policy for every subprocess that can execute model- or
/// repository-controlled code. Downstream Stella crates must use this rather
/// than maintaining a second, drifting credential deny-list.
pub mod subprocess_env;
pub mod tasks;
pub mod temp_roots;
pub mod validate;
pub mod write;

pub use registry::ToolRegistry;

/// Turn a workspace-relative `path` into a full path, refusing one that names
/// a location outside `root`.
///
/// # This is for naming, not for opening
///
/// **Anything that opens, reads, writes, creates or unlinks a file must go
/// through [`rootfd::RootHandle`] instead.** This function answers a question
/// about a *string* and then hands the string back, and #938 is the gap that
/// leaves: for a path whose tail does not exist yet it walks up to the
/// deepest existing ancestor and re-appends the missing components without
/// validating them, so the interior directories of a brand-new file were
/// never checked at all. Whatever the caller does next re-resolves every
/// component from scratch, and anything that moved in between — a symlink
/// planted by the model's own [`bash`] tool, or by a `build.rs` in the
/// repository under audit — is followed. `O_NOFOLLOW` on the final component
/// does not close it, because the final component is not the one that moved.
///
/// What survives here is the set of callers that need a *name* rather than a
/// descriptor, and for which there is nothing to race because nothing is
/// opened: an argument handed to a subprocess; a working directory for one;
/// the ledger's record of which workspace file a touch referred to. For those
/// the lexical question is the whole question.
///
/// # What it checks
///
/// `Path::starts_with` is a *lexical* component comparison — it does not
/// resolve `..`.  On Linux where `std::env::temp_dir()` is `/tmp`, the path
/// `../../etc/passwd` joined to `/tmp` produces `/tmp/../../etc/passwd` which
/// *lexically* starts with `/tmp` but *semantically* resolves to `/etc/passwd`.
///
/// So this canonicalises `root` (which must exist) and then normalises the
/// joined path.  If the file already exists, it is canonicalised directly.
/// If it does not exist, the path is normalised by walking up to the deepest
/// existing ancestor, canonicalising that, and re-appending the remaining
/// non-existent components.  If the normalised full path does not start with
/// the canonicalised root, the path escapes the workspace and the caller must
/// reject it.
///
/// Returns the resolved full path on success, or `None` if the path
/// escapes the workspace root or `root` itself cannot be canonicalised.
pub fn resolve_within_root(root: &std::path::Path, path: &str) -> Option<std::path::PathBuf> {
    // Canonicalise root — it must exist (it's the workspace root).
    let canon_root = root.canonicalize().ok()?;

    let joined = canon_root.join(path);

    // If the file already exists, canonicalise it directly.
    if let Ok(canon) = joined.canonicalize() {
        return if canon.starts_with(&canon_root) {
            Some(canon)
        } else {
            None
        };
    }

    // The path doesn't fully resolve (write/edit creating a new file, or a
    // component is missing). Walk up to the deepest *existing* component —
    // detected with `symlink_metadata` (lstat), NOT `exists()`. `exists()`
    // follows symlinks and so returns `false` for a DANGLING symlink, which
    // would let this loop treat the link's own name as a brand-new file
    // inside root and hand it back — the OS then follows the link on write
    // and escapes the workspace. lstat stops at the link itself so we can
    // resolve or reject it.
    let mut existing = joined.clone();
    let mut tail: Vec<std::path::PathBuf> = Vec::new();
    loop {
        match existing.symlink_metadata() {
            Ok(meta) if meta.file_type().is_symlink() => {
                // A symlink is the deepest existing component. Resolve its
                // target: if it dangles or points outside root, reject; if
                // it stays inside, continue resolving from the target so any
                // remaining tail is appended to the real in-root location.
                let target = existing.canonicalize().ok()?;
                if !target.starts_with(&canon_root) {
                    return None;
                }
                existing = target;
                break;
            }
            // Ordinary existing dir/file — deepest real ancestor found.
            Ok(_) => break,
            // This component doesn't exist yet: peel it into the tail and
            // keep walking up.
            Err(_) => {
                let parent = existing.parent()?;
                let name = existing.file_name()?;
                tail.push(std::path::PathBuf::from(name));
                existing = parent.to_path_buf();
                if existing == canon_root || existing == std::path::Path::new("/") {
                    break;
                }
            }
        }
    }

    let canon_existing = existing.canonicalize().ok()?;
    let mut full = canon_existing;
    for name in tail.into_iter().rev() {
        full = full.join(name);
    }

    if full.starts_with(&canon_root) {
        Some(full)
    } else {
        None
    }
}

/// Normalize a model-supplied path to the workspace-relative POSIX form the
/// file tools report by: resolved against the workspace root (collapsing `.`
/// and `..`, following the same escape rules every tool enforces), stripped
/// back to relative, and joined with `/`. Returns `None` when the path
/// escapes the workspace or resolves to the root itself.
pub fn normalize_workspace_path(root: &std::path::Path, raw: &str) -> Option<String> {
    let full = resolve_within_root(root, raw)?;
    let canon_root = root.canonicalize().ok()?;
    let rel = full.strip_prefix(&canon_root).ok()?;
    let mut parts: Vec<&str> = Vec::new();
    for component in rel.components() {
        match component {
            std::path::Component::Normal(name) => parts.push(name.to_str()?),
            // `resolve_within_root` output is an absolute path under the
            // canonical root; anything else here means it wasn't resolvable
            // to a plain file path.
            _ => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_within_root_rejects_dotdot_escape() {
        let dir = std::env::temp_dir();
        // Create a subdir so canonicalize works
        let subdir = dir.join(format!("stella_resolve_test_{}", std::process::id()));
        std::fs::create_dir_all(&subdir).unwrap();

        // A safe path inside the root
        let safe = resolve_within_root(&subdir, "file.txt");
        assert!(safe.is_some());

        // An escape attempt — on Linux /tmp is shallow so ../../etc/passwd
        // resolves outside the root.
        let escaped = resolve_within_root(&subdir, "../../etc/passwd");
        assert!(escaped.is_none(), "path escape should be rejected");

        std::fs::remove_dir_all(&subdir).ok();
    }

    #[test]
    fn resolve_within_root_allows_nested_safe_path() {
        let dir = std::env::temp_dir();
        let subdir = dir.join(format!("stella_resolve_safe_{}", std::process::id()));
        std::fs::create_dir_all(subdir.join("sub/deep")).unwrap();

        let safe = resolve_within_root(&subdir, "sub/deep/file.txt");
        assert!(safe.is_some());

        std::fs::remove_dir_all(&subdir).ok();
    }

    /// A DANGLING symlink inside the root must not be a valid write target:
    /// the OS follows it on write and escapes the workspace. `exists()`
    /// returns false for a dangling link, so the old walk handed back
    /// `root/link` — this is the witness that it no longer does.
    #[cfg(unix)]
    #[test]
    fn resolve_within_root_rejects_write_through_a_dangling_symlink() {
        let dir = std::env::temp_dir();
        let root = dir.join(format!("stella_resolve_dangling_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();

        // `link` -> a target that does not exist (yet). Writing "link"
        // would create that outside-the-root target via symlink follow.
        let outside = dir.join(format!("stella_resolve_outside_{}", std::process::id()));
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        assert_eq!(
            resolve_within_root(&root, "link"),
            None,
            "writing through a dangling symlink must be rejected"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    /// A symlink pointing at an existing directory OUTSIDE the root must not
    /// launder a nested path back inside the confinement check.
    #[cfg(unix)]
    #[test]
    fn resolve_within_root_rejects_write_through_a_symlink_to_outside_dir() {
        let dir = std::env::temp_dir();
        let root = dir.join(format!(
            "stella_resolve_outlink_root_{}",
            std::process::id()
        ));
        let outside = dir.join(format!("stella_resolve_outlink_out_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("out")).unwrap();

        assert_eq!(
            resolve_within_root(&root, "out/evil.txt"),
            None,
            "a nested path through an outward symlink must be rejected"
        );

        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    /// An in-root symlink is fine: writes through it stay confined.
    #[cfg(unix)]
    #[test]
    fn resolve_within_root_allows_write_through_an_in_root_symlink() {
        let dir = std::env::temp_dir();
        let root = dir.join(format!("stella_resolve_inlink_{}", std::process::id()));
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("alias")).unwrap();

        let resolved =
            resolve_within_root(&root, "alias/new.txt").expect("in-root symlink write allowed");
        assert!(resolved.starts_with(root.canonicalize().unwrap()));

        std::fs::remove_dir_all(&root).ok();
    }

    /// The workspace-relative form the file tools report paths in.
    #[test]
    fn normalize_workspace_path_returns_posix_relative_form() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        assert_eq!(
            normalize_workspace_path(dir.path(), "src/main.rs").as_deref(),
            Some("src/main.rs")
        );
        assert_eq!(normalize_workspace_path(dir.path(), "../escape"), None);
        assert_eq!(
            normalize_workspace_path(dir.path(), "."),
            None,
            "the root itself is not a file path"
        );
    }
}
