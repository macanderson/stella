// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The parallel backlog wave behind `drive --backlog --parallel N`.
//!
//! The serial loop works one issue at a time. A backlog of ready issues
//! that do not block each other deserves as many workers as the governor
//! says this box can host. This module only bridges two systems that
//! already exist. The ready fold (`super::ready`) picks the issues, and
//! `stella_fleet` fans them out. One [`Task`] per issue. Every task
//! `isolated`, so each worker gets its own worktree, at the governor's
//! width. Nothing here re-invents scheduling. The plan is a single wave
//! of tasks with no edges, and the fleet's width is the only throttle.
//!
//! # No double claims, twice over
//!
//! Before an issue enters the plan, this process takes its `issue:<n>`
//! lease (`super::claim`), just as the serial loop would. So a serial
//! peer on another checkout defers off every issue in the wave. Inside
//! the run, the fleet also leases each `task:<id>` under its own run id.
//! Both live in the same workspace ledger. Both expire on their own if
//! this process dies.
//!
//! # What a worker is
//!
//! A real child `stella run` (`super::work::run_turn`) with the issue
//! quoted as data, in a worktree, under a budget slice. Never an
//! embedded engine holding the parent's keys. The run's spend cap is
//! divided across the width before any child starts. So a wave's
//! children, all in flight at once, cannot spend past what the operator
//! granted the run.
//!
//! The serial path picks its worker. A wave does not. It runs stella's
//! own turn loop. So a workspace that chose another agent is refused
//! here, rather than given stella under a setting that names one.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use stella_fleet::git::{SystemGitCli, WorktreeManager};
use stella_fleet::{
    Fleet, FleetConfig, FleetWorker, Ledger, Plan, Task, WorkerControls, WorkerOutcome,
};
use stella_protocol::issue::Issue;

use super::audit::{self, Action as Audit};
use super::budget::RunBudget;
use super::config::LoopConfig;
use super::state::LoopState as Durable;
use super::turn_flags::TurnFlags;
use crate::settings::toml_config::WorkerKind;

/// Refuse a wave that cannot run the worker the workspace chose.
///
/// A wave dispatches to `super::work::run_turn`. That is a child `stella
/// run`. The serial path's worker branch is not on this route. So a wave
/// under `kind = "claude"` would run stella. The setting would name one
/// agent and the loop would use another. That is the swap the typed choice
/// exists to stop.
fn refuse_unsupported_worker(
    worker: &crate::settings::toml_config::WorkerSection,
) -> Result<(), String> {
    match worker.kind {
        WorkerKind::Stella => Ok(()),
        WorkerKind::Claude => Err("a parallel wave runs stella's own turn loop, so \
             worker.kind = \"claude\" cannot be honoured here. Drop --parallel to work \
             issues one at a time with the worker you chose, or set worker.kind = \
             \"stella\" for the wave."
            .to_owned()),
    }
}

