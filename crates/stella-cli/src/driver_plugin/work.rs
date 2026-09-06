// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The one unit of work a driver session may hold, and what runs it.
//!
//! `doc:backlog-self-driving` §3.2. The `work` verbs are the only part of the
//! loop that changes a file. The rest ranks, decides, or reports.
//!
//! # One unit, held by the session
//!
//! A driver asks `work_start` with an issue key. It gets a report back. The
//! session keeps that unit. So `work_status` and `work_abandon` need no key of
//! their own. They act on the one the session holds.
//!
//! A second `work_start` while a unit holds a change is turned down, not
//! queued. Two checkouts under one session would be two claims. The loop could
//! not give back one without the other. Which unit to work is the driver's
//! call.
//!
//! # It runs the turn Stella already runs
//!
//! [`self_driving_cmd::work::start`](crate::self_driving_cmd::work::start) is
//! the whole of it. It is the path `stella self-driving` takes. It cuts a
//! checkout outside the fleet's own space. It quotes the issue as data. It
//! spends one bounded `stella run`. Then it reads the tree, not what the turn
//! said about it. Serving the verb here changes who asks. It changes nothing
//! about what runs.
//!
//! # Why not `child_turn`
//!
//! The wrapper socket has a capability that spends a model call for a plugin.
//! It is the wrong one here. `stella_runtime::wrapper::child_turn` builds every
//! child through `SubAgentSpec::read_only`. The turn sits behind
//! `ReadOnlyTools`. It cannot write a file. Its own module states that as a
//! promise. A work unit is the case that promise rules out. Widening it would
//! hand a write to every plugin that holds a read.
//!
//! # Why the turn runs on a blocking thread
//!
//! `work::start` builds its own tokio runtime and calls `block_on`. But
//! [`stella_runtime::wrapper::DriverCapabilities::perform`], which
//! [`super::capabilities::HostDriverCapabilities`] implements, is already
//! inside the session's runtime, so a straight call would panic.
//! [`tokio::task::spawn_blocking`] moves the work to a thread with no runtime
//! on it. That is what the inner `block_on` needs.
//!
//! # The port
//!
//! [`WorkRunner`] lets a test drive the verb with no model call. It is the one
//! seam. The slot, the refusals and the report all sit above it. So a test
//! here tests the code that ships.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use stella_plugin::{HostCallFailure, HostCallRefusal, WorkReport, WorkState};
use stella_protocol::issue::Issue;

use crate::self_driving_cmd::budget::RunBudget;
use crate::self_driving_cmd::config::LoopConfig;
use crate::self_driving_cmd::work::WorkOutcome;

/// What works a unit, behind a port.
///
/// Two verbs, not one. Running a turn and giving a checkout back fail for
/// different reasons, and a caller branches on both.
#[async_trait]
pub(crate) trait WorkRunner: Send + Sync {
    /// Run `issue` to a diff, or say why it did not get there.
    async fn run(&self, issue: &Issue) -> Result<WorkOutcome, String>;

    /// Release the checkout at `path`, keeping whatever it committed.
    async fn release(&self, path: &Path) -> Result<(), String>;
}

/// The shipping runner: the loop's own `work start`, on a blocking thread.
pub(crate) struct SpawnedWorkRunner {
    /// The workspace the loop was started in.
    root: PathBuf,
    /// This workspace's own loop settings — the branch prefix, the commit
    /// signature, and which worker runs the turn.
    config: LoopConfig,
    /// The session's ceiling, and what has gone against it.
    ///
    /// Behind a lock. A driver may send two asks at once, and a budget that
    /// two turns both read as full is no budget. The lock is never held across
    /// an `await`: the value is copied out, spent, and put back.
    budget: Mutex<RunBudget>,
}

impl SpawnedWorkRunner {
    /// A runner over one workspace, under `budget`.
    pub(crate) fn new(root: PathBuf, config: LoopConfig, budget: RunBudget) -> Self {
        Self {
            root,
            config,
            budget: Mutex::new(budget),
        }
    }

    /// The session's budget, as it stands.
    ///
    /// A poisoned lock is read through, not passed on. This is
    /// `DriverCallGate::refusals`'s rule. Losing the number because some other
    /// ask panicked is the silence the channel refuses. It is also AGENTS.md
    /// rule 5's line on an `unwrap` over runtime state.
    fn budget_now(&self) -> RunBudget {
        self.budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Fold what a finished turn spent back into the session's budget.
    fn settle(&self, spent: RunBudget) {
        *self
            .budget
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = spent;
    }
}

#[async_trait]
impl WorkRunner for SpawnedWorkRunner {
    async fn run(&self, issue: &Issue) -> Result<WorkOutcome, String> {
        let root = self.root.clone();
        let config = self.config.clone();
        let issue = issue.clone();
        let mut budget = self.budget_now();

        let (outcome, budget) = tokio::task::spawn_blocking(move || {
            let outcome = crate::self_driving_cmd::work::start(
                &root,
                &issue,
                &mut budget,
                &config.attribution,
                &config.worker,
            );
            (outcome, budget)
        })
        .await
        .map_err(|error| format!("the work unit's thread did not finish: {error}"))?;

        // Settled whichever way the turn went. A turn that stopped early
        // still spent. Charging only the wins would let a run of failures
        // spend with no bound. That is `RunBudget::record`'s own argument, one
        // level up.
        self.settle(budget);
        outcome
    }

