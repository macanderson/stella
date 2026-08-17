// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A machine-checked description of the **wrapper socket** wire format.
//!
//! `doc:wrapper-socket` §3 commitment 2 says `docs/wire/` is generated and
//! gate-checked "precisely so that a renamed field or a re-tagged variant lands
//! on the author's screen instead of in a consumer's parser; the wrapper wire
//! contract joins it on the same terms". This module is that join.
//!
//! [`crate::WrapperRequest`] and [`crate::WrapperResponse`] are the only two
//! things that cross the process boundary
//! (`stella_runtime::wrapper::subprocess::exchange` encodes one and decodes the
//! other, and nothing else on this wire is serialized), so they are the two
//! roots this module publishes. Everything reachable from them is published
//! with them.
//!
//! # Why a corpus and not a JSON Schema
//!
//! This is the honest part, and it is a **declared gap, not a silence**.
//!
//! `stella-protocol` and `stella-serve` publish JSON Schema, derived by
//! `schemars` from `#[cfg_attr(feature = "schema", derive(JsonSchema))]` on the
//! types themselves. That is the better artifact and it is where this should
//! end up (#3532). It cannot be built from outside `crate::wire`: `schemars`
//! generates a schema from a `JsonSchema` impl, and a `JsonSchema` impl only
//! exists where the type is defined. Writing one by hand here would be a second
//! copy of the contract — the hand-maintained wire documentation
//! `scripts/check-wire-schema.sh`'s own header calls "the single most dangerous
//! drift in this document".
//!
//! So this publishes the next-best thing that is still **derived**: every wire
//! message, serialized by the same `Serialize` impls the socket uses, in both
//! its fullest and its emptiest legal form. What that catches:
//!
//! - a renamed field — the key changes in every case that carries it;
//! - a re-tagged variant or a changed `rename_all` — the tag changes;
//! - an added or removed field — a key appears or disappears in the full case;
//! - an optional field made required, or a required one made optional — the
//!   *minimal* case is what makes this visible, which is why every message
//!   appears twice.
//!
//! What it does **not** catch, and what the schemars upgrade would: a widened
//! or narrowed scalar type (`u32` → `u64`), and a string field that gains a
//! format or pattern constraint. Neither changes any byte of the corpus.
//!
//! # Totality is the compiler's job, not a reviewer's
//!
//! A corpus that silently omits a variant is decoration. Every closed enum on
//! this wire is enumerated by a **successor function** rather than an array,
//! because the compiler checks a `match` and cannot check a `&[…]`: adding a
//! variant makes the successor match non-exhaustive (`E0004`), so the new
//! variant reaches the corpus in the change that introduces it. Every struct is
//! built with an exhaustive literal and no `..Default::default()`, so adding a
//! field is `E0063` in this file.
//!
//! That is the same enforcement shape as invariant 10's consumer ledger, for
//! the same reason: a table nothing makes you write is a table that goes stale.
//!
//! # Determinism
//!
//! `serde_json`'s object map is a `BTreeMap` here (the workspace does not
//! enable `preserve_order`), so keys sort; every array below is in declaration
//! order. Running the exporter twice produces no diff the second time —
//! `scripts/check-wire-schema.sh` depends on exactly that.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};
use stella_protocol::candidate::CandidateHandle;

use crate::{
    AfterTurnRequest, AfterTurnResponse, BeforeTurnRequest, BeforeTurnResponse, CandidateGrant,
    FlipObservation, ObservedEvidence, PROTOCOL_VERSION, PublishedSignal, Signal, SignalKind,
    SignalValue, StageName, TestBaseline, TestPlan, TurnOutcome, VolatileContext, WrapperPoint,
    WrapperRequest, WrapperResponse,
};

/// The committed artifact's filename.
pub const WRAPPER_WIRE: &str = "wrapper.wire.json";

/// What a reader of the generated file is told before they edit it.
const NOTE: &str = "GENERATED FILE — DO NOT EDIT. Every message the wrapper \
     socket carries, serialized by the same impls stella_runtime's subprocess \
     transport uses. Regenerate with `bash scripts/export-agentevent-schema.sh`; \
     guarded by `scripts/check-wire-schema.sh` (`make wire-schema`). Source of \
     truth: crates/stella-plugin/src/wire.rs. Each message appears twice — \
     `full` populates every optional field, `minimal` omits every one that may \
     be omitted — so a field changing between required and optional is a diff \
     here. This is a corpus, not a JSON Schema: see \
     crates/stella-plugin/src/wire_corpus.rs for what that does and does not \
     catch (#3532).";

