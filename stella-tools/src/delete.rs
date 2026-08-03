//! `delete_file` — remove a file inside the workspace. Completes the CRUD
//! set (create/read/update/delete) so file lifecycle is fully visible to
//! the Files Touched tracker; agents should prefer this over `bash rm`
//! (which the tracker cannot attribute).
//!
//! A **symlink is removed as the link**, never as its target. That takes its
//! own resolver (`resolve_entry_within_root`) because
//! [`crate::resolve_within_root`] canonicalizes, which is right for every
//! other tool — read/edit/write all want the real file — and exactly wrong
//! here: canonicalizing `vendor/config.toml → ../../real/config.toml` and
//! then `remove_file`-ing the result deletes the real file and leaves the
//! link dangling. Silent data loss on any repository carrying in-tree
//! symlinks (vendored configs, `node_modules/.bin`).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;

pub struct DeleteFile;

/// Resolve `path` to the entry to remove **without following a final
/// symlink**.
///
/// The parent goes through [`crate::resolve_within_root`], so containment is
/// enforced exactly as everywhere else (including through intermediate
/// symlinked directories); only the last component is re-joined verbatim.
/// `file_name` returns `None` for `""`, `.`, and any path ending in `..`, so
/// those are rejected here rather than resolving to a directory.
///
/// `None` means the path escapes the workspace root. A path whose parent does
/// not exist resolves fine and then fails the caller's own existence check
/// with the missing-path message.
fn resolve_entry_within_root(root: &Path, path: &str) -> Option<PathBuf> {
    let relative = Path::new(path);
    let name = relative.file_name()?;
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let parent_full = if parent.as_os_str().is_empty() {
        root.canonicalize().ok()?
    } else {
        crate::resolve_within_root(root, parent.to_str()?)?
    };
    Some(parent_full.join(name))
}

#[async_trait]
impl Tool for DeleteFile {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "delete_file".into(),
            description: "Delete a file in the workspace. Prefer this over `bash rm`. A symlink is removed as the link; its target is left alone.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Workspace-relative path" },
                    "reason": { "type": "string", "description": "Why you are deleting this file — recorded in the session's file-touch audit log" }
                },
                "required": ["path"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput {
        let Some(path) = input.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::Error {
                message: "missing required field `path`".into(),
            };
        };
        let Some(full) = resolve_entry_within_root(root, path) else {
            return ToolOutput::Error {
                message: format!("path `{path}` escapes the workspace root"),
            };
        };
        // lstat, not `is_file()`: the whole point is to see the link rather
        // than what it points at.
        let file_type = match tokio::fs::symlink_metadata(&full).await {
            Ok(meta) => meta.file_type(),
            Err(_) => {
                return ToolOutput::Error {
                    message: format!(
                        "`{path}` is not a file (directories and missing paths are not deletable \
                         with this tool)"
                    ),
                };
            }
        };
        if !file_type.is_file() && !file_type.is_symlink() {
            return ToolOutput::Error {
                message: format!(
                    "`{path}` is not a file (directories and missing paths are not deletable \
                     with this tool)"
                ),
            };
        }
        // Read the target BEFORE unlinking so the report can name what was
        // spared — the model otherwise cannot tell a link deletion from a
        // file deletion, which is the whole ambiguity this fixes.
        let target = if file_type.is_symlink() {
            tokio::fs::read_link(&full).await.ok()
        } else {
            None
        };
        match tokio::fs::remove_file(&full).await {
            Ok(()) => ToolOutput::Ok {
                content: match target {
                    Some(target) => format!(
                        "deleted symlink {path} — the link only; its target `{}` is untouched",
                        target.display()
                    ),
                    None => format!("deleted {path}"),
                },
            },
            Err(e) => ToolOutput::Error {
                message: format!("could not delete `{path}`: {e}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deletes_a_file_and_rejects_escapes_dirs_and_ghosts() {
        let root = std::env::temp_dir().join(format!("stella_delete_{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("kill-me.txt"), "bye").unwrap();

        let ok = DeleteFile
            .execute(&serde_json::json!({"path": "kill-me.txt"}), &root)
            .await;
        assert!(!ok.is_error(), "{ok:?}");
        assert!(!root.join("kill-me.txt").exists());

        for (input, why) in [
            (serde_json::json!({"path": "../outside.txt"}), "escape"),
            (serde_json::json!({"path": "sub"}), "directory"),
            (serde_json::json!({"path": "ghost.txt"}), "missing"),
            (serde_json::json!({}), "no path"),
        ] {
            let out = DeleteFile.execute(&input, &root).await;
            assert!(out.is_error(), "{why} must be rejected: {out:?}");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    /// The audit witness: deleting a symlink must unlink the LINK, leaving its
    /// target intact. Before this, the resolver canonicalized first and the
    /// tool removed the target — silent data loss in any tree with in-tree
    /// symlinks, reported as an ordinary `deleted <path>`.
    #[cfg(unix)]
    #[tokio::test]
    async fn deleting_a_symlink_removes_the_link_not_its_target() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("vendor")).unwrap();
        std::fs::write(dir.path().join("real.toml"), "keep me").unwrap();
        std::os::unix::fs::symlink("../real.toml", dir.path().join("vendor/config.toml")).unwrap();

        let out = DeleteFile
            .execute(
                &serde_json::json!({"path": "vendor/config.toml"}),
                dir.path(),
            )
            .await;
        match out {
            ToolOutput::Ok { content } => {
                assert!(content.contains("symlink"), "{content}");
                assert!(content.contains("real.toml"), "{content}");
            }
            ToolOutput::Error { message } => panic!("expected ok, got: {message}"),
        }
        assert!(
            !dir.path()
                .join("vendor/config.toml")
                .symlink_metadata()
                .is_ok(),
            "the link itself must be gone"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("real.toml")).unwrap(),
            "keep me",
            "the symlink's target must survive"
        );
    }

    /// A dangling symlink is still a link the agent may want gone; before the
    /// fix it resolved to nothing and the tool reported it as missing.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dangling_symlink_is_deletable() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("nowhere.txt", dir.path().join("ghost-link")).unwrap();

        let out = DeleteFile
            .execute(&serde_json::json!({"path": "ghost-link"}), dir.path())
            .await;
        assert!(!out.is_error(), "{out:?}");
        assert!(dir.path().join("ghost-link").symlink_metadata().is_err());
    }

    /// The last component is re-joined verbatim, so containment has to come
    /// from the parent's own resolution — including through a symlinked
    /// directory that points outside the workspace.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlinked_directory_leading_outside_the_root_still_rejects() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "private").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();

        let out = DeleteFile
            .execute(
                &serde_json::json!({"path": "escape/secret.txt"}),
                dir.path(),
            )
            .await;
        assert!(out.is_error(), "{out:?}");
        assert!(outside.path().join("secret.txt").exists());
    }

    /// `file_name()` is `None` for these, so they can never resolve to the
    /// directory they name.
    #[tokio::test]
    async fn dot_and_dotdot_tails_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        for tail in ["sub/.", "sub/..", "."] {
            let out = DeleteFile
                .execute(&serde_json::json!({"path": tail}), dir.path())
                .await;
            assert!(out.is_error(), "`{tail}` must be rejected: {out:?}");
        }
        assert!(dir.path().join("sub").is_dir());
    }
}
