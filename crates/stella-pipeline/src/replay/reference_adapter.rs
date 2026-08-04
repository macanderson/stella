//! The reference-engine adapter (#462): map the reference engine's
//! `stream-json` onto a structurally valid [`AgentEvent`] stream, so its runs
//! can be recorded as [`super::golden::RecordingSource::Reference`] goldens.
//!
//! `tests/reference_conformance.rs` pins why this exists: the reference wire
//! format's overlap with this protocol is exactly inverted. The events that
//! parse (`text`, `reasoning`) are structurally inert, and every event that
//! carries structural identity either lands as `Unknown` (`tool`, `result`)
//! or is rejected on its body (`stage`). A recording imported without this
//! adapter would look like a trajectory and assert nothing.
//!
//! # Fidelity posture: fail closed, adapt deliberately
//!
//! The manifest's `adapter` field names this module *because the mapping is
//! where the fidelity of a reference trajectory actually lives* — so every
//! mapping here is either total (tool pairing, the terminal event) or fails
//! closed on input it has not been taught (`stage` labels, unknown event
//! kinds). An unmapped label or a new reference event kind is a hard error
//! naming the line, never a guess: extending the table is part of adapting a
//! new recording, on purpose, in review.
//!
//! # What the adapter supplies that the wire lacks
//!
//! - **Typed stages.** The reference emits a free-text `label`; this maps the
//!   known labels onto [`StageKind`] via the module's label table
//!   (`stage_kind_for_label`).
//! - **Tool pairing.** The reference has no `call_id` on either phase, so the
//!   pairing invariant is unrepresentable on its wire. The adapter synthesizes
//!   deterministic ids (`ref-1`, `ref-2`, …) and pairs each `phase:"end"` to
//!   the oldest open call of the same tool name (the reference runs tools
//!   sequentially; FIFO is the only order its wire can support).
//! - **A terminal `Complete`.** The reference terminates with `result`; the
//!   adapter translates it, carrying the model and a zero cost (the reference
//!   does not report spend — a volatile field the differ ignores anyway).
//!
//! The reference transmits no tool arguments, so `ToolCall::input` is an empty
//! object: a reference golden asserts tool *identity and pairing*, not
//! arguments. That is the honest ceiling of its wire format.

use serde::Deserialize;
use stella_protocol::{AgentEvent, StageKind, ToolCall, ToolOutput};

/// The id recorded in a reference golden's manifest `adapter` field. Versioned
/// so a recording made by an older mapping stays attributable after the
/// mapping evolves.
pub const REFERENCE_ADAPTER: &str = "replay::reference_adapter@v1";

/// One reason a reference stream could not be adapted. Fail-closed by design —
/// see the module docs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReferenceAdapterError {
    /// A line was not valid JSON, or carried no `type` tag.
    #[error("line {line}: not a tagged reference event: {message}")]
    Malformed { line: usize, message: String },
    /// A `stage` label the mapping table has not been taught.
    #[error(
        "line {line}: unmapped reference stage label `{label}` — extend \
         stage_kind_for_label deliberately before recording"
    )]
    UnmappedStage { line: usize, label: String },
    /// An event kind this adapter has never seen.
    #[error(
        "line {line}: unknown reference event kind `{kind}` — the reference \
         emitter grew; extend the adapter deliberately before recording"
    )]
    UnknownKind { line: usize, kind: String },
    /// A `phase:"end"` with no open call of that name to pair to.
    #[error("line {line}: tool end for `{name}` with no open start")]
    UnpairedToolEnd { line: usize, name: String },
    /// The stream ended with calls still open — the reference run was torn.
    #[error("stream ended with {count} unpaired tool start(s)")]
    UnpairedToolStarts { count: usize },
}

/// The reference engine's five-kind envelope, deserialized loosely — every
/// field the adapter reads is named here, everything else is ignored.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ReferenceEvent {
    Stage {
        label: String,
    },
    Text {
        delta: String,
    },
    Reasoning {
        delta: String,
    },
    Tool {
        phase: String,
        name: String,
        #[serde(default)]
        ok: Option<bool>,
        #[serde(default, rename = "durationMs")]
        duration_ms: Option<u64>,
    },
    Result {
        #[serde(default)]
        model: Option<String>,
    },
}

/// The known reference stage labels, mapped onto the one stage vocabulary.
/// Fail-closed: an absent entry is an [`ReferenceAdapterError::UnmappedStage`],
/// and adding one is a reviewed change to this table.
fn stage_kind_for_label(label: &str) -> Option<StageKind> {
    match label {
        "evaluating the prompt" => Some(StageKind::Triage),
        "recalling context" => Some(StageKind::ContextRecall),
        "planning" => Some(StageKind::Plan),
        "writing the witness" => Some(StageKind::Witness),
        "executing" => Some(StageKind::Execute),
        "verifying" => Some(StageKind::Verify),
        "judging" => Some(StageKind::Verifier),
        "reflecting" => Some(StageKind::Reflect),
        _ => None,
    }
}

