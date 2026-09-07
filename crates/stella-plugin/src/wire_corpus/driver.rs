//! The driver channel's own frames in the wrapper corpus.
//!
//! A second dispatch context, beside the first rather than folded into it.
//! The two are different consents. A reader of this corpus should see that a
//! `drive` point is not a wrapper point.
//!
//! Split from [`super`] so neither file nears the size ceiling.
//! `doc:backlog-self-driving` §3.0 is the design.

use serde_json::Value;

use super::case;
use crate::{
    AbandonArgs, BacklogEntry, BacklogPage, ClaimReport, DecideArgs, DeliverAction, DeliverCi,
    DeliverDecision, DeliverEscalation, DeliverMergeability, DeliverObservation, DeliverReview,
    DeliverState, DriveNext, DriveRequest, DriveResponse, DriverArgs, DriverCall,
    DriverCallRequest, DriverCallResponse, DriverOk, HostCallFailure, HostCallRefusal, MergeReport,
    OpenReport, PullRequestArgs, ReadyReport, UnitArgs, WorkReport, WorkState,
};

/// The session frames: the host's opening, and the two answers that end it.
pub(super) fn session() -> Result<Value, serde_json::Error> {
    Ok(Value::Array(vec![
        case("open", &DriveRequest::new("cycle-7"))?,
        // Both terminal answers. A driver that halts and a driver that sleeps
        // are the two shapes a host must handle. Neither is the other's
        // default.
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

/// A capability ask, in both of its shapes.
///
/// `args` is the request's one optional member. So the pair is what turns a
/// move between required and optional into a diff here. `backlog_next` reads
/// no arguments and sends no key. `work_start` names the unit it is about.
///
/// Then a table for each other verb that reads one. Each is its own contract,
/// and a driver author writes against it.
pub(super) fn calls() -> Result<Value, serde_json::Error> {
    Ok(Value::Array(vec![
        case(
            "backlog_next",
            &DriverCallRequest {
                id: 1,
                call: DriverCall::BacklogNext,
                args: None,
            },
        )?,
        case(
            "backlog_claim",
            &DriverCallRequest {
                id: 2,
                call: DriverCall::BacklogClaim,
                args: Some(DriverArgs {
                    backlog_claim: Some(UnitArgs {
                        issue: "1234".into(),
                    }),
                    work_start: None,
                    work_abandon: None,
                    deliver_observe: None,
                    deliver_next: None,
                    deliver_ready: None,
                    deliver_merge: None,
                }),
            },
        )?,
        case(
            "work_start",
            &DriverCallRequest {
                id: 3,
                call: DriverCall::WorkStart,
                args: Some(DriverArgs {
                    backlog_claim: None,
                    work_start: Some(UnitArgs {
                        issue: "1234".into(),
                    }),
                    work_abandon: None,
                    deliver_observe: None,
                    deliver_next: None,
                    deliver_ready: None,
                    deliver_merge: None,
                }),
            },
        )?,
        case(
            "work_abandon",
            &DriverCallRequest {
                id: 4,
                call: DriverCall::WorkAbandon,
                args: Some(DriverArgs {
                    backlog_claim: None,
                    work_start: None,
                    work_abandon: Some(AbandonArgs {
                        reason: "the base moved under it".into(),
                    }),
                    deliver_observe: None,
                    deliver_next: None,
                    deliver_ready: None,
                    deliver_merge: None,
                }),
            },
        )?,
        case(
            "deliver_observe",
            &DriverCallRequest {
                id: 5,
                call: DriverCall::DeliverObserve,
                args: Some(DriverArgs {
                    backlog_claim: None,
                    work_start: None,
                    work_abandon: None,
                    deliver_observe: Some(PullRequestArgs { pr: "4102".into() }),
                    deliver_next: None,
                    deliver_ready: None,
                    deliver_merge: None,
                }),
            },
        )?,
        // The one ask that carries a whole reading back in, at its fullest.
        // Every fact is set and both counts are non-zero. The counts are the
        // members that may be dropped, so a `minimal` case sits beside it.
        case(
            "deliver_next",
            &DriverCallRequest {
                id: 6,
                call: DriverCall::DeliverNext,
                args: Some(DriverArgs {
                    backlog_claim: None,
                    work_start: None,
                    work_abandon: None,
                    deliver_observe: None,
                    deliver_next: Some(DecideArgs {
                        observation: observation(),
                        fixes: 1,
                        rebases: 0,
                    }),
                    deliver_ready: None,
                    deliver_merge: None,
                }),
            },
        )?,
        case(
            "deliver_next/minimal",
            &DriverCallRequest {
                id: 7,
                call: DriverCall::DeliverNext,
                args: Some(DriverArgs {
                    backlog_claim: None,
                    work_start: None,
                    work_abandon: None,
                    deliver_observe: None,
                    deliver_next: Some(DecideArgs {
                        observation: DeliverObservation {
                            pr: "4102".into(),
                            ci: DeliverCi::Pending,
                            base_ci: DeliverCi::Pending,
                            mergeable: DeliverMergeability::Unknown,
                            review: DeliverReview::None,
                            draft: false,
                            settled: false,
                        },
                        fixes: 0,
                        rebases: 0,
                    }),
                    deliver_ready: None,
                    deliver_merge: None,
                }),
            },
        )?,
        case(
            "deliver_ready",
            &DriverCallRequest {
                id: 8,
                call: DriverCall::DeliverReady,
                args: Some(DriverArgs {
                    backlog_claim: None,
                    work_start: None,
                    work_abandon: None,
                    deliver_observe: None,
                    deliver_next: None,
                    deliver_ready: Some(PullRequestArgs { pr: "4102".into() }),
                    deliver_merge: None,
                }),
            },
        )?,
        case(
            "deliver_merge",
            &DriverCallRequest {
                id: 9,
                call: DriverCall::DeliverMerge,
                args: Some(DriverArgs {
                    backlog_claim: None,
                    work_start: None,
                    work_abandon: None,
                    deliver_observe: None,
                    deliver_next: None,
                    deliver_ready: None,
                    deliver_merge: Some(PullRequestArgs { pr: "4102".into() }),
                }),
            },
        )?,
    ]))
}

/// One read of the forge. Every fact is set away from its default, so a
/// dropped member shows up as a diff.
fn observation() -> DeliverObservation {
    DeliverObservation {
        pr: "4102".into(),
        ci: DeliverCi::Red,
        base_ci: DeliverCi::Green,
        mergeable: DeliverMergeability::Clean,
        review: DeliverReview::ChangesRequested,
        draft: true,
        settled: true,
    }
}

/// The host's answers to those asks.
pub(super) fn results() -> Result<Value, serde_json::Error> {
    Ok(Value::Array(vec![
        // A verb that reports nothing answers the empty `ok` table. Those
        // are still the bytes every driver written against B0 reads. Every
        // member of `DriverOk` is dropped when absent.
        case("ok/empty", &DriverCallResponse::ok(1, DriverOk::default()))?,
        // One case per verb this host reports on. A new member here is a wire
        // change, and the corpus exists to put that on the screen.
        case(
            "ok/backlog",
            &DriverCallResponse::ok(
                4,
                DriverOk {
                    backlog: Some(BacklogPage {
                        issues: vec![BacklogEntry {
                            key: "1234".into(),
                            title: "the queue is read over a port".into(),
                            labels: vec!["bug".into(), "P1".into()],
                            url: "https://example.invalid/1234".into(),
                        }],
                    }),
                    claim: None,
                    work: None,
                    pull_request: None,
                    observation: None,
                    decision: None,
                    ready: None,
                    merge: None,
                },
            ),
        )?,
        case(
            "ok/claim",
            &DriverCallResponse::ok(
                5,
                DriverOk {
                    backlog: None,
                    claim: Some(ClaimReport {
                        issue: "1234".into(),
                        held: false,
                        holder: "self-driving:8412".into(),
                    }),
                    work: None,
                    pull_request: None,
                    observation: None,
                    decision: None,
                    ready: None,
                    merge: None,
                },
            ),
        )?,
        case(
            "ok/work",
            &DriverCallResponse::ok(
                6,
                DriverOk {
                    backlog: None,
                    claim: None,
                    work: Some(WorkReport {
                        issue: "1234".into(),
                        state: WorkState::Changed,
                        branch: "stella/1234".into(),
                        stat: " 2 files changed, 31 insertions(+)".into(),
                        detail: String::new(),
                    }),
                    pull_request: None,
                    observation: None,
                    decision: None,
                    ready: None,
                    merge: None,
                },
            ),
        )?,
        // The emptiest legal report: a slot with nothing in it. Every
        // optional member is dropped, so a driver reads `state` alone.
        case(
            "ok/work/idle",
            &DriverCallResponse::ok(
                7,
                DriverOk {
                    backlog: None,
                    claim: None,
                    work: Some(WorkReport::default()),
                    pull_request: None,
                    observation: None,
                    decision: None,
                    ready: None,
                    merge: None,
                },
            ),
        )?,
        case(
            "ok/pull_request",
            &DriverCallResponse::ok(
                8,
                DriverOk {
                    backlog: None,
                    claim: None,
                    work: None,
                    pull_request: Some(OpenReport {
                        issue: "1234".into(),
                        pr: "4102".into(),
                        branch: "stella/1234".into(),
                    }),
                    observation: None,
                    decision: None,
                    ready: None,
                    merge: None,
                },
            ),
        )?,
        case(
            "ok/observation",
            &DriverCallResponse::ok(
                9,
                DriverOk {
                    backlog: None,
                    claim: None,
                    work: None,
                    pull_request: None,
                    observation: Some(observation()),
                    decision: None,
                    ready: None,
                    merge: None,
                },
            ),
        )?,
        // Both shapes of decision. `escalation` is the family's one
        // omissible member. An escalation names what ran out. Every other
        // action drops the key.
        case(
            "ok/decision",
            &DriverCallResponse::ok(
                10,
                DriverOk {
                    backlog: None,
                    claim: None,
                    work: None,
                    pull_request: None,
                    observation: None,
                    decision: Some(DeliverDecision {
                        pr: "4102".into(),
                        state: DeliverState::Escalated,
                        action: DeliverAction::Escalate,
                        escalation: Some(DeliverEscalation::ReviewNeedsHuman),
                    }),
                    ready: None,
                    merge: None,
                },
            ),
        )?,
        case(
            "ok/decision/minimal",
            &DriverCallResponse::ok(
                11,
                DriverOk {
                    backlog: None,
                    claim: None,
                    work: None,
                    pull_request: None,
                    observation: None,
                    decision: Some(DeliverDecision {
                        pr: "4102".into(),
                        state: DeliverState::CiPending,
                        action: DeliverAction::Wait,
                        escalation: None,
                    }),
                    ready: None,
                    merge: None,
                },
            ),
        )?,
        case(
            "ok/ready",
            &DriverCallResponse::ok(
                12,
                DriverOk {
                    backlog: None,
                    claim: None,
                    work: None,
                    pull_request: None,
                    observation: None,
                    decision: None,
                    ready: Some(ReadyReport { pr: "4102".into() }),
                    merge: None,
                },
            ),
        )?,
        case(
            "ok/merge",
            &DriverCallResponse::ok(
                13,
                DriverOk {
                    backlog: None,
                    claim: None,
                    work: None,
                    pull_request: None,
                    observation: None,
                    decision: None,
                    ready: None,
                    merge: Some(MergeReport { pr: "4102".into() }),
                },
            ),
        )?,
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
                HostCallFailure::new(
                    HostCallRefusal::Unsupported,
                    "this host does not perform \"work_start\" yet",
                ),
            ),
        )?,
    ]))
}
