//! Turning a finished fleet run into terminal output.
//!
//! Split out of `fleet_cmd.rs` when it crossed the 1500-line ceiling. These
//! four are the group with no callers inside the parent beyond the two print
//! points and no dependency on the run machinery — they read a finished report
//! and write to stdout, which is why they move together and why nothing else
//! had to move with them.

use std::time::Duration;

use colored::Colorize;

use super::*;

/// Cap on the per-task summary line so the report table stays a table.
pub(super) const SUMMARY_CHARS: usize = 96;
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
pub(super) fn truncate(s: &str) -> String {
    let one_line = s.replace('\n', " ");
    let mut out: String = one_line.chars().take(SUMMARY_CHARS).collect();
    if one_line.chars().count() > SUMMARY_CHARS {
        out.push('…');
    }
    out
}
/// The live grid's one-screen recap, printed on the normal screen after the
/// dashboard restores it: each task's final status, wall-clock ELAPSED, and
/// tool-call count, then the total SESSION time. The `render_report` below
/// follows with the durable details (spend, commits, worktrees).
pub(super) fn print_dash_summary(res: &FleetDashResult) {
    let fmt_elapsed = |d: Duration| {
        let s = d.as_secs();
        format!("{:02}:{:02}", s / 60, s % 60)
    };
    let fmt_session = |d: Duration| {
        let s = d.as_secs();
        format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    };
    println!();
    for t in &res.tasks {
        let glyph = match t.status {
            FleetStatus::Done => t.status.glyph().green(),
            FleetStatus::Failed | FleetStatus::Killed => t.status.glyph().red(),
            FleetStatus::Blocked => t.status.glyph().yellow(),
            _ => t.status.glyph().normal(),
        };
        println!(
            "  {glyph} {} — {} ({}, {} tool call{})",
            t.id.bold(),
            t.title,
            fmt_elapsed(t.elapsed),
            t.tool_calls,
            if t.tool_calls == 1 { "" } else { "s" },
        );
    }
    let tail = if res.detached {
        " (detached — run continued to completion)"
    } else {
        ""
    };
    println!(
        "  {} session {}{tail}",
        "·".dimmed(),
        fmt_session(res.session_elapsed).bold()
    );
}
/// The end-of-run report: per task its outcome, spend, commits, and (when
/// isolated) the worktree that holds the work, then the totals and where the
/// receipts live.
pub(super) fn render_report(plan: &Plan, report: &FleetRunReport, ledger_path: &Path) {
    println!();
    for handle in &report.handles {
        let ok = handle.outcome.success;
        let mark = if ok { "✓".green() } else { "✗".red() };
        let title = plan
            .task(&handle.task_id)
            .map(|t| t.title.as_str())
            .unwrap_or("");
        println!(
            "  {mark} {} — {} (${:.4}, {} commit{})",
            handle.task_id.bold(),
            title,
            handle.outcome.cost_usd,
            handle.outcome.commits.len(),
            if handle.outcome.commits.len() == 1 {
                ""
            } else {
                "s"
            },
        );
        if let Some(worktree) = &handle.worktree {
            println!(
                "      {} {} @ {}",
                "↳".dimmed(),
                worktree.branch.bright_magenta(),
                worktree.path.display().to_string().dimmed()
            );
        }
        if !handle.outcome.summary.is_empty() {
            println!("      {}", handle.outcome.summary.dimmed());
        }
        // Durable-failure notices (a ledger close that failed after the
        // worker settled, a dispatch lease lost mid-run) are composed in
        // stella-fleet — this file is at its size ceiling (#1677).
        for notice in stella_fleet::handle_notices(handle) {
            println!("      {} {notice}", "!".yellow());
        }
    }
    for (task_id, reason) in &report.dispatch_failures {
        println!(
            "  {} {} — dispatch failed: {}",
            "✗".red(),
            task_id.bold(),
            reason.dimmed()
        );
    }
    if !report.skipped.is_empty() {
        println!(
            "  {} skipped (dependency failed or budget stop): {}",
            "○".yellow(),
            report.skipped.join(", ").dimmed()
        );
    }
    println!(
        "\n  total ${:.4} · ledger {} · worktrees kept for review (`git worktree list`)\n",
        report.total_cost_usd(),
        ledger_path.display(),
    );
}
