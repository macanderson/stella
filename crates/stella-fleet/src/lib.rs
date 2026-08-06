// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella-fleet` — the multi-agent fleet layer.
//!
//! Five pieces, one seam:
//!
//! - [`plan`] — the **planner DAG**: [`Plan`]/[`Task`] with dependency edges,
//!   share-by-default isolation, wave scheduling ([`Plan::ready_tasks`]), topological
//!   order, and cycle detection.
//! - [`git`] — **git-worktree isolation** over the [`GitCli`] port: a
//!   dedicated worktree per *isolated* task (share-by-default: shared-tree
//!   tasks run in the repo root), plus commit helpers that *always* use
//!   explicit pathspecs (a fleet worker can never sweep a sibling's staged
//!   files).
//! - [`gc`] — **reclaiming what a run leaves behind** (issue #1217): the
//!   conservative sweep over `.stella/worktrees/` and the `fleet/` branch
//!   namespace that `stella fleet clean` drives. Nothing in flight, dirty, or
//!   carrying unmerged commits is removed without `--force`, and every kept
//!   candidate reports why.
//! - [`ledger`] — the **commit ledger**: one embedded SQLite file recording
//!   every run, task, attempt, commit, lineage edge, and per-task USD spend.
//! - [`monitor`] — the **PR/CI monitor** over the [`GhCli`] port: live PR
//!   reconciliation and a capped-deferred-wait CI watcher (L-E4).
//! - [`cache_schedule`] — **warmest-first scheduling** (issue #269): the pure,
//!   no-I/O heuristic that orders equal-priority [`RunnableSession`]s by
//!   soonest-to-expire prompt-cache prefix ([`warmest_first`]), so a session
//!   about to lose its cached prefix is resumed before a colder one takes the
//!   slot. [`Fleet::with_cache_warmth`] is the dispatch-side seam, and since
//!   issue #1222 `stella fleet` installs a real lookup through it: the CLI
//!   projects each task's last-attempt timestamp (the ledger's
//!   [`Ledger::last_attempt_finish_ms`], wall-clock-stamped) through the
//!   provider's cache TTL (`stella-cli/src/fleet_warmth.rs`) — the
//!   pricing/TTL *policy* stays on the CLI side of the crate boundary, as
//!   this crate's design requires.
//!
//! and [`fleet`] — **THE dispatch seam** ([`Fleet::dispatch`], L-E9): the one
//! API subagent fan-out goes through, claiming a task's declared paths as
//! cooperative file locks (`stella-store`'s `file_locks`) for the attempt's
//! duration, stamping lineage into the ledger, metering child spend into
//! the parent [`stella_core::BudgetGuard`], and handing every worker its
//! per-task control lines ([`WorkerControls`] through the [`FleetWorker`]
//! port, driven by [`Fleet::pause_task`] / [`Fleet::resume_task`] /
//! [`Fleet::stop_task`] — which the `stella fleet` dashboard's
//! `[p]`/`[r]`/`[x]` keys reach through `stella-cli`'s control pump;
//! restart = re-dispatch).
//!
//! Design constraints: we shell out
//! to the `git`/`gh` binaries via `tokio::process` behind port traits rather
//! than linking libgit2 (heavy native build), every external interaction is a
//! port so every test runs against fakes, and there is no `unwrap`/`panic`
//! outside tests.
//!
//! [`GitCli`]: git::GitCli
//! [`GhCli`]: monitor::GhCli
//! [`Plan`]: plan::Plan
//! [`Task`]: plan::Task
//! [`Plan::ready_tasks`]: plan::Plan::ready_tasks
//! [`RunnableSession`]: cache_schedule::RunnableSession
//! [`warmest_first`]: cache_schedule::warmest_first
//! [`Ledger::last_attempt_finish_ms`]: ledger::Ledger::last_attempt_finish_ms
//! [`Fleet::dispatch`]: fleet::Fleet::dispatch
//! [`Fleet::with_cache_warmth`]: fleet::Fleet::with_cache_warmth
//! [`Fleet::pause_task`]: fleet::Fleet::pause_task
//! [`Fleet::resume_task`]: fleet::Fleet::resume_task
//! [`Fleet::stop_task`]: fleet::Fleet::stop_task
//! [`WorkerControls`]: fleet::WorkerControls
//! [`FleetWorker`]: fleet::FleetWorker

pub mod cache_schedule;
pub mod fleet;
pub mod gc;
pub mod git;
pub mod ledger;
pub mod monitor;
pub mod plan;

pub use cache_schedule::{RunnableSession, warmest_first};
pub use fleet::{
    CacheWarmthLookup, Fleet, FleetConfig, FleetError, FleetRunReport, FleetWorker, TaskHandle,
    WorkerControls, WorkerOutcome,
};
pub use gc::{
    BranchAction, BranchVerdict, Gc, GcError, GcOptions, GcReport, KeepReason, WorktreeAction,
    WorktreeActivity, WorktreeVerdict,
};
pub use git::{
    GitCli, GitError, GitOutput, RemoveOutcome, SystemGitCli, Worktree, WorktreeEntry,
    WorktreeError, WorktreeManager,
};
pub use ledger::{
    AttemptFinish, AttemptId, AttemptStart, CommitRecord, Ledger, LedgerError, RunRecord,
};
pub use monitor::{
    CiConclusion, CiRun, CiRunStatus, CiSnapshot, CiWatchOutcome, GhCli, GhError, GhOutput,
    Monitor, MonitorError, Sleeper, SystemGhCli, TimeoutReason, TokioSleeper, WatchConfig,
    WatchPhase, commit_event, parse_pr_number, pr_event, pr_event_with_ci,
};
pub use plan::{Isolation, Plan, PlanError, Task, TaskId};
