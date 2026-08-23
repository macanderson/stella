// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A machine-checked description of the **wrapper socket** wire format.
//!
//! `doc:wrapper-socket` §3 commitment 2 says `docs/wire/` is generated and
//! gate-checked "precisely so that a renamed field or a re-tagged variant lands
//! on the author's screen instead of in a consumer's parser; the wrapper wire
//! contract joins it on the same terms". This module is that join.
//!
//! [`crate::WrapperRequest`] and [`crate::WrapperResponse`] were the only two
//! things that crossed the process boundary until the host-call channel landed
//! (#3540, `doc:wrapper-socket` §6b); [`crate::HostCallRequest`] and
//! [`crate::HostCallResponse`] are the other two, and they cross the same pipes
//! in the same conversation. The driver channel (#3599 B0) adds its own
//! roots — [`crate::DriveRequest`]/[`crate::DriveResponse`] and
//! [`crate::DriverCallRequest`]/[`crate::DriverCallResponse`]. Every root
//! here is published along with everything reachable from it.
//!
//! # Why a corpus, beside the schema rather than instead of it
//!
//! This module was written as a **declared gap**: `stella-protocol` and
//! `stella-serve` publish JSON Schema derived by `schemars`, this published a
//! corpus, and the reason was mechanical — a `JsonSchema` impl only exists
//! where the type is defined, and `crate::wire` was held by another session.
//! The derives are on those types now and
//! [`crate::wire_schema`] publishes `wrapper.schema.json` from them (#3532).
//!
//! The corpus stays, because the two artifacts answer different questions. This
//! one publishes every wire message serialized by the same `Serialize` impls
//! the socket uses, in both its fullest and its emptiest legal form. What that
//! catches:
//!
//! - a renamed field — the key changes in every case that carries it;
//! - a re-tagged variant or a changed `rename_all` — the tag changes;
//! - an added or removed field — a key appears or disappears in the full case;
//! - an optional field made required, or a required one made optional — the
//!   *minimal* case is what makes this visible, which is why a message with an
//!   optional member appears twice. A message with none appears once: a second
//!   identical case would assert an optionality that does not exist.
//!
//! What it does **not** catch, and what the schema does: a widened or narrowed
//! scalar type (`u32` → `u64`), and a string field that gains a format or
//! pattern constraint. Neither changes any byte of the corpus.
//!
//! What the schema does not catch, and this does: the **bytes**. A schema is
//! not runnable backwards into a rendering — a field that starts serializing as
//! `null` rather than being omitted, a `skip_serializing_if` that stops firing,
//! a tag whose spelling and whose schema move together — all leave a legal
//! document behind. The corpus shows the two exact strings a plugin's parser
//! will meet. It also covers the host-call and driver channels, which the
//! schema deliberately does not.
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
    AdoptCandidateArgs, AdoptCandidateResult, AfterTurnRequest, AfterTurnResponse,
    BeforeTurnRequest, BeforeTurnResponse, CandidateFanoutArgs, CandidateFanoutResult,
    CandidateGrant, ChildTurnArgs, ChildTurnResult, DriveNext, DriveRequest, DriveResponse,
    DriverCall, DriverCallRequest, DriverCallResponse, DriverOk, FanoutCandidate, FlipObservation,
    HostCall, HostCallArgs, HostCallFailure, HostCallOk, HostCallRefusal, HostCallRequest,
    HostCallResponse, HostStage, ObservedEvidence, PROTOCOL_VERSION, PublishedSignal, RecallArgs,
    RecallFrame, RecallResult, RunTestArgs, Signal, SignalKind, SignalValue, StageName,
    TestBaseline, TestPlan, TestRunResult, TurnOutcome, VolatileContext, WrapperPoint,
    WrapperRequest, WrapperResponse,
};

/// The committed artifact's filename.
pub const WRAPPER_WIRE: &str = "wrapper.wire.json";

