//! `write_file` — create or overwrite a file, creating parent dirs.
//! Successful writes record the written bytes in the session's read-state
//! ledger (#331): the model knows the content it just produced, so its own
//! write must never be misattributed later as out-of-band drift.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::read::ReadLedger;
use crate::registry::Tool;

#[derive(Default)]
pub struct WriteFile {
    ledger: Arc<ReadLedger>,
}

impl WriteFile {
    /// Construct sharing the registry's read-state ledger.
    pub fn with_ledger(ledger: Arc<ReadLedger>) -> Self {
        Self { ledger }
    }
}

#[async_trait]
impl Tool for WriteFile {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "write_file".into(),
            description: "Create or overwrite a file. Creates parent directories as needed.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path relative to workspace root" },
                    "content": { "type": "string", "description": "Full file content to write" },
                    "reason": { "type": "string", "description": "Why you are creating/overwriting this file — recorded in the session's file-touch audit log" },
                    "storage_intent": { "type": "string", "description": "Only when creating a database table/column that the storage gate flagged as similar to an existing one: one sentence of purpose plus why the existing objects don't fit. Recorded in stella.storage.toml." }
                },
                "required": ["path", "content"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    #[allow(clippy::collapsible_if)]
    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput {
        let path = match input.get("path").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => {
                return ToolOutput::Error {
                    message: "missing required field `path`".into(),
                };
            }
        };
        let content = match input.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => {
                return ToolOutput::Error {
                    message: "missing required field `content`".into(),
                };
            }
        };

        // The root is opened once and held; every component of `path` is then
        // opened relative to the one above it, and the parent directories are
        // created *during* that walk. There is no `create_dir_all` here any
        // more, because a `create_dir_all` re-resolves the whole prefix from a
        // string — the window #938 is about.
        let handle = match crate::rootfd::RootHandle::open(root) {
            Ok(handle) => Arc::new(handle),
            Err(e) => {
                return ToolOutput::Error {
                    message: format!("cannot open workspace root: {e}"),
                };
            }
        };

        match crate::durable_write::write_file_durably_at(
            handle,
            path.to_string(),
            content.as_bytes().to_vec(),
            true,
        )
        .await
        {
            Ok(()) => {
                self.ledger.record_known(root, path, content);
                let bytes = content.len();
                ToolOutput::Ok {
                    content: format!("wrote {bytes} bytes to {path}"),
                }
            }
            Err(e) if e.is_escape() => ToolOutput::Error {
                message: format!("path `{path}` escapes workspace root ({e})"),
            },
            Err(e) => ToolOutput::Error {
                message: format!("failed to write `{path}`: {e}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writes_file_and_creates_parent_dirs() {
        let dir = std::env::temp_dir();
        let path = format!("stella_write_test_{}/sub/dir/file.txt", std::process::id());
        let result = WriteFile::default()
            .execute(
                &serde_json::json!({"path": path, "content": "hello stella"}),
                &dir,
            )
            .await;
        match result {
            ToolOutput::Ok { content } => assert!(content.contains("wrote 12 bytes")),
            ToolOutput::Error { message } => panic!("expected ok, got: {message}"),
        }
        let written = tokio::fs::read_to_string(dir.join(&path)).await.unwrap();
        assert_eq!(written, "hello stella");
        let _ = tokio::fs::remove_dir_all(
            dir.join(format!("stella_write_test_{}", std::process::id())),
        )
        .await;
    }

    #[tokio::test]
    async fn path_escape_returns_error() {
        let dir = std::env::temp_dir();
        let result = WriteFile::default()
            .execute(
                &serde_json::json!({"path": "../../etc/bad", "content": "x"}),
                &dir,
            )
            .await;
        assert!(result.is_error());
    }

    #[tokio::test]
    async fn missing_content_returns_error() {
        let dir = std::env::temp_dir();
        let result = WriteFile::default()
            .execute(&serde_json::json!({"path": "ok.txt"}), &dir)
            .await;
        assert!(result.is_error());
    }
}
