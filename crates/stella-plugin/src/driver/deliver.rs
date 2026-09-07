//! The `deliver` family's wire tables: what a pull-request ask carries in,
//! and what the host answers with.
//!
//! `doc:backlog-self-driving` §3.3. [`super`] is the channel these ride on. Its
//! header states the rule they land under. A table arrives with the verb that
//! reads it, never ahead of it.
//!
//! # Why the words are spelled again here
//!
//! `stella_autonomy` already names all of this. It has `CiConclusion`,
//! `Mergeability`, `PrState` and `Action`. This crate may not take that
//! dependency. AGENTS.md's workspace table makes `stella-protocol` its only
//! workspace edge. Widening it would push the loop's policy types into every
//! plugin manifest reader in the tree.
//!
//! So these are the wire's own names. The host maps between the two sets in
//! `crates/stella-cli/src/driver_plugin/deliver.rs`. That map is total both
//! ways, so the two vocabularies cannot drift apart.
//!
//! # What a driver may decide with these
//!
//! [`DecideArgs`] carries the driver's own reading. So `deliver_next` is
//! arithmetic over facts the driver already holds. It buys no second read of
//! the forge.
//!
//! The merge does not work that way. The host reads the forge itself first. A
//! driver that reported a green build it never saw gets a refusal.

use serde::{Deserialize, Serialize};

/// Which pull request an ask is about.
///
/// The forge's own number, as a string. [`super::BacklogEntry::key`] gives the
/// reason: a second forge numbers differently, and a driver hands back what
/// the host handed it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PullRequestArgs {
    /// The pull request.
    pub pr: String,
}

/// What one read of the forge said about a pull request's checks.
///
/// Three, not a forge's whole vocabulary. A cancelled run, a skipped run and a
/// run that never started all mean "no answer yet" here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliverCi {
    /// No answer yet. Waiting is the only thing left to do.
    #[default]
    Pending,
    /// Every check that may block passed.
    Green,
    /// At least one of them failed.
    Red,
}

/// Whether the branch merges as it stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliverMergeability {
    /// No conflict with the base.
    Clean,
    /// The base moved under it.
    Conflicted,
    /// The forge has not worked it out yet. Apart from [`Self::Clean`], and the
    /// default. Reading "not known yet" as "clean" is how a merge runs into a
    /// conflict.
    #[default]
    Unknown,
}

/// What human review has said, if anything.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliverReview {
    /// Nobody has reviewed it.
    #[default]
    None,
    /// A reviewer asked for changes. Outranks a green build.
    ChangesRequested,
    /// A reviewer approved it.
    Approved,
}

/// One read of the forge. Facts only. The decision is [`DeliverDecision`].
///
/// Every fact a decision reads is required on the wire. Leave one out and the
/// decision runs over a default nobody observed. The default that matters most
/// is an unknown mergeability, and it reads as "wait".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliverObservation {
    /// The pull request this is about, echoed back so a driver can join it.
    pub pr: String,
    /// What this pull request's checks said.
    pub ci: DeliverCi,
    /// What the *same* checks said on the base branch.
    ///
    /// The whole comparison rests on this. Without it a red build looks the
    /// same as an inherited one. A loop then spends its budget fixing somebody
    /// else's breakage.
    pub base_ci: DeliverCi,
    /// Whether the branch still merges cleanly.
    pub mergeable: DeliverMergeability,
    /// What review has said.
    pub review: DeliverReview,
    /// Whether it is still a draft.
    #[serde(default)]
    pub draft: bool,
    /// Whether it has already merged or closed.
    ///
    /// Beside the facts rather than among them. The machine does not decide
    /// over it. A merged pull request reports an unknown mergeability, which
    /// reads as "wait" forever.
    #[serde(default)]
    pub settled: bool,
}

/// What `deliver_next` decides over: one reading, and what this loop has spent
/// on this pull request so far.
///
/// The counts are the driver's own. The host keeps no memory of a driver's
/// earlier cycles. A session is one conversation, and a loop that pushed two
/// fixes across two days is the only party that knows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecideArgs {
    /// What the driver read.
    pub observation: DeliverObservation,
    /// Fix pushes already made against this pull request's own red build.
    #[serde(default)]
    pub fixes: u32,
    /// Rebases already made against a moved base.
    #[serde(default)]
    pub rebases: u32,
}

/// Where a pull request the loop opened now stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliverState {
    /// Opened, not yet pushed for checks.
    #[default]
    Draft,
    /// Checks are running, or the forge has not worked out whether it merges.
    /// Both mean "no answer yet".
    CiPending,
    /// Its checks failed and the base is green. This one is ours.
    CiRed,
    /// Its checks failed and so does the base. This one is not ours.
    BaseBroken,
    /// The base moved under it.
    Conflicted,
    /// Green, out of draft, waiting on review.
    ReadyForReview,
    /// A reviewer asked for changes.
    ReviewChangesRequested,
    /// Cleared to merge.
    Approved,
    /// Landed.
    Merged,
    /// Handed to a human.
    Escalated,
}

/// The single next thing to do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliverAction {
    /// Read again later. The right answer far more often than the others.
    #[default]
    Wait,
    /// Write a fix for this pull request's own failure, and push it.
    PushFix,
    /// Rebase onto the moved base and push.
    Rebase,
    /// Take it out of draft.
    MarkReady,
    /// Merge it.
    Merge,
    /// Stop, and tell a human why. [`DeliverDecision::escalation`] says what
    /// ran out.
    Escalate,
}

