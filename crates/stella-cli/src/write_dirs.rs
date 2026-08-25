//! Where the operator's extra write directories come from, and how the two
//! sources compose.
//!
//! Two surfaces name directories a write tool may touch outside the workspace
//! root: `stella.toml`'s `[workspace] allowed_dirs` and the `--allow-dir`
//! flag (`STELLA_ALLOW_DIR`). They are **additive**, not last-wins: an
//! operator who adds one directory on the command line is widening the
//! project's scope for this invocation, and silently dropping the project's
//! own list would revoke a permission nobody asked to revoke — the failure
//! would look like a tool refusing a path the committed config plainly allows.
//!
//! The decision itself lives in `stella_core::workspace_scope::SessionScope`;
//! this module only answers *which directories the host hands it*, which is
//! why the whole resolution is one pure function over borrowed data
//! (invariant 2).

use std::path::{Path, PathBuf};

/// The union of the configured and command-line directories, each resolved to
/// an absolute path against `workspace_root`, in a stable order: configured
/// entries first, then the flag's, duplicates dropped.
///
/// Relative entries resolve against the workspace root rather than the process
/// working directory. A committed `allowed_dirs = ["../shared"]` has to mean
/// the same directory for every teammate, and the cwd a run is launched from
/// is not a property of the workspace.
///
/// Blank entries are dropped rather than resolving to the root itself: a
/// trailing comma in `--allow-dir a,b,` is a typo, and reading it as "grant
/// the workspace root" would be a grant the operator never wrote.
///
/// No filesystem access, so a directory that does not exist yet is carried
/// through unchanged — the scope decision is about paths, and the tool that
/// eventually opens one reports its own I/O error by name.
pub(crate) fn resolve(
    workspace_root: &Path,
    configured: &[String],
    cli: &[String],
) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for entry in configured.iter().chain(cli.iter()) {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let candidate = Path::new(trimmed);
        let resolved = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            workspace_root.join(candidate)
        };
        if !out.contains(&resolved) {
            out.push(resolved);
        }
    }
    out
}

/// A tool registry rooted at this run's workspace, already holding the
/// operator's extra write directories.
///
/// The single assembly point for the grant. Every session-starting path builds
/// its registry through here, so a new one cannot forget the
/// `allow_write_dirs` call and silently ship a narrower scope than the
/// operator configured — the failure mode would be a tool refusing a path the
/// committed config plainly allows, which reads as a bug in the tool.
pub(crate) fn registry_for(cfg: &crate::config::Config) -> stella_tools::ToolRegistry {
    registry_rooted_at(cfg, cfg.workspace_root.clone())
}