/// Run one wave of ready issues at `width` workers, then return.
///
/// A wave is bounded. It takes at most `max(max_issues, width)` issues,
/// so asking for more workers than the default issue bound does not idle
/// them. It settles when every dispatched worker settles. Running forever
/// stays the serial loop's job. An operator who wants the wave again runs
/// it again, and the ready fold reads the tracker fresh each time.
pub(super) fn wave(
    durable: &Durable,
    root: &Path,
    cfg: &LoopConfig,
    provider: &dyn stella_protocol::issue::IssueProvider,
    flags: &TurnFlags,
    width: u32,
    max_issues: u32,
) -> Result<(), String> {
    super::work::refuse_if_unsteered(root)?;
    refuse_unsupported_worker(&cfg.worker)?;

    let ready = super::ready::ready_full(provider, cfg)?;
    let bound = (max_issues.max(width)) as usize;

    let session_id = audit::begin_session();
    audit::record(
        durable,
        Audit::SessionStarted,
        Some(&session_id),
        &format!(
            "parallel wave began — up to {bound} ready issue(s) across {width} worker(s){}",
            flags
                .spend_limit
                .map_or(String::new(), |cap| format!(", ${cap} for the whole wave")),
        ),
    );

    // Take the issue leases first, in readiness order. An issue a live
    // peer holds is deferred by name, never contested. A ledger that will
    // not answer fails open, the same direction every claim probe takes.
    // Each grant is wrapped as a `MirroredLease`, so the `drop` below that
    // frees the ledger rows also tells GitHub each issue is free again.
    let mut taken: Vec<(Issue, Option<super::claim_mirror::MirroredLease<'_>>)> = Vec::new();
    for issue in ready {
        if taken.len() >= bound {
            break;
        }
        match super::claim::acquire(root, issue.key.as_str()) {
            super::claim::Claim::Granted(lease) => {
                audit::record(
                    durable,
                    Audit::Claimed,
                    Some(issue.key.as_str()),
                    "taken off the ready backlog for the wave",
                );
                durable.update_stats(|s| s.issues_claimed += 1);
                let mirrored = super::claim_mirror::MirroredLease::new(
                    lease,
                    provider,
                    issue.key.as_str(),
                    &cfg.attribution.issue_comment,
                );
                taken.push((issue, Some(mirrored)));
            }
            super::claim::Claim::HeldBy(owner) => {
                audit::record(
                    durable,
                    Audit::Deferred,
                    Some(issue.key.as_str()),
                    &format!("{owner} is already on it; the wave takes the next candidate"),
                );
                durable.update_stats(|s| s.issues_deferred += 1);
            }
            super::claim::Claim::Unavailable => taken.push((issue, None)),
        }
    }

    if taken.is_empty() {
        audit::record(
            durable,
            Audit::Waited,
            None,
            "the ready backlog offers nothing this wave can take — nothing was dispatched",
        );
        return Ok(());
    }

    let base = super::work::base_ref(root);
    let base_branch = base.rsplit('/').next().unwrap_or("main").to_owned();
    let base_sha = super::state::git(root, &["rev-parse", "--verify", &base])
        .ok_or_else(|| format!("cannot resolve the wave's base ref {base}"))?;

    let issues: Vec<Issue> = taken.iter().map(|(issue, _)| issue.clone()).collect();
    let plan = plan_for(&issues, &cfg.attribution.commit, &base_branch);

    let outcome = dispatch(durable, root, flags, width, &plan, &base_sha);
    // The leases live exactly as long as the dispatch. Dropping them here,
    // on every outcome, frees a failed wave's issues for the next pass
    // rather than leaving them to time out.
    drop(taken);

    let report = outcome?;
    audit::record(
        durable,
        Audit::SessionStopped,
        Some(&session_id),
        &format!(
            "wave settled — {} of {} task(s) succeeded, ${:.4} spent",
            report.completed.len(),
            plan.tasks.len(),
            report.total_cost_usd(),
        ),
    );
    if report.budget_aborted {
        return Err(format!(
            "budget cap reached after ${:.4} — the wave stopped launching workers",
            report.total_cost_usd()
        ));
    }
    if !report.all_succeeded() {
        return Err("one or more wave workers failed — see the journal above".to_string());
    }
    Ok(())
}

/// One single-wave fleet plan: a task per ready issue, every task isolated.
///
/// Isolated is the point, not a default. Each worker commits its own
/// change against the same base, which is exactly what worktree isolation
/// exists for. No `depends_on` edges, because readiness already said these
/// issues do not block each other. So the whole plan is one wave, and the
/// dispatch width is the only ordering.
fn plan_for(issues: &[Issue], commit_signature: &str, base_branch: &str) -> Plan {
    Plan::new(
        issues
            .iter()
            .map(|issue| {
                let mut prompt = super::work::prompt_for(issue, commit_signature);
                prompt.push_str(&format!(
                    "\nWhen the work is committed, push the current branch to origin and open \
                     a pull request against {base_branch} for it.\n"
                ));
                Task::new(
                    format!("issue-{}", issue.key.as_str()),
                    issue.title.clone(),
                    prompt,
                )
                .isolated()
            })
            .collect(),
    )
}

/// Fan the plan out through the fleet and wait for it to settle.
fn dispatch(
    durable: &Durable,
    root: &Path,
    flags: &TurnFlags,
    width: u32,
    plan: &Plan,
    base_sha: &str,
) -> Result<stella_fleet::FleetRunReport, String> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch — cannot mint a wave run id")?
        .as_millis();
    // Ends in this process's pid. The fleet's claim reaper parses that
    // tail to decide whether a holder is still alive.
    let run_id = format!("drive-{now_ms}-{}", std::process::id());

    let ledger_path = stella_store::workspace_private_sqlite_path(root, "fleet.db")
        .map_err(|error| format!("could not prepare private fleet state: {error}"))?;
    let ledger = Ledger::open(&ledger_path)
        .map_err(|error| format!("could not open the fleet ledger: {error}"))?;

    // The run's cap, divided across the width, so the children in flight
    // cannot together spend past it. The parent guard still meters the
    // total and stops launching when it is spent.
    let mut child_flags = flags.clone();
    child_flags.spend_limit = flags.spend_limit.map(|cap| cap / f64::from(width.max(1)));

    let fleet = Fleet::new(
        TurnWorker {
            state_root: root.to_path_buf(),
            flags: child_flags,
        },
        WorktreeManager::new(SystemGitCli, root.to_path_buf()).with_run_scope(&run_id),
        ledger,
        crate::agent::build_budget_guard(flags.spend_limit),
        crate::runtime::WallClock,
        FleetConfig::new(&run_id, base_sha).with_max_concurrency(width.max(1) as usize),
    )
    .map_err(|error| format!("could not start the wave's fleet: {error}"))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start a runtime for the wave: {error}"))?;
    let report = runtime
        .block_on(fleet.run_plan(plan))
        .map_err(|error| format!("the wave's fleet run failed: {error}"))?;

    for handle in &report.handles {
        let key = handle
            .task_id
            .strip_prefix("issue-")
            .unwrap_or(&handle.task_id)
            .to_string();
        let action = if handle.outcome.success {
            Audit::WorkChanged
        } else {
            Audit::WorkFailed
        };
        let branch = handle
            .worktree
            .as_ref()
            .map_or(String::new(), |wt| format!(" on {}", wt.branch));
        audit::record(
            durable,
            action,
            Some(&key),
            &format!("{}{branch}", clip(&handle.outcome.summary)),
        );
    }
    for (task_id, reason) in &report.dispatch_failures {
        audit::record(
            durable,
            Audit::WorkFailed,
            Some(task_id),
            &format!("dispatch failed before a worker ran: {reason}"),
        );
    }

    Ok(report)
}

