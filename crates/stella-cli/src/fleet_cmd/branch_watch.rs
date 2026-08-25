// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella fleet --watch`: what happens to a fleet's branches *after* the
//! fan-out settles — which branches are worth watching, the capped CI wait on
//! each, and the one report line each produces.
//!
//! A submodule of [`crate::fleet_cmd`] rather than a `mod` block inside
//! `fleet_cmd.rs`, on the same reasoning `wrapped.rs` gives: the parent
//! already carries the fan-out, the dashboard tee, the claim tap and the
//! report, and this is the one concern in the file that begins where all of
//! those have finished. The parent crossed the 1500-line ceiling (#4821) and
//! this is the seam that was already there.

use super::*;

/// One fleet branch's post-fanout verdict: the capped CI watch outcome plus
/// the branch's reconciled PR status. `pr` is `None` when the branch has no
/// PR yet — branches are left for review, so that is a normal state, not an
/// error.
pub(super) struct BranchWatch {
    pub(super) task_id: TaskId,
    pub(super) branch: String,
    pub(super) ci: Result<CiWatchOutcome, MonitorError>,
    pub(super) pr: Option<PrStatus>,
}

impl BranchWatch {
    /// Green iff CI completed with a passing overall conclusion — a timeout,
    /// a monitor error, and a failing conclusion are all red.
    pub(super) fn is_green(&self) -> bool {
        matches!(
            &self.ci,
            Ok(CiWatchOutcome::Completed { conclusion, .. }) if !conclusion.is_failure()
        )
    }
}

/// The branches worth watching after the fan-out: every successful task that
/// landed commits, keyed by the branch its commits actually record (correct
/// for isolated worktrees and shared-tree tasks alike), deduped so a branch
/// shared by several tasks is watched once.
pub(super) fn watch_targets(report: &FleetRunReport) -> Vec<(TaskId, String)> {
    let mut seen = HashSet::new();
    report
        .handles
        .iter()
        .filter(|h| h.outcome.success)
        .filter_map(|h| {
            let branch = h.outcome.commits.last()?.branch.clone();
            seen.insert(branch.clone())
                .then(|| (h.task_id.clone(), branch))
        })
        .collect()
}

/// Watch one fleet branch: its CI to completion (the monitor's capped
/// deferred wait, L-E4), then a live PR-status reconcile — `gh pr view`
/// resolves a branch name to its PR.
pub(super) async fn watch_branch<H: GhCli>(
    monitor: &Monitor<H>,
    task_id: &str,
    branch: &str,
) -> BranchWatch {
    let ci = monitor.watch_ci(branch).await;
    let pr = monitor.pr_status(branch).await.ok();
    BranchWatch {
        task_id: task_id.to_string(),
        branch: branch.to_string(),
        ci,
        pr,
    }
}

/// One report line per watched branch: verdict mark, CI outcome, PR status.
pub(super) fn render_watch_line(watch: &BranchWatch) {
    let mark = if watch.is_green() {
        "✓".green()
    } else {
        "✗".red()
    };
    let ci = match &watch.ci {
        Ok(CiWatchOutcome::Completed {
            conclusion,
            summary,
        }) => {
            let verdict = if conclusion.is_failure() {
                "red"
            } else {
                "green"
            };
            format!("CI {verdict} — {summary}")
        }
        Ok(CiWatchOutcome::TimedOut {
            reason,
            last_observed,
            waited_ms,
        }) => {
            let reason = match reason {
                TimeoutReason::CumulativeCap => "cumulative cap",
                TimeoutReason::Stalled => "stalled",
                TimeoutReason::NoRunsStarted => "no CI runs started",
            };
            format!(
                "CI watch timed out ({reason}) after {}m — last: {last_observed}",
                waited_ms / 60_000
            )
        }
        Err(e) => format!("CI watch failed: {e}"),
    };
    let pr = match watch.pr {
        Some(PrStatus::Draft) => "PR draft",
        Some(PrStatus::Open) => "PR open",
        Some(PrStatus::Merged) => "PR merged",
        Some(PrStatus::Closed) => "PR closed",
        None => "no PR",
    };
    println!(
        "  {mark} {} {} — {ci} · {pr}",
        watch.task_id.bold(),
        watch.branch.bright_magenta()
    );
}
