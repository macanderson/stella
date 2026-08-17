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
    let registry = stella_tools::ToolRegistry::new(cfg.workspace_root.clone());
    registry.allow_write_dirs(cfg.allowed_write_dirs.iter().cloned());
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/work/project")
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