/// Every committed artifact, as `(filename, contents)`.
///
/// # Errors
///
/// Propagates a `serde_json` failure. None of these values can produce one —
/// no non-string map key, no non-finite float — but the exporter reports it
/// rather than panicking, because a wire contract that cannot be written must
/// say so (invariant 5).
pub fn artifacts() -> Result<Vec<(&'static str, String)>, serde_json::Error> {
    let mut body = serde_json::to_string_pretty(&corpus()?)?;
    body.push('\n');
    Ok(vec![(WRAPPER_WIRE, body)])
}

/// The whole corpus, as one JSON document.
///
/// # Errors
///
/// Propagates a `serde_json` failure while encoding a message.
pub fn corpus() -> Result<Value, serde_json::Error> {
    Ok(json!({
        "note": NOTE,
        "protocol_version": PROTOCOL_VERSION,
        "requests": requests()?,
        "responses": responses()?,
        "parts": parts()?,
        "vocabulary": vocabulary()?,
    }))
}

/// One labelled case: the name a diff reads, and the bytes it is about.
fn case<T: Serialize>(name: &'static str, message: &T) -> Result<Value, serde_json::Error> {
    Ok(json!({ "case": name, "message": serde_json::to_value(message)? }))
}

fn requests() -> Result<Value, serde_json::Error> {
    Ok(Value::Array(vec![
        case(
            "before_turn/full",
            &WrapperRequest::BeforeTurn(before_turn_request_full()),
        )?,
        case(
            "before_turn/minimal",
            &WrapperRequest::BeforeTurn(before_turn_request_minimal()),
        )?,
        case(
            "after_turn/full",
            &WrapperRequest::AfterTurn(after_turn_request_full()),
        )?,
        case(
            "after_turn/minimal",
            &WrapperRequest::AfterTurn(after_turn_request_minimal()),
        )?,
    ]))
}

fn responses() -> Result<Value, serde_json::Error> {
    Ok(Value::Array(vec![
        case(
            "before_turn/full",
            &WrapperResponse::BeforeTurn(before_turn_response_full()),
        )?,
        case(
            "before_turn/minimal",
            &WrapperResponse::BeforeTurn(before_turn_response_minimal()),
        )?,
        case(
            "after_turn/full",
            &WrapperResponse::AfterTurn(after_turn_response_full()),
        )?,
        case(
            "after_turn/minimal",
            &WrapperResponse::AfterTurn(after_turn_response_minimal()),
        )?,
    ]))
}

/// The nested types, in the forms the four messages above do not reach.
///
/// A message's `minimal` case omits its optional members entirely, so the
/// emptiest legal form of a *nested* type has nowhere to appear inside it.
/// Publishing them here is what makes `CandidateGrant::test` going required, or
/// `TestPlan::args` losing its `skip_serializing_if`, a diff.
fn parts() -> Result<Value, serde_json::Error> {
    Ok(Value::Array(vec![
        case("candidate_grant/full", &candidate_grant_full())?,
        case("candidate_grant/minimal", &candidate_grant_minimal())?,
        case("test_plan/full", &test_plan_full())?,
        case("test_plan/minimal", &test_plan_minimal())?,
        case("turn_outcome/full", &turn_outcome_full())?,
        case("turn_outcome/minimal", &turn_outcome_minimal())?,
        case("observed_evidence/full", &observed_evidence_full())?,
        case("observed_evidence/minimal", &observed_evidence_minimal())?,
        case("volatile_context", &volatile_context())?,
    ]))
}

/// Every value of every closed enum on this wire, in its serialized form.
///
/// The messages above carry one variant each; a re-tagged variant they do not
/// happen to carry would otherwise be invisible. Each list is enumerated by the
/// successor functions below, so it cannot fall behind its enum.
fn vocabulary() -> Result<Value, serde_json::Error> {
    Ok(json!({
        "wrapper_point": values(enumerate(WrapperPoint::BeforeTurn, point_after))?,
        "stage": values(enumerate(StageName::Triage, stage_after))?,
        "test_baseline": values(enumerate(TestBaseline::NotRun, baseline_after))?,
        "flip_observation": values(enumerate(FlipObservation::NotAttempted, flip_after))?,
        "signal_value": values(enumerate(SignalValue::Boolean(true), signal_value_after))?,
        // Each signal paired with a value of its declared kind, so the pair a
        // host actually reads is what the corpus pins.
        "published_signal": values(
            enumerate(Signal::TestCommand, signal_after)
                .into_iter()
                .map(well_typed)
                .collect(),
        )?,
    }))
}

