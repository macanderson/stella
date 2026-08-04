// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The session debug log (L-T8) and the secure append primitive behind it.
//!
//! Lived in `shell.rs` until the single-session surface was deleted (#936).
//! It never belonged to that shell: the deck writes through it, and the panic
//! hook in `crate::term` appends panics to the same file, so it outlived the
//! module it happened to be declared in.

use std::io;
use std::path::PathBuf;

use stella_protocol::AgentEvent;

use crate::input::UserInput;

/// A best-effort structured debug log (L-T8). Never panics and never fails the
/// TUI on an IO error — a lost log line must never take down the session.
#[derive(Debug, Clone, Default)]
pub struct DebugLog {
    path: Option<PathBuf>,
}

impl DebugLog {
    /// A log that writes to `path`, or a no-op sink when `path` is `None`.
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path }
    }

    /// True when this log actually writes somewhere.
    pub fn is_active(&self) -> bool {
        self.path.is_some()
    }

    /// Record an inbound `AgentEvent`.
    pub fn event(&self, event: &AgentEvent) {
        let payload = serde_json::to_value(event).unwrap_or(serde_json::Value::Null);
        self.append("event", payload);
    }

    /// Record an outbound user input.
    pub fn input(&self, input: &UserInput) {
        self.append(
            "input",
            serde_json::json!({ "input": format!("{input:?}") }),
        );
    }

    /// Record a free-form note.
    pub fn note(&self, msg: &str) {
        self.append("note", serde_json::json!({ "msg": msg }));
    }

    fn append(&self, kind: &str, payload: serde_json::Value) {
        if let Some(path) = &self.path {
            let _ = append_json_line(path, kind, payload);
        }
    }
}

/// Append one structured JSON line to `path` (best-effort). Also used by the
/// panic hook in `crate::term` to record panics into the same log.
#[cfg(unix)]
pub(crate) fn append_json_line(
    path: &PathBuf,
    kind: &str,
    payload: serde_json::Value,
) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(io::Error::other(
            "debug log must be a single-link regular file",
        ));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    let line = serde_json::json!({ "ts_ms": ts_ms, "kind": kind, "payload": payload });
    writeln!(file, "{line}")
}

#[cfg(not(unix))]
pub(crate) fn append_json_line(
    path: &PathBuf,
    _kind: &str,
    _payload: serde_json::Value,
) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "secure debug-log persistence is unsupported on this platform: {}",
            path.display()
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_log_is_inactive_without_a_path() {
        let log = DebugLog::new(None);
        assert!(!log.is_active());
        // No path → no panic, no file, pure no-op.
        log.event(&AgentEvent::Complete {
            model: "glm".into(),
            cost_usd: 0.0,
        });
        log.note("nothing happens");
    }

    #[test]
    fn debug_log_appends_structured_event_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cli.output");
        let log = DebugLog::new(Some(path.clone()));
        assert!(log.is_active());
        log.event(&AgentEvent::Stage {
            name: stella_protocol::StageKind::Execute,
        });
        log.input(&UserInput::Prompt {
            text: "hi".into(),
            attachments: Vec::new(),
        });
        log.note("done");

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 3, "one JSON line per record:\n{contents}");
        // Each line is valid JSON carrying the kind + a timestamp.
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v.get("kind").is_some());
            assert!(v.get("ts_ms").is_some());
        }
        let kinds: Vec<String> = lines
            .iter()
            .map(|l| {
                serde_json::from_str::<serde_json::Value>(l).unwrap()["kind"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(kinds, vec!["event", "input", "note"]);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn debug_log_rejects_a_symlink_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("outside.jsonl");
        std::fs::write(&target, "outside\n").unwrap();
        let path = dir.path().join("debug.jsonl");
        symlink(&target, &path).unwrap();

        assert!(append_json_line(&path, "note", serde_json::json!({})).is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "outside\n");
    }
}
