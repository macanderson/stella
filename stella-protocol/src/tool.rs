//! The engine's one internal tool-call schema. Provider adapters translate
//! to/from their own dialect (`anthropic-tools`, `openai-json`,
//! `gemini-functions`). Nothing outside `stella-model` should ever construct a
//! provider-native tool-call shape directly.

use serde::{Deserialize, Serialize};

/// A tool schema advertised to the model: name, description, and a JSON
/// Schema for its input. Kept as `serde_json::Value` rather than a typed
/// schema struct so any tool (built-in or MCP-supplied) can describe itself
/// without a second schema language.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    /// The tool's identifier, as the model must spell it in a
    /// [`ToolCall::name`]. Unique within one registry.
    pub name: String,
    /// What the tool does, written for the model rather than for a human
    /// reader — this text is the whole basis on which it decides to call.
    pub description: String,
    /// JSON Schema for the object the model must send as
    /// [`ToolCall::input`].
    pub input_schema: serde_json::Value,
    /// True when the tool cannot mutate any state (filesystem, processes,
    /// environment) — the engine may run consecutive read-only calls from
    /// one step concurrently. Defaults to false so unknown/external tools
    /// are treated as mutating, the safe direction.
    #[serde(default)]
    pub read_only: bool,
}

/// One tool invocation the model requested.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ToolCall {
    /// Stable id correlating this call to its eventual `ToolResult`.
    pub call_id: String,
    /// Which tool to run — matches the [`ToolSchema::name`] it was chosen
    /// from.
    pub name: String,
    /// The arguments, as the model produced them. Runtime data: validate
    /// against [`ToolSchema::input_schema`] rather than trusting the shape.
    pub input: serde_json::Value,
}

/// The output of running a tool — success or a typed, named failure. Never a
/// bare string: every tool result is inspectable without string-sniffing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ToolOutput {
    /// The tool ran to completion.
    Ok {
        /// What the tool produced, as the model will read it.
        content: String,
    },
    /// The tool failed.
    Error {
        /// Why it failed, phrased so the model can act on it — the model
        /// sees this text and retries against it.
        message: String,
    },
}

impl ToolOutput {
    /// Whether this result is the `Error` arm — the one place a consumer
    /// should branch on tool failure, instead of sniffing the content string.
    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(self, ToolOutput::Error { .. })
    }
}

/// A tool result reported back to the model, correlated to its call.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    /// The [`ToolCall::call_id`] this answers.
    pub call_id: String,
    /// What running the call produced — success or a named failure.
    pub output: ToolOutput,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_output_roundtrips_ok_and_error() {
        let ok = ToolOutput::Ok {
            content: "hi".into(),
        };
        let err = ToolOutput::Error {
            message: "boom".into(),
        };
        for variant in [ok, err] {
            let json = serde_json::to_string(&variant).unwrap();
            let back: ToolOutput = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn is_error_reports_correctly() {
        assert!(
            !ToolOutput::Ok {
                content: String::new()
            }
            .is_error()
        );
        assert!(
            ToolOutput::Error {
                message: String::new()
            }
            .is_error()
        );
    }

    #[test]
    fn read_only_defaults_to_false_the_safe_direction() {
        // A schema serialized before the field existed (or by an external
        // MCP tool that doesn't know about it) must deserialize as mutating.
        let json = r#"{"name":"t","description":"d","input_schema":{}}"#;
        let schema: ToolSchema = serde_json::from_str(json).unwrap();
        assert!(!schema.read_only);
    }

    #[test]
    fn tool_call_roundtrips() {
        let call = ToolCall {
            call_id: "call_1".into(),
            name: "read_file".into(),
            input: serde_json::json!({ "path": "src/main.rs" }),
        };
        let json = serde_json::to_string(&call).unwrap();
        let back: ToolCall = serde_json::from_str(&json).unwrap();
        assert_eq!(back, call);
    }
}
