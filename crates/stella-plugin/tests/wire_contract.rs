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
    AfterTurnRequest, AfterTurnResponse, BeforeTurnRequest, BeforeTurnResponse, CandidateGrant,
    Continuation, Correction, EvidenceProvenance, EvidenceSet, FlipObservation, ObservedEvidence,
    Outcome, PROTOCOL_VERSION, PluginManifest, PublishedSignal, RoundState, Signal, SignalValue,
    StageName, StopReason, TamperFinding, TestBaseline, TestPlan, TurnOutcome, UndecidedReason,
    UnmetBecause, UnmetRequirement, Verdict, VerdictRule, VolatileContext, WrapperPoint,
    WrapperRequest, WrapperResponse,
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

/// The grant a host mints for a live candidate: the handle, the root the
/// plugin works in, and the test the host would run there.
fn grant() -> CandidateGrant {
    CandidateGrant::new(CandidateHandle::new("candidate-7"), "/tmp/candidate-7").with_test(
        TestPlan::new("pytest", vec!["tests/test_flip.py".into(), "-q".into()])
            .with_baseline(TestBaseline::Failed),
    )
}

fn before_request() -> BeforeTurnRequest {
    BeforeTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "staged-v1".into(),
        stage: StageName::Research,
        round: 2,
        goal: "make the flaky test deterministic".into(),
        candidate: Some(grant()),
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
        stage: Some(StageName::Verify),
        round: 2,
        goal: "make the flaky test deterministic".into(),
        candidate: Some(grant()),
        turn: TurnOutcome {
            completed: true,
            answer: "rewrote the sleep as a barrier".into(),
            tools: Some(vec!["read_file".into(), "edit_file".into()]),
            changed_files: Some(vec!["crates/stella-core/src/driver.rs".into()]),
        },
    }
}

/// What a plugin may report — no tamper finding, because that is the host's
/// (#3499).
fn observed() -> ObservedEvidence {
    ObservedEvidence {
        flip: FlipObservation::Achieved,
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
        evidence: observed(),
    });
    // The merged set is not a message — the host assembles it — but it crosses
    // a crate boundary to `judge`, so invariant 4 covers it all the same.
    round_trip(&EvidenceSet::from_observed(
        observed(),
        TamperFinding::Clean,
    ));
    round_trip(&WrapperRequest::BeforeTurn(before_request()));
    round_trip(&WrapperRequest::AfterTurn(after_request()));
    round_trip(&WrapperResponse::BeforeTurn(before_response()));
    round_trip(&WrapperResponse::AfterTurn(AfterTurnResponse {
        protocol_version: PROTOCOL_VERSION,
        evidence: ObservedEvidence::nothing(),
    }));
}

#[test]
fn every_decision_type_round_trips_byte_for_byte() {
    round_trip(&Verdict::Met {
        evidence: EvidenceProvenance::PluginReported,
    });
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
    round_trip(&Outcome::Met {
        evidence: EvidenceProvenance::PluginReported,
    });
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

/// **The #3498 witness.** Everything a plugin needs to act on the candidate
/// arrives *in the request*: the root it works in and the test invocation it
/// runs, both readable by a program that speaks nothing but JSON.
///
/// Failing before the change for the plainest reason — `candidate` was a bare
/// `CandidateHandle`, a string whose only implementation was in-process async
/// Rust over a `&dyn` port, so `body.candidate.root` and
/// `body.candidate.test.program` did not exist. Track C's three reference
/// plugins took the test command from `[runtime] env` instead, which is the
/// socket's own rule ("every capability arrives in the request") being bent by
/// the socket.
#[test]
fn a_plugin_reads_the_candidate_root_and_its_test_out_of_the_request() {
    let json = round_trip(&WrapperRequest::AfterTurn(after_request()));
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    let candidate = &value["body"]["candidate"];

    assert_eq!(candidate["handle"], "candidate-7");
    assert_eq!(
        candidate["root"], "/tmp/candidate-7",
        "a plugin that runs a test suite needs a directory, and it must be in the request"
    );
    assert_eq!(candidate["test"]["program"], "pytest");
    assert_eq!(
        candidate["test"]["args"],
        serde_json::json!(["tests/test_flip.py", "-q"]),
        "argv, never a shell string — the host parsed it before minting the grant"
    );
    assert_eq!(
        candidate["test"]["baseline"], "failed",
        "the red half of the flip travels with the plan rather than in an env var"
    );

    // The four baselines are distinct answers, not a bool with a spare state:
    // `unobserved` is what a timed-out or unspawnable run reports, and reading
    // it as red is the #860 bug arriving on the wire.
    for (baseline, wire) in [
        (TestBaseline::NotRun, "not-run"),
        (TestBaseline::Passed, "passed"),
        (TestBaseline::Failed, "failed"),
        (TestBaseline::Unobserved, "unobserved"),
    ] {
        assert_eq!(serde_json::to_value(baseline).unwrap(), wire);
        round_trip(&baseline);
    }
    assert_eq!(TestBaseline::default(), TestBaseline::NotRun);

    // A host with no test to give says so, rather than leaving a plugin to
    // guess at one.
    let bare = CandidateGrant::new(CandidateHandle::new("candidate-1"), "/tmp/candidate-1");
    assert_eq!(bare.test, None);
    assert_eq!(
        round_trip(&bare),
        r#"{"handle":"candidate-1","root":"/tmp/candidate-1"}"#
    );
}

/// **The #3524 witness for the after-turn stage.** A `[wrapper]` program may
/// declare several stages per round; before this field, the request that
/// carries a round's *evidence* named none of them, so a plugin could not tell
/// which stage it was reporting on and a host could not correlate two evidence
/// sets from one round. `before_turn` has named its stage since #3388.
///
/// Failing before the change for the plainest reason — `AfterTurnRequest` had
/// no `stage` field, so `body.stage` did not exist and the fixture did not
/// compile.
#[test]
fn an_after_turn_request_names_the_stage_its_evidence_is_about() {
    let json = round_trip(&WrapperRequest::AfterTurn(after_request()));
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        value["body"]["stage"], "verify",
        "the evidence half of a round must say which stage produced it"
    );

    // The correlation the field exists for: one round, two messages, one
    // stage — the join a host needs to pair a contribution with its evidence.
    let before = before_request();
    let after = AfterTurnRequest {
        stage: Some(before.stage),
        round: before.round,
        ..after_request()
    };
    assert_eq!(after.stage, Some(before.stage));
    assert_eq!(after.round, before.round);

    // A host that runs no stage program omits the field rather than naming a
    // stand-in boundary, and the omission survives the round trip as `None`.
    let unstaged = AfterTurnRequest {
        stage: None,
        ..after_request()
    };
    let json = round_trip(&unstaged);
    assert!(
        !json.contains("\"stage\""),
        "no stage program means no stage key, not a default one: {json}"
    );
    assert_eq!(
        serde_json::from_str::<AfterTurnRequest>(&json)
            .expect("a message without a stage still parses")
            .stage,
        None
    );
}

