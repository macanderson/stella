//! `delete_file` — remove a file inside the workspace. Completes the CRUD
//! set (create/read/update/delete) so file lifecycle is fully visible to
//! the Files Touched tracker; agents should prefer this over `bash rm`
//! (which the tracker cannot attribute).
//!
//! A **symlink is removed as the link**, never as its target. Every other file
//! tool wants the real file, so the confined walk expands a symlink at the
//! leaf for them; deleting is the one operation that acts on the directory
//! entry instead. Following the link here would remove
//! `vendor/config.toml`'s target and leave the link dangling — silent data
//! loss on any repository carrying in-tree symlinks (vendored configs,
//! `node_modules/.bin`).
//!
//! That is why this module reaches for [`RootHandle::symlink_stat`],
//! [`RootHandle::read_link`] and [`RootHandle::remove_file`], the three
//! entry-level calls, rather than the resolving [`RootHandle::stat`]. The
//! containment argument is unchanged either way: interior components are still
//! opened `O_NOFOLLOW` off the held descriptor, so only the final name's own
//! indirection differs.

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;
use crate::rootfd::{EntryKind, RootHandle};

pub struct DeleteFile;

/// What the blocking worker saw and did: the removal happened, and this is the
/// symlink target it spared, if the entry was a link.
///
/// Read *before* the unlink, because afterwards there is nothing left to ask —
/// and naming the spared target is the whole reason the model can tell a link
/// deletion from a file deletion.
enum Removed {
    File,
    Symlink(std::path::PathBuf),
}

#[async_trait]
impl Tool for DeleteFile {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "delete_file".into(),
            description: "Delete a file in the workspace. Prefer this over `bash rm`. A symlink is removed as the link; its target is left alone. To delete several files, send them in ONE call with `files`: every path is checked before any of them is removed.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Workspace-relative path" },
                    "files": crate::batch::plural_schema(
                        serde_json::json!({
                            "path": { "type": "string", "description": "Workspace-relative path" }
                        }),
                        &["path"],
                        "Several files deleted in one call — every path is scope-checked and \
                         confirmed to be a file before any of them is removed.",
                    ),
                    "reason": { "type": "string", "description": "Why you are deleting this file — recorded in the session's file-touch audit log" }
                },
                "required": []
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        if crate::batch::is_plural(input, FILES_KEY) {
            return delete_batch(input, ctx).await;
        }
        delete_one(input, ctx).await
    }
}

/// The plural key: several files removed in one call.
const FILES_KEY: &str = "files";

/// Parse one target — shared by the single form and by every element of
/// `files`.
fn delete_target(value: &Value) -> Result<String, crate::input::InputError> {
    crate::input::required_str(value, "path").map(str::to_string)
}

