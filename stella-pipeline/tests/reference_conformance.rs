//! The adapter contract a **reference** engine has to satisfy before its runs
//! can be recorded as golden trajectories — pinned executably.
//!
//! Issue #462 asks for reference trajectories recorded from the TS engine. The
//! engine is reachable, but it does not speak this protocol: its
//! `--output-format stream-json` emits a five-kind, untyped envelope
//! (`stage`/`text`/`reasoning`/`tool`/`result`) while `AgentEvent` is a
//! tagged enum of ~34 variants. Recording it therefore needs an adapter, and
//! this file specifies what that adapter must fix.
//!
//! The samples below are the reference emitter's wire shapes transcribed from
//! its source — they are **not** a recording, and nothing here is dressed up as
//! one. Their only job is to hold the gap still so it cannot be forgotten or
//! quietly papered over.
//!
//! # The finding this file exists to pin
//!
//! The overlap between the two formats is exactly inverted from what a golden
//! replay needs. The reference events that deserialize into a *typed*
//! `AgentEvent` (`text`, `reasoning`) are precisely the ones
//! [`event_signature`] treats as structurally inert; every event that carries
//! structural identity — the stage kind, the tool call pairing, the terminator
//! — lands in one of two buckets, neither of which is usable:
//!
//!   - `tool` and `result` are tags this protocol has never heard of, so
//!     forward compatibility preserves them as `AgentEvent::Unknown`.
//!     Preserved is not understood: an `Unknown` carries no structural
//!     identity and is excluded from the structural diff entirely.
//!   - `stage` collides with a tag this protocol *does* define, so it reaches
//!     the typed decoder and is rejected on its body. That rejection is the
//!     forward-compat rule working as designed, not a gap in it.
//!
//! A half-imported reference recording would thus be *all* volatile content
//! and *no* structure: it would parse into something that looks like a
//! trajectory and asserts nothing. That is why [`GoldenTrajectory`] gates on
//! load instead of trusting a recording.

use stella_pipeline::replay::event_signature;
use stella_pipeline::replay::golden::{GoldenError, GoldenTrajectory};
use stella_protocol::AgentEvent;

/// One line of the reference engine's `stream-json` output, with the reason it
/// cannot serve as an `AgentEvent`.
struct ReferenceLine {
    /// The reference engine's wire shape.
    json: &'static str,
    /// What an adapter has to supply for this line to become an `AgentEvent`.
    adapter_must: &'static str,
}

/// Every event kind the reference engine's `stream-json` emitter produces.
fn reference_stream() -> Vec<ReferenceLine> {
    vec![
        ReferenceLine {
            // The reference emits a human-readable label and drops the stage
            // kind entirely; `AgentEvent::Stage` needs a typed `StageKind`.
            json: r#"{"type":"stage","label":"evaluating the prompt","detail":"single"}"#,
            adapter_must: "map the reference's free-text stage label onto a typed StageKind",
        },
        ReferenceLine {
            json: r#"{"type":"text","delta":"Reading the file."}"#,
            adapter_must: "nothing — this one already parses, which is the hazard",
        },
        ReferenceLine {
            json: r#"{"type":"reasoning","delta":"The test asserts..."}"#,
            adapter_must: "nothing — this one already parses, which is the hazard",
        },
        ReferenceLine {
            // No `call_id` on either phase, so `validate_stream`'s tool-pairing
            // invariant is not merely unmet — it is unrepresentable.
            json: r#"{"type":"tool","phase":"start","name":"read_file"}"#,
            adapter_must: "synthesize a call_id and split phase=start into tool_start",
        },
        ReferenceLine {
            json: r#"{"type":"tool","phase":"end","name":"read_file","ok":true,"durationMs":12}"#,
            adapter_must: "pair phase=end back to its call_id and map ok onto a ToolOutput",
        },
        ReferenceLine {
            // The reference terminates with `result`; this protocol terminates
            // with `complete`, and `validate_stream` requires it to be last.
            json: r#"{"type":"result","ok":true,"text":"done","steps":3,"model":"m"}"#,
            adapter_must: "translate the result envelope into a terminal Complete event",
        },
    ]
}

/// The kinds that deserialize into a *typed* variant — recorded here so the
/// list cannot grow silently. Both are volatile-content events.
const ALREADY_PARSES: [&str; 2] = ["text", "reasoning"];