/// What a reader of the generated file is told before they edit it.
const NOTE: &str = "GENERATED FILE — DO NOT EDIT. Every message the wrapper \
     socket carries, serialized by the same impls stella_runtime's subprocess \
     transport uses. Regenerate with `bash scripts/export-agentevent-schema.sh`; \
     guarded by `scripts/check-wire-schema.sh` (`make wire-schema`). Source of \
     truth: crates/stella-plugin/src/wire.rs, src/host_call.rs and \
     src/driver.rs. A message \
     with an optional member appears twice — `full` populates every optional \
     field, `minimal` omits every one that may be omitted — so a field \
     changing between required and optional is a diff here. This is a corpus, \
     not a JSON Schema; wrapper.schema.json is the derived schema beside it, \
     and neither subsumes the other — see \
     crates/stella-plugin/src/wire_corpus.rs and src/wire_schema.rs for what \
     each does and does not catch (#3532).";

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
        "host_calls": host_calls()?,
        "host_results": host_results()?,
        "driver_session": driver_session()?,
        "driver_calls": driver_calls()?,
        "driver_results": driver_results()?,
        "parts": parts()?,
        "vocabulary": vocabulary()?,
    }))
}

/// One labelled case: the name a diff reads, and the bytes it is about.
fn case<T: Serialize>(name: &'static str, message: &T) -> Result<Value, serde_json::Error> {
    Ok(json!({ "case": name, "message": serde_json::to_value(message)? }))
}

