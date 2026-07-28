//! `screenshot` — capture the screen (or a window/region) to a PNG under
//! `.stella/screenshots/`, for work verification: a judge or reviewer can
//! demand visual evidence that a UI change actually rendered.
//!
//! The capture lands on disk and the tool returns its path + size. With
//! multimodal attachments shipped end-to-end (protocol `Attachment`,
//! adapter media blocks, deck image paste), the path can be re-attached
//! for vision review — and the artifact stands on its own as evidence
//! humans open and agents cite.

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::exec;
// Single-quotes the capture path for this module's `bash -c` line — the
// crate's one POSIX escaper, which `exec` owns because it owns that runner.
use crate::exec::shell_quote;
use crate::registry::Tool;

/// The capture target for one call: `.stella/screenshots/<stamp>-<label>.png`
/// with the label sanitized to a path-safe slug. Shared by execute and the
/// `command.started` gate so both compose the same shape; the stamp is taken
/// at each composition, so the two paths may differ by the seconds between
/// them — the command around the path is identical.
fn capture_file(input: &Value, root: &std::path::Path) -> std::path::PathBuf {
    let label = input
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or("capture")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(48)
        .collect::<String>();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    root.join(".stella/screenshots")
        .join(format!("{stamp}-{label}.png"))
}

/// The platform capture chain over one already-shell-quoted output path:
/// macOS `screencapture`; Linux `grim` (Wayland) then `import`
/// (X11/ImageMagick). Each is silent + non-interactive. Needs a shell (the
/// `||` fallbacks), which is why the path arrives quoted, not as argv.
fn capture_command(quoted_path: &str) -> String {
    if cfg!(target_os = "macos") {
        format!("screencapture -x {quoted_path}")
    } else {
        format!(
            "grim {quoted_path} 2>/dev/null || import -window root {quoted_path} 2>/dev/null || \
             (echo 'no capture backend (grim or imagemagick import)' >&2; exit 1)"
        )
    }
}

pub struct Screenshot;

#[async_trait]
impl Tool for Screenshot {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "screenshot".into(),
            description: "Capture the screen to a PNG in .stella/screenshots/ as verification \
                          evidence. Returns the file path."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "label": { "type": "string", "description": "Short slug for the filename" }
                }
            }),
            read_only: false,
        }
    }

    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput {
        let file = capture_file(input, root);
        let dir = file
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        if let Err(e) = tokio::fs::create_dir_all(&dir).await {
            return ToolOutput::Error {
                message: format!("could not create {}: {e}", dir.display()),
            };
        }
        // The capture chain needs a shell (the `||` fallbacks), so the path
        // is quoted rather than passed as argv. The workspace root is not
        // ours to assume well-formed: a directory named `it's mine` would
        // otherwise close the literal and hand the remainder to `bash -c`
        // as code.
        let command = capture_command(&shell_quote(&file.to_string_lossy()));
        match exec::run(&command, root, 30).await {
            Ok((0, _)) => {
                let size = tokio::fs::metadata(&file)
                    .await
                    .map(|m| m.len())
                    .unwrap_or(0);
                if size == 0 {
                    return ToolOutput::Error {
                        message: "capture produced an empty file — is screen recording \
                                  permission granted?"
                            .into(),
                    };
                }
                let rel = file
                    .strip_prefix(root)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| file.display().to_string());
                ToolOutput::Ok {
                    content: format!("captured {rel} ({size} bytes)"),
                }
            }
            Ok((code, output)) => ToolOutput::Error {
                message: format!("screen capture failed (exit {code}): {output}"),
            },
            Err(e) => ToolOutput::Error { message: e },
        }
    }

    // A `bash -c` composer joins the `command.started` fence (#804): the
    // gate sees the same composed chain execute runs (stamp aside).
    async fn command_for_gate(&self, input: &Value, root: &std::path::Path) -> Option<String> {
        let file = capture_file(input, root);
        Some(capture_command(&shell_quote(&file.to_string_lossy())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_mutating_and_named() {
        let schema = Screenshot.schema();
        assert_eq!(schema.name, "screenshot");
        assert!(!schema.read_only);
    }

    #[tokio::test]
    async fn label_is_sanitized_into_the_filename_path() {
        // Can't actually capture in CI — assert the failure path is a named
        // error, not a panic, when no backend/permission exists. On a dev
        // Mac with permission this passes through the success path instead;
        // both are acceptable shapes.
        let root = std::env::temp_dir().join(format!("stella_shot_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let out = Screenshot
            .execute(&serde_json::json!({"label": "../weird label!!"}), &root)
            .await;
        match out {
            ToolOutput::Ok { content } => assert!(content.contains("weirdlabel")),
            ToolOutput::Error { message } => assert!(!message.is_empty()),
        }
        std::fs::remove_dir_all(&root).ok();
    }
}