/// The kinds this protocol also names, so they reach the typed decoder and are
/// rejected on their *body*. `stage` collides by name only: the reference
/// sends a free-text `label`, `AgentEvent::Stage` needs a typed `StageKind`.
///
/// This is the forward-compat rule's other branch, and the reason the rule is
/// scoped to tags alone: a recognized tag carrying a foreign body is a real
/// mismatch that must stay loud, not a version skew to be absorbed.
const REJECTED_ON_BODY: [&str; 1] = ["stage"];

fn kind(json: &str) -> String {
    let value: serde_json::Value = serde_json::from_str(json).expect("sample is valid JSON");
    value["type"]
        .as_str()
        .expect("sample is tagged")
        .to_string()
}

#[test]
fn the_reference_wire_format_is_not_this_protocol() {
    let mut foreign = Vec::new();
    let mut rejected = Vec::new();
    for line in reference_stream() {
        let parsed = serde_json::from_str::<AgentEvent>(line.json);
        let kind = kind(line.json);
        if ALREADY_PARSES.contains(&kind.as_str()) {
            assert!(
                parsed.is_ok_and(|e| !e.is_unknown()),
                "`{kind}` was expected to parse as a typed variant; if it \
                 stopped, this file's account of the gap is stale: {}",
                line.json
            );
        } else if REJECTED_ON_BODY.contains(&kind.as_str()) {
            assert!(
                parsed.is_err(),
                "`{kind}` is a tag this protocol knows, so a foreign body must \
                 be a hard error rather than being absorbed as Unknown \
                 (adapter must: {})",
                line.adapter_must
            );
            rejected.push(kind);
        } else {
            // A tag this protocol has never heard of. It is preserved rather
            // than rejected — but preserved is not understood: it lands as
            // `Unknown`, which carries no structural identity.
            let event = parsed.expect("an unrecognized tag must be preserved, not rejected");
            assert!(
                event.is_unknown(),
                "`{kind}` deserialized into a typed variant — the reference \
                 format and this protocol have converged, so the adapter \
                 contract needs revisiting (adapter must: {})",
                line.adapter_must
            );
            foreign.push(kind);
        }
    }
    assert_eq!(
        rejected,
        vec!["stage"],
        "the name-collision case must stay accounted for"
    );
    assert_eq!(
        foreign,
        vec!["tool", "tool", "result"],
        "every structurally-identifying reference event must be accounted for"
    );
}

#[test]
fn no_reference_event_yields_structural_identity() {
    // The damning half of the finding, and it survives the open enum intact.
    // Whether a reference line parses as a typed variant (`text`/`reasoning`)
    // or is preserved as `Unknown` (`tool`/`result`), the signature it
    // produces is one the differ treats as inert. Importing a reference
    // recording therefore yields a trajectory that asserts nothing about
    // stages, tool pairing, or termination — exactly as before.
    for line in reference_stream() {
        let Ok(event) = serde_json::from_str::<AgentEvent>(line.json) else {
            continue;
        };
        let signature = event_signature(&event);
        let inert =
            matches!(signature.as_str(), "text" | "reasoning") || signature.starts_with("unknown:");
        assert!(
            inert,
            "a reference event that parses must be structurally inert, got `{signature}`"
        );
    }
}

#[test]
fn a_reference_recording_is_rejected_as_a_golden_rather_than_half_imported() {
    let recording: String = reference_stream()
        .iter()
        .map(|l| format!("{}\n", l.json))
        .collect();
    let manifest = r#"{
        "task_id": "reference_smoke",
        "description": "a reference-engine run recorded verbatim",
        "source": { "kind": "reference", "engine": "ts-engine", "adapter": "none" },
        "event_count": 6
    }"#;
    match GoldenTrajectory::parse("reference_smoke", manifest, &recording) {
        Err(GoldenError::Recording { task_id, .. }) => assert_eq!(task_id, "reference_smoke"),
        other => panic!(
            "a raw reference recording must be refused with a typed error, got {other:?} — \
             silently accepting one is the failure mode this gate exists to prevent"
        ),
    }
}

#[test]
fn every_reference_event_kind_has_a_stated_adapter_obligation() {
    // The contract is only useful if it stays complete: a new reference event
    // kind added to the sample set without a stated obligation fails here.
    for line in reference_stream() {
        assert!(
            !line.adapter_must.is_empty(),
            "reference event `{}` has no stated adapter obligation",
            kind(line.json)
        );
    }
}