/// Adapt one reference `stream-json` document into an [`AgentEvent`] stream
/// that satisfies the protocol's structural invariants — the stream a
/// [`super::golden::GoldenTrajectory`] with a `Reference` source is built
/// from.
pub fn adapt_reference_stream(jsonl: &str) -> Result<Vec<AgentEvent>, ReferenceAdapterError> {
    let mut events = Vec::new();
    // Open calls, oldest first: (tool name, synthesized call_id).
    let mut open: Vec<(String, String)> = Vec::new();
    let mut next_id = 0usize;
    for (index, raw) in jsonl.lines().enumerate() {
        let line = index + 1;
        if raw.trim().is_empty() {
            continue;
        }
        let event: ReferenceEvent = serde_json::from_str(raw).map_err(|e| {
            match serde_json::from_str::<serde_json::Value>(raw) {
                Ok(value) => match value.get("type").and_then(|t| t.as_str()) {
                    Some(kind) => ReferenceAdapterError::UnknownKind {
                        line,
                        kind: kind.to_string(),
                    },
                    None => ReferenceAdapterError::Malformed {
                        line,
                        message: "no `type` tag".to_string(),
                    },
                },
                Err(_) => ReferenceAdapterError::Malformed {
                    line,
                    message: e.to_string(),
                },
            }
        })?;
        match event {
            ReferenceEvent::Stage { label } => {
                let name = stage_kind_for_label(&label)
                    .ok_or(ReferenceAdapterError::UnmappedStage { line, label })?;
                events.push(AgentEvent::Stage { name });
            }
            ReferenceEvent::Text { delta } => events.push(AgentEvent::Text { delta }),
            ReferenceEvent::Reasoning { delta } => events.push(AgentEvent::Reasoning { delta }),
            ReferenceEvent::Tool {
                phase,
                name,
                ok,
                duration_ms,
            } => match phase.as_str() {
                "start" => {
                    next_id += 1;
                    let call_id = format!("ref-{next_id}");
                    open.push((name.clone(), call_id.clone()));
                    events.push(AgentEvent::ToolStart {
                        call: ToolCall {
                            call_id,
                            name,
                            // The reference wire carries no arguments; a
                            // reference golden asserts identity + pairing.
                            input: serde_json::json!({}),
                        },
                    });
                }
                "end" => {
                    let position = open
                        .iter()
                        .position(|(open_name, _)| *open_name == name)
                        .ok_or_else(|| ReferenceAdapterError::UnpairedToolEnd {
                            line,
                            name: name.clone(),
                        })?;
                    let (_, call_id) = open.remove(position);
                    let output = if ok.unwrap_or(false) {
                        ToolOutput::Ok {
                            content: String::new(),
                        }
                    } else {
                        ToolOutput::Error {
                            message: "reference reported failure".to_string(),
                        }
                    };
                    events.push(AgentEvent::ToolResult {
                        call_id,
                        output,
                        duration_ms: duration_ms.unwrap_or(0),
                        speculated: false,
                    });
                }
                other => {
                    return Err(ReferenceAdapterError::Malformed {
                        line,
                        message: format!("unknown tool phase `{other}`"),
                    });
                }
            },
            ReferenceEvent::Result { model } => events.push(AgentEvent::Complete {
                model: model.unwrap_or_else(|| "reference".to_string()),
                cost_usd: 0.0,
            }),
        }
    }
    if !open.is_empty() {
        return Err(ReferenceAdapterError::UnpairedToolStarts { count: open.len() });
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unmapped_stage_label_fails_closed() {
        let err = adapt_reference_stream(r#"{"type":"stage","label":"doing a thing"}"#)
            .expect_err("unmapped label must not be guessed");
        assert!(matches!(
            err,
            ReferenceAdapterError::UnmappedStage { line: 1, .. }
        ));
    }

    #[test]
    fn an_unknown_event_kind_fails_closed() {
        let err = adapt_reference_stream(r#"{"type":"telemetry","x":1}"#)
            .expect_err("a new reference kind must be adapted deliberately");
        assert!(
            matches!(err, ReferenceAdapterError::UnknownKind { kind, .. } if kind == "telemetry")
        );
    }

    #[test]
    fn interleaved_same_name_calls_pair_fifo() {
        let stream = [
            r#"{"type":"tool","phase":"start","name":"read_file"}"#,
            r#"{"type":"tool","phase":"start","name":"read_file"}"#,
            r#"{"type":"tool","phase":"end","name":"read_file","ok":true}"#,
            r#"{"type":"tool","phase":"end","name":"read_file","ok":false}"#,
        ]
        .join("\n");
        let events = adapt_reference_stream(&stream).expect("pairs");
        let ids: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolResult { call_id, .. } => Some(call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["ref-1", "ref-2"], "oldest open call ends first");
    }

    #[test]
    fn a_torn_run_with_open_calls_is_refused() {
        let err = adapt_reference_stream(r#"{"type":"tool","phase":"start","name":"bash"}"#)
            .expect_err("an unpaired start is a torn recording");
        assert!(matches!(
            err,
            ReferenceAdapterError::UnpairedToolStarts { count: 1 }
        ));
    }

    #[test]
    fn an_end_with_no_start_is_refused() {
        let err =
            adapt_reference_stream(r#"{"type":"tool","phase":"end","name":"bash","ok":true}"#)
                .expect_err("an unpaired end is not inventable");
        assert!(matches!(
            err,
            ReferenceAdapterError::UnpairedToolEnd { line: 1, .. }
        ));
    }
}
