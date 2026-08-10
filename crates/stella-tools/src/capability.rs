//! Capability probing: check whether a binary is available on PATH without
//! executing it.
//!
//! The tool walks the PATH the child spawns actually see (identical to the
//! parent process's PATH, which is what `subprocess_env` inherits) and tests
//! executability. Results are memoized per session — a second probe for the
//! same name hits the cache, so install-mid-session won't be seen until
//! restart. This is the honest answer about a cached environment.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::{Value, json};
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;

/// One memoized probe result.
#[derive(Clone)]
pub struct CapabilityProbe {
    pub resolved: Option<PathBuf>,
}

/// `probe_capability` — check whether a binary is available on PATH.
pub struct ProbeCapability {
    memo: Mutex<HashMap<String, CapabilityProbe>>,
}

impl ProbeCapability {
    pub fn new() -> Self {
        Self {
            memo: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for ProbeCapability {
    fn default() -> Self {
        Self::new()
    }
}

/// Validate that a name is a valid command word: [A-Za-z0-9._+-], 1-64 chars.
fn validate_name(name: &str) -> Result<(), String> {
    let ok_len = (1..=64).contains(&name.len());
    let ok_chars = name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-'));
    if !ok_len || !ok_chars {
        return Err(format!(
            "invalid capability name {name:?}: names are 1-64 chars of [A-Za-z0-9._+-] \
             (a bare command word, never a path)"
        ));
    }
    Ok(())
}

/// Walk the PATH and return the first executable matching the name.
///
/// The PATH is read from `std::env::var("PATH")`, which is the environment a
/// child process would inherit. The `subprocess_env` module's scrub functions
/// do not mutate PATH (they remove credentials and ambient authority only),
/// so `std::env::var` is the correct source of truth.
fn resolve_on_path(name: &str, path_var: &str) -> Option<PathBuf> {
    std::env::split_paths(path_var.as_ref() as &OsStr)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                candidate
                    .metadata()
                    .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                    .unwrap_or(false)
            }
            #[cfg(not(unix))]
            {
                candidate.is_file()
            }
        })
}