fn values<T: Serialize>(items: Vec<T>) -> Result<Value, serde_json::Error> {
    items
        .into_iter()
        .map(|item| serde_json::to_value(item))
        .collect::<Result<Vec<_>, _>>()
        .map(Value::Array)
}

/// Walk a closed enum from its first variant to its last.
fn enumerate<T: Copy>(first: T, next: fn(T) -> Option<T>) -> Vec<T> {
    std::iter::successors(Some(first), |item| next(*item)).collect()
}

// ---------------------------------------------------------------------------
// Successor functions. Each one is exhaustive by the compiler: a new variant
// makes the `match` non-exhaustive, and the only way to compile again is to
// place it in the chain — which puts it in the corpus.
// ---------------------------------------------------------------------------

fn point_after(point: WrapperPoint) -> Option<WrapperPoint> {
    match point {
        WrapperPoint::BeforeTurn => Some(WrapperPoint::AfterTurn),
        WrapperPoint::AfterTurn => None,
    }
}

fn stage_after(stage: StageName) -> Option<StageName> {
    match stage {
        StageName::Triage => Some(StageName::Recall),
        StageName::Recall => Some(StageName::Research),
        StageName::Research => Some(StageName::Plan),
        StageName::Plan => Some(StageName::Scope),
        StageName::Scope => Some(StageName::Execute),
        StageName::Execute => Some(StageName::Witness),
        StageName::Witness => Some(StageName::Verify),
        StageName::Verify => Some(StageName::Verdict),
        StageName::Verdict => Some(StageName::Reflect),
        StageName::Reflect => Some(StageName::ContextWrite),
        StageName::ContextWrite => Some(StageName::Complete),
        StageName::Complete => None,
    }
}

fn baseline_after(baseline: TestBaseline) -> Option<TestBaseline> {
    match baseline {
        TestBaseline::NotRun => Some(TestBaseline::Passed),
        TestBaseline::Passed => Some(TestBaseline::Failed),
        TestBaseline::Failed => Some(TestBaseline::Unobserved),
        TestBaseline::Unobserved => None,
    }
}

fn flip_after(flip: FlipObservation) -> Option<FlipObservation> {
    use FlipObservation as F;
    match flip {
        F::NotAttempted => Some(F::Achieved),
        F::Achieved => Some(F::NotAchieved),
        F::NotAchieved => Some(F::Unsatisfiable),
        F::Unsatisfiable => Some(F::Unobservable),
        F::Unobservable => None,
    }
}

/// The two shapes [`SignalKind`] enumerates. The payloads are fixed sample
/// values: what is pinned is the tag and the JSON type beneath it.
fn signal_value_after(value: SignalValue) -> Option<SignalValue> {
    match value {
        SignalValue::Boolean(_) => Some(SignalValue::Count(3)),
        SignalValue::Count(_) => None,
    }
}

/// The published-signal vocabulary.
///
/// `Signal::ALL` exists and would be shorter, but nothing enforces that it is
/// exhaustive — it is a hand-written array, and a variant added without
/// touching it compiles. This chain does not.
fn signal_after(signal: Signal) -> Option<Signal> {
    match signal {
        Signal::TestCommand => Some(Signal::Candidates),
        Signal::Candidates => Some(Signal::BudgetMetered),
        Signal::BudgetMetered => Some(Signal::Conversational),
        Signal::Conversational => Some(Signal::Questions),
        Signal::Questions => Some(Signal::Plans),
        Signal::Plans => Some(Signal::Verifies),
        Signal::Verifies => Some(Signal::WantsWitness),
        Signal::WantsWitness => Some(Signal::WantsVerifier),
        Signal::WantsVerifier => Some(Signal::MutatingActions),
        Signal::MutatingActions => Some(Signal::DiffLines),
        Signal::DiffLines => Some(Signal::WitnessAuthored),
        Signal::WitnessAuthored => Some(Signal::FlipAchieved),
        Signal::FlipAchieved => Some(Signal::TestsRed),
        Signal::TestsRed => Some(Signal::TestsGreen),
        Signal::TestsGreen => None,
    }
}