/// Delete several files, checking every one before removing any.
///
/// A delete cannot be rolled back, so this validates the whole batch first —
/// scope, existence, and that each target is a file rather than a directory.
/// The failure that actually happens (a typo'd path, a directory, something
/// outside the scope) is caught with the tree still intact. A mid-batch IO
/// failure is the case no ordering can prevent, and it is reported naming
/// exactly what had already been removed.
async fn delete_batch(input: &Value, ctx: &crate::ctx::ToolCtx) -> ToolOutput {
    let targets = match crate::batch::targets(input, FILES_KEY, "path", delete_target) {
        Ok(targets) => targets,
        Err(err) => return ToolOutput::from(err),
    };

    // Pass one: resolve and classify. Nothing is unlinked in this loop.
    let mut planned = Vec::with_capacity(targets.len());
    for (index, target) in targets.iter().enumerate() {
        let (scope_root, path) = match ctx.resolve_for_write(target) {
            Ok(resolved) => resolved,
            Err(refusal) => {
                return ToolOutput::error(format!(
                    "`{FILES_KEY}`[{index}] (`{target}`): {refusal} — nothing was deleted"
                ));
            }
        };
        let handle = match RootHandle::open(&scope_root) {
            Ok(handle) => std::sync::Arc::new(handle),
            Err(e) => return ToolOutput::error(format!("cannot open workspace root: {e}")),
        };
        let kind = tokio::task::spawn_blocking({
            let (handle, path) = (std::sync::Arc::clone(&handle), path.clone());
            move || handle.symlink_stat(&path).map(|s| s.kind())
        })
        .await;
        match kind {
            Ok(Ok(EntryKind::File | EntryKind::Symlink)) => planned.push((handle, path)),
            // An escape is a different mistake from a directory, and the
            // single form has always said so. Collapsing them here would tell
            // a model that `sub/..` "is not a file", which is true and useless.
            Ok(Err(e)) if e.is_escape() => {
                return ToolOutput::error(format!(
                    "`{FILES_KEY}`[{index}]: path `{path}` escapes the workspace root ({e}) — \
                     nothing was deleted"
                ));
            }
            Ok(Ok(_)) | Ok(Err(_)) => {
                return ToolOutput::error(format!(
                    "`{FILES_KEY}`[{index}]: `{path}` is not a file (directories and missing \
                     paths are not deletable with this tool) — nothing was deleted"
                ));
            }
            Err(e) => {
                return ToolOutput::error(format!("could not inspect `{path}`: {e}"));
            }
        }
    }

    // Pass two: every target checked, so remove.
    let mut removed: Vec<String> = Vec::with_capacity(planned.len());
    for (handle, path) in planned {
        let outcome = tokio::task::spawn_blocking({
            let (handle, path) = (std::sync::Arc::clone(&handle), path.clone());
            move || handle.remove_file(&path)
        })
        .await;
        // The one failure no ordering can prevent: the checks all passed and
        // the unlink itself failed. Nothing can be undone at this point, so
        // the report names exactly what is already gone rather than implying
        // the batch was atomic.
        let failure = match outcome {
            Ok(Ok(())) => {
                removed.push(path);
                continue;
            }
            Ok(Err(e)) => e.to_string(),
            Err(e) => e.to_string(),
        };
        return ToolOutput::error(format!(
            "could not delete `{path}`: {failure} — this batch had already deleted {} file(s): \
             {}",
            removed.len(),
            removed.join(", ")
        ));
    }
    ToolOutput::ok(format!(
        "deleted {} file(s): {}",
        removed.len(),
        removed.join(", ")
    ))
}