/// Why the machine gave up.
///
/// Typed, because a human reading an escalation needs to know what to do next.
/// Fix something, wait for something, or raise a ceiling are three different
/// moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliverEscalation {
    /// The same build kept failing after the ceiling.
    FixCeilingReached,
    /// The branch kept conflicting after the ceiling.
    RebaseCeilingReached,
    /// A reviewer asked for something the loop may not decide.
    ReviewNeedsHuman,
}

/// What the machine decided, given one reading.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliverDecision {
    /// The pull request this is about.
    pub pr: String,
    /// Where it now stands.
    pub state: DeliverState,
    /// The one next action.
    pub action: DeliverAction,
    /// What ran out, when the action is [`DeliverAction::Escalate`].
    ///
    /// Absent otherwise. A reason on a `wait` would be a sentence about an
    /// escalation that never happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<DeliverEscalation>,
}

/// The pull request `deliver_open` opened.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenReport {
    /// The unit it closes.
    pub issue: String,
    /// The pull request, as the forge numbers it.
    pub pr: String,
    /// The branch it carries.
    pub branch: String,
}

/// The pull request `deliver_merge` merged.
///
/// One field. The ask succeeding is the whole answer. A merge that did not
/// happen comes back as a refusal naming what the host saw, not as a report
/// with a false flag in it. The number is here so a driver that sent two asks
/// can tell which one landed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeReport {
    /// The pull request that merged.
    pub pr: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every fact the machine reads is required on the wire. An ask that
    /// leaves one out fails to decode. It does not get a decision over a
    /// default nobody observed.
    #[test]
    fn an_observation_missing_a_fact_the_machine_reads_does_not_decode() {
        let err = serde_json::from_str::<DeliverObservation>(
            r#"{"pr":"7","ci":"green","mergeable":"clean","review":"approved"}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("base_ci"), "{err}");
    }

    /// `draft` and `settled` are the two the machine does not read. Both
    /// default to the answer that keeps a loop moving.
    #[test]
    fn the_two_facts_the_machine_does_not_read_may_be_left_out() {
        let observed: DeliverObservation = serde_json::from_str(
            r#"{"pr":"7","ci":"green","base_ci":"green","mergeable":"clean","review":"approved"}"#,
        )
        .expect("an observation without the two optional facts reads back");
        assert!(!observed.draft);
        assert!(!observed.settled);
    }

    /// An escalation reason rides only on the action that has one. So a driver
    /// reading `escalation` never reads a sentence about something that did
    /// not happen.
    #[test]
    fn a_decision_that_is_not_an_escalation_carries_no_reason() {
        let waiting = DeliverDecision {
            pr: "7".into(),
            state: DeliverState::CiPending,
            action: DeliverAction::Wait,
            escalation: None,
        };
        let json = serde_json::to_string(&waiting).expect("a decision serializes");
        assert_eq!(json, r#"{"pr":"7","state":"ci_pending","action":"wait"}"#);
        assert_eq!(
            serde_json::from_str::<DeliverDecision>(&json).expect("and reads back"),
            waiting
        );

        let gave_up = DeliverDecision {
            pr: "7".into(),
            state: DeliverState::Escalated,
            action: DeliverAction::Escalate,
            escalation: Some(DeliverEscalation::ReviewNeedsHuman),
        };
        let json = serde_json::to_string(&gave_up).expect("an escalation serializes");
        assert!(
            json.contains(r#""escalation":"review_needs_human""#),
            "{json}"
        );
        assert_eq!(
            serde_json::from_str::<DeliverDecision>(&json).expect("and reads back"),
            gave_up
        );
    }

    /// A `deliver` ask and its answer survive the shared envelope. The ask
    /// names the one verb whose table it carries.
    #[test]
    fn a_deliver_ask_and_its_answer_round_trip() {
        use super::super::{
            DriverArgs, DriverCall, DriverCallRequest, DriverCallResponse, DriverMessage, DriverOk,
        };

        let ask = DriverMessage::Call(DriverCallRequest {
            id: 9,
            call: DriverCall::DeliverMerge,
            args: Some(DriverArgs {
                deliver_merge: Some(PullRequestArgs { pr: "7".into() }),
                ..DriverArgs::default()
            }),
        });
        let json = serde_json::to_string(&ask).expect("a merge ask serializes");
        assert_eq!(
            json,
            r#"{"id":9,"call":"deliver_merge","args":{"deliver_merge":{"pr":"7"}}}"#
        );
        assert_eq!(
            serde_json::from_str::<DriverMessage>(&json).expect("and reads back"),
            ask
        );
        let DriverMessage::Call(call) = &ask else {
            panic!("a call decoded as a session response");
        };
        assert_eq!(
            call.args
                .as_ref()
                .expect("the ask carries a table")
                .tables(),
            vec![DriverCall::DeliverMerge]
        );

        let answered = DriverCallResponse::ok(
            9,
            DriverOk {
                merge: Some(MergeReport { pr: "7".into() }),
                ..DriverOk::default()
            },
        );
        let json = serde_json::to_string(&answered).expect("a merge report serializes");
        assert_eq!(
            serde_json::from_str::<DriverCallResponse>(&json).expect("and reads back"),
            answered
        );
    }

    /// The counts a driver keeps are its own. A driver that has tried nothing
    /// sends neither.
    #[test]
    fn a_decide_ask_may_carry_no_attempts_at_all() {
        let asked: DecideArgs = serde_json::from_str(
            r#"{"observation":{"pr":"7","ci":"red","base_ci":"green","mergeable":"clean","review":"none"}}"#,
        )
        .expect("a first attempt reads back");
        assert_eq!(asked.fixes, 0);
        assert_eq!(asked.rebases, 0);
        assert_eq!(asked.observation.ci, DeliverCi::Red);
    }
}
