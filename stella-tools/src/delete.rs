//! `delete_file` — remove a file inside the workspace. Completes the CRUD
//! set (create/read/update/delete) so file lifecycle is fully visible to
//! the Files Touched tracker; agents should prefer this over `bash rm`
//! (which the tracker cannot attribute).

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;

pub struct DeleteFile;

#[async_trait]
impl Tool for DeleteFile {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "delete_file".into(),
            description: "Delete a file in the workspace. Prefer this over `bash rm`.".into(),
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
        let handle = match crate::rootfd::RootHandle::open(root) {
            Ok(handle) => std::sync::Arc::new(handle),
            Err(e) => {
                return ToolOutput::Error {
                    message: format!("cannot open workspace root: {e}"),
                };
            }
        };
        // Classify and unlink on one blocking worker. `unlinkat` never follows
        // a symlink, so nothing planted between the two can redirect the
        // removal — the check and the act see the same directory descriptor.
        let outcome = tokio::task::spawn_blocking({
            let (handle, path) = (std::sync::Arc::clone(&handle), path.to_string());
            move || -> Result<bool, crate::rootfd::RootError> {
                if !handle.stat(&path)?.is_file() {
                    return Ok(false);
                }
                handle.remove_file(&path)?;
                Ok(true)
            }
        })
        .await;
        match outcome {
            Ok(Ok(true)) => ToolOutput::Ok {
                content: format!("deleted {path}"),
            },
            Ok(Ok(false)) => ToolOutput::Error {
                message: format!(
                    "`{path}` is not a file (directories and missing paths are not deletable \
                     with this tool)"
                ),
            },
            Ok(Err(e)) if e.is_escape() => ToolOutput::Error {
                message: format!("path `{path}` escapes the workspace root ({e})"),
            },
            // A missing path reaches here as the `stat` failing, and must read
            // as the same refusal a directory gets — this tool deletes files.
            Ok(Err(crate::rootfd::RootError::Io(e)))
                if e.kind() == std::io::ErrorKind::NotFound =>
            {
                ToolOutput::Error {
                    message: format!(
                        "`{path}` is not a file (directories and missing paths are not deletable \
                         with this tool)"
                    ),
                }
            }
            Ok(Err(e)) => ToolOutput::Error {
                message: format!("could not delete `{path}`: {e}"),
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
}