    async fn release(&self, path: &Path) -> Result<(), String> {
        let root = self.root.clone();
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || crate::self_driving_cmd::work::release(&root, &path))
            .await
            .map_err(|error| format!("the release did not finish: {error}"))?
    }
}

/// The unit a session holds, if it holds one.
#[derive(Debug, Clone, Default)]
struct Held {
    /// The tracker key.
    issue: String,
    /// Where the unit stands.
    state: WorkState,
    /// The branch the work sits on, when the tree holds a change.
    branch: String,
    /// What `git` said the turn left behind.
    stat: String,
    /// The checkout, so a release has something to remove.
    path: Option<PathBuf>,
    /// What the turn said, or why it did not finish.
    detail: String,
}

impl Held {
    /// What the driver reads.
    fn report(&self) -> WorkReport {
        WorkReport {
            issue: self.issue.clone(),
            state: self.state,
            branch: self.branch.clone(),
            stat: self.stat.clone(),
            detail: self.detail.clone(),
        }
    }
}

/// The session's work slot: nothing, or one unit.
///
/// A `Mutex`, not a `RwLock`: every step here writes. It is never held across
/// an `await`. The turn runs outside it, and the slot is read and written
/// around that.
pub(crate) struct WorkSlot {
    held: Mutex<Option<Held>>,
    runner: Box<dyn WorkRunner>,
}

impl WorkSlot {
    /// An empty slot over `runner`.
    pub(crate) fn new(runner: Box<dyn WorkRunner>) -> Self {
        Self {
            held: Mutex::new(None),
            runner,
        }
    }

    /// Read the slot.
    fn peek(&self) -> Option<Held> {
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Write the slot.
    fn put(&self, unit: Option<Held>) {
        *self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = unit;
    }

    /// `work_start` — run `issue` and keep what came of it.
    ///
    /// A unit that changed nothing, or that did not finish, is written over.
    /// There is no checkout to lose and no branch to deliver, so holding the
    /// slot shut would only stop the loop working the next issue.
    ///
    /// # Errors
    ///
    /// [`HostCallRefusal::Unavailable`] when the session holds a unit that left
    /// a change. Abandoning that one clears it. [`HostCallRefusal::Failed`]
    /// when the turn could not be run at all.
    pub(crate) async fn start(&self, issue: &Issue) -> Result<WorkReport, HostCallFailure> {
        if let Some(held) = self.peek()
            && held.state == WorkState::Changed
        {
            return Err(HostCallFailure::new(
                HostCallRefusal::Unavailable,
                format!(
                    "this session already holds {} on branch {}; a driver works one unit at a \
                     time, so deliver it or ask for `work_abandon` before starting another",
                    held.issue, held.branch
                ),
            ));
        }

        let outcome = self.runner.run(issue).await.map_err(|reason| {
            HostCallFailure::new(
                HostCallRefusal::Failed,
                format!("{}: {reason}", issue.key.as_str()),
            )
        })?;

        let unit = match outcome {
            WorkOutcome::Changed { branch, path, stat } => Held {
                issue: issue.key.as_str().to_string(),
                state: WorkState::Changed,
                branch,
                stat,
                path: Some(path),
                detail: String::new(),
            },
            // Not an error, and not an empty answer. An issue the loop
            // cannot act on is a real outcome. The turn's last word is the
            // only thing that tells "nothing to do here" from "the money ran
            // out first".
            WorkOutcome::NoChange { why } => Held {
                issue: issue.key.as_str().to_string(),
                state: WorkState::NoChange,
                detail: why,
                ..Held::default()
            },
            WorkOutcome::Failed { reason } => Held {
                issue: issue.key.as_str().to_string(),
                state: WorkState::Failed,
                detail: reason,
                ..Held::default()
            },
        };
        let report = unit.report();
        self.put(Some(unit));
        Ok(report)
    }

    /// `work_status` — what the session holds.
    ///
    /// [`WorkState::Idle`] before a unit is started, and after one is given
    /// back. A verb that answered nothing would leave a driver unable to tell
    /// an empty slot from a host that did not do the call.
    pub(crate) fn status(&self) -> WorkReport {
        self.peek().map(|held| held.report()).unwrap_or_default()
    }

    /// `work_abandon` — give the unit back, saying why.
    ///
    /// The checkout goes. The branch stays. A turn that committed keeps its
    /// work, so this is safe to reach for. It frees disk. It throws nothing
    /// away.
    ///
    /// # Errors
    ///
    /// [`HostCallRefusal::Unavailable`] when the session holds nothing. A
    /// driver that lost track is told so, rather than handed a success that
    /// freed nothing.
    pub(crate) async fn abandon(&self, reason: &str) -> Result<WorkReport, HostCallFailure> {
        let held = self.peek().ok_or_else(|| {
            HostCallFailure::new(
                HostCallRefusal::Unavailable,
                "this session holds no unit of work, so there is nothing to abandon",
            )
        })?;

        // Reported, never fatal. A checkout that will not go is something
        // the operator must know: it will collide with the next attempt at
        // this issue. It is still no reason to keep the slot full.
        let released = match &held.path {
            Some(path) => self.runner.release(path).await,
            None => Ok(()),
        };
        self.put(None);

        let mut report = held.report();
        report.state = WorkState::Idle;
        report.detail = match released {
            Ok(()) => reason.to_string(),
            Err(error) => format!("{reason} — and the checkout would not release: {error}"),
        };
        Ok(report)
    }
}
