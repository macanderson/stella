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
use stella_pipeline::ports::ScopeDecision;
use stella_protocol::{
    AgentEvent, CompletionRequest, CompletionResult, ModelCallRole, ProviderError, ScopeProposal,
    ToolOutput,
};

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
    ///
    /// `provider_id` and `role` say **which agent** is asking (#1297). One
    /// turn used to mean one model, so neither was needed; a judged goal run
    /// and a turn that spawns sub-agents both put several agents behind one
    /// turn id, and a host that cannot tell them apart cannot route the verifier
    /// to a different family — which is the entire point of an independent
    /// verifier. Both are additive: a host that ignores them behaves exactly as
    /// before, answering every request with the turn's one model.
    ProviderRequest {
        request_id: String,
        /// The provider the caller asked to serve THIS call: the turn's own
        /// `provider_id`, or the override on its goal/sub-agent block.
        provider_id: String,
        /// What the call is for, so a host can route by role rather than by
        /// string-matching a provider id.
        role: ModelCallRole,
        request: CompletionRequest,
    },
    /// The turn reached a step boundary and is **holding** there, because
    /// `POST /v1/turns/{id}/pause` asked it to (#932).
    ///
    /// Emitted once per hold, from inside the pause gate, at the moment the
    /// turn actually parks — which is not the moment the POST was accepted.
    /// That difference is the whole point: the POST says "hold at the next
    /// boundary", the frame says "the boundary was reached and nothing further
    /// will happen until you release it".
    ///
    /// Because it is an ordinary numbered frame it lands in the retained ring
    /// like any other, so a subscriber that drops during a hold and reconnects
    /// with `?after=` re-learns the hold from the replay. Without it a
    /// reconnecting host cannot tell a held turn from a slow one — the stream
    /// is silent either way.
    ///
    /// `reason` is whatever the pausing host wrote on the POST body, and
    /// `null` when it wrote none: the client that reconnects need not be the
    /// process that paused.
    TurnHeld { reason: Option<String> },
    /// A pipeline-driven turn (`pipeline` on `POST /v1/turns`, #1288) reached
    /// its scope-review gate and is parked on the host's decision, keyed by
    /// `request_id` exactly like [`ServerFrame::ToolRequest`] /
    /// [`ServerFrame::ProviderRequest`]. The host answers with
    /// [`ScopeReviewResultIn`] on `POST /v1/turns/{id}/approve`.
    ///
    /// This is the pipeline's `ApprovalGate::review` (L-E5), remoted rather
    /// than answered in-process — the API's counterpart to the CLI's
    /// interactive scope-review card. Nothing else about a served turn
    /// changes: the same `AgentEvent::ScopeReview` the pipeline already
    /// emits still carries the human-readable proposal on the ordinary event
    /// stream, so this frame is only the request half of the round trip.
    ScopeReviewRequest {
        request_id: String,
        proposal: ScopeProposal,
    },
    /// The hold announced by [`ServerFrame::TurnHeld`] is over and the turn is
    /// proceeding (#932). Emitted once, paired with the `TurnHeld` before it.
    ///
    /// Carries nothing, deliberately: the gate is released by
    /// `POST /resume`, by a cancel, and by the turn's entry being torn down,
    /// and from inside the gate those are indistinguishable. A host learns
    /// which it was from what follows — more frames, or `turn_complete`.
    TurnReleased,
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
            // The wire frame deliberately does not carry `kind` yet — a
            // serve-surface field is a spec + conformance change, tracked
            // separately. The reason string still travels.
            TurnOutcome::Aborted {
                reason, cost_usd, ..
            } => TurnOutcomeWire::Aborted { reason, cost_usd },
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

/// Host → engine: the human's decision on a [`ServerFrame::ScopeReviewRequest`].
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeReviewResultIn {
    pub request_id: String,
    pub decision: ScopeDecisionWire,
}