/// The same grant, for a registry rooted somewhere other than the session's
/// workspace: a fleet worker's worktree, a best-of-N candidate's snapshot.
///
/// These exist because the *root* differs, never because the grant should.
/// An operator who allowed a shared directory meant it for the work, and the
/// work is what runs in these registries — so leaving them out made
/// `--allow-dir` a no-op in the default interactive shell, in every fleet
/// worker and in every pipeline candidate, which is exactly the silent
/// narrowing [`registry_for`]'s doc promises cannot happen.
///
/// The root is honoured as given: a worktree root makes
/// `stella_core::workspace_scope` treat that worktree as the workspace, which
/// is what lets a worker write its own tree while the origin project stays
/// invisible to it.
pub(crate) fn registry_rooted_at(
    cfg: &crate::config::Config,
    root: PathBuf,
) -> stella_tools::ToolRegistry {
    let registry = stella_tools::ToolRegistry::new(root);
    registry.allow_write_dirs(cfg.allowed_write_dirs.iter().cloned());
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/work/project")
    }

    /// The invariant [`registry_for`]'s doc claims, enforced instead of
    /// asserted in prose.
    ///
    /// An adversarial audit found the doc was false the day it was written:
    /// five production paths called `ToolRegistry::new` directly and never
    /// granted, so `--allow-dir` was a silent no-op in the Command Deck (the
    /// default interactive shell), in `--resume`, in every fleet worker and in
    /// every best-of-N candidate. A comment cannot hold that line; a test
    /// naming the constructor can.
    ///
    /// A source scan rather than a runtime check, because the defect is
    /// *which call site was used*, which no assembled registry can report. A
    /// new session path either goes through this module or adds itself to the
    /// exemption list with a reason a reviewer reads.
    #[test]
    fn every_production_registry_is_built_through_this_module() {
        /// Call sites that legitimately construct a registry directly, each
        /// with the reason it needs no grant.
        const EXEMPT: &[(&str, &str)] = &[
            ("src/write_dirs.rs", "this module — it IS the grant"),
            (
                "src/enterprise_telemetry.rs",
                "a content-free rollup probe that dispatches no model-supplied \
                 path, so it has nothing to confine",
            ),
            (
                "src/tool_docs.rs",
                "the docs generator: it reads `schemas()` and never executes a \
                 tool, and it must document the surface a bare registry \
                 advertises rather than one session's grants",
            ),
        ];

        let crate_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

        // An exemption naming a file that no longer exists can never match,
        // so it reads as a granted permission while granting nothing. One
        // outlived the deletion of `stella-pipeline` (#4392).
        let stale: Vec<&str> = EXEMPT
            .iter()
            .map(|(file, _)| *file)
            .filter(|file| !crate_dir.join(file).exists())
            .collect();
        assert!(
            stale.is_empty(),
            "exemption(s) naming a file that no longer exists: {stale:?}. \
             Delete the entry, or repoint it at the file that replaced it."
        );

        let mut offenders: Vec<String> = Vec::new();
        let mut stack = vec![crate_dir.join("src")];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs") {
                    continue;
                }
                let relative = path
                    .strip_prefix(crate_dir)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                // Test modules build registries freely; the invariant is about
                // what a real session does.
                if relative.contains("tests") {
                    continue;
                }
                if EXEMPT.iter().any(|(file, _)| relative == *file) {
                    continue;
                }
                let Ok(source) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for (number, line) in source.lines().enumerate() {
                    // An inline `#[cfg(test)]` module ends the production half
                    // of a file. Test code builds registries freely — the
                    // invariant is about what a real session does.
                    if line.trim_start().starts_with("#[cfg(test)]") {
                        break;
                    }
                    if line.contains("ToolRegistry::new(") {
                        offenders.push(format!("{relative}:{}", number + 1));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these build a ToolRegistry without the operator's write grants — \
             route them through `write_dirs::registry_for` / \
             `registry_rooted_at`, or add them to EXEMPT with a reason: {offenders:?}"
        );
    }

    /// The two surfaces UNION. The flag was the shape most likely to be
    /// written as an override, and an override here revokes a permission the
    /// committed config granted — so this is the witness that it does not.
    #[test]
    fn cli_and_config_directories_union_rather_than_replace() {
        let resolved = resolve(
            &root(),
            &["/srv/shared".to_string()],
            &["/srv/scratch".to_string()],
        );
        assert_eq!(
            resolved,
            vec![PathBuf::from("/srv/shared"), PathBuf::from("/srv/scratch")],
            "a --allow-dir must widen the configured list, never replace it"
        );
    }

    /// A relative entry is anchored to the workspace root, not the process cwd
    /// — the committed config has to mean one directory for every teammate.
    #[test]
    fn a_relative_entry_resolves_against_the_workspace_root() {
        let resolved = resolve(
            &root(),
            &["../shared-lib".to_string()],
            &["vendor".to_string()],
        );
        assert_eq!(
            resolved,
            vec![
                PathBuf::from("/work/project/../shared-lib"),
                PathBuf::from("/work/project/vendor"),
            ]
        );
    }

    #[test]
    fn blank_entries_are_dropped_and_duplicates_collapse() {
        let resolved = resolve(
            &root(),
            &["/srv/shared".to_string(), "  ".to_string()],
            &["/srv/shared".to_string(), String::new()],
        );
        assert_eq!(resolved, vec![PathBuf::from("/srv/shared")]);
    }
}
