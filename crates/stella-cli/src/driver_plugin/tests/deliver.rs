// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `deliver` verbs, over a forge that answers from memory.
//!
//! Split from [`super`] so neither file nears the size ceiling. The two halves
//! also ask different questions. That module is about binding a driver to its
//! process. This one is about what the host does with a branch.

use std::sync::Arc;

use stella_autonomy::{CiConclusion, Mergeability, Observation, ReviewState};
use stella_plugin::{
    DecideArgs, DeliverAction, DeliverCi, DeliverMergeability, DeliverObservation, DeliverReview,
    DeliverState, PullRequestArgs,
};

use super::*;
use crate::driver_plugin::deliver::DeliverForge;
use crate::self_driving_cmd::deliver::Reading;

/// A forge that answers from memory. No test here runs `git push` or `gh`.
///
/// The seam is [`DeliverForge`] and nothing above it. So these tests drive the
/// shipping desk, the shipping refusals and the shipping machine.
pub(super) struct FixtureForge {
    /// What one read of the forge comes back as. A mark-ready rewrites it, so
    /// the second read of a draft sees what the first ask changed — which is
    /// the whole of what a draft-to-merge run has to observe.
    reading: Mutex<Reading>,
    /// Every pull request a merge was asked for, in order.
    merged: Mutex<Vec<String>>,
    /// Every pull request a mark-ready was asked for, in order.
    readied: Mutex<Vec<String>>,
    /// Every `(branch, issue, title)` an open was asked for, in order.
    opened: Mutex<Vec<(String, String, String)>>,
}

impl Default for FixtureForge {
    /// A pull request whose checks are still running. That is the state a
    /// freshly opened one is in.
    fn default() -> Self {
        Self::seeing(reading(CiConclusion::Pending, ReviewState::None))
    }
}

impl FixtureForge {
    fn seeing(reading: Reading) -> Self {
        Self {
            reading: Mutex::new(reading),
            merged: Mutex::new(Vec::new()),
            readied: Mutex::new(Vec::new()),
            opened: Mutex::new(Vec::new()),
        }
    }

