// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The pull request a driver session opens, reads, and merges.
//!
//! `doc:backlog-self-driving` §3.3. [`super::work`] is the sibling that
//! changes files. This is what happens to the branch after that.
//!
//! # The merge does not trust the driver
//!
//! `deliver_next` decides over a reading the *driver* sends. That is what
//! makes it cheap. A loop that already read the forge can ask what to do
//! without paying for a second read.
//!
//! `deliver_merge` does not work that way. It reads the forge itself. It runs
//! the same machine over what *it* saw. It merges only if that says
//! [`Action::Merge`]. A driver that reported a green build it never saw gets a
//! refusal naming the state the host found.
//!
//! That is the whole safety case for putting a merge on a plugin channel, and
//! it holds without trusting the program. The branch protection a repository
//! declares is read by
//! [`self_driving_cmd::deliver::observe`](crate::self_driving_cmd::deliver::observe).
//! No message a driver writes reaches past it.
//!
//! # A human still approves
//!
//! [`DeliverPolicy::default`] asks for review, and the channel has no way to
//! turn that off. An operator who wants their own loop merging unreviewed work
//! says so to `stella self-driving drive --no-review`, by hand. A plugin
//! asking for the same thing over a socket would be the loop granting itself
//! the consent. `doc:backlog-self-driving` §3.6 refuses that.
//!
//! # Two vocabularies, one map
//!
//! `stella_plugin::driver::deliver` spells the forge's facts again. That crate
//! may not depend on `stella-autonomy`, per AGENTS.md's workspace table.
//! [`from_observation`] and [`decision_of`] are the whole of the map. Both are
//! total `match`es, so a case added on either side is a build error here.
//!
//! # The port
//!
//! [`DeliverForge`] is the one seam. The refusals, the machine and the map all
//! sit above it. So a test that hands over a fixture forge tests the code that
//! ships.
//!
//! Every method behind that seam shells out to `gh` and `git`, and blocks
//! while it does. So the shipping forge moves each to a blocking thread.
//! `perform` already runs inside the driver session's runtime, and a blocking
//! `Command` there would stall the socket the driver waits on.

use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use stella_autonomy::{
    Action, Attempts, CiConclusion, DeliverPolicy, EscalationReason, Mergeability, Observation,
    PrState, ReviewState, deliver_next,
};
use stella_plugin::{
    DecideArgs, DeliverAction, DeliverCi, DeliverDecision, DeliverEscalation, DeliverMergeability,
    DeliverObservation, DeliverReview, DeliverState, HostCallFailure, HostCallRefusal, MergeReport,
    OpenReport,
};

use crate::self_driving_cmd::config::LoopConfig;
use crate::self_driving_cmd::deliver::Reading;

/// What opens, reads and merges a pull request, behind a port.
#[async_trait]
pub(crate) trait DeliverForge: Send + Sync {
    /// Push `branch` and open the draft pull request that closes `issue`.
    ///
    /// Answers the forge's number for it.
    async fn open(&self, branch: &str, issue: &str, title: &str) -> Result<String, String>;

    /// One read of the forge, plus the second read of the base it needs.
    async fn observe(&self, pr: &str) -> Result<Reading, String>;

    /// Merge it.
    async fn merge(&self, pr: &str) -> Result<(), String>;
}

/// The shipping forge: the loop's own `deliver` verbs, on a blocking thread.
pub(crate) struct GhDeliverForge {
    /// The workspace the loop was started in — where the branch is pushed
    /// from.
    root: PathBuf,
    /// This workspace's own loop settings: the pull request signature, and
    /// which checks are allowed to block a merge.
    config: LoopConfig,
}

impl GhDeliverForge {
    /// A forge over one workspace.
    pub(crate) fn new(root: PathBuf, config: LoopConfig) -> Self {
        Self { root, config }
    }
}

#[async_trait]
impl DeliverForge for GhDeliverForge {
    async fn open(&self, branch: &str, issue: &str, title: &str) -> Result<String, String> {
        let root = self.root.clone();
        let signature = self.config.attribution.pull_request.clone();
        let (branch, issue, title) = (branch.to_owned(), issue.to_owned(), title.to_owned());
        tokio::task::spawn_blocking(move || {
            crate::self_driving_cmd::deliver::open(&root, &branch, &issue, &title, &signature)
        })
        .await
        .map_err(|error| format!("the pull request did not finish opening: {error}"))?
    }

    async fn observe(&self, pr: &str) -> Result<Reading, String> {
        let policy = self.config.merge.clone();
        let pr = pr.to_owned();
        tokio::task::spawn_blocking(move || crate::self_driving_cmd::deliver::observe(&pr, &policy))
            .await
            .map_err(|error| format!("the forge read did not finish: {error}"))?
    }