/// **The #3524 witness for [`VolatileContext`].** Invariant 7's rule — a
/// contribution rides *after* the byte-stable prefix, never inside it — was
/// carried by a doc comment over two public `String`s, so
/// `prompt.push_str(&ctx.text)` compiled, changed no type, and left every test
/// green. This test is out of crate, which is the only place the privacy is
/// observable, and it pins the whole reachable surface: a provenance label for
/// a journal, and spending the value as the message it is.
///
/// Failing before the change because `label()` did not exist. The other half —
/// that `ctx.text` no longer compiles — cannot be asserted from a test that
/// must itself compile, so it is pinned by the `compile_fail,E0616` doctest on
/// the type.
#[test]
fn a_contribution_is_reachable_only_as_a_label_and_a_user_message() {
    let ctx = VolatileContext::new("recall", "the last run failed on I/O");
    assert_eq!(ctx.label(), "recall");

    let message = ctx.into_message();
    assert_eq!(
        message.role,
        stella_protocol::completion::MessageRole::User,
        "the one exit is a message that is not index 0's system message"
    );
    assert_eq!(message.content, "the last run failed on I/O");

    // The body still crosses the wire — privacy is about what a *host* can
    // reach for in Rust, not about hiding the contribution from the plugin
    // that sent it.
    let json = round_trip(&VolatileContext::new("verdict", "budget exceeded"));
    assert_eq!(json, r#"{"label":"verdict","text":"budget exceeded"}"#);
}

/// **The #3500 witness**, on the vector Track C ships as
/// `plugins/testdata/09-unknown-envelope-field.request.json`: an unknown key
/// beside `point`/`body` is a refusal, not a silent drop.
///
/// Failing before the change because serde's reader for an adjacently tagged
/// enum ignores every other key — so all three reference plugins refused this
/// message while the host that receives them accepted it, which is exactly
/// backwards.
#[test]
fn an_unknown_envelope_key_is_refused_rather_than_ignored() {
    let body = r#"
      "point": "after_turn",
      "body": {
        "protocol_version": 1,
        "wrapper": "verify-v1",
        "round": 0,
        "goal": "make the failing test pass",
        "candidate": {"handle": "candidate-1", "root": "/tmp/candidate-1"},
        "turn": {"completed": true, "answer": "done", "changed_files": ["src/lib.rs"]}
      }
    "#;

    let err = serde_json::from_str::<WrapperRequest>(&format!("{{{body}, \"extra\": 1}}"))
        .expect_err("an unknown envelope key must not be silently dropped");
    assert!(
        err.to_string().contains("extra"),
        "the error must name the key, got {err}"
    );

    // ...and the same message without it decodes, which is what proves `extra`
    // was the refusal's cause rather than something else in the vector.
    let accepted = serde_json::from_str::<WrapperRequest>(&format!("{{{body}}}"))
        .expect("the vector is otherwise a well-formed request");
    assert_eq!(accepted.point(), WrapperPoint::AfterTurn);

    // The key order is not part of the contract: an envelope whose body
    // precedes its point decodes the same way, so a plugin author's JSON
    // library cannot fail them by writing its map in the other order.
    let reordered = r#"{"body": {"protocol_version": 1, "evidence":
        {"flip": "achieved"}}, "point": "after_turn"}"#;
    assert_eq!(
        serde_json::from_str::<WrapperResponse>(reordered)
            .expect("either order decodes")
            .point(),
        WrapperPoint::AfterTurn
    );

    // A response is held to the same line: the host refuses what a plugin says
    // it does not understand, in the direction that matters most.
    let err = serde_json::from_str::<WrapperResponse>(
        r#"{"point": "before_turn", "body": {"protocol_version": 1}, "cost_usd": 0.02}"#,
    )
    .expect_err("an unknown key on a response is refused too");
    assert!(
        err.to_string().contains("cost_usd"),
        "the error must name the key, got {err}"
    );

    // The body keeps its own strictness through the new envelope reader — the
    // property that made adjacent framing worth having in the first place.
    let err = serde_json::from_str::<WrapperResponse>(
        r#"{"point": "before_turn", "body": {"protocol_version": 1, "system_prompt": "you are..."}}"#,
    )
    .expect_err("an unknown key inside the body is refused");
    assert!(
        err.to_string().contains("system_prompt"),
        "the error must name the field, got {err}"
    );
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