    fn merges(&self) -> Vec<String> {
        self.merged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn readies(&self) -> Vec<String> {
        self.readied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn opens(&self) -> Vec<(String, String, String)> {
        self.opened
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl DeliverForge for FixtureForge {
    async fn open(&self, branch: &str, issue: &str, title: &str) -> Result<String, String> {
        self.opened
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((branch.to_owned(), issue.to_owned(), title.to_owned()));
        Ok("4102".to_owned())
    }

    async fn observe(&self, _pr: &str) -> Result<Reading, String> {
        Ok(self
            .reading
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone())
    }

    async fn ready(&self, pr: &str) -> Result<(), String> {
        self.readied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(pr.to_owned());
        self.reading
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .observation
            .draft = false;
        Ok(())
    }

    async fn merge(&self, pr: &str) -> Result<(), String> {
        self.merged
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(pr.to_owned());
        Ok(())
    }
}

/// One read of the forge. Everything but the two facts under test is held at
/// a value that lets a decision through.
fn reading(ci: CiConclusion, review: ReviewState) -> Reading {
    Reading {
        observation: Observation {
            ci,
            base_ci: CiConclusion::Green,
            mergeable: Mergeability::Clean,
            review,
            draft: false,
        },
        settled: false,
        waived: Vec::new(),
        verified_locally: false,
    }
}

/// The same read, of a pull request still in draft — the state
/// `deliver_open` leaves one in.
fn draft_reading(ci: CiConclusion, review: ReviewState) -> Reading {
    let mut still_a_draft = reading(ci, review);
    still_a_draft.observation.draft = true;
    still_a_draft
}

// ---------------------------------------------------------------------------
// The deliver verbs
// ---------------------------------------------------------------------------

/// A manifest that grants `bash` and declares every `deliver` verb beside the
/// work, which is what the shipped package declares.
const GRANTS_DELIVER: &str = r#"
name = "stella-selfdriving"
[loop]
participation = "none"
[[capabilities]]
tool = "bash"
risk = "destructive"
purpose = "read the defect queue, work an issue, and deliver it"
[driver]
calls = [
  "backlog_next",
  "work_start",
  "deliver_open",
  "deliver_observe",
  "deliver_next",
  "deliver_ready",
  "deliver_merge",
]
[driver.process]
argv = ["/bin/sh"]
timeout_secs = 5
"#;

/// The whole session over one forge. The tracker, the worker and the desk all
/// answer from memory.
fn capabilities_delivering(manifest: &str, forge: Arc<FixtureForge>) -> HostDriverCapabilities {
    let roster = roster(vec![installed(manifest, "/opt/pkgs/selfdriving")]);
    HostDriverCapabilities::new(
        "stella-selfdriving",
        PluginGates::from_roster(&roster),
        Box::new(one_open_issue()),
        LoopConfig::default(),
        PathBuf::from("/tmp/stella-driver-test"),
        WorkSlot::new(Box::new(FixtureWorker::answering(changed()))),
        DeliverDesk::new(Box::new(SharedForge(forge))),
    )
}

/// A handle a test keeps while the desk holds the forge. It is how the calls
/// the forge recorded are read back after the session.
struct SharedForge(Arc<FixtureForge>);

#[async_trait]
impl DeliverForge for SharedForge {
    async fn open(&self, branch: &str, issue: &str, title: &str) -> Result<String, String> {
        self.0.open(branch, issue, title).await
    }

    async fn observe(&self, pr: &str) -> Result<Reading, String> {
        self.0.observe(pr).await
    }

    async fn ready(&self, pr: &str) -> Result<(), String> {
        self.0.ready(pr).await
    }

    async fn merge(&self, pr: &str) -> Result<(), String> {
        self.0.merge(pr).await
    }
}

/// The arguments an ask about one pull request carries.
fn observing(pr: &str) -> Option<DriverArgs> {
    Some(DriverArgs {
        deliver_observe: Some(PullRequestArgs { pr: pr.to_string() }),
        ..DriverArgs::default()
    })
}

fn merging(pr: &str) -> Option<DriverArgs> {
    Some(DriverArgs {
        deliver_merge: Some(PullRequestArgs { pr: pr.to_string() }),
        ..DriverArgs::default()
    })
}

fn readying(pr: &str) -> Option<DriverArgs> {
    Some(DriverArgs {
        deliver_ready: Some(PullRequestArgs { pr: pr.to_string() }),
        ..DriverArgs::default()
    })
}

/// One `deliver_next` ask, over a pull request nobody has tried anything on.
fn deciding(ci: DeliverCi, base_ci: DeliverCi) -> Option<DriverArgs> {
    Some(DriverArgs {
        deliver_next: Some(DecideArgs {
            observation: DeliverObservation {
                pr: "4102".into(),
                ci,
                base_ci,
                mergeable: DeliverMergeability::Clean,
                review: DeliverReview::None,
                draft: false,
                settled: false,
            },
            fixes: 0,
            rebases: 0,
        }),
        ..DriverArgs::default()
    })
}

/// **The witness.** `deliver_open` pushes the session's own worked unit. It
/// reports the pull request the forge numbered.
///
/// Nothing weaker can pass it. With the verb answering `unsupported` there is
/// no `pull_request` member to read and no branch for the host to push.
#[tokio::test]
async fn deliver_open_pushes_the_unit_the_session_worked() {
    let forge = Arc::new(FixtureForge::default());
    let host = capabilities_delivering(GRANTS_DELIVER, Arc::clone(&forge));

    host.perform(DriverCall::WorkStart, work_on("41"))
        .await
        .expect("the unit starts");

    let opened = host
        .perform(DriverCall::DeliverOpen, None)
        .await
        .expect("a worked unit is delivered")
        .pull_request
        .expect("a served open carries its report");
    assert_eq!(opened.issue, "41");
    assert_eq!(opened.pr, "4102");
    assert_eq!(opened.branch, "stella/41");

    // The title a maintainer reads in the list is the issue's own, under
    // this workspace's prefix. It is not the key the driver sent.
    let asked = forge.opens();
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert_eq!(asked[0].0, "stella/41");
    assert!(asked[0].2.contains("issue 41"), "{asked:?}");
    let closes = format!("(#{})", "41");
    assert!(asked[0].2.contains(&closes), "{asked:?}");

    // One branch at a time. A second open would leave the session holding two
    // pull requests it could not tell apart in a later ask.
    let refused = host
        .perform(DriverCall::DeliverOpen, None)
        .await
        .expect_err("a session delivers one branch");
    assert_eq!(refused.refusal, HostCallRefusal::Unavailable);
    assert!(refused.detail.contains("4102"), "{refused}");
}

/// A session with nothing worked has no branch to push. The ask is refused,
/// and the refusal names the remedy. The other choice would be a pull request
/// for whatever the checkout sits on.
#[tokio::test]
async fn opening_without_a_worked_unit_is_refused() {
    let refused = capabilities_delivering(GRANTS_DELIVER, Arc::new(FixtureForge::default()))
        .perform(DriverCall::DeliverOpen, None)
        .await
        .expect_err("there is nothing to deliver");
    assert_eq!(refused.refusal, HostCallRefusal::Unavailable);
    assert!(refused.detail.contains("work_start"), "{refused}");
}

/// `deliver_observe` carries the forge's facts across. That includes the one
/// the whole comparison rests on: do the same checks fail on the base?
#[tokio::test]
async fn deliver_observe_reports_the_base_beside_the_pull_request() {
    let forge = Arc::new(FixtureForge::seeing(Reading {
        observation: Observation {
            ci: CiConclusion::Red,
            base_ci: CiConclusion::Red,
            mergeable: Mergeability::Clean,
            review: ReviewState::None,
            draft: true,
        },
        settled: false,
        waived: Vec::new(),
        verified_locally: false,
    }));

    let observed = capabilities_delivering(GRANTS_DELIVER, forge)
        .perform(DriverCall::DeliverObserve, observing("4102"))
        .await
        .expect("a granted plugin is served the read")
        .observation
        .expect("a served read carries its facts");
    assert_eq!(observed.pr, "4102");
    assert_eq!(observed.ci, DeliverCi::Red);
    assert_eq!(observed.base_ci, DeliverCi::Red);
    assert_eq!(observed.mergeable, DeliverMergeability::Clean);
    assert_eq!(observed.review, DeliverReview::None);
    assert!(observed.draft, "{observed:?}");
    assert!(!observed.settled, "{observed:?}");
}

/// `deliver_next` is the machine, over what the driver read. A red build the
/// base also fails is not this pull request's to fix. The loop waits. It does
/// not spend its budget on somebody else's breakage.
#[tokio::test]
async fn deliver_next_separates_a_red_build_from_a_red_base() {
    let host = capabilities_delivering(GRANTS_DELIVER, Arc::new(FixtureForge::default()));

    let ours = host
        .perform(
            DriverCall::DeliverNext,
            deciding(DeliverCi::Red, DeliverCi::Green),
        )
        .await
        .expect("a decision is served")
        .decision
        .expect("with a decision");
    assert_eq!(ours.state, DeliverState::CiRed);
    assert_eq!(ours.action, DeliverAction::PushFix);
    assert_eq!(ours.escalation, None);

    let inherited = host
        .perform(
            DriverCall::DeliverNext,
            deciding(DeliverCi::Red, DeliverCi::Red),
        )
        .await
        .expect("a decision is served")
        .decision
        .expect("with a decision");
    assert_eq!(inherited.state, DeliverState::BaseBroken);
    assert_eq!(inherited.action, DeliverAction::Wait);
}

/// **The safety witness.** A merge rests on what this host read.
///
/// The driver sends no facts to `deliver_merge`. The ask names a pull request
/// and nothing else. So a loop that wanted to merge a red build would have to
/// make the forge answer green. Here the forge answers red, and the refusal
/// names the verdict the host reached.
#[tokio::test]
async fn a_merge_is_refused_on_this_hosts_own_reading() {
    let forge = Arc::new(FixtureForge::seeing(reading(
        CiConclusion::Red,
        ReviewState::Approved,
    )));
    let refused = capabilities_delivering(GRANTS_DELIVER, Arc::clone(&forge))
        .perform(DriverCall::DeliverMerge, merging("4102"))
        .await
        .expect_err("a red build does not merge");
    assert_eq!(refused.refusal, HostCallRefusal::Forbidden);
    assert!(refused.detail.contains("CiRed"), "{refused}");
    assert!(forge.merges().is_empty(), "{:?}", forge.merges());
}

/// The other way round. A green, approved, mergeable pull request merges, and
/// the number the ask named is the one that reached the forge.
#[tokio::test]
async fn a_merge_lands_when_the_hosts_own_reading_approves_it() {
    let forge = Arc::new(FixtureForge::seeing(reading(
        CiConclusion::Green,
        ReviewState::Approved,
    )));
    let merged = capabilities_delivering(GRANTS_DELIVER, Arc::clone(&forge))
        .perform(DriverCall::DeliverMerge, merging("4102"))
        .await
        .expect("an approved pull request merges")
        .merge
        .expect("with a report");
    assert_eq!(merged.pr, "4102");
    assert_eq!(forge.merges(), ["4102"]);
}

/// A human still approves. The channel has no way to say otherwise. So a green
/// build with nobody's review on it waits. That is what stops a plugin
/// granting itself the consent an operator gives by hand.
#[tokio::test]
async fn a_green_build_nobody_reviewed_does_not_merge_itself() {
    let forge = Arc::new(FixtureForge::seeing(reading(
        CiConclusion::Green,
        ReviewState::None,
    )));
    let refused = capabilities_delivering(GRANTS_DELIVER, Arc::clone(&forge))
        .perform(DriverCall::DeliverMerge, merging("4102"))
        .await
        .expect_err("an unreviewed pull request does not merge itself");
    assert_eq!(refused.refusal, HostCallRefusal::Forbidden);
    assert!(forge.merges().is_empty(), "{:?}", forge.merges());
}

/// **The witness.** A green, approved **draft** goes from `deliver_observe` to
/// a merge with no human step in between.
///
/// The machine answers `mark_ready` for a green draft
/// (`stella_autonomy::deliver_next`'s green arm), and before `deliver_ready`
/// existed nothing on the channel could act on that: the run stopped here and
/// a person took every autonomous pull request out of draft by hand. The merge
/// is asserted at the end because a mark-ready that did not reach the forge
/// would leave `draft` set, and the machine then answers `mark_ready` again
/// rather than merging.
#[tokio::test]
async fn a_green_draft_reaches_a_merge_with_no_human_step() {
    let forge = Arc::new(FixtureForge::seeing(draft_reading(
        CiConclusion::Green,
        ReviewState::Approved,
    )));
    let host = capabilities_delivering(GRANTS_DELIVER, Arc::clone(&forge));

    let observed = host
        .perform(DriverCall::DeliverObserve, observing("4102"))
        .await
        .expect("the forge is read")
        .observation
        .expect("with an observation");
    assert!(observed.draft, "the fixture opens as a draft");

    let decided = host
        .perform(
            DriverCall::DeliverNext,
            Some(DriverArgs {
                deliver_next: Some(DecideArgs {
                    observation: observed,
                    fixes: 0,
                    rebases: 0,
                }),
                ..DriverArgs::default()
            }),
        )
        .await
        .expect("the machine decides")
        .decision
        .expect("with a decision");
    assert_eq!(decided.action, DeliverAction::MarkReady);
    assert_eq!(decided.state, DeliverState::ReadyForReview);

    let readied = host
        .perform(DriverCall::DeliverReady, readying("4102"))
        .await
        .expect("a green draft is taken out of draft")
        .ready
        .expect("with a report");
    assert_eq!(readied.pr, "4102");
    assert_eq!(forge.readies(), ["4102"]);

    let merged = host
        .perform(DriverCall::DeliverMerge, merging("4102"))
        .await
        .expect("and the pull request that is out of draft merges")
        .merge
        .expect("with a report");
    assert_eq!(merged.pr, "4102");
    assert_eq!(forge.merges(), ["4102"]);
}

/// A draft whose checks have not finished stays a draft. The host re-reads the
/// forge and runs the machine over its own answer, so `deliver_ready` is held
/// to the same rule as `deliver_merge` — a driver cannot spend the property
/// that a pull request which never goes green never asks a human to look at it.
#[tokio::test]
async fn a_draft_that_is_not_green_is_not_taken_out_of_draft() {
    let forge = Arc::new(FixtureForge::seeing(draft_reading(
        CiConclusion::Pending,
        ReviewState::Approved,
    )));
    let refused = capabilities_delivering(GRANTS_DELIVER, Arc::clone(&forge))
        .perform(DriverCall::DeliverReady, readying("4102"))
        .await
        .expect_err("a draft waiting on checks is not taken out of draft");
    assert_eq!(refused.refusal, HostCallRefusal::Forbidden);
    assert!(refused.detail.contains("CiPending"), "{refused}");
    assert!(forge.readies().is_empty(), "{:?}", forge.readies());
}

/// A `deliver` ask that names no pull request is refused. The host does not
/// point it at whatever this session holds. Those are two different pull
/// requests, and answering about the wrong one is worse than not answering.
#[tokio::test]
async fn a_deliver_ask_that_names_no_pull_request_is_refused() {
    let host = capabilities_delivering(GRANTS_DELIVER, Arc::new(FixtureForge::default()));

    let blank = host
        .perform(DriverCall::DeliverObserve, observing("  "))
        .await
        .expect_err("a blank number names no pull request");
    assert_eq!(blank.refusal, HostCallRefusal::Failed);
    assert!(blank.detail.contains("names no pull request"), "{blank}");

    // And `deliver_open` reads no arguments. It acts on the unit the session
    // already holds.
    let unread = host
        .perform(DriverCall::DeliverOpen, merging("4102"))
        .await
        .expect_err("deliver_open reads no arguments");
    assert_eq!(unread.refusal, HostCallRefusal::Failed);
    assert!(unread.detail.contains("deliver_merge"), "{unread}");
}

/// The two `deliver` verbs that touch the forge spend the shell. So they are
/// held to the grant the tracker read is. `deliver_next` is not. It reads no
/// forge, so asking for `bash` would ask a human to grant a power it never
/// uses.
#[tokio::test]
async fn only_the_deliver_verbs_that_reach_the_forge_need_the_shell() {
    const NO_SHELL: &str = r#"
name = "stella-selfdriving"
[loop]
participation = "none"
[[capabilities]]
tool = "write_file"
risk = "medium"
purpose = "remember what a cycle learned"
[driver]
calls = ["deliver_observe", "deliver_next", "deliver_merge"]
[driver.process]
argv = ["/bin/sh"]
timeout_secs = 5
"#;
    let host = capabilities_delivering(NO_SHELL, Arc::new(FixtureForge::default()));

    let refused = host
        .perform(DriverCall::DeliverObserve, observing("4102"))
        .await
        .expect_err("a plugin that was not granted the shell is refused the read");
    assert_eq!(refused.refusal, HostCallRefusal::Forbidden);
    assert!(refused.detail.contains("stella-selfdriving"), "{refused}");

    let served = host
        .perform(
            DriverCall::DeliverNext,
            deciding(DeliverCi::Pending, DeliverCi::Green),
        )
        .await
        .expect("arithmetic needs no shell")
        .decision
        .expect("with a decision");
    assert_eq!(served.action, DeliverAction::Wait);
}

/// **The end-to-end witness for the mark-ready.** The shipped program carries a
/// green draft to a merge, over the real transport, with nobody touching the
/// forge in between.
///
/// `main.py` had no `deliver_ready` ask to make while the verb did not exist,
/// so this run ended in a sleep with the pull request still a draft. It ends in
/// a halt naming the merge now, and the fixture records both asks in order.
#[cfg(unix)]
#[test]
fn the_shipped_program_carries_a_green_draft_to_a_merge() {
    const DELIVERS_A_DRAFT: &str = r#"
name = "stella-selfdriving"
[loop]
participation = "none"
[[capabilities]]
tool = "bash"
risk = "destructive"
purpose = "read the defect queue, work an issue, and deliver it"
[driver]
calls = [
  "backlog_next",
  "backlog_claim",
  "work_start",
  "deliver_open",
  "deliver_observe",
  "deliver_next",
  "deliver_ready",
  "deliver_merge",
]
[driver.process]
argv = ["python3", "${plugin_dir}/main.py"]
timeout_secs = 60
env = ["PATH"]
"#;

    let forge = Arc::new(FixtureForge::seeing(draft_reading(
        CiConclusion::Green,
        ReviewState::Approved,
    )));
    let (delivered, refusals) =
        drive_shipped_program_over(DELIVERS_A_DRAFT, Box::new(SharedForge(Arc::clone(&forge))));
    match delivered.expect("the session ended with a next") {
        DriveNext::Halt { reason } => assert!(reason.contains("merged"), "{reason}"),
        DriveNext::Sleep { secs } => {
            panic!("a green draft must not need a human to advance it: slept {secs}s")
        }
    }
    assert_eq!(forge.readies(), ["4102"]);
    assert_eq!(forge.merges(), ["4102"]);
    assert!(
        refusals.is_empty(),
        "every ask this cycle makes was declared: {refusals:?}"
    );
}

/// **The end-to-end witness.** The shipped program, through the real
/// transport, reaching the pull request.
///
/// The first arm is what could not happen before. The work is declared and
/// `deliver_open` is not. `main.py` works the issue, then asks to deliver it.
/// It had no code to make that ask while the verb answered `unsupported`.
///
/// The second declares the whole family. The program opens the pull request,
/// reads the forge, is told to wait on checks that have not finished, and
/// sleeps.
#[cfg(unix)]
#[test]
fn the_shipped_program_delivers_the_issue_it_worked() {
    const WORKS_BUT_MAY_NOT_DELIVER: &str = r#"
name = "stella-selfdriving"
[loop]
participation = "none"
[[capabilities]]
tool = "bash"
risk = "destructive"
purpose = "read the defect queue and work an issue"
[driver]
calls = ["backlog_next", "backlog_claim", "work_start"]
[driver.process]
argv = ["python3", "${plugin_dir}/main.py"]
timeout_secs = 60
env = ["PATH"]
"#;

    const DELIVERS: &str = r#"
name = "stella-selfdriving"
[loop]
participation = "none"
[[capabilities]]
tool = "bash"
risk = "destructive"
purpose = "read the defect queue, work an issue, and deliver it"
[driver]
calls = [
  "backlog_next",
  "backlog_claim",
  "work_start",
  "deliver_open",
  "deliver_observe",
  "deliver_next",
]
[driver.process]
argv = ["python3", "${plugin_dir}/main.py"]
timeout_secs = 60
env = ["PATH"]
"#;

    let (stopped, _) = drive_shipped_program(WORKS_BUT_MAY_NOT_DELIVER);
    match stopped.expect("the session ended with a next") {
        DriveNext::Halt { reason } => {
            assert!(reason.contains("deliver_open"), "{reason}");
            assert!(reason.contains("undeclared"), "{reason}");
            assert!(reason.contains("stella/41"), "{reason}");
        }
        DriveNext::Sleep { secs } => panic!("the work was served, yet the driver slept {secs}s"),
    }

    let (delivered, refusals) = drive_shipped_program(DELIVERS);
    match delivered.expect("the session ended with a next") {
        // Checks that have not finished are a later cycle's business. The
        // program sleeps.
        DriveNext::Sleep { secs } => assert!(secs > 0, "a sleep of {secs}s is not a wait"),
        DriveNext::Halt { reason } => {
            panic!("a pull request waiting on checks must not end the loop: {reason}")
        }
    }
    assert!(
        refusals.is_empty(),
        "every ask this cycle makes was declared: {refusals:?}"
    );
}