/// Pair a signal with a value of its declared kind — the only pairing
/// [`PublishedSignal::is_well_typed`] accepts, so the corpus publishes nothing
/// a host would refuse.
fn well_typed(signal: Signal) -> PublishedSignal {
    let value = match signal.kind() {
        SignalKind::Boolean => SignalValue::Boolean(true),
        SignalKind::Count => SignalValue::Count(3),
    };
    PublishedSignal { signal, value }
}

// ---------------------------------------------------------------------------
// The messages. Every literal below is exhaustive: no `..Default::default()`,
// so a new field is a compile error here before it is a silent omission there.
// ---------------------------------------------------------------------------

fn before_turn_request_full() -> BeforeTurnRequest {
    BeforeTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "witness".to_string(),
        stage: StageName::Execute,
        round: 1,
        goal: "make the failing test pass".to_string(),
        candidate: Some(candidate_grant_full()),
        published: vec![well_typed(Signal::WitnessAuthored)],
    }
}

fn before_turn_request_minimal() -> BeforeTurnRequest {
    BeforeTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "witness".to_string(),
        stage: StageName::Triage,
        round: 0,
        goal: "make the failing test pass".to_string(),
        candidate: None,
        published: Vec::new(),
    }
}

fn after_turn_request_full() -> AfterTurnRequest {
    AfterTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "witness".to_string(),
        // The same stage `before_turn_request_full` names for the same round:
        // the corpus shows the correlation the field exists for, not just the
        // key's presence.
        stage: Some(StageName::Execute),
        round: 1,
        goal: "make the failing test pass".to_string(),
        candidate: Some(candidate_grant_full()),
        turn: turn_outcome_full(),
    }
}

fn after_turn_request_minimal() -> AfterTurnRequest {
    AfterTurnRequest {
        protocol_version: PROTOCOL_VERSION,
        wrapper: "witness".to_string(),
        // A host that runs no stage program omits the key entirely, which is
        // what "minimal" is for: the absence is the documented shape.
        stage: None,
        round: 0,
        goal: "make the failing test pass".to_string(),
        candidate: None,
        turn: turn_outcome_minimal(),
    }
}

fn before_turn_response_full() -> BeforeTurnResponse {
    BeforeTurnResponse {
        protocol_version: PROTOCOL_VERSION,
        context: vec![volatile_context()],
        role: Some("verifier".to_string()),
        scope: vec!["crates/stella-plugin/src/wire.rs".to_string()],
        publish: vec![well_typed(Signal::FlipAchieved)],
    }
}

fn before_turn_response_minimal() -> BeforeTurnResponse {
    BeforeTurnResponse {
        protocol_version: PROTOCOL_VERSION,
        context: Vec::new(),
        role: None,
        scope: Vec::new(),
        publish: Vec::new(),
    }
}

fn after_turn_response_full() -> AfterTurnResponse {
    AfterTurnResponse {
        protocol_version: PROTOCOL_VERSION,
        evidence: observed_evidence_full(),
    }
}

fn after_turn_response_minimal() -> AfterTurnResponse {
    AfterTurnResponse {
        protocol_version: PROTOCOL_VERSION,
        evidence: observed_evidence_minimal(),
    }
}

fn candidate_grant_full() -> CandidateGrant {
    CandidateGrant {
        handle: CandidateHandle::new("candidate-1"),
        root: "/var/folders/stella-candidate-1".to_string(),
        test: Some(test_plan_full()),
    }
}

fn candidate_grant_minimal() -> CandidateGrant {
    CandidateGrant {
        handle: CandidateHandle::new("candidate-1"),
        root: "/var/folders/stella-candidate-1".to_string(),
        test: None,
    }
}

fn test_plan_full() -> TestPlan {
    TestPlan {
        program: "cargo".to_string(),
        args: vec![
            "test".to_string(),
            "-p".to_string(),
            "stella-plugin".to_string(),
        ],
        baseline: TestBaseline::Failed,
    }
}

fn test_plan_minimal() -> TestPlan {
    TestPlan {
        program: "cargo".to_string(),
        args: Vec::new(),
        baseline: TestBaseline::NotRun,
    }
}

fn turn_outcome_full() -> TurnOutcome {
    TurnOutcome {
        completed: true,
        answer: "the witness now passes".to_string(),
        tools: vec!["read_file".to_string(), "edit_file".to_string()],
        changed_files: vec!["crates/stella-plugin/src/wire.rs".to_string()],
    }
}

