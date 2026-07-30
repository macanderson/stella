//! The wire vocabulary between the engine (running in this server) and the
//! host that drives it (e.g. Oxagen).
//!
//! Two directions, deliberately asymmetric:
//!
//! - **Outbound** ([`ServerFrame`]) is the single engine → host stream. It
//!   carries [`AgentEvent`]s for UI, plus the *reverse-RPC requests* the
//!   engine raises when it needs the host to run a governed side effect (a
//!   model completion, a tool call). Each request carries a `request_id`.
//! - **Inbound** ([`ToolResultIn`], [`ProviderResultIn`]) is the host → engine
//!   direction: the host POSTs the result of a reverse request back, keyed by
//!   that `request_id`, which unblocks the engine step waiting on it.
//!
//! This is the "reverse tool-call protocol" of ADR-033 Option B. The engine
//! never executes a tool or calls a model with ambient authority — every such
//! effect re-enters the host, which runs it through `kernel.invoke()` /
//! `@oxagen/ai` and reports back.

use serde::{Deserialize, Serialize};
use stella_core::TurnOutcome;
use stella_protocol::{AgentEvent, CompletionRequest, CompletionResult, ProviderError, ToolOutput};

/// One frame emitted by the engine toward the host over the outbound stream.
///
/// Not `Clone`: every frame is produced once and moved onto the channel, so no
/// consumer ever needs a second copy. The variants own their payloads outright
/// rather than borrowing — the port adapters in `remote.rs` pay whatever copy
/// that costs once, at construction (a completion request arrives borrowed and
/// is materialized with `CompletionRequestRef::into_owned`; a tool's `input`
/// arrives as `&Value` and is cloned there) — which is what lets a frame cross
/// from the session thread to the server runtime.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    /// A normal agent event — stream it to the UI. Never requires a response.
    Event { event: AgentEvent },
    /// The engine needs the host to run a tool call and POST the result back
    /// ([`ToolResultIn`]) keyed by `request_id`. The engine step that raised
    /// it is parked until then.
    ToolRequest {
        request_id: String,
        name: String,
        input: serde_json::Value,
    },
    /// The engine needs the host to run a model completion and POST the result
    /// back ([`ProviderResultIn`]) keyed by `request_id`. The host owns the
    /// model call (metering, gateway, BYOK); the engine only orchestrates.
    ProviderRequest {
        request_id: String,
        request: CompletionRequest,
    },
    /// Terminal frame: the turn ended. No further frames follow for this turn.
    TurnComplete { outcome: TurnOutcomeWire },
}

/// Serializable projection of [`TurnOutcome`] for the wire. `TurnOutcome` lives
/// in `stella-core` and is not itself `Serialize`, so the boundary owns the
/// mapping.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TurnOutcomeWire {
    Completed {
        text: String,
        cost_usd: f64,
    },
    Aborted {
        reason: String,
        #[serde(default)]
        cost_usd: f64,
    },
}

impl From<TurnOutcome> for TurnOutcomeWire {
    fn from(outcome: TurnOutcome) -> Self {
        match outcome {
            TurnOutcome::Completed { text, cost_usd } => {
                TurnOutcomeWire::Completed { text, cost_usd }
            }
            TurnOutcome::Aborted { reason, cost_usd } => {
                TurnOutcomeWire::Aborted { reason, cost_usd }
            }
        }
    }
}

/// Host → engine: the result of a [`ServerFrame::ToolRequest`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultIn {
    pub request_id: String,
    pub output: ToolOutput,
}

/// Host → engine: the result of a [`ServerFrame::ProviderRequest`] — either a
/// completed model response or a classified provider error.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderResultIn {
    pub request_id: String,
    #[serde(flatten)]
    pub outcome: ProviderOutcomeIn,
}

/// The success-or-error half of a [`ProviderResultIn`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ProviderOutcomeIn {
    Ok { result: CompletionResult },
    Error { error: ProviderErrorWire },
}

/// Serializable mirror of [`ProviderError`]'s taxonomy. The host classifies the
/// failure at its adapter (never re-derived here) and sends the class; the
/// engine reconstructs a real [`ProviderError`] so its retry logic behaves
/// exactly as it would with a local provider.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderErrorWire {
    Transport {
        message: String,
    },
    RateLimited {
        message: String,
        #[serde(default)]
        retry_after_ms: Option<u64>,
    },
    Auth {
        message: String,
    },
    UnknownModel {
        slug: String,
    },
    Malformed {
        message: String,
    },
    Cancelled,
    Terminal {
        message: String,
    },
}