async fn delete_one(input: &Value, ctx: &crate::ctx::ToolCtx) -> ToolOutput {
    let path = match crate::input::required_str(input, "path") {
        Ok(v) => v,
        Err(err) => {
            return ToolOutput::from(err);
        }
    };
    // A delete is the one built-in effect the agent cannot undo, so the
    // scope is consulted before the descriptor walk starts.
    let (scope_root, path) = match ctx.resolve_for_write(path) {
        Ok(resolved) => resolved,
        Err(refusal) => return ToolOutput::error(refusal.to_string()),
    };
    let path = path.as_str();
    let handle = match RootHandle::open(&scope_root) {
        Ok(handle) => std::sync::Arc::new(handle),
        Err(e) => {
            return ToolOutput::error(format!("cannot open workspace root: {e}"));
        }
    };
    // Classify, read the link, and unlink on one blocking worker. All three
    // are entry-level calls off the same held directory descriptor, so
    // nothing planted between them can redirect the removal — and none of
    // them expands the leaf, which is what keeps a link's target out of it.
    let outcome = tokio::task::spawn_blocking({
        let (handle, path) = (std::sync::Arc::clone(&handle), path.to_string());
        move || -> Result<Option<Removed>, crate::rootfd::RootError> {
            let kind = handle.symlink_stat(&path)?.kind();
            let removed = match kind {
                EntryKind::File => Removed::File,
                // Read the target BEFORE unlinking: afterwards the link is
                // gone and the report could not name what it spared.
                EntryKind::Symlink => Removed::Symlink(handle.read_link(&path)?),
                EntryKind::Dir | EntryKind::Other => return Ok(None),
            };
            handle.remove_file(&path)?;
            Ok(Some(removed))
        }
    })
    .await;
    match outcome {
        Ok(Ok(Some(Removed::File))) => ToolOutput::ok(format!("deleted {path}")),
        Ok(Ok(Some(Removed::Symlink(target)))) => ToolOutput::ok(format!(
            "deleted symlink {path} — the link only; its target `{}` is untouched",
            target.display()
        )),
        Ok(Ok(None)) => ToolOutput::error(format!(
            "`{path}` is not a file (directories and missing paths are not deletable \
                     with this tool)"
        )),
        Ok(Err(e)) if e.is_escape() => {
            ToolOutput::error(format!("path `{path}` escapes the workspace root ({e})"))
        }
        // A missing path reaches here as the `stat` failing, and must read
        // as the same refusal a directory gets — this tool deletes files.
        Ok(Err(crate::rootfd::RootError::Io(e))) if e.kind() == std::io::ErrorKind::NotFound => {
            ToolOutput::error(format!(
                "`{path}` is not a file (directories and missing paths are not deletable \
                         with this tool)"
            ))
        }
        Ok(Err(e)) => ToolOutput::error(format!("could not delete `{path}`: {e}")),
        Err(e) => ToolOutput::error(format!("could not delete `{path}`: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare execution context rooted at `root` — every file-tool test
    /// drives the tool through one, since `Tool::execute` takes the
    /// context rather than the bare root path it used to (#3284).
    fn cx(root: impl AsRef<std::path::Path>) -> crate::ctx::ToolCtx {
        crate::ctx::ToolCtx::bare(root.as_ref().to_path_buf())
    }

    #[tokio::test]
    async fn deletes_a_file_and_rejects_escapes_dirs_and_ghosts() {
        let root = std::env::temp_dir().join(format!("stella_delete_{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("kill-me.txt"), "bye").unwrap();

        let ok = DeleteFile
            .execute(&serde_json::json!({"path": "kill-me.txt"}), &cx(&root))
            .await;
        assert!(!ok.is_error(), "{ok:?}");
        assert!(!root.join("kill-me.txt").exists());

        for (input, why) in [
            (serde_json::json!({"path": "../outside.txt"}), "escape"),
            (serde_json::json!({"path": "sub"}), "directory"),
            (serde_json::json!({"path": "ghost.txt"}), "missing"),
            (serde_json::json!({}), "no path"),
        ] {
            let out = DeleteFile.execute(&input, &cx(&root)).await;
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
                &cx(dir.path()),
            )
            .await;
        match out {
            ToolOutput::Ok { content, .. } => {
                assert!(content.contains("symlink"), "{content}");
                assert!(content.contains("real.toml"), "{content}");
            }
            ToolOutput::Error { message, .. } => panic!("expected ok, got: {message}"),
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
            .execute(&serde_json::json!({"path": "ghost-link"}), &cx(dir.path()))
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
                &cx(dir.path()),
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
                .execute(&serde_json::json!({"path": tail}), &cx(dir.path()))
                .await;
            assert!(out.is_error(), "`{tail}` must be rejected: {out:?}");
        }
        assert!(dir.path().join("sub").is_dir());
    }

    // ── batching (#4151) ──────────────────────────────────────────────────

    #[tokio::test]
    async fn one_call_deletes_several_files() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a.rs", "b.rs"] {
            std::fs::write(dir.path().join(name), "x").unwrap();
        }
        let out = DeleteFile
            .execute(
                &serde_json::json!({"files": [{"path": "a.rs"}, {"path": "b.rs"}]}),
                &cx(dir.path()),
            )
            .await;
        assert!(!out.is_error(), "{out:?}");
        assert!(!dir.path().join("a.rs").exists());
        assert!(!dir.path().join("b.rs").exists());
    }

    /// A delete cannot be undone, so the whole batch is checked first. The
    /// failure that actually happens — a typo'd path, a directory, an escape —
    /// is caught with the tree still intact.
    #[tokio::test]
    async fn a_batch_naming_one_bad_path_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.rs"), "x").unwrap();
        std::fs::create_dir(dir.path().join("adir")).unwrap();

        for bad in [
            serde_json::json!({"path": "ghost.rs"}),
            serde_json::json!({"path": "adir"}),
            serde_json::json!({"path": "../outside.rs"}),
        ] {
            let out = DeleteFile
                .execute(
                    &serde_json::json!({"files": [{"path": "real.rs"}, bad]}),
                    &cx(dir.path()),
                )
                .await;
            assert!(out.is_error(), "{out:?}");
            assert!(
                dir.path().join("real.rs").exists(),
                "the deletable file must survive a batch that was going to fail: {out:?}"
            );
        }
    }
}