fn turn_outcome_minimal() -> TurnOutcome {
    TurnOutcome {
        completed: false,
        answer: String::new(),
        tools: Vec::new(),
        changed_files: Vec::new(),
    }
}

fn observed_evidence_full() -> ObservedEvidence {
    ObservedEvidence {
        flip: FlipObservation::Achieved,
        measurements: BTreeMap::from([("p50_ms".to_string(), 103)]),
    }
}

fn observed_evidence_minimal() -> ObservedEvidence {
    ObservedEvidence {
        flip: FlipObservation::Unobservable,
        measurements: BTreeMap::new(),
    }
}

/// Built through the constructor rather than as a literal, because
/// [`VolatileContext`]'s fields are private: the body is reachable only by
/// spending the value as a user message, which is invariant 7 held by the
/// compiler rather than by review (#3524).
fn volatile_context() -> VolatileContext {
    VolatileContext::new("witness", "the authored test fails on the pre-turn tree")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `scripts/check-wire-schema.sh` regenerates into a temp directory and
    /// diffs; a corpus that varied between runs would fail the gate at random
    /// and teach everyone to ignore it.
    #[test]
    fn the_corpus_is_byte_identical_across_runs() {
        let first = artifacts().expect("the corpus encodes");
        let second = artifacts().expect("the corpus encodes");
        assert_eq!(first, second);
    }

    /// The guard's whole claim is that the committed bytes describe the *live*
    /// types. Every message must therefore survive the round trip the socket
    /// itself performs — a corpus of shapes nothing would accept proves
    /// nothing.
    #[test]
    fn every_published_message_is_one_the_socket_would_accept() {
        let doc = corpus().expect("the corpus encodes");
        for entry in doc["requests"].as_array().expect("requests is an array") {
            let message = &entry["message"];
            let decoded: WrapperRequest =
                serde_json::from_value(message.clone()).expect("a published request decodes");
            assert_eq!(
                &serde_json::to_value(&decoded).expect("re-encodes"),
                message
            );
        }
        for entry in doc["responses"].as_array().expect("responses is an array") {
            let message = &entry["message"];
            let decoded: WrapperResponse =
                serde_json::from_value(message.clone()).expect("a published response decodes");
            assert_eq!(
                &serde_json::to_value(&decoded).expect("re-encodes"),
                message
            );
        }
    }

    /// The `minimal` half of each pair is what makes optionality visible, and
    /// it only does that if it is genuinely the emptier of the two.
    ///
    /// Two claims, and deliberately not a third. Its keys must be a **subset**
    /// of the full case's — a key only the minimal case carries means the pair
    /// was mis-authored and the diff would then be about the corpus rather than
    /// about the wire. And the two must differ *somewhere*, or the second case
    /// is dead weight a later reader would delete.
    ///
    /// What is **not** asserted is that the minimal case has fewer keys.
    /// `AfterTurnResponse` has two fields and both are required, so its pair
    /// differs only inside `evidence` — and demanding a smaller key set there
    /// would be demanding that every message have an optional field, which is
    /// a claim about the wire nobody made.
    #[test]
    fn a_minimal_case_never_carries_a_key_its_full_case_omits() {
        let doc = corpus().expect("the corpus encodes");
        for section in ["requests", "responses"] {
            let cases = doc[section].as_array().expect("a section is an array");
            for pair in cases.chunks(2) {
                let (full, minimal) = (&pair[0]["message"], &pair[1]["message"]);
                let full_body = full["body"].as_object().expect("a full body is an object");
                let minimal_body = minimal["body"]
                    .as_object()
                    .expect("a minimal body is an object");
                for key in minimal_body.keys() {
                    assert!(
                        full_body.contains_key(key),
                        "{section} {} carries `{key}`, which its full case does not",
                        pair[1]["case"],
                    );
                }
                assert_ne!(
                    full, minimal,
                    "{section} {} is identical to its full case",
                    pair[1]["case"],
                );
            }
        }
    }

    /// Every signal published in the vocabulary is one a host would believe.
    #[test]
    fn the_signal_vocabulary_is_well_typed() {
        for signal in enumerate(Signal::TestCommand, signal_after) {
            assert!(well_typed(signal).is_well_typed(), "{signal:?}");
        }
    }
}