/// A fleet worker that is a child `stella run` — the same turn the serial
/// loop spawns. So a wave worker gets exactly what a serial work unit
/// gets: the workspace's steering, its own budget slice, its own summary.
struct TurnWorker {
    /// The repository root the durable state is keyed to. Handed to every
    /// child so what it learns outlasts its throwaway worktree.
    state_root: PathBuf,
    /// Per-child flags, spend limit already divided across the width.
    flags: TurnFlags,
}

#[async_trait]
impl FleetWorker for TurnWorker {
    async fn run(
        &self,
        task: &Task,
        workspace_root: &Path,
        _controls: WorkerControls,
    ) -> WorkerOutcome {
        let dir = workspace_root.to_path_buf();
        let state_root = self.state_root.clone();
        let prompt = task.prompt.clone();
        let flags = self.flags.clone();
        // The child is a blocking wait on a real process. Parking it on
        // the blocking pool keeps the fleet free to run the rest of the
        // wave.
        let settled = tokio::task::spawn_blocking(move || {
            let mut budget = RunBudget::new(flags);
            let turn = super::work::run_turn(&dir, &state_root, &prompt, &mut budget);
            (turn, budget.spent())
        })
        .await;
        match settled {
            Ok((Ok(summary), spent)) => WorkerOutcome {
                cost_usd: spent,
                commits: Vec::new(),
                summary: clip(&summary),
                success: true,
            },
            Ok((Err(error), spent)) => WorkerOutcome {
                cost_usd: spent,
                commits: Vec::new(),
                summary: clip(&error),
                success: false,
            },
            Err(join) => WorkerOutcome {
                cost_usd: 0.0,
                commits: Vec::new(),
                summary: format!("the worker's thread did not settle: {join}"),
                success: false,
            },
        }
    }
}

