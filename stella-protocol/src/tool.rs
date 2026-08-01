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
    /// True when one announced call may safely EXECUTE TWICE — the claim
    /// speculative execution needs (#923). A stream attempt that fails
    /// after announcing its read-only prefix re-announces it on retry, so
    /// every speculated call must tolerate a duplicate run. That is a
    /// stronger claim than [`read_only`](Self::read_only): a web search
    /// mutates no workspace state yet burns a metered API call each run,
    /// and a graph query writes catch-up state to its own database on the
    /// way to answering. Only a tool that is BOTH `read_only` and
    /// `speculation_safe` is ever run before its step commits. Defaults to
    /// false so external tools (MCP servers foremost) are never speculated
    /// unless they opt in — the failure mode of the opposite default is
    /// invisible and lands on the user's bill.
    #[serde(default)]
    pub speculation_safe: bool,
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
        assert!(
            !schema.speculation_safe,
            "an external tool that never heard of speculation must not be \
             speculated — its duplicate-run cost is invisible from here (#923)"
        );
    }

    #[test]
    fn speculation_safe_is_independent_of_read_only_on_the_wire() {
        // The two claims travel separately: a read-only tool that costs
        // quota per call (web search) declares read_only without
        // speculation_safe, and each side survives a round trip.
        let json = r#"{"name":"t","description":"d","input_schema":{},"read_only":true}"#;
        let schema: ToolSchema = serde_json::from_str(json).unwrap();
        assert!(schema.read_only && !schema.speculation_safe);
        let back: ToolSchema =
            serde_json::from_str(&serde_json::to_string(&schema).unwrap()).unwrap();
        assert_eq!(back, schema);
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
