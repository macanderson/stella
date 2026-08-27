//! The wrapper socket's wire contract, asserted rather than promised.
//!
//! AGENTS.md #4 says every type crossing a crate boundary round-trips through
//! `serde_json` byte-for-byte. For this module that is not one rule among
//! several — it *is* the contract, because `doc:wrapper-socket` §3 makes the
//! serialized form primary and the Rust trait a typed view of it. A field that
//! survives `to_string` but not the trip back is a plugin that loads and then
//! silently says something else.
//!
//! What each test here pins:
//!
//! - **Byte-for-byte, both directions**, over a value of every type on the
//!   wire, including the ones a lazy fixture would leave `None`/empty.
//! - **Every case of every closed vocabulary**, because an enum is where a
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
    AdoptCandidateArgs, AdoptCandidateResult, AfterTurnRequest, AfterTurnResponse,
    BeforeTurnRequest, BeforeTurnResponse, CandidateFanoutArgs, CandidateFanoutResult,
    CandidateGrant, ChildTurnResult, Continuation, Correction, EvidenceProvenance, EvidenceSet,
    FanoutCandidate, FlipObservation, HostCall, HostCallOk, HostCallResponse, HostStage,
    ObservedEvidence, Outcome, PROTOCOL_VERSION, PanelDenial, PanelEmphasis, PanelFrame, PanelInk,
    PanelLease, PanelLine, PanelOverflow, PanelPaint, PanelPatch, PanelPoint, PanelRect,
    PanelRequest, PanelResponse, PanelSpan, PanelStyle, PanelText, PluginManifest, PublishedSignal,
    RecallResult, RoundState, Signal, SignalValue, StageName, StopReason, TamperFinding,
    TestBaseline, TestPlan, TestRunResult, TurnOutcome, UndecidedReason, UnmetBecause,
    UnmetRequirement, Verdict, VerdictRule, VolatileContext, WrapperPoint, WrapperRequest,
    WrapperResponse,
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
        stage: StageName::Host(HostStage::Research),
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
        witness: vec!["tests/flip.rs".into()],
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
        stage: Some(StageName::Host(HostStage::Verify)),
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
        detail: None,
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
        detail: None,
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
    // a crate boundary to `judge`, so AGENTS.md #4 covers it all the same.
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

