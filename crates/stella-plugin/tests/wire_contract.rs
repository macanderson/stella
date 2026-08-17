//! The wrapper socket's wire contract, asserted rather than promised.
//!
//! Invariant 4 says every type crossing a crate boundary round-trips through
//! `serde_json` byte-for-byte. For this module that is not one invariant among
//! several — it *is* the contract, because `doc:wrapper-socket` §3 makes the
//! serialized form primary and the Rust trait a typed view of it. A field that
//! survives `to_string` but not the trip back is a plugin that loads and then
//! silently says something else.
//!
//! What each test here pins:
//!
//! - **Byte-for-byte, both directions**, over a value of every type on the
//!   wire, including the ones a lazy fixture would leave `None`/empty.
//! - **Every variant of every closed vocabulary**, because an enum is where a
//!   rename is invisible: `serde(rename_all)` changes every spelling at once
//!   and no type check notices.
//! - **`protocol_version` on every message**, which is the whole of the
//!   additive-only story from a reader's side.
//! - **Unknown fields are refused.** A field the host does not know, at a
//!   version the host accepts, is a typo — and this crate's posture is that a
//!   declaration which quietly does nothing is worse than one that refuses.

use std::collections::BTreeMap;

use serde::Serialize;
use serde::de::DeserializeOwned;
use stella_plugin::{
    AfterTurnRequest, AfterTurnResponse, BeforeTurnRequest, BeforeTurnResponse, Continuation,
    Correction, EvidenceSet, FlipObservation, Outcome, PROTOCOL_VERSION, PluginManifest,
    PublishedSignal, RoundState, Signal, SignalValue, StageName, StopReason, TamperFinding,
    TurnOutcome, UndecidedReason, UnmetBecause, UnmetRequirement, Verdict, VerdictRule,
    VolatileContext, WrapperPoint, WrapperRequest, WrapperResponse,
};
use stella_protocol::CandidateHandle;

/// Serialize, parse, serialize again: the bytes must be identical and so must
/// the value. Returns the JSON so a caller can also assert its shape.
fn round_trip<T>(value: &T) -> String
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(value).expect("value serializes");
    let parsed: T = serde_json::from_str(&json).expect("value parses back");
    assert_eq!(&parsed, value, "value changed across the round trip");
    let again = serde_json::to_string(&parsed).expect("parsed value serializes");
    assert_eq!(json, again, "bytes changed across the round trip");
    json
}

fn before_request() -> BeforeTurnRequest {
    BeforeTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "staged-v1".into(),
        stage: StageName::Research,
        round: 2,
        goal: "make the flaky test deterministic".into(),
        candidate: Some(CandidateHandle::new("candidate-7")),
        published: vec![PublishedSignal {
            signal: Signal::Questions,
            value: SignalValue::Count(3),
        }],
    }
}

fn before_response() -> BeforeTurnResponse {
    BeforeTurnResponse {
        protocol_version: PROTOCOL_VERSION,
        context: vec![VolatileContext::new("recall", "the last run failed on I/O")],
        role: Some("triage".into()),
        scope: vec!["crates/stella-core/src/driver.rs".into()],
        publish: vec![PublishedSignal {
            signal: Signal::Conversational,
            value: SignalValue::Boolean(false),
        }],
    }
}

fn after_request() -> AfterTurnRequest {
    AfterTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "staged-v1".into(),
        round: 2,
        goal: "make the flaky test deterministic".into(),
        candidate: Some(CandidateHandle::new("candidate-7")),
        turn: TurnOutcome {
            completed: true,
            answer: "rewrote the sleep as a barrier".into(),
            tools: vec!["read_file".into(), "edit_file".into()],
            changed_files: vec!["crates/stella-core/src/driver.rs".into()],
        },
    }
}

fn evidence() -> EvidenceSet {
    EvidenceSet {
        flip: FlipObservation::Achieved,
        tamper: TamperFinding::Clean,
        measurements: BTreeMap::from([("p50".to_string(), 103), ("p99".to_string(), 128)]),
    }
}

