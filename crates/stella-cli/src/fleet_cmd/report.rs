//! What a finished fleet run prints.
//!
//! Its own file. `fleet_cmd.rs` is at its size ceiling. This half only
//! reads: the grid recap, the per-task report, the totals. None of it runs
//! while the fleet does.

use std::path::Path;
use std::time::Duration;

use colored::Colorize;
use stella_fleet::{FleetRunReport, Plan};
use stella_tui::{FleetDashResult, FleetStatus};

use super::SUMMARY_CHARS;

/// One line, capped, with a mark where it was cut.
pub(super) fn truncate(s: &str) -> String {
    let one_line = s.replace('\n', " ");
    let mut out: String = one_line.chars().take(SUMMARY_CHARS).collect();
    if one_line.chars().count() > SUMMARY_CHARS {
        out.push('…');
    }
    out
}

/// The grid's one-screen recap, printed once the dashboard gives the
/// terminal back. Per task: status, time spent, tool calls. Then the whole
/// session's time. [`render_report`] follows with spend, commits and
/// worktrees.
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

/// The end-of-run report. Per task: outcome, spend, commits, and the
/// worktree that holds the work. Then the totals, and where the receipts
/// live.
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
        // A ledger close that failed after the worker settled. A lease lost
        // mid-run. stella-fleet writes those lines. It holds the facts.
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