/// One successful host-call answer, named by [`ok_case`] rather than by hand.
///
/// The naming is the point: it routes every published answer through the
/// `match` that makes [`HostCallOk`] total here, so a new variant cannot reach
/// this list unnamed. `suffix` distinguishes the `full`/`minimal` pair for the
/// one variant that has an omissible member.
fn ok_result(id: u32, ok: HostCallOk, suffix: &str) -> Result<Value, serde_json::Error> {
    let name = format!("{}{suffix}", ok_case(&ok));
    Ok(json!({
        "case": name,
        "message": serde_json::to_value(HostCallResponse::ok(id, ok))?,
    }))
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

/// The calls a plugin may make mid-point (`doc:wrapper-socket` §6b).
///
/// The other half of the conversation, and it is published on the same terms:
/// these are messages that cross the process boundary, so a renamed field or a
/// re-tagged capability has to land as a diff here rather than in a plugin's
/// parser. A capability with an optional member appears in its fullest and
/// emptiest legal form, for the reason every other message does — that pair is
/// what makes `limit` going required a diff.
fn host_calls() -> Result<Value, serde_json::Error> {
    Ok(Value::Array(vec![
        case("recall/full", &recall_call_full())?,
        case("recall/minimal", &recall_call_minimal())?,
        // No optional member, so no pair: publishing one value twice under two
        // names would assert an optionality that does not exist.
        case("child_turn", &child_turn_call())?,
        case("run_test", &run_test_call())?,
        case("candidate_fanout", &candidate_fanout_call())?,
        case("adopt_candidate", &adopt_candidate_call())?,
    ]))
}

/// The host's answers to those calls.
fn host_results() -> Result<Value, serde_json::Error> {
    Ok(Value::Array(vec![
        ok_result(1, HostCallOk::Recall(recall_result_full()), "/full")?,
        ok_result(1, HostCallOk::Recall(RecallResult::default()), "/minimal")?,
        // [`HostCallOk`] is untagged, so the `ok` table is the only thing that
        // tells a plugin which result it is holding. Publishing it is therefore
        // publishing the *discriminator*: a key renamed here does not merely
        // change a field, it makes a `child_turn` answer decode as the `recall`
        // variant tried before it. No optional member, so no pair.
        ok_result(4, HostCallOk::ChildTurn(child_turn_result()), "")?,
        // The one result with an omissible member, so it appears twice: a
        // `run_test` that observed nothing prints no `output` key at all, and a
        // reader that started requiring it would show up here.
        ok_result(3, HostCallOk::RunTest(test_run_result_full()), "/full")?,
        ok_result(
            3,
            HostCallOk::RunTest(test_run_result_minimal()),
            "/minimal",
        )?,
        ok_result(
            5,
            HostCallOk::CandidateFanout(candidate_fanout_result()),
            "",
        )?,
        // Both its members are required, which is exactly what keeps it out of
        // the `recall` variant tried before it: `RecallResult`'s only field
        // defaults, so an empty table already belongs to that arm. Publishing
        // the adoption answer beside it makes the same point once more — the
        // two tables share no key at all, and this file is where a change to
        // that becomes a diff.
        ok_result(6, HostCallOk::AdoptCandidate(adopt_candidate_result()), "")?,
        case(
            "err/full",
            &HostCallResponse::err(
                2,
                HostCallFailure::new(
                    HostCallRefusal::Undeclared,
                    "this plugin's manifest does not declare \"child_turn\" in [loop] calls",
                ),
            ),
        )?,
        case(
            "err/minimal",
            &HostCallResponse::err(2, HostCallFailure::new(HostCallRefusal::Failed, "")),
        )?,
    ]))
}

/// The driver channel's session frames — the host's opening and the two
/// answers that end it (`doc:backlog-self-driving` §3.0, #3599 B0).
///
/// A second dispatch context, published beside the first rather than folded
/// into it, because the two are different consents and a reader of this corpus
/// should be able to see that a `drive` point is not a wrapper point.
fn driver_session() -> Result<Value, serde_json::Error> {
    Ok(Value::Array(vec![
        case("open", &DriveRequest::new("cycle-7"))?,
        // Both terminal answers, because a driver that halts and a driver that
        // sleeps are the two shapes a host must handle and neither is the
        // other's default.
        case(
            "sleep",
            &DriveResponse {
                next: DriveNext::Sleep { secs: 900 },
            },
        )?,
        case(
            "halt",
            &DriveResponse {
                next: DriveNext::Halt {
                    reason: "budget spent".into(),
                },
            },
        )?,
    ]))
}

/// A capability ask, in the one shape it has at B0.
///
/// No `args` key, and no `full`/`minimal` pair, because the request carries no
/// optional member — the arguments land with the verb that needs them, and an
/// `args` appearing here later is exactly the diff that should be reviewed.
fn driver_calls() -> Result<Value, serde_json::Error> {
    Ok(Value::Array(vec![case(
        "backlog_next",
        &DriverCallRequest {
            id: 1,
            call: DriverCall::BacklogNext,
        },
    )?]))
}

/// The host's answers to those asks.
fn driver_results() -> Result<Value, serde_json::Error> {
    Ok(Value::Array(vec![
        // The empty `ok` table is the contract, not an omission: a host
        // answering with a payload the driver's reader denies is a decode
        // error, so this case going non-empty is a wire change.
        case("ok", &DriverCallResponse::ok(1, DriverOk {}))?,
        case(
            "err/undeclared",
            &DriverCallResponse::err(
                2,
                HostCallFailure::new(
                    HostCallRefusal::Undeclared,
                    "this plugin's manifest does not declare \"deliver_merge\" in [driver] calls",
                ),
            ),
        )?,
        case(
            "err/unsupported",
            &DriverCallResponse::err(
                3,
                HostCallFailure::new(HostCallRefusal::Unsupported, "B0 serves no capability yet"),
            ),
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
        case(
            "turn_outcome/measured-nothing",
            &turn_outcome_measured_nothing(),
        )?,
        case("turn_outcome/minimal", &turn_outcome_minimal())?,
        case("observed_evidence/full", &observed_evidence_full())?,
        case("observed_evidence/minimal", &observed_evidence_minimal())?,
        case("volatile_context", &volatile_context())?,
        case("recall_frame/full", &recall_frame_full())?,
        case("recall_frame/minimal", &recall_frame_minimal())?,
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
        "stage": values(stage_vocabulary())?,
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
        "host_call": values(enumerate(HostCall::Recall, host_call_after))?,
        "host_call_refusal": values(enumerate(
            HostCallRefusal::Undeclared,
            host_call_refusal_after,
        ))?,
        // Published from `DriverCall::all()` rather than walked with a
        // successor function: the list is already the enum's own source of
        // truth for the consent rendering, and a second walk would be a second
        // thing to keep exhaustive.
        "driver_call": values(DriverCall::all().to_vec())?,
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

/// The stage vocabulary, which is the one list on this wire that is **not**
/// closed (#3963).
///
/// The host's own twelve are still enumerated by an exhaustive successor —
/// the compiler still refuses a thirteenth boundary that is not placed in the
/// chain — and a contributed name rides at the end. That last entry is the
/// part a reader of the committed corpus most needs: it is what says the field
/// is a plain string a plugin may fill with its own word, and not the enum the
/// twelve above could otherwise be mistaken for.
fn stage_vocabulary() -> Vec<StageName> {
    enumerate(HostStage::Triage, stage_after)
        .into_iter()
        .map(StageName::Host)
        .chain(std::iter::once(StageName::new(CONTRIBUTED_STAGE_SAMPLE)))
        .collect()
}

/// The contributed stage the corpus carries. `triage-lite` is the name
/// `doc:roleless-core` and #3963 both use for the worked example, so the
/// committed artifact reads as the same story the spec tells.
const CONTRIBUTED_STAGE_SAMPLE: &str = "triage-lite";

fn stage_after(stage: HostStage) -> Option<HostStage> {
    match stage {
        HostStage::Triage => Some(HostStage::Recall),
        HostStage::Recall => Some(HostStage::Research),
        HostStage::Research => Some(HostStage::Plan),
        HostStage::Plan => Some(HostStage::Scope),
        HostStage::Scope => Some(HostStage::Execute),
        HostStage::Execute => Some(HostStage::Witness),
        HostStage::Witness => Some(HostStage::Verify),
        HostStage::Verify => Some(HostStage::Verdict),
        HostStage::Verdict => Some(HostStage::Reflect),
        HostStage::Reflect => Some(HostStage::ContextWrite),
        HostStage::ContextWrite => Some(HostStage::Complete),
        HostStage::Complete => None,
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

/// The capability vocabulary. Closed by design, and this chain is what keeps a
/// fourth capability from reaching the wire without reaching the corpus.
fn host_call_after(call: HostCall) -> Option<HostCall> {
    match call {
        HostCall::Recall => Some(HostCall::ChildTurn),
        HostCall::ChildTurn => Some(HostCall::RunTest),
        HostCall::RunTest => Some(HostCall::CandidateFanout),
        HostCall::CandidateFanout => Some(HostCall::AdoptCandidate),
        HostCall::AdoptCandidate => None,
    }
}

/// The refusal vocabulary — the codes a plugin branches on to degrade.
fn host_call_refusal_after(refusal: HostCallRefusal) -> Option<HostCallRefusal> {
    use HostCallRefusal as R;
    match refusal {
        R::Undeclared => Some(R::Forbidden),
        R::Forbidden => Some(R::Unsupported),
        R::Unsupported => Some(R::AllowanceSpent),
        R::AllowanceSpent => Some(R::Unavailable),
        R::Unavailable => Some(R::Failed),
        R::Failed => None,
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
        stage: StageName::Host(HostStage::Execute),
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
        stage: StageName::Host(HostStage::Triage),
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
        stage: Some(StageName::Host(HostStage::Execute)),
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
        witness: vec!["tests/flip.rs".to_string()],
        publish: vec![well_typed(Signal::FlipAchieved)],
    }
}

fn before_turn_response_minimal() -> BeforeTurnResponse {
    BeforeTurnResponse {
        protocol_version: PROTOCOL_VERSION,
        context: Vec::new(),
        role: None,
        scope: Vec::new(),
        witness: Vec::new(),
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
        tools: Some(vec!["read_file".to_string(), "edit_file".to_string()]),
        changed_files: Some(vec!["crates/stella-plugin/src/wire.rs".to_string()]),
    }
}

/// A host that **does** report both facts, about a turn that did nothing — the
/// `[]` half of the `null`-vs-`[]` distinction #3552 turns on. Published beside
/// the other two so a future edit that collapses the empty case back into the
/// absent one is a diff in this corpus rather than a silent re-widening.
fn turn_outcome_measured_nothing() -> TurnOutcome {
    TurnOutcome {
        completed: true,
        answer: "nothing needed changing".to_string(),
        tools: Some(Vec::new()),
        changed_files: Some(Vec::new()),
    }
}

/// A host that reports neither fact: both keys are absent, which is "not
/// measured here" and never "empty".
fn turn_outcome_minimal() -> TurnOutcome {
    TurnOutcome {
        completed: false,
        answer: String::new(),
        tools: None,
        changed_files: None,
    }
}

fn observed_evidence_full() -> ObservedEvidence {
    ObservedEvidence {
        flip: FlipObservation::Achieved,
        measurements: BTreeMap::from([("p50_ms".to_string(), 103)]),
        detail: Some("the retry budget is still unbounded on the timeout path".to_string()),
    }
}

fn observed_evidence_minimal() -> ObservedEvidence {
    ObservedEvidence {
        flip: FlipObservation::Unobservable,
        measurements: BTreeMap::new(),
        detail: None,
    }
}

fn recall_call_full() -> HostCallRequest {
    HostCallRequest {
        id: 1,
        args: HostCallArgs::Recall(RecallArgs {
            goal: "make the failing test pass".to_string(),
            limit: Some(8),
        }),
    }
}

fn recall_call_minimal() -> HostCallRequest {
    HostCallRequest {
        id: 1,
        args: HostCallArgs::Recall(RecallArgs {
            goal: "make the failing test pass".to_string(),
            limit: None,
        }),
    }
}

/// `child_turn` has no optional member, so its two cases are the same value —
/// published anyway, because the pair is what makes a field *becoming* optional
/// a diff.
fn child_turn_call() -> HostCallRequest {
    HostCallRequest {
        id: 2,
        args: HostCallArgs::ChildTurn(ChildTurnArgs {
            role: "verifier".to_string(),
            instruction: "grade the diff against the requirement".to_string(),
        }),
    }
}

fn run_test_call() -> HostCallRequest {
    HostCallRequest {
        id: 3,
        args: HostCallArgs::RunTest(RunTestArgs {
            candidate: CandidateHandle::new("candidate-1"),
        }),
    }
}

fn candidate_fanout_call() -> HostCallRequest {
    HostCallRequest {
        id: 4,
        args: HostCallArgs::CandidateFanout(CandidateFanoutArgs {
            role: "candidate".to_string(),
            instruction: "make the failing test pass".to_string(),
            width: 3,
        }),
    }
}

fn adopt_candidate_call() -> HostCallRequest {
    HostCallRequest {
        id: 5,
        args: HostCallArgs::AdoptCandidate(AdoptCandidateArgs {
            candidate: CandidateHandle::new("candidate-2"),
        }),
    }
}

fn candidate_fanout_result() -> CandidateFanoutResult {
    CandidateFanoutResult {
        // Asked for three, ran two: the clamp is a fact the corpus publishes
        // rather than a case a reader has to imagine.
        requested: 3,
        candidates: vec![
            FanoutCandidate {
                candidate: CandidateHandle::new("candidate-1"),
                root: "/tmp/stella-candidates/candidate-1".to_string(),
                report: "added the retry and its regression test".to_string(),
                completed: true,
                files_changed: 2,
                lines_changed: 41,
            },
            FanoutCandidate {
                candidate: CandidateHandle::new("candidate-2"),
                root: "/tmp/stella-candidates/candidate-2".to_string(),
                report: "rewrote the backoff; the test still fails".to_string(),
                completed: false,
                files_changed: 1,
                lines_changed: 12,
            },
        ],
    }
}

fn adopt_candidate_result() -> AdoptCandidateResult {
    AdoptCandidateResult {
        adopted: CandidateHandle::new("candidate-1"),
        discarded: vec![CandidateHandle::new("candidate-2")],
    }
}

fn test_run_result_full() -> TestRunResult {
    TestRunResult {
        candidate: CandidateHandle::new("candidate-1"),
        assertions: TestBaseline::Passed,
        output: "test tests::flip ... ok".to_string(),
    }
}

fn test_run_result_minimal() -> TestRunResult {
    TestRunResult {
        candidate: CandidateHandle::new("candidate-1"),
        assertions: TestBaseline::Unobserved,
        output: String::new(),
    }
}

/// Totality for [`HostCallOk`], whose arms carry payloads and so cannot be
/// walked by the fieldless successor the closed enums above use.
///
/// A `match` with no wildcard is still the compiler's check, which is the
/// property this file is built on: adding a variant to that union is an `E0004`
/// **here**, in the file that publishes it, rather than a silent omission from
/// the one artifact a plugin author reads to learn what the discriminator is.
/// It was a hand-written list until `run_test` was added (#3580) and nothing
/// would have noticed the omission.
fn ok_case(ok: &HostCallOk) -> &'static str {
    match ok {
        HostCallOk::Recall(_) => "recall",
        HostCallOk::ChildTurn(_) => "child_turn",
        HostCallOk::RunTest(_) => "run_test",
        HostCallOk::CandidateFanout(_) => "candidate_fanout",
        HostCallOk::AdoptCandidate(_) => "adopt_candidate",
    }
}

fn child_turn_result() -> ChildTurnResult {
    ChildTurnResult {
        role: "reviewer".to_string(),
        seat: "research".to_string(),
        report: "the diff drops the retry on a 429".to_string(),
        completed: true,
    }
}

fn recall_result_full() -> RecallResult {
    RecallResult {
        frames: vec![recall_frame_full()],
    }
}

fn recall_frame_full() -> RecallFrame {
    RecallFrame {
        label: "symbol: retry_budget".to_string(),
        kind: "symbol".to_string(),
        source: "codegraph.db".to_string(),
        uri: Some("src/retry.rs".to_string()),
        content: "pub const RETRY_BUDGET: u32 = 3;".to_string(),
    }
}

fn recall_frame_minimal() -> RecallFrame {
    RecallFrame {
        label: "lesson: retries".to_string(),
        kind: "memory".to_string(),
        source: "context.db".to_string(),
        uri: None,
        content: "the retry budget was exhausted last run".to_string(),
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
        for entry in doc["host_calls"]
            .as_array()
            .expect("host_calls is an array")
        {
            let message = &entry["message"];
            let decoded: HostCallRequest =
                serde_json::from_value(message.clone()).expect("a published host call decodes");
            assert_eq!(
                &serde_json::to_value(&decoded).expect("re-encodes"),
                message
            );
        }
        for entry in doc["host_results"]
            .as_array()
            .expect("host_results is an array")
        {
            let message = &entry["message"];
            let decoded: HostCallResponse =
                serde_json::from_value(message.clone()).expect("a published answer decodes");
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
        for section in ["requests", "responses", "host_calls", "host_results"] {
            let cases = doc[section].as_array().expect("a section is an array");
            // Paired by name rather than by position. A `chunks(2)` walk was
            // enough while every message had exactly two cases; the host-call
            // section publishes capabilities with no optional member at all
            // (`child_turn`), so pairing by position would compare two
            // different capabilities and call the difference optionality.
            for entry in cases {
                let case = entry["case"].as_str().expect("a case is named");
                let Some(stem) = case.strip_suffix("/minimal") else {
                    continue;
                };
                let full = cases
                    .iter()
                    .find(|other| other["case"] == json!(format!("{stem}/full")))
                    .unwrap_or_else(|| panic!("{section} {case} has no /full case beside it"));
                // A point message carries its payload under `body`; a host-call
                // message is flat. Either way what is compared is the table a
                // consumer parses.
                let table = |message: &Value| {
                    message
                        .get("body")
                        .unwrap_or(message)
                        .as_object()
                        .expect("a published message is an object")
                        .clone()
                };
                let full_body = table(&full["message"]);
                let minimal_body = table(&entry["message"]);
                for key in minimal_body.keys() {
                    assert!(
                        full_body.contains_key(key),
                        "{section} {case} carries `{key}`, which its full case does not",
                    );
                }
                assert_ne!(
                    full["message"], entry["message"],
                    "{section} {case} is identical to its full case",
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
