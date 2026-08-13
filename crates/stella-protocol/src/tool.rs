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

/// Why a tool call failed, as a closed machine-readable set (#3145).
///
/// [`ToolOutput::Error`]'s `message` is prose written for the model to retry
/// against; this is the axis a *measurement* needs, because a per-tool error
/// rate cannot mean anything while a tool defect, model misuse, and a policy
/// refusal all count as the same failure. The variants partition failures by
/// **whose problem they are**: the model's ([`Self::InvalidInput`],
/// [`Self::NotFound`]), the policy plane's ([`Self::PermissionDenied`],
/// [`Self::RefusedByPolicy`]), the world's ([`Self::Timeout`],
/// [`Self::Environment`]), or ours ([`Self::Internal`]).
///
/// Deliberately **not** here: an `abandoned` class. A call whose turn ended
/// before it returned never produced a `ToolOutput` at all — abandonment is a
/// store-side lifecycle fact, not an error a tool can report.
///
/// Forward-compat matches [`crate::receipt::BlockKind`]: an unknown token
/// written by a newer emitter deserializes to [`Self::Other`] rather than
/// failing the whole event. Lossy on the way out for the same reason —
/// re-serializing writes `"other"`, not the original token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// The call's arguments could not be read as the tool's schema — a
    /// missing required field, a wrong type. The model's mistake: it counts
    /// against the prompt/tool menu, never against the tool.
    InvalidInput,
    /// The named subject does not exist — an unknown tool, a path or symbol
    /// that is not there to operate on.
    NotFound,
    /// The operating environment refused the operation (filesystem
    /// permissions, containment) — distinct from a *policy* refusal, which
    /// is [`Self::RefusedByPolicy`].
    PermissionDenied,
    /// A policy plane declined the call — an extension deny, a human "no",
    /// an approval that could not be asked. Working as designed, never a
    /// tool defect.
    RefusedByPolicy,
    /// The operation ran out of its time budget.
    Timeout,
    /// The world outside the tool failed it — a network error, a missing
    /// binary, a full disk.
    Environment,
    /// The tool's own defect — our bug, and the class an error-rate ceiling
    /// is actually about.
    Internal,
    /// A class this reader does not recognize (written by a newer emitter).
    /// Never constructed by this build's classifiers.
    #[serde(other)]
    Other,
}

impl ErrorClass {
    /// The wire/storage token — byte-identical to the serde `snake_case`
    /// spelling (pinned by test), so a store column and the JSON wire never
    /// disagree about what a class is called.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidInput => "invalid_input",
            Self::NotFound => "not_found",
            Self::PermissionDenied => "permission_denied",
            Self::RefusedByPolicy => "refused_by_policy",
            Self::Timeout => "timeout",
            Self::Environment => "environment",
            Self::Internal => "internal",
            Self::Other => "other",
        }
    }

    /// Parse a stored token. `None` for the empty string (the store's
    /// "unclassified" spelling); an unrecognized non-empty token reads as
    /// [`Self::Other`] rather than erroring, because this parses bytes a
    /// newer build may have written — the same tolerance
    /// `ToolCallState::parse` extends in `stella-store`.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "" => None,
            "invalid_input" => Some(Self::InvalidInput),
            "not_found" => Some(Self::NotFound),
            "permission_denied" => Some(Self::PermissionDenied),
            "refused_by_policy" => Some(Self::RefusedByPolicy),
            "timeout" => Some(Self::Timeout),
            "environment" => Some(Self::Environment),
            "internal" => Some(Self::Internal),
            _ => Some(Self::Other),
        }
    }
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
        /// Which [`ErrorClass`] this failure falls in (#3145). `None` is a
        /// declared default meaning "unclassified" — the site that built
        /// this error has not been audited into a class yet, which is
        /// distinct from any class it could be assigned. Optional and
        /// absent-when-`None` so every payload written before the field
        /// existed round-trips byte-identically (invariant #4), and so the
        /// message bytes the model sees are never perturbed by
        /// classification.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        class: Option<ErrorClass>,
    },
}

impl ToolOutput {
    /// Whether this result is the `Error` arm — the one place a consumer
    /// should branch on tool failure, instead of sniffing the content string.
    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(self, ToolOutput::Error { .. })
    }

    /// An unclassified failure — [`ToolOutput::Error`] with `class: None`.
    ///
    /// The migration-friendly constructor: a call site that has not been
    /// audited into an [`ErrorClass`] uses this and stays exactly as
    /// classified as it was before the field existed.
    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        ToolOutput::Error {
            message: message.into(),
            class: None,
        }
    }

    /// A classified failure — [`ToolOutput::Error`] carrying the
    /// [`ErrorClass`] the site has been audited into (#3145).
    #[must_use]
    pub fn classified_error(class: ErrorClass, message: impl Into<String>) -> Self {
        ToolOutput::Error {
            message: message.into(),
            class: Some(class),
        }
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
        let err = ToolOutput::error("boom");
        let classified = ToolOutput::classified_error(ErrorClass::NotFound, "no such tool");
        for variant in [ok, err, classified] {
            let json = serde_json::to_string(&variant).unwrap();
            let back: ToolOutput = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn unclassified_error_serializes_byte_identically_to_the_pre_class_shape() {
        // Invariant #4 for #3145: `class: None` must be absent on the wire,
        // so every payload written before the field existed — and every
        // unclassified one written after — is the same bytes.
        let json = serde_json::to_string(&ToolOutput::error("boom")).unwrap();
        assert_eq!(json, r#"{"error":{"message":"boom"}}"#);
    }

    #[test]
    fn old_error_payload_without_class_decodes_as_unclassified() {
        // A journal written before #3145 carries no `class` key at all.
        let json = r#"{"error":{"message":"boom"}}"#;
        let back: ToolOutput = serde_json::from_str(json).unwrap();
        assert_eq!(back, ToolOutput::error("boom"));
    }

    #[test]
    fn unknown_error_class_token_degrades_to_other_not_a_decode_failure() {
        // Forward-compat, the BlockKind discipline: a class minted by a
        // newer build must not make an older reader drop the whole event.
        let json = r#"{"error":{"message":"boom","class":"quota_exceeded"}}"#;
        let back: ToolOutput = serde_json::from_str(json).unwrap();
        assert_eq!(
            back,
            ToolOutput::classified_error(ErrorClass::Other, "boom")
        );
    }

    #[test]
    fn error_class_token_and_serde_spelling_agree() {
        // `as_str`/`parse` are the store's spelling of the class; the serde
        // token is the wire's. They are pinned to each other so a stored
        // token and a wire token can never diverge.
        for class in [
            ErrorClass::InvalidInput,
            ErrorClass::NotFound,
            ErrorClass::PermissionDenied,
            ErrorClass::RefusedByPolicy,
            ErrorClass::Timeout,
            ErrorClass::Environment,
            ErrorClass::Internal,
            ErrorClass::Other,
        ] {
            let wire = serde_json::to_value(class).unwrap();
            assert_eq!(wire, serde_json::Value::String(class.as_str().into()));
            assert_eq!(ErrorClass::parse(class.as_str()), Some(class));
        }
        assert_eq!(ErrorClass::parse(""), None, "empty is unclassified");
        assert_eq!(
            ErrorClass::parse("quota_exceeded"),
            Some(ErrorClass::Other),
            "an unknown stored token is surfaced, not dropped"
        );
    }

    #[test]
    fn is_error_reports_correctly() {
        assert!(
            !ToolOutput::Ok {
                content: String::new()
            }
            .is_error()
        );
        assert!(ToolOutput::error("").is_error());
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