    async fn merge(&self, pr: &str) -> Result<(), String> {
        let pr = pr.to_owned();
        tokio::task::spawn_blocking(move || crate::self_driving_cmd::deliver::merge(&pr))
            .await
            .map_err(|error| format!("the merge did not finish: {error}"))?
    }
}

/// The pull request a driver session has open, if it has one.
///
/// One, for [`super::work::WorkSlot`]'s reason: a session works one unit, so
/// it delivers one branch. A second `deliver_open` while the first is still
/// open would be a session holding two pull requests it could not tell apart
/// in a later ask.
pub(crate) struct DeliverDesk {
    /// The pull request this session opened, and the unit it closes.
    open: Mutex<Option<OpenReport>>,
    forge: Box<dyn DeliverForge>,
}

impl DeliverDesk {
    /// An empty desk over `forge`.
    pub(crate) fn new(forge: Box<dyn DeliverForge>) -> Self {
        Self {
            open: Mutex::new(None),
            forge,
        }
    }

    /// `deliver_open` — push `branch` and open the pull request for `issue`.
    ///
    /// # Errors
    ///
    /// [`HostCallRefusal::Unavailable`] when this session already opened one,
    /// and [`HostCallRefusal::Failed`] when the push or the forge refused.
    pub(crate) async fn open(
        &self,
        issue: &str,
        branch: &str,
        title: &str,
    ) -> Result<OpenReport, HostCallFailure> {
        if let Some(held) = self.peek() {
            return Err(HostCallFailure::new(
                HostCallRefusal::Unavailable,
                format!(
                    "this session already opened pull request {} for {}; a driver delivers one \
                     branch at a time",
                    held.pr, held.issue
                ),
            ));
        }

        let pr = self
            .forge
            .open(branch, issue, title)
            .await
            .map_err(|reason| {
                HostCallFailure::new(
                    HostCallRefusal::Failed,
                    format!("could not open a pull request for {issue}: {reason}"),
                )
            })?;

        let report = OpenReport {
            issue: issue.to_owned(),
            pr,
            branch: branch.to_owned(),
        };
        *self
            .open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(report.clone());
        Ok(report)
    }

    /// `deliver_observe` — one read of the forge.
    ///
    /// # Errors
    ///
    /// [`HostCallRefusal::Failed`] when the forge could not be read. An
    /// unreadable forge is not an observation with unknown facts in it: those
    /// two read as `wait` and as `no answer`, and a loop that could not tell
    /// them apart would wait on a repository it has lost access to.
    pub(crate) async fn observe(&self, pr: &str) -> Result<DeliverObservation, HostCallFailure> {
        let reading = self.read(pr).await?;
        Ok(from_observation(pr, &reading))
    }

    /// `deliver_merge` — merge, if this host's own read says so.
    ///
    /// # Errors
    ///
    /// [`HostCallRefusal::Forbidden`] when the machine did not say
    /// [`Action::Merge`] over what this host observed, naming the state it
    /// found. [`HostCallRefusal::Failed`] when the forge could not be read or
    /// the merge itself failed.
    pub(crate) async fn merge(&self, pr: &str) -> Result<MergeReport, HostCallFailure> {
        let reading = self.read(pr).await?;
        let decision = decide(pr, &from_observation(pr, &reading), Attempts::default());
        if decision.action != DeliverAction::Merge {
            return Err(HostCallFailure::new(
                HostCallRefusal::Forbidden,
                format!(
                    "this host read pull request {pr} for itself and its own verdict is \
                     {state:?}/{action:?}, not a merge — a merge is performed on what this host \
                     observed, never on what a driver reported",
                    state = decision.state,
                    action = decision.action
                ),
            ));
        }

        self.forge.merge(pr).await.map_err(|reason| {
            HostCallFailure::new(
                HostCallRefusal::Failed,
                format!("could not merge pull request {pr}: {reason}"),
            )
        })?;
        Ok(MergeReport { pr: pr.to_owned() })
    }