/// Wire mirror of [`stella_pipeline::ports::ScopeDecision`]. That type lives in
/// `stella-pipeline` and is not itself `Serialize`/`Deserialize` — this crate
/// owns the wire mapping, exactly as [`TurnOutcomeWire`] owns `TurnOutcome`'s.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum ScopeDecisionWire {
    /// Execute the plan as proposed.
    Approve,
    /// Execute only the steps at these indices into the proposed step list.
    Trim {
        keep_steps: Vec<usize>,
    },
    /// Re-plan with the reviewer's own words folded into the next attempt. An
    /// empty `note` reads as [`ScopeDecision::Abort`] — the same collapse
    /// [`stella_pipeline::ports::StdioApprovalGate`] makes for a bare "no".
    Revise {
        note: String,
    },
    Abort,
}

impl From<ScopeDecisionWire> for ScopeDecision {
    fn from(wire: ScopeDecisionWire) -> Self {
        match wire {
            ScopeDecisionWire::Approve => ScopeDecision::Approve,
            ScopeDecisionWire::Trim { keep_steps } => ScopeDecision::Trim { keep_steps },
            ScopeDecisionWire::Revise { note } => ScopeDecision::Revise { note },
            ScopeDecisionWire::Abort => ScopeDecision::Abort,
        }
    }
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

/// Host → engine: a batch of streamed fragments for an in-flight
/// [`ServerFrame::ProviderRequest`] — the incremental half of a provider
/// answer (#1165), POSTed to `POST /v1/turns/{id}/provider-delta` and keyed by
/// the same `request_id` the terminating [`ProviderResultIn`] answers.
///
/// Strictly optional: a host that cannot stream never POSTs one and keeps
/// exactly its old behavior. Strictly advisory, with the same contract as
/// `ToolCallObserver::text_delta`: the definitive text is the
/// `CompletionResult` on the eventual provider result — a retried model call
/// re-streams from the start with no reset marker, and consumers replace the
/// preview with the authoritative `Text` event when it lands.
///
/// A batch rather than one fragment per POST, because a per-token HTTP
/// request would cost more than the latency it buys: the host accumulates
/// whatever chunking its own stream hands it and flushes on its own cadence.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderDeltaIn {
    pub request_id: String,
    /// The fragments, in stream order. Must not be empty — an empty batch
    /// carries no information and is refused at the route.
    pub deltas: Vec<ProviderDelta>,
}