/// **The #3587 wire witness.** A plugin can name the artifacts its flip is
/// measured against, and the host reads them.
///
/// It fails on the old contract for the plainest reason there is:
/// `BeforeTurnResponse` denies unknown fields, so a plugin that wrote
/// `"witness": [...]` did not get a narrower answer — it did not parse at all,
/// and its whole contribution was dropped as a decode fault. Which is why the
/// host's tamper watch could only ever cover the artifacts a test invocation
/// spelled out in its argv, and a plugin whose witness is `cargo test --test
/// flip` reached `TamperFinding::NotChecked` on every run, forever.
///
/// Additive: the version does not move, and a plugin that says nothing
/// produces the identical bytes it always did — the second half below.
#[test]
fn a_wrapper_declares_the_artifacts_its_flip_is_measured_against() {
    let declared: BeforeTurnResponse = serde_json::from_str(
        r#"{"protocol_version": 1, "witness": ["tests/flip.rs", "tests/second.rs"]}"#,
    )
    .expect("a declared witness list is part of the contract");
    assert_eq!(
        declared.witness,
        vec!["tests/flip.rs".to_string(), "tests/second.rs".to_string()]
    );
    assert_eq!(declared.protocol_version, PROTOCOL_VERSION);
    round_trip(&declared);

    // Omissible, and omitted rather than written empty: a plugin built against
    // the older contract sends the same bytes and reads the same answer.
    let silent: BeforeTurnResponse =
        serde_json::from_str(r#"{"protocol_version": 1}"#).expect("declaring nothing is legal");
    assert!(silent.witness.is_empty());
    let encoded = serde_json::to_string(&silent).expect("encodes");
    assert!(
        !encoded.contains("witness"),
        "an empty declaration must not appear on the wire: {encoded}"
    );
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
        stage: Some(before.stage.clone()),
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

/// **The #3524 witness for [`VolatileContext`].** AGENTS.md #7's rule — a
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

/// **The #3518 witness.** A malformed body reports *where* it is malformed.
///
/// #3500's envelope held `body` as a `serde_json::Value` until `point` had
/// been read, then re-deserialized out of that buffer through
/// `serde::de::Error::custom` — which carries the field name and drops the
/// `at line N column M` a direct `from_slice` would have had. The field name
/// is what makes the failure *decidable*; the position is what a plugin author
/// navigates by, and the whole reason they are reading the error at all.
///
/// Asserted on both key orders, because the fix keeps the buffer for the
/// reversed one: `point` first is the order the host itself always writes and
/// the one that keeps positions, and `body` first must still decode — a JSON
/// library that writes its map in the other order cannot be made wrong by this.
#[test]
fn a_malformed_body_reports_the_position_the_author_navigates_by() {
    // Pretty-printed rather than one line, so a line number is a real answer
    // and not always 1.
    let point_first = "{\n  \"point\": \"before_turn\",\n  \"body\": {\n    \
                       \"protocol_version\": 1,\n    \"system_prompt\": \"you are...\"\n  }\n}";
    let err = serde_json::from_str::<WrapperResponse>(point_first)
        .expect_err("an unknown key inside the body is refused");
    let text = err.to_string();
    assert!(text.contains("system_prompt"), "got {text}");
    assert!(
        text.contains("at line 5 column"),
        "the position is the half an author navigates by, got {text}"
    );
    assert_eq!(err.line(), 5, "got {text}");

    // The reversed order still decodes, and still names the field. It reaches
    // the buffer, so it has no position to offer — which is the trade this
    // change makes rather than the regression it would be if the
    // common path had lost one too.
    let body_first = "{\n  \"body\": {\n    \"protocol_version\": 1,\n    \
                      \"system_prompt\": \"you are...\"\n  },\n  \"point\": \"before_turn\"\n}";
    let err = serde_json::from_str::<WrapperResponse>(body_first)
        .expect_err("the body is held to the same line whichever key came first");
    assert!(err.to_string().contains("system_prompt"), "got {err}");

    // And a well-formed message decodes in either order, which is what proves
    // the two assertions above failed on the body rather than on the ordering.
    for text in [
        r#"{"point": "before_turn", "body": {"protocol_version": 1}}"#,
        r#"{"body": {"protocol_version": 1}, "point": "before_turn"}"#,
    ] {
        assert_eq!(
            serde_json::from_str::<WrapperResponse>(text)
                .expect("either order decodes")
                .point(),
            WrapperPoint::BeforeTurn
        );
    }

    // The envelope's own keys stay singular. A repeated `point` silently won
    // by last-write under the derived reader; here it is named.
    let err = serde_json::from_str::<WrapperResponse>(
        r#"{"point": "before_turn", "point": "after_turn", "body": {"protocol_version": 1}}"#,
    )
    .expect_err("a repeated envelope key is a malformed message, not a preference");
    assert!(err.to_string().contains("point"), "got {err}");

    // And an envelope missing one of them says which.
    let err = serde_json::from_str::<WrapperResponse>(r#"{"point": "before_turn"}"#)
        .expect_err("an envelope with no body is not a response");
    assert!(err.to_string().contains("body"), "got {err}");
    let err = serde_json::from_str::<WrapperResponse>(r#"{"body": {"protocol_version": 1}}"#)
        .expect_err("an envelope with no point addresses nothing");
    assert!(err.to_string().contains("point"), "got {err}");
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

/// The fan-out capability's four tables, byte-for-byte in both directions
/// (#3844). `width` and `requested` are the two numbers the whole capability
/// turns on — a plugin's ask and the host's answer about what it clamped that
/// ask to — and a field that survives `to_string` but not the trip back is a
/// plugin that scored three candidates believing it asked for one.
#[test]
fn the_candidate_fanout_tables_round_trip_byte_for_byte() {
    let args = CandidateFanoutArgs {
        role: "candidate".to_string(),
        instruction: "make tests/test_flip.py pass".to_string(),
        width: 3,
    };
    assert_eq!(
        round_trip(&args),
        r#"{"role":"candidate","instruction":"make tests/test_flip.py pass","width":3}"#
    );

    let result = CandidateFanoutResult {
        requested: 8,
        candidates: vec![FanoutCandidate {
            candidate: CandidateHandle::new("candidate-1"),
            root: "/tmp/stella-candidates/candidate-1".to_string(),
            report: "added the retry".to_string(),
            completed: true,
            files_changed: 2,
            lines_changed: 41,
        }],
    };
    let json = round_trip(&result);
    assert!(
        json.starts_with(r#"{"requested":8,"candidates":[{"candidate":"candidate-1""#),
        "{json}"
    );

    round_trip(&AdoptCandidateArgs {
        candidate: CandidateHandle::new("candidate-1"),
    });
    assert_eq!(
        round_trip(&AdoptCandidateResult {
            adopted: CandidateHandle::new("candidate-1"),
            discarded: vec![CandidateHandle::new("candidate-2")],
        }),
        r#"{"adopted":"candidate-1","discarded":["candidate-2"]}"#
    );
    // The empty-siblings shape, which a width-1 fan-out produces and which
    // `skip_serializing_if` is what keeps off the wire.
    assert_eq!(
        round_trip(&AdoptCandidateResult {
            adopted: CandidateHandle::new("candidate-1"),
            discarded: Vec::new(),
        }),
        r#"{"adopted":"candidate-1"}"#
    );
}

/// `HostCall` is a closed vocabulary, and the two new members are the reason
/// this test is worth extending rather than trusting: `serde(rename_all)`
/// changes every spelling at once, and `[loop] calls` is written by hand in a
/// manifest a human consented to. A renamed capability is a grant that stops
/// matching the thing it grants.
#[test]
fn every_host_call_is_pinned_on_both_sides() {
    for (call, wire) in [
        (HostCall::Recall, "recall"),
        (HostCall::ChildTurn, "child_turn"),
        (HostCall::RunTest, "run_test"),
        (HostCall::CandidateFanout, "candidate_fanout"),
        (HostCall::AdoptCandidate, "adopt_candidate"),
    ] {
        assert_eq!(serde_json::to_value(call).unwrap(), wire);
        assert_eq!(call.to_string(), wire, "Display must match the wire string");
        assert_eq!(call.as_str(), wire);
        round_trip(&call);
    }
}

/// [`HostCallOk`] is untagged, so the `ok` table is the only thing telling a
/// plugin which result it holds — and the rule for adding a case is that
/// its required keys are disjoint from every other's. Asserted rather than
/// reviewed: `RecallResult`'s only field defaults, so it decodes `{}` and is
/// tried first, which makes it the arm a new table falls into when someone
/// gives a required field a `#[serde(default)]`.
#[test]
fn every_host_call_result_decodes_as_the_variant_it_was_encoded_as() {
    let answers = [
        HostCallOk::Recall(RecallResult::default()),
        HostCallOk::ChildTurn(ChildTurnResult {
            role: "reviewer".to_string(),
            seat: "research".to_string(),
            report: "the diff drops the retry".to_string(),
            completed: true,
        }),
        HostCallOk::CandidateFanout(CandidateFanoutResult {
            requested: 3,
            candidates: Vec::new(),
        }),
        HostCallOk::AdoptCandidate(AdoptCandidateResult {
            adopted: CandidateHandle::new("candidate-1"),
            discarded: Vec::new(),
        }),
        // #3580's addition, and the near miss: this and the adoption answer
        // both carry one handle, and they stay disjoint because the *key*
        // differs — `candidate` here, `adopted` there. Its
        // emptiest form is the interesting one, since `output` may be omitted:
        // `{"candidate": …, "assertions": …}`, which shares no required key
        // with any arm above.
        HostCallOk::RunTest(TestRunResult {
            candidate: CandidateHandle::new("candidate-1"),
            assertions: TestBaseline::Unobserved,
            output: String::new(),
        }),
    ];
    for answer in answers {
        let response = HostCallResponse::ok(1, answer.clone());
        let text = serde_json::to_string(&response).expect("encodes");
        let decoded: HostCallResponse = serde_json::from_str(&text).expect("decodes");
        assert_eq!(
            decoded, response,
            "{text} decoded as a different untagged arm"
        );
    }

    // The one that would go wrong first: a fan-out answer with nothing to
    // report is `{"requested":0,"candidates":[]}` and NOT `{}`, which is what
    // keeps it out of the `recall` arm tried before it.
    let empty = serde_json::to_string(&HostCallResponse::ok(
        1,
        HostCallOk::CandidateFanout(CandidateFanoutResult {
            requested: 0,
            candidates: Vec::new(),
        }),
    ))
    .expect("encodes");
    assert_eq!(
        empty,
        r#"{"result":1,"ok":{"requested":0,"candidates":[]}}"#
    );
}

/// A fan-out width is a number a manifest declares, and the two rules it gets
/// are `max_calls`'s — bound something, and never bound it to zero — pointed
/// at one capability rather than the list. A width beside a manifest that
/// cannot fan out is a number that bounds nothing, which is the manifest that
/// quietly does nothing (#3844).
#[test]
fn a_fanout_width_must_bound_a_fanout_this_manifest_can_actually_ask_for() {
    let manifest = |calls: &str, width: &str| {
        PluginManifest::from_toml_str(&format!(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"before_turn\"]\n\
             calls = [{calls}]\n{width}"
        ))
    };

    let declared = manifest("\"candidate_fanout\"", "max_fanout_width = 2").expect("loads");
    assert_eq!(declared.loop_grant.max_fanout_width, Some(2));
    assert!(declared.loop_grant.permits_call(HostCall::CandidateFanout));
    assert!(
        !declared.loop_grant.permits_call(HostCall::AdoptCandidate),
        "declaring a fan-out must not silently grant the adoption beside it"
    );

    assert!(matches!(
        manifest("\"recall\"", "max_fanout_width = 2").expect_err("a width bounding nothing"),
        stella_plugin::ManifestError::MaxFanoutWidthRequiresFanout
    ));
    assert!(matches!(
        manifest("\"candidate_fanout\"", "max_fanout_width = 0").expect_err("a zero width"),
        stella_plugin::ManifestError::ZeroMaxFanoutWidth
    ));
    assert_eq!(
        manifest("\"candidate_fanout\"", "")
            .expect("loads")
            .loop_grant
            .max_fanout_width,
        None,
        "absent is the host's ceiling, not a load error"
    );
}

/// The whole-run child-turn ceiling gets the same two rules, against its own
/// capability — and is a *different key* from `max_calls`, which is the point
/// of #3839: one is per point conversation and resets, the other never does.
#[test]
fn a_child_turn_ceiling_must_bound_a_call_this_manifest_can_actually_make() {
    let manifest = |calls: &str, ceiling: &str| {
        PluginManifest::from_toml_str(&format!(
            "name = \"x\"\n[loop]\nparticipation = \"steering\"\npoints = [\"after_turn\"]\n\
             calls = [{calls}]\n{ceiling}"
        ))
    };

    let declared = manifest("\"child_turn\"", "max_calls = 1\nmax_child_turns = 8").expect("loads");
    assert_eq!(declared.loop_grant.max_calls, Some(1));
    assert_eq!(declared.loop_grant.max_child_turns, Some(8));

    assert!(matches!(
        manifest("\"recall\"", "max_child_turns = 2").expect_err("a ceiling bounding nothing"),
        stella_plugin::ManifestError::MaxChildTurnsRequiresChildTurn
    ));
    assert!(matches!(
        manifest("\"child_turn\"", "max_child_turns = 0").expect_err("a zero ceiling"),
        stella_plugin::ManifestError::ZeroMaxChildTurns
    ));
    assert_eq!(
        manifest("\"child_turn\"", "")
            .expect("loads")
            .loop_grant
            .max_child_turns,
        None,
        "absent falls back to max_calls, and then to the host's ceiling"
    );
}

// ---------------------------------------------------------------------------
// The panel channel (`design/tui-v2/SPEC.md` §12, #5054). A third dispatch
// context, held to the identical contract: byte-for-byte in both directions,
// every closed vocabulary pinned, and every table refusing a key it does not
// know.
// ---------------------------------------------------------------------------

/// The rectangle a host leases an eight-by-two panel for one tick.
fn lease() -> PanelLease {
    PanelLease::new("gates", 42, PanelRect::new(8, 2), 33)
}

/// Glyphs the contract accepts, for a test that is about something else.
fn glyphs(text: &str) -> PanelText {
    PanelText::new(text).expect("plain glyphs")
}

/// One row of one style.
fn row(text: &str) -> PanelLine {
    PanelLine::new(vec![PanelSpan::new(glyphs(text), PanelStyle::plain())])
}

/// Both frame shapes, the styled span and the unstyled one, the populated row
/// and the empty one — every optional member of the panel wire in both of its
/// states, in one pass.
#[test]
fn every_panel_message_round_trips_byte_for_byte() {
    let request = round_trip(&PanelRequest::new(lease()));
    let value: serde_json::Value = serde_json::from_str(&request).unwrap();
    assert_eq!(value["point"], "frame");
    assert_eq!(value["body"]["protocol_version"], PROTOCOL_VERSION);
    assert_eq!(value["body"]["rect"]["cols"], 8);
    assert_eq!(value["body"]["rect"]["rows"], 2);

    let styled = PanelPaint::Lines(vec![
        PanelLine::new(vec![
            PanelSpan::new(
                glyphs("3 "),
                PanelStyle {
                    fg: Some(PanelInk::Green),
                    bg: Some(PanelInk::Panel),
                    emphasis: vec![PanelEmphasis::Bold, PanelEmphasis::Underline],
                },
            ),
            PanelSpan::new(glyphs("green"), PanelStyle::plain()),
        ]),
        PanelLine::default(),
    ]);
    let lines = round_trip(&PanelResponse::new(PanelFrame::new(42, styled)));
    let value: serde_json::Value = serde_json::from_str(&lines).unwrap();
    assert_eq!(value["point"], "frame");
    // An unstyled span omits the key and an empty row omits its spans, so the
    // emptiest legal frame is smaller than a reader of the type would guess.
    assert!(
        value["body"]["paint"]["lines"][0]["spans"][1]
            .get("style")
            .is_none()
    );
    assert!(value["body"]["paint"]["lines"][1].get("spans").is_none());

    let diff = PanelPaint::Diff(vec![PanelPatch::new(
        1,
        6,
        glyphs("ok"),
        PanelStyle::ink(PanelInk::Silver),
    )]);
    let patched = round_trip(&PanelResponse::new(PanelFrame::new(43, diff)));
    let value: serde_json::Value = serde_json::from_str(&patched).unwrap();
    assert_eq!(value["body"]["paint"]["diff"][0]["row"], 1);
    assert_eq!(value["body"]["paint"]["diff"][0]["col"], 6);
    assert_eq!(value["body"]["paint"]["diff"][0]["text"], "ok");

    round_trip(&PanelRect::new(0, 0));
    round_trip(&PanelStyle::plain());
    round_trip(&glyphs("stella*"));
}

/// **The witness for #5054.** A frame may address the cells it was leased and
/// no others, and the refusal names the cell that ran past the edge instead of
/// reporting that something, somewhere, did not fit.
///
/// Four directions, because a rectangle has four ways out and a check that
/// counted only rows would pass a row of the right count and the wrong width.
#[test]
fn a_panel_frame_addressing_a_cell_outside_its_lease_is_refused() {
    let lease = lease();

    // Exactly filling the lease is inside it: the edge is addressable, and a
    // panel that could not use its last column would be leased a lie.
    let full = PanelFrame::new(
        42,
        PanelPaint::Lines(vec![row("12345678"), row("12345678")]),
    );
    assert_eq!(lease.admits(&full), Ok(()));
    let corner = PanelFrame::new(
        42,
        PanelPaint::Diff(vec![PanelPatch::new(
            1,
            7,
            glyphs("x"),
            PanelStyle::plain(),
        )]),
    );
    assert_eq!(lease.admits(&corner), Ok(()));

    let tall = PanelFrame::new(42, PanelPaint::Lines(vec![row("a"), row("b"), row("c")]));
    assert_eq!(
        lease.admits(&tall),
        Err(PanelOverflow::Rows { lines: 3, rows: 2 })
    );

    let wide = PanelFrame::new(42, PanelPaint::Lines(vec![row("123456789")]));
    assert_eq!(
        lease.admits(&wide),
        Err(PanelOverflow::Line {
            line: 0,
            cells: 9,
            cols: 8,
        })
    );

    let below = PanelFrame::new(
        42,
        PanelPaint::Diff(vec![PanelPatch::new(
            2,
            0,
            glyphs("x"),
            PanelStyle::plain(),
        )]),
    );
    assert_eq!(
        lease.admits(&below),
        Err(PanelOverflow::Row { row: 2, rows: 2 })
    );

    let past = PanelFrame::new(
        42,
        PanelPaint::Diff(vec![PanelPatch::new(
            0,
            7,
            glyphs("no"),
            PanelStyle::plain(),
        )]),
    );
    assert_eq!(
        lease.admits(&past),
        Err(PanelOverflow::Patch {
            row: 0,
            col: 7,
            cells: 2,
            cols: 8,
        })
    );

    // A run of no glyphs is anchored too. `col + 0` is inside every lease, so
    // an extent check on its own would admit a patch naming a column the host's
    // buffer does not have — nothing to blit, and exactly the coordinate a host
    // that reads the column before it measures the run would index with.
    let anchored_out = PanelFrame::new(
        42,
        PanelPaint::Diff(vec![PanelPatch::new(0, 8, glyphs(""), PanelStyle::plain())]),
    );
    assert_eq!(
        lease.admits(&anchored_out),
        Err(PanelOverflow::Patch {
            row: 0,
            col: 8,
            cells: 0,
            cols: 8,
        })
    );

    // And the refusal is readable: a host printing it names the coordinates a
    // plugin author has to change.
    let printed = lease
        .admits(&past)
        .expect_err("the patch runs past the edge")
        .to_string();
    assert!(printed.contains("column 7"), "got {printed}");
}

/// A panel writes glyphs and never an escape sequence, in Rust and on the wire.
///
/// The Rust half is the type: [`PanelText`]'s body is private, so a value can
/// only come from the constructor that refuses control characters. The wire
/// half is this — a frame carrying `ESC [ 2 J` is a **decode error**, so a host
/// never holds one to inspect and no amount of care about how it blits could
/// have mattered.
#[test]
fn a_panel_frame_carrying_an_escape_sequence_does_not_decode() {
    // Written as JSON escapes, so the hazard is what a plugin's own encoder
    // would put on the pipe and not something only a Rust literal can say.
    for hazard in [
        "\\u001b[2J",
        "\\u001b]0;stella\\u0007",
        "\\u001b[1;1H",
        "a\\nb",
        "col\\u0009umn",
    ] {
        let json = format!(
            "{{\"point\":\"frame\",\"body\":{{\"protocol_version\":1,\"tick\":1,\
             \"paint\":{{\"diff\":[{{\"row\":0,\"col\":0,\"text\":\"{hazard}\"}}]}}}}}}"
        );
        let err = serde_json::from_str::<PanelResponse>(&json)
            .expect_err("an escape sequence must not decode as drawable glyphs");
        assert!(
            err.to_string().contains("control character"),
            "the refusal must say why, got {err}"
        );
    }

    // The same refusal on the other frame shape, so neither of them is the
    // soft one.
    let json = "{\"point\":\"frame\",\"body\":{\"protocol_version\":1,\"tick\":1,\
                \"paint\":{\"lines\":[{\"spans\":[{\"text\":\"\\u001b[31mred\"}]}]}}}";
    assert!(serde_json::from_str::<PanelResponse>(json).is_err());
}

/// Every closed vocabulary the panel channel adds, on both sides.
#[test]
fn every_panel_vocabulary_is_pinned_on_both_sides() {
    assert_eq!(serde_json::to_value(PanelPoint::Frame).unwrap(), "frame");
    assert_eq!(PanelPoint::Frame.to_string(), "frame");
    round_trip(&PanelPoint::Frame);

    for (denial, wire) in [
        (PanelDenial::Network, "network"),
        (PanelDenial::WriteOutsideSandbox, "write-outside-sandbox"),
    ] {
        assert_eq!(serde_json::to_value(denial).unwrap(), wire);
        assert_eq!(denial.to_string(), wire);
        round_trip(&denial);
    }
    assert_eq!(
        PanelDenial::all(),
        &[PanelDenial::Network, PanelDenial::WriteOutsideSandbox],
        "the denial set is closed, and its order is the one a prompt prints"
    );

    // The spellings are `design/tui-v2/SPEC.md` §3.1's own token names, so a
    // panel asks for the same colour the palette table publishes and a reader
    // crosses between the two documents without a mapping.
    for (ink, wire) in [
        (PanelInk::Bg, "bg"),
        (PanelInk::Panel, "panel"),
        (PanelInk::Hl, "hl"),
        (PanelInk::Border, "border"),
        (PanelInk::Rule, "rule"),
        (PanelInk::Gold, "gold"),
        (PanelInk::GoldBright, "gold_bright"),
        (PanelInk::Silver, "silver"),
        (PanelInk::SilverType, "silver_type"),
        (PanelInk::Text, "text"),
        (PanelInk::Muted, "muted"),
        (PanelInk::Dim, "dim"),
        (PanelInk::Green, "green"),
        (PanelInk::Red, "red"),
        (PanelInk::DiffAddBg, "diff_add_bg"),
        (PanelInk::DiffDelBg, "diff_del_bg"),
    ] {
        assert_eq!(serde_json::to_value(ink).unwrap(), wire);
        round_trip(&ink);
    }
    assert!(
        serde_json::from_str::<PanelInk>("\"#EFC53F\"").is_err(),
        "a panel names a token and never a colour of its own, so the hue clamp \
         holds over plugin pixels too"
    );

    for (emphasis, wire) in [
        (PanelEmphasis::Bold, "bold"),
        (PanelEmphasis::Dim, "dim"),
        (PanelEmphasis::Italic, "italic"),
        (PanelEmphasis::Underline, "underline"),
    ] {
        assert_eq!(serde_json::to_value(emphasis).unwrap(), wire);
        round_trip(&emphasis);
    }
    for absent in ["reverse", "blink"] {
        assert!(
            serde_json::from_str::<PanelEmphasis>(&format!("\"{absent}\"")).is_err(),
            "\"{absent}\" is not a panel's to ask for"
        );
    }
}

/// The panel tables refuse a key they do not know, as every other table on this
/// wire does — and one of those keys is the title, whose absence is the
/// anti-spoofing rule: the host writes the label from the name a human
/// consented to, so a plugin cannot call itself `GATES`.
#[test]
fn a_panel_may_not_name_itself_or_carry_a_key_the_contract_lacks() {
    let err = serde_json::from_str::<PanelResponse>(
        "{\"point\":\"frame\",\"body\":{\"protocol_version\":1,\"tick\":1,\
         \"title\":\"GATES\",\"paint\":{\"lines\":[]}}}",
    )
    .expect_err("a frame does not name the panel");
    assert!(err.to_string().contains("title"), "got {err}");

    let err = PluginManifest::from_toml_str(
        "name = \"gates\"\n[panel]\ntitle = \"GATES\"\ndenies = [\"network\", \
         \"write-outside-sandbox\"]",
    )
    .expect_err("a [panel] block does not name the panel either");
    assert!(err.to_string().contains("title"), "got {err}");

    let err = serde_json::from_str::<PanelRequest>(
        "{\"point\":\"frame\",\"body\":{\"protocol_version\":1,\"panel\":\"gates\",\
         \"tick\":1,\"rect\":{\"cols\":4,\"rows\":1,\"x\":9},\"budget_ms\":33}}",
    )
    .expect_err("a leased rectangle carries an extent and no origin");
    assert!(err.to_string().contains("`x`"), "got {err}");
}

/// The `[panel]` block is a consent document, so it names every limit a panel
/// accepts before it loads — a block naming fewer is refused by the denial it
/// left out instead of being read as a narrower panel.
#[test]
fn a_panel_block_must_name_every_denial_it_accepts() {
    let manifest = |denies: &str| {
        PluginManifest::from_toml_str(&format!("name = \"gates\"\n[panel]\ndenies = [{denies}]"))
    };

    let loaded = manifest("\"network\", \"write-outside-sandbox\"").expect("loads");
    let panel = loaded.panel.expect("the block parsed");
    assert!(panel.denies(PanelDenial::Network));
    assert!(panel.denies(PanelDenial::WriteOutsideSandbox));
    assert_eq!(panel.missing_denial(), None);
    // Declaring a panel buys no say in a turn, and needs none.
    assert_eq!(
        loaded.loop_grant.participation,
        stella_plugin::Participation::None
    );

    assert!(matches!(
        manifest("\"network\"").expect_err("a panel that keeps its filesystem"),
        stella_plugin::ManifestError::PanelDenialMissing {
            denial: PanelDenial::WriteOutsideSandbox
        }
    ));
    assert!(matches!(
        manifest("").expect_err("a panel that accepts nothing"),
        stella_plugin::ManifestError::PanelDenialMissing {
            denial: PanelDenial::Network
        }
    ));
    assert!(matches!(
        manifest("\"network\", \"network\", \"write-outside-sandbox\"")
            .expect_err("a repeated limit is an editing mistake"),
        stella_plugin::ManifestError::DuplicatePanelDenial {
            denial: PanelDenial::Network
        }
    ));
    let err = manifest("\"read-anything\"").expect_err("the denial set is Stella's");
    assert!(err.to_string().contains("read-anything"), "got {err}");

    assert!(
        PluginManifest::from_toml_str("name = \"bundle\"")
            .expect("loads")
            .panel
            .is_none(),
        "no [panel] block is no panel, and never an empty one"
    );
}