#[async_trait]
impl Tool for ProbeCapability {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "probe_capability".into(),
            description: "Check whether a binary is available on PATH. Answers with the resolved \
                path or a cached not-found message (install mid-session won't be seen until \
                restart). Never executes the binary — a PATH lookup only, safe to call for any \
                name."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "Command name: 1-64 chars of [A-Za-z0-9._+-]."}
                },
                "required": ["name"]
            }),
            read_only: true,
            speculation_safe: true,
        }
    }

    async fn execute(&self, input: &Value, _root: &std::path::Path) -> ToolOutput {
        let name = match input.get("name").and_then(Value::as_str) {
            Some(n) => n,
            None => {
                return ToolOutput::Error {
                    message: "missing required string field \"name\"".to_string(),
                };
            }
        };

        // Validate the name.
        if let Err(message) = validate_name(name) {
            return ToolOutput::Error { message };
        }

        // Check memoized result first.
        let memo = self
            .memo
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(probe) = memo.get(name) {
            return match &probe.resolved {
                Some(path) => ToolOutput::Ok {
                    content: format!("{}: {}", name, path.display()),
                },
                None => ToolOutput::Error {
                    message: format!(
                        "{}: not found on PATH (cached for this session — install mid-session won't be seen until restart)",
                        name
                    ),
                },
            };
        }
        drop(memo);

        // Perform the PATH lookup.
        let path_var = match std::env::var("PATH") {
            Ok(p) => p,
            Err(_) => {
                let probe = CapabilityProbe { resolved: None };
                let mut memo = self
                    .memo
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                memo.insert(name.to_string(), probe);
                return ToolOutput::Error {
                    message: format!("{}: PATH is not set (cached for this session)", name),
                };
            }
        };

        let resolved = resolve_on_path(name, &path_var);
        let is_resolved = resolved.is_some();
        let result_str = match &resolved {
            Some(path) => format!("{}: {}", name, path.display()),
            None => format!(
                "{}: not found on PATH (cached for this session — install mid-session won't be seen until restart)",
                name
            ),
        };

        // Memoize and return.
        let probe = CapabilityProbe { resolved };
        let mut memo = self
            .memo
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        memo.insert(name.to_string(), probe);

        if is_resolved {
            ToolOutput::Ok {
                content: result_str,
            }
        } else {
            ToolOutput::Error {
                message: result_str,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probes_sh_resolves_to_path() {
        let tool = ProbeCapability::new();
        let input = json!({"name": "sh"});
        let output = tool.execute(&input, std::path::Path::new("/")).await;

        match output {
            ToolOutput::Ok { content } => {
                assert!(
                    content.starts_with("sh: "),
                    "Expected 'sh: ...' format, got: {}",
                    content
                );
            }
            ToolOutput::Error { message } => {
                panic!("sh should resolve on test system; got error: {}", message);
            }
        }
    }

    #[tokio::test]
    async fn absent_capability_returns_not_found() {
        let tool = ProbeCapability::new();
        let input = json!({"name": "definitely-not-a-real-tool-xyz"});
        let output = tool.execute(&input, std::path::Path::new("/")).await;

        match output {
            ToolOutput::Ok { content } => {
                panic!("Expected error for absent tool; got: {}", content);
            }
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("not found on PATH"),
                    "Expected not-found message, got: {}",
                    message
                );
                assert!(
                    message.contains("cached for this session"),
                    "Expected cache caveat, got: {}",
                    message
                );
            }
        }
    }

    #[tokio::test]
    async fn second_call_uses_memo() {
        let tool = ProbeCapability::new();
        let input = json!({"name": "definitely-not-a-real-tool-xyz"});

        // First call — memo miss, PATH walk happens.
        let output1 = tool.execute(&input, std::path::Path::new("/")).await;
        match output1 {
            ToolOutput::Error { message } => {
                assert!(message.contains("not found on PATH"));
            }
            ToolOutput::Ok { .. } => panic!("Expected error for absent tool"),
        }

        // Second call — should hit memo without walking PATH.
        let output2 = tool.execute(&input, std::path::Path::new("/")).await;
        match output2 {
            ToolOutput::Error { message } => {
                assert!(message.contains("not found on PATH"));
                assert!(message.contains("cached for this session"));
            }
            ToolOutput::Ok { .. } => panic!("Expected error for absent tool"),
        }

        // Verify memoization by checking the memo directly.
        let memo = tool.memo.lock().unwrap();
        assert!(memo.contains_key("definitely-not-a-real-tool-xyz"));
    }

    #[tokio::test]
    async fn validation_rejects_invalid_names() {
        let tool = ProbeCapability::new();

        // Test each invalid case.
        let long_name = "a".repeat(65);
        let invalid_cases: Vec<(&str, &str)> = vec![
            ("../sh", "path separator"),
            ("a b", "space"),
            ("", "empty"),
            (&long_name, "too long"),
        ];

        for (name, reason) in invalid_cases {
            let input = json!({"name": name});
            let output = tool.execute(&input, std::path::Path::new("/")).await;

            match output {
                ToolOutput::Error { message } => {
                    assert!(
                        message.contains("invalid capability name"),
                        "Expected validation error for {} ({}); got: {}",
                        name,
                        reason,
                        message
                    );
                }
                ToolOutput::Ok { content } => {
                    panic!(
                        "Expected error for invalid name {} ({}); got: {}",
                        name, reason, content
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn validation_accepts_valid_names() {
        let tool = ProbeCapability::new();

        let valid_cases = ["cargo-watch", "g++", "python3.12", "node_modules", "a+b"];

        for name in &valid_cases {
            let input = json!({"name": name});
            let output = tool.execute(&input, std::path::Path::new("/")).await;

            // We don't assert Success here (the binary may not exist), just that
            // validation passed (either Ok or "not found").
            match output {
                ToolOutput::Error { message } => {
                    assert!(
                        message.contains("not found on PATH")
                            || message.contains("PATH is not set"),
                        "Expected not-found, not validation error for {}: {}",
                        name,
                        message
                    );
                }
                ToolOutput::Ok { content } => {
                    // Tool was found.
                    assert!(content.contains(':'));
                }
            }
        }
    }

    #[tokio::test]
    async fn missing_required_field() {
        let tool = ProbeCapability::new();
        let input = json!({});
        let output = tool.execute(&input, std::path::Path::new("/")).await;

        match output {
            ToolOutput::Error { message } => {
                assert!(message.contains("missing required string field"));
            }
            ToolOutput::Ok { .. } => panic!("Expected error for missing field"),
        }
    }
}