    fn peek(&self) -> Option<OpenReport> {
        self.open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    async fn read(&self, pr: &str) -> Result<Reading, HostCallFailure> {
        self.forge.observe(pr).await.map_err(|reason| {
            HostCallFailure::new(
                HostCallRefusal::Failed,
                format!("could not read pull request {pr}: {reason}"),
            )
        })
    }
}

/// `deliver_next` — the deterministic transition, over what a driver observed.
///
/// Pure: no forge is read and no model call is spent, which is why this verb
/// is served without asking [`super::capabilities::HostDriverCapabilities`]'s
/// shell gate. It is arithmetic over numbers the driver already holds.
#[must_use]
pub(crate) fn decide_from(args: &DecideArgs) -> DeliverDecision {
    decide(
        &args.observation.pr,
        &args.observation,
        Attempts {
            fixes: args.fixes,
            rebases: args.rebases,
        },
    )
}

/// The machine, over one observation.
///
/// A settled pull request short-circuits before the machine sees it, for the
/// reason `self_driving_cmd::deliver::Reading` carries the flag at all: a
/// merged pull request reports an unknown mergeability, which the machine
/// correctly reads as "wait" — forever.
fn decide(pr: &str, observed: &DeliverObservation, attempts: Attempts) -> DeliverDecision {
    if observed.settled {
        return DeliverDecision {
            pr: pr.to_owned(),
            state: DeliverState::Merged,
            action: DeliverAction::Wait,
            escalation: None,
        };
    }
    let transition = deliver_next(
        PrState::CiPending,
        &to_observation(observed),
        attempts,
        &DeliverPolicy::default(),
    );
    decision_of(pr, transition.state, transition.action)
}

/// One read of the forge, as the wire carries it.
fn from_observation(pr: &str, reading: &Reading) -> DeliverObservation {
    let observed = &reading.observation;
    DeliverObservation {
        pr: pr.to_owned(),
        ci: ci_of(observed.ci),
        base_ci: ci_of(observed.base_ci),
        mergeable: match observed.mergeable {
            Mergeability::Clean => DeliverMergeability::Clean,
            Mergeability::Conflicted => DeliverMergeability::Conflicted,
            Mergeability::Unknown => DeliverMergeability::Unknown,
        },
        review: match observed.review {
            ReviewState::None => DeliverReview::None,
            ReviewState::ChangesRequested => DeliverReview::ChangesRequested,
            ReviewState::Approved => DeliverReview::Approved,
        },
        draft: observed.draft,
        settled: reading.settled,
    }
}

/// The same facts, back in the machine's own vocabulary.
fn to_observation(observed: &DeliverObservation) -> Observation {
    Observation {
        ci: conclusion_of(observed.ci),
        base_ci: conclusion_of(observed.base_ci),
        mergeable: match observed.mergeable {
            DeliverMergeability::Clean => Mergeability::Clean,
            DeliverMergeability::Conflicted => Mergeability::Conflicted,
            DeliverMergeability::Unknown => Mergeability::Unknown,
        },
        review: match observed.review {
            DeliverReview::None => ReviewState::None,
            DeliverReview::ChangesRequested => ReviewState::ChangesRequested,
            DeliverReview::Approved => ReviewState::Approved,
        },
        draft: observed.draft,
    }
}

fn ci_of(conclusion: CiConclusion) -> DeliverCi {
    match conclusion {
        CiConclusion::Pending => DeliverCi::Pending,
        CiConclusion::Green => DeliverCi::Green,
        CiConclusion::Red => DeliverCi::Red,
    }
}

fn conclusion_of(ci: DeliverCi) -> CiConclusion {
    match ci {
        DeliverCi::Pending => CiConclusion::Pending,
        DeliverCi::Green => CiConclusion::Green,
        DeliverCi::Red => CiConclusion::Red,
    }
}

/// A transition, as the wire carries it.
fn decision_of(pr: &str, state: PrState, action: Action) -> DeliverDecision {
    DeliverDecision {
        pr: pr.to_owned(),
        state: match state {
            PrState::Draft => DeliverState::Draft,
            PrState::CiPending => DeliverState::CiPending,
            PrState::CiRed => DeliverState::CiRed,
            PrState::BaseBroken => DeliverState::BaseBroken,
            PrState::Conflicted => DeliverState::Conflicted,
            PrState::ReadyForReview => DeliverState::ReadyForReview,
            PrState::ReviewChangesRequested => DeliverState::ReviewChangesRequested,
            PrState::Approved => DeliverState::Approved,
            PrState::Merged => DeliverState::Merged,
            PrState::Escalated => DeliverState::Escalated,
        },
        action: match action {
            Action::Wait => DeliverAction::Wait,
            Action::PushFix => DeliverAction::PushFix,
            Action::Rebase => DeliverAction::Rebase,
            Action::MarkReady => DeliverAction::MarkReady,
            Action::Merge => DeliverAction::Merge,
            Action::Escalate { .. } => DeliverAction::Escalate,
        },
        escalation: match action {
            Action::Escalate { reason } => Some(match reason {
                EscalationReason::FixCeilingReached => DeliverEscalation::FixCeilingReached,
                EscalationReason::RebaseCeilingReached => DeliverEscalation::RebaseCeilingReached,
                EscalationReason::ReviewNeedsHuman => DeliverEscalation::ReviewNeedsHuman,
            }),
            _ => None,
        },
    }
}