fn unmet() -> UnmetRequirement {
    UnmetRequirement {
        requirement: "within-budget".into(),
        statement: "the benchmark is inside its budget".into(),
        because: UnmetBecause::Budget {
            check: "p50 <= 105".into(),
            reported: 118,
        },
    }
}

#[test]
fn every_message_round_trips_byte_for_byte() {
    round_trip(&before_request());
    round_trip(&before_response());
    round_trip(&after_request());
    round_trip(&AfterTurnResponse {
        protocol_version: PROTOCOL_VERSION,
        evidence: evidence(),
    });
    round_trip(&WrapperRequest::BeforeTurn(before_request()));
    round_trip(&WrapperRequest::AfterTurn(after_request()));
    round_trip(&WrapperResponse::BeforeTurn(before_response()));
    round_trip(&WrapperResponse::AfterTurn(AfterTurnResponse {
        protocol_version: PROTOCOL_VERSION,
        evidence: EvidenceSet::unobserved(),
    }));
}

#[test]
fn every_decision_type_round_trips_byte_for_byte() {
    round_trip(&Verdict::Met);
    round_trip(&Verdict::Unmet {
        unmet: vec![unmet()],
    });
    round_trip(&Verdict::Undecided {
        reason: UndecidedReason::MeasurementMissing {
            requirement: "within-budget".into(),
            measurement: "p50".into(),
        },
    });
    round_trip(&RoundState {
        holds_spent: 1,
        host_max_holds: 3,
    });
    round_trip(&Continuation::Again {
        correction: Correction {
            unmet: vec![unmet()],
            guidance: VolatileContext::new("verdict", "the p50 budget is still exceeded"),
        },
    });
    round_trip(&Continuation::Stop {
        outcome: Outcome::Unmet {
            unmet: vec![unmet()],
            stopped: StopReason::AllowanceSpent {
                spent: 2,
                allowed: 2,
            },
        },
    });
    round_trip(&Outcome::Met);
    round_trip(&Outcome::Undecided {
        reason: UndecidedReason::NoOracle,
    });
}

/// A rule read off a real manifest, not a hand-built one: the wire form of the
/// thing a human actually consented to has to survive the trip, or a remote
/// host evaluates something the install prompt never showed.
#[test]
fn a_verdict_rule_read_from_a_manifest_round_trips() {
    let manifest = PluginManifest::from_toml_str(include_str!("fixtures/perf-budget.toml"))
        .expect("the falsifier fixture loads");
    let rule = VerdictRule::from_manifest(&manifest);
    assert!(
        !rule.requirements.is_empty() && rule.oracle.is_some(),
        "the fixture is an arbiter with an oracle, or this test proves nothing"
    );
    round_trip(&rule);

    // ...and one with neither half: a steering wrapper that gathers nothing.
    round_trip(&VerdictRule::default());
}

/// Every closed vocabulary, exhaustively. A `rename_all` added or changed on
/// any of these silently renames every spelling at once, and nothing else in
/// the tree would notice.
#[test]
fn every_closed_vocabulary_is_pinned_on_both_sides() {
    for (point, wire) in [
        (WrapperPoint::BeforeTurn, "before_turn"),
        (WrapperPoint::AfterTurn, "after_turn"),
    ] {
        assert_eq!(serde_json::to_value(point).unwrap(), wire);
        assert_eq!(
            point.to_string(),
            wire,
            "Display must match the wire string"
        );
        round_trip(&point);
    }

    for (flip, wire) in [
        (FlipObservation::NotAttempted, "not-attempted"),
        (FlipObservation::Achieved, "achieved"),
        (FlipObservation::NotAchieved, "not-achieved"),
        (FlipObservation::Unsatisfiable, "unsatisfiable"),
        (FlipObservation::Unobservable, "unobservable"),
    ] {
        assert_eq!(serde_json::to_value(flip).unwrap(), wire);
        round_trip(&flip);
    }

    for tamper in [
        TamperFinding::Clean,
        TamperFinding::NotChecked,
        TamperFinding::Tampered {
            artifact: "benches/baseline.json".into(),
        },
    ] {
        round_trip(&tamper);
    }
    assert_eq!(
        serde_json::to_value(TamperFinding::NotChecked).unwrap(),
        "not-checked"
    );

    for reason in [
        UndecidedReason::NoOracle,
        UndecidedReason::Undecidable {
            requirement: "within-budget".into(),
        },
        UndecidedReason::MeasurementMissing {
            requirement: "within-budget".into(),
            measurement: "p50".into(),
        },
        UndecidedReason::UnreadableCheck {
            requirement: "within-budget".into(),
            reason: "not a comparison".into(),
        },
        UndecidedReason::FlipUnobservable,
        UndecidedReason::WitnessUnsatisfiable,
        UndecidedReason::TamperUnchecked,
    ] {
        round_trip(&reason);
    }

    for because in [
        UnmetBecause::NoFlip {
            observed: FlipObservation::NotAchieved,
        },
        UnmetBecause::Budget {
            check: "p50 <= 105".into(),
            reported: 118,
        },
        UnmetBecause::Tampered {
            artifact: "tests/witness.rs".into(),
        },
    ] {
        round_trip(&because);
    }

    for stopped in [
        StopReason::NotAnArbiter,
        StopReason::AllowanceSpent {
            spent: 3,
            allowed: 3,
        },
    ] {
        round_trip(&stopped);
    }

    for value in [SignalValue::Boolean(true), SignalValue::Count(9)] {
        round_trip(&value);
    }
}