/// One journal-sized line out of a child's JSON summary or error text.
fn clip(text: &str) -> String {
    let line = text.lines().last().unwrap_or(text).trim();
    let mut out: String = line.chars().take(200).collect();
    if out.len() < line.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use stella_fleet::Isolation;
    use stella_protocol::issue::{Issue, IssueClass, IssueKey, IssueState};

    use super::*;

    fn issue(number: u64, title: &str) -> Issue {
        Issue {
            key: IssueKey(number.to_string()),
            title: title.to_string(),
            body: format!("body of {number}"),
            state: IssueState::Open,
            class: IssueClass::Task,
            labels: Vec::new(),
            created_at: String::new(),
            updated_at: String::new(),
            url: String::new(),
            parent: None,
        }
    }

    /// **The witness.** A backlog of ready issues becomes a plan the
    /// fleet can drain in ONE wave. A task per issue. No `depends_on`
    /// edges. Every task `isolated`, so each worker owns a worktree. This
    /// is the shape the width throttles. A plan with stray edges would
    /// chain the very issues the ready fold said were free of each other.
    #[test]
    fn three_independent_ready_issues_make_a_three_task_single_wave_plan() {
        let issues = vec![issue(11, "first"), issue(12, "second"), issue(13, "third")];
        let plan = plan_for(&issues, "signed-by-the-loop", "main");

        plan.validate().expect("the wave plan is a valid DAG");
        assert_eq!(plan.tasks.len(), 3, "one task per ready issue");

        // The whole plan is ready at once — a single wave.
        let first_wave = plan.ready_tasks(&std::collections::HashSet::new());
        assert_eq!(first_wave.len(), 3, "no task waits on another");

        for task in &plan.tasks {
            assert_eq!(
                task.isolation,
                Isolation::Isolated,
                "every wave worker gets its own worktree"
            );
            assert!(task.depends_on.is_empty(), "readiness means independent");
        }
        assert_eq!(plan.tasks[0].id, "issue-11");
        assert!(
            plan.tasks[0].prompt.contains("body of 11"),
            "the worker's prompt quotes the issue it was built from"
        );
        assert!(
            plan.tasks[0].prompt.contains("signed-by-the-loop"),
            "the loop's commit signature reaches the worker's contract"
        );
        assert!(
            plan.tasks[0]
                .prompt
                .contains("open a pull request against main"),
            "a wave worker is told to deliver, not only to commit"
        );
    }

    /// **Witness.** A wave refuses a worker it cannot run.
    ///
    /// A wave runs a child `stella run`. Under a claude worker it would run
    /// stella instead. The refusal names both ways out, so the choice stays
    /// with the operator.
    #[test]
    fn a_wave_refuses_a_worker_it_cannot_run() {
        let stella = crate::settings::toml_config::WorkerSection::default();
        assert_eq!(
            stella.kind,
            WorkerKind::Stella,
            "the default worker is the one a wave runs"
        );
        assert!(refuse_unsupported_worker(&stella).is_ok());

        let claude = crate::settings::toml_config::WorkerSection {
            kind: WorkerKind::Claude,
            ..Default::default()
        };
        let refusal = refuse_unsupported_worker(&claude)
            .expect_err("a wave cannot run claude, so it must say so rather than run stella");
        assert!(
            refusal.contains("--parallel"),
            "the refusal names the flag to drop: {refusal}"
        );
        assert!(
            refusal.contains("worker.kind"),
            "the refusal names the setting to change: {refusal}"
        );
    }
}