impl From<ProviderErrorWire> for ProviderError {
    fn from(wire: ProviderErrorWire) -> Self {
        match wire {
            ProviderErrorWire::Transport { message } => ProviderError::Transport(message),
            ProviderErrorWire::RateLimited {
                message,
                retry_after_ms,
            } => ProviderError::RateLimited {
                message,
                retry_after_ms,
            },
            ProviderErrorWire::Auth { message } => ProviderError::Auth(message),
            ProviderErrorWire::UnknownModel { slug } => ProviderError::UnknownModel { slug },
            ProviderErrorWire::Malformed { message } => ProviderError::Malformed(message),
            ProviderErrorWire::Cancelled => ProviderError::Cancelled,
            ProviderErrorWire::Terminal { message } => ProviderError::Terminal(message),
        }
    }
}

impl From<&ProviderError> for ProviderErrorWire {
    fn from(err: &ProviderError) -> Self {
        match err {
            ProviderError::Transport(m) => ProviderErrorWire::Transport { message: m.clone() },
            ProviderError::RateLimited {
                message,
                retry_after_ms,
            } => ProviderErrorWire::RateLimited {
                message: message.clone(),
                retry_after_ms: *retry_after_ms,
            },
            ProviderError::Auth(m) => ProviderErrorWire::Auth { message: m.clone() },
            ProviderError::UnknownModel { slug } => {
                ProviderErrorWire::UnknownModel { slug: slug.clone() }
            }
            ProviderError::Malformed(m) => ProviderErrorWire::Malformed { message: m.clone() },
            ProviderError::Cancelled => ProviderErrorWire::Cancelled,
            ProviderError::Terminal(m) => ProviderErrorWire::Terminal { message: m.clone() },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aborted_turn_wire_retains_settled_cost() {
        let wire = TurnOutcomeWire::from(TurnOutcome::Aborted {
            reason: "budget exceeded".into(),
            cost_usd: 1.25,
        });

        assert_eq!(
            wire,
            TurnOutcomeWire::Aborted {
                reason: "budget exceeded".into(),
                cost_usd: 1.25,
            }
        );
        let json = serde_json::to_value(wire).expect("wire outcome serializes");
        assert_eq!(json["cost_usd"], 1.25);
    }

    /// The host parses these tags and field names by hand, in another
    /// language. Nothing else in this crate pins them, so a rename that reads
    /// as a refactor here silently breaks every client — and `docs/design/
    /// serve-surface.md` names exactly this ("the single most dangerous drift
    /// in this document"). Assert the wire shape, not the Rust shape.
    #[test]
    fn the_outbound_frame_tags_and_field_names_are_the_wire_contract() {
        let tool = serde_json::to_value(ServerFrame::ToolRequest {
            request_id: "tool-0".to_string(),
            name: "echo".to_string(),
            input: serde_json::json!({ "text": "hi" }),
        })
        .unwrap();
        assert_eq!(tool["type"], "tool_request");
        assert_eq!(tool["request_id"], "tool-0");
        assert_eq!(tool["name"], "echo");
        assert_eq!(tool["input"]["text"], "hi");

        let provider = serde_json::to_value(ServerFrame::ProviderRequest {
            request_id: "prov-0".to_string(),
            request: CompletionRequest {
                messages: Vec::new(),
                max_output_tokens: None,
                temperature: None,
                effort: None,
                reasoning: None,
                params: None,
                tools: Vec::new(),
            },
        })
        .unwrap();
        assert_eq!(provider["type"], "provider_request");
        assert_eq!(provider["request_id"], "prov-0");
        assert!(provider["request"].is_object(), "{provider}");

        let event = serde_json::to_value(ServerFrame::Event {
            event: AgentEvent::Text {
                delta: "hello".to_string(),
            },
        })
        .unwrap();
        assert_eq!(event["type"], "event");
        assert_eq!(
            event["event"]["type"], "text",
            "the agent event keeps its own `type`, nested under `event`"
        );

        let done = serde_json::to_value(ServerFrame::TurnComplete {
            outcome: TurnOutcomeWire::Completed {
                text: "done".to_string(),
                cost_usd: 0.5,
            },
        })
        .unwrap();
        assert_eq!(done["type"], "turn_complete");
        assert_eq!(done["outcome"]["status"], "completed");
    }

    /// The two inbound bodies, in the exact shape a host POSTs them. The
    /// provider result's `status` is flattened *alongside* `request_id`, not
    /// nested — a detail no reader of the Rust type would guess.
    #[test]
    fn the_inbound_bodies_parse_from_the_shapes_a_host_posts() {
        let tool: ToolResultIn = serde_json::from_value(serde_json::json!({
            "request_id": "tool-0",
            "output": { "ok": { "content": "echoed" } },
        }))
        .expect("tool result body");
        assert_eq!(tool.request_id, "tool-0");
        assert!(matches!(tool.output, ToolOutput::Ok { .. }));

        let ok: ProviderResultIn = serde_json::from_value(serde_json::json!({
            "request_id": "prov-0",
            "status": "ok",
            "result": {
                "text": "done",
                "usage": { "reported": true, "input_tokens": 1, "output_tokens": 2 },
                "model": "mock",
                "cost_usd": 0.0,
            },
        }))
        .expect("provider ok body");
        assert_eq!(ok.request_id, "prov-0");
        let ProviderOutcomeIn::Ok { result } = ok.outcome else {
            panic!("status `ok` must select the success arm");
        };
        assert_eq!(result.text, "done");

        let failed: ProviderResultIn = serde_json::from_value(serde_json::json!({
            "request_id": "prov-1",
            "status": "error",
            "error": { "kind": "rate_limited", "message": "slow down" },
        }))
        .expect("provider error body");
        let ProviderOutcomeIn::Error { error } = failed.outcome else {
            panic!("status `error` must select the failure arm");
        };
        let err: ProviderError = error.into();
        assert!(
            err.is_retryable(),
            "the host's classification must survive the wire: {err}"
        );
    }

    /// The host classifies a failure once, at its own adapter, and the engine's
    /// retry logic must behave exactly as it would with a local provider. That
    /// only holds if every class survives the round trip — a variant that maps
    /// to the wrong one silently changes whether a failed call is retried.
    #[test]
    fn every_provider_error_class_survives_the_round_trip() {
        let cases = [
            ProviderError::Transport("dns".into()),
            ProviderError::RateLimited {
                message: "429".into(),
                retry_after_ms: Some(1500),
            },
            ProviderError::RateLimited {
                message: "429, no hint".into(),
                retry_after_ms: None,
            },
            ProviderError::Auth("bad key".into()),
            ProviderError::UnknownModel {
                slug: "glm-5.2".into(),
            },
            ProviderError::Malformed("truncated".into()),
            ProviderError::Cancelled,
            ProviderError::Terminal("refused".into()),
        ];
        for original in cases {
            let wire = ProviderErrorWire::from(&original);
            let json = serde_json::to_string(&wire).expect("wire error serializes");
            let parsed: ProviderErrorWire =
                serde_json::from_str(&json).expect("wire error parses back");
            let back: ProviderError = parsed.into();
            assert_eq!(
                back.to_string(),
                original.to_string(),
                "class or payload changed across the wire: {json}"
            );
            assert_eq!(
                back.is_retryable(),
                original.is_retryable(),
                "retry classification changed across the wire: {json}"
            );
        }
    }

    /// `retry_after_ms` is the one optional field on the error wire, and it
    /// rides `serde(default)` — a host that omits it must not fail the body.
    #[test]
    fn a_rate_limit_without_a_backoff_hint_still_parses() {
        let wire: ProviderErrorWire =
            serde_json::from_value(serde_json::json!({ "kind": "rate_limited", "message": "429" }))
                .expect("the hint is optional");
        let ProviderErrorWire::RateLimited { retry_after_ms, .. } = wire else {
            panic!("wrong variant");
        };
        assert_eq!(retry_after_ms, None);
    }

    #[test]
    fn legacy_aborted_turn_without_cost_deserializes_as_zero() {
        let wire: TurnOutcomeWire = serde_json::from_value(serde_json::json!({
            "status": "aborted",
            "reason": "old client"
        }))
        .expect("legacy aborted wire shape remains readable");

        assert_eq!(
            wire,
            TurnOutcomeWire::Aborted {
                reason: "old client".into(),
                cost_usd: 0.0,
            }
        );
    }
}