/// One streamed fragment of an in-flight model completion.
///
/// Text and thinking are distinct variants rather than one string because the
/// two must never be confused downstream: thinking renders as collapsible,
/// visibly-secondary content while answer text is the reply — the same
/// separation `ToolCallObserver` keeps between `text_delta` and
/// `reasoning_delta`, carried across the wire.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderDelta {
    /// A fragment of user-visible answer text.
    Text { text: String },
    /// A fragment of thinking/chain-of-thought content.
    Reasoning { text: String },
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
        /// Accounting a host's dying stream had already observed. Carried
        /// across the wire so a remote provider loses no more usage than a
        /// local one does; `serde(default)` keeps hosts that predate the
        /// field (and the many failures with nothing to report) valid.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        partial: Option<stella_protocol::PartialUsage>,
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
            ProviderErrorWire::Transport { message, partial } => {
                let error = ProviderError::transport(message);
                match partial {
                    Some(partial) => error.with_partial(partial),
                    None => error,
                }
            }
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
            ProviderError::Transport { message, partial } => ProviderErrorWire::Transport {
                message: message.clone(),
                partial: *partial,
            },
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
            kind: stella_core::AbortKind::DeliberateStop,
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
    /// as a refactor here silently breaks every client — and `docs/spec/
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
            provider_id: "mock".to_string(),
            role: ModelCallRole::Worker,
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

        let held = serde_json::to_value(ServerFrame::TurnHeld {
            reason: Some("waiting on a human".to_string()),
        })
        .unwrap();
        assert_eq!(held["type"], "turn_held");
        assert_eq!(held["reason"], "waiting on a human");

        // The reason is always *present*, null rather than absent, so a host
        // reads one shape whether or not the pausing client wrote one.
        let bare = serde_json::to_value(ServerFrame::TurnHeld { reason: None }).unwrap();
        assert_eq!(bare["type"], "turn_held");
        assert!(
            bare.get("reason").is_some_and(serde_json::Value::is_null),
            "a reasonless hold still carries the key: {bare}"
        );

        let released = serde_json::to_value(ServerFrame::TurnReleased).unwrap();
        assert_eq!(released["type"], "turn_released");
        assert_eq!(
            released.as_object().map(serde_json::Map::len),
            Some(1),
            "a release says only that it happened: {released}"
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

    /// The delta body, in the exact shape a streaming host POSTs it. The
    /// `kind` tag is what keeps answer text and thinking apart on the wire —
    /// a host that omits it must fail the parse rather than have its
    /// deliberation published as the model's answer.
    #[test]
    fn the_provider_delta_body_parses_and_keeps_text_and_reasoning_apart() {
        let posted: ProviderDeltaIn = serde_json::from_value(serde_json::json!({
            "request_id": "prov-0",
            "deltas": [
                { "kind": "reasoning", "text": "weighing…" },
                { "kind": "text", "text": "Hel" },
                { "kind": "text", "text": "lo" },
            ],
        }))
        .expect("provider delta body");
        assert_eq!(posted.request_id, "prov-0");
        assert_eq!(posted.deltas.len(), 3);
        assert!(matches!(
            posted.deltas[0],
            ProviderDelta::Reasoning { ref text } if text == "weighing…"
        ));
        assert!(matches!(
            posted.deltas[1],
            ProviderDelta::Text { ref text } if text == "Hel"
        ));

        // An untagged fragment must not parse: guessing the channel is how
        // thinking gets published as the answer.
        assert!(
            serde_json::from_value::<ProviderDelta>(serde_json::json!({ "text": "hi" })).is_err(),
            "a fragment without a `kind` tag must be refused"
        );
    }

    /// The host classifies a failure once, at its own adapter, and the engine's
    /// retry logic must behave exactly as it would with a local provider. That
    /// only holds if every class survives the round trip — a variant that maps
    /// to the wrong one silently changes whether a failed call is retried.
    #[test]
    fn every_provider_error_class_survives_the_round_trip() {
        let cases = [
            ProviderError::transport("dns"),
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

    /// The scope-review request/result pair (#1288), in the exact shape a
    /// client parses/posts — same discipline as
    /// `the_outbound_frame_tags_and_field_names_are_the_wire_contract`.
    #[test]
    fn the_scope_review_frame_and_its_result_keep_their_wire_shape() {
        let request = serde_json::to_value(ServerFrame::ScopeReviewRequest {
            request_id: "scope-0".to_string(),
            proposal: ScopeProposal {
                summary: "add a widget".to_string(),
                steps: vec!["write the widget".to_string()],
                estimated_files: 1,
                estimated_cost_usd: Some(0.02),
                ..Default::default()
            },
        })
        .unwrap();
        assert_eq!(request["type"], "scope_review_request");
        assert_eq!(request["request_id"], "scope-0");
        assert_eq!(request["proposal"]["summary"], "add a widget");

        let approve: ScopeReviewResultIn = serde_json::from_value(serde_json::json!({
            "request_id": "scope-0",
            "decision": { "decision": "approve" },
        }))
        .expect("an approve body parses");
        assert!(matches!(approve.decision, ScopeDecisionWire::Approve));

        let trim: ScopeReviewResultIn = serde_json::from_value(serde_json::json!({
            "request_id": "scope-0",
            "decision": { "decision": "trim", "keep_steps": [0, 2] },
        }))
        .expect("a trim body parses");
        assert!(matches!(
            trim.decision,
            ScopeDecisionWire::Trim { ref keep_steps } if keep_steps == &[0, 2]
        ));

        let revise: ScopeDecision = ScopeDecisionWire::Revise {
            note: "smaller, please".to_string(),
        }
        .into();
        assert_eq!(
            revise,
            ScopeDecision::Revise {
                note: "smaller, please".to_string()
            }
        );

        let abort: ScopeDecision = ScopeDecisionWire::Abort.into();
        assert_eq!(abort, ScopeDecision::Abort);
    }
}