/// The framing, spelled out: a request names its point beside its body, so a
/// non-Rust plugin reads one field to dispatch and a host can tell a
/// mis-addressed answer from a malformed one.
#[test]
fn the_envelope_names_the_point_beside_the_body() {
    let json = round_trip(&WrapperRequest::AfterTurn(after_request()));
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["point"], "after_turn");
    assert_eq!(value["body"]["protocol_version"], PROTOCOL_VERSION);
    assert_eq!(value["body"]["turn"]["completed"], true);

    let request = WrapperRequest::AfterTurn(after_request());
    assert_eq!(request.point(), WrapperPoint::AfterTurn);
    assert_eq!(request.protocol_version(), PROTOCOL_VERSION);

    let response = WrapperResponse::BeforeTurn(before_response());
    assert_eq!(response.point(), WrapperPoint::BeforeTurn);
    assert_eq!(response.protocol_version(), PROTOCOL_VERSION);
}

/// Every message carries its version — the half of "additive-only" a reader
/// can act on. A message that omits it does not parse at all, so a host is
/// never left inferring which contract it is holding.
#[test]
fn a_message_without_a_protocol_version_does_not_parse() {
    let err = serde_json::from_str::<BeforeTurnResponse>(r#"{"role": "triage"}"#)
        .expect_err("protocol_version is required");
    assert!(
        err.to_string().contains("protocol_version"),
        "the error must name the missing field, got {err}"
    );

    // The empty contribution a wrapper returns for a stage it has nothing to
    // say at still carries the version.
    assert_eq!(
        BeforeTurnResponse::empty().protocol_version,
        PROTOCOL_VERSION
    );
    round_trip(&BeforeTurnResponse::empty());
}

#[test]
fn an_unknown_field_is_refused_rather_than_ignored() {
    let err = serde_json::from_str::<BeforeTurnResponse>(
        r#"{"protocol_version": 1, "system_prompt": "you are..."}"#,
    )
    .expect_err("an unknown field must not be silently dropped");
    assert!(
        err.to_string().contains("system_prompt"),
        "the error must name the field, got {err}"
    );
}

/// The absence that is the design: a plugin can address `before_turn` and
/// `after_turn` and nothing else. `judge` and `again` are host functions, so
/// there is no message that asks a plugin for a verdict — in Rust or in any
/// other language (`doc:pipeline-as-plugins` §6).
#[test]
fn there_is_no_wire_message_that_asks_a_plugin_for_a_verdict() {
    for point in ["judge", "again", "verdict", "continuation"] {
        let json = format!(r#"{{"point": "{point}", "body": {{}}}}"#);
        assert!(
            serde_json::from_str::<WrapperRequest>(&json).is_err(),
            "\"{point}\" must not be addressable on this wire"
        );
    }
}
