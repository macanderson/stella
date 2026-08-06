//! `stella fleet clean` — reclaim the worktrees and `fleet/*` branches that
//! finished runs left behind (issue #1217).
//!
//! `stella fleet` deliberately leaves an isolated task's worktree and branch
//! in place so the result can be reviewed. Nothing ever removed them, and
//! since every slug hashes its run scope, every run of every isolated task
//! added a directory and a branch that outlived it — a workspace's terminal
//! state was disk exhaustion. This is the door out, and it is explicit: no
//! part of `stella fleet` calls it for you.
//!
//! Everything that decides *what may go* lives in
//! [`stella_fleet::gc`](stella_fleet::gc) behind the `GitCli` port; this
//! module is the flag surface, the ledger read, and the printed report. It is
//! a sibling of `fleet_cmd.rs` rather than a section of it because that file
//! is a grandfathered god file closed to growth (AGENTS.md § *God files*).

use std::path::Path;

use clap::Subcommand;
use colored::Colorize;
use stella_fleet::{BranchAction, Gc, GcOptions, GcReport, Ledger, SystemGitCli, WorktreeAction};

use crate::config::Config;
use crate::tui;

/// The `stella fleet <cmd>` subcommands. Kept in its own enum (rather than
/// more flags on `stella fleet`) so the maintenance verb cannot be confused
/// with a task prompt — with one caveat worth knowing: a positional prompt
/// whose **first word** is exactly a subcommand name is read as that
/// subcommand. Prefix the prompts with `--` when that happens
/// (`stella fleet -- clean up the dead code`).
#[derive(Subcommand, Debug, Clone)]
pub enum FleetCmd {
    /// Reclaim finished isolated worktrees and their fleet/* branches
    ///
    /// Sweeps .stella/worktrees/ and the fleet/ branch namespace, removing
    /// only what is provably safe to remove: a worktree with no unfinished
    /// attempt in the ledger, a clean working tree, and a branch whose
    /// commits the base ref already contains. Anything else is KEPT and the
    /// reason printed. The main checkout and every branch outside fleet/ are
    /// never touched, with or without --force. Offline: git plus
    /// .stella/private/fleet.db, needs no API key.
    Clean {
        /// Decide and print, remove nothing
        #[arg(long)]
        dry_run: bool,

        /// Also reclaim worktrees with uncommitted changes and branches whose
        /// commits are not in the base ref — this DESTROYS that work. Never
        /// touches a worktree whose attempt is still in flight.
        #[arg(long)]
        force: bool,

        /// Only consider worktrees whose last attempt finished at least this
        /// many days ago (a worktree with no recorded finish time is kept)
        #[arg(long, value_name = "DAYS")]
        age: Option<u32>,

        /// The ref a branch's commits must already be contained in to count
        /// as reclaimable (default: origin/HEAD, else main, else master)
        #[arg(long, value_name = "REF")]
        base_ref: Option<String>,
    },
}

/// Run one `stella fleet` subcommand.
pub async fn run(cfg: &Config, cmd: &FleetCmd) -> Result<(), String> {
    match cmd {
        FleetCmd::Clean {
            dry_run,
            force,
            age,
            base_ref,
        } => {
            clean(
                &cfg.workspace_root,
                &GcOptions {
                    dry_run: *dry_run,
                    force: *force,
                    min_age_days: *age,
                    base_ref: base_ref.clone(),
                },
            )
            .await
        }
    }
}

async fn clean(root: &Path, opts: &GcOptions) -> Result<(), String> {
    // The ledger is the only thing that knows whether a run is still using a
    // worktree, so a sweep that cannot read it must not proceed on the files
    // alone — an in-flight worktree looks exactly like a finished one on disk.
    let ledger_path = stella_store::workspace_private_sqlite_path(root, "fleet.db")
        .map_err(|e| format!("could not locate the fleet ledger: {e}"))?;
    let activity = if ledger_path.exists() {
        Ledger::open(&ledger_path)
            .map_err(|e| format!("could not open the fleet ledger: {e}"))?
            .worktree_activity()
            .map_err(|e| format!("could not read the fleet ledger: {e}"))?
    } else {
        // No ledger at all: no run has ever been recorded here, so nothing can
        // be in flight. The file/branch predicates still decide everything.
        Vec::new()
    };

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("system clock is before the Unix epoch: {e}"))?
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;

    let report = Gc::new(SystemGitCli, root)
        .sweep(&activity, opts, now_ms)
        .await
        .map_err(|e| format!("fleet clean failed: {e}"))?;

    print_report(&report);
    if report.had_failures() {
        return Err("some worktrees or branches could not be removed — see above".to_string());
    }
    Ok(())
}

fn print_report(report: &GcReport) {
    tui::section_header("Stella — fleet clean");
    println!(
        "  base {}{}\n",
        report.base_ref,
        if report.dry_run {
            "  (dry run — nothing was removed)"
        } else {
            ""
        }
    );

    for verdict in &report.worktrees {
        let name = verdict.path.file_name().map_or_else(
            || verdict.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let branch = verdict.branch.as_deref().unwrap_or("(detached)");
        match &verdict.action {
            WorktreeAction::Removed { branch_deleted } => println!(
                "    {} {name} — removed{}",
                "✓".green(),
                if *branch_deleted {
                    format!(", branch {branch} deleted")
                } else {
                    String::new()
                }
            ),
            WorktreeAction::WouldRemove {
                branch_would_delete,
            } => println!(
                "    {} {name} — would remove{}",
                "·".dimmed(),
                if *branch_would_delete {
                    format!(", with branch {branch}")
                } else {
                    String::new()
                }
            ),
            WorktreeAction::Kept(reason) => {
                println!("    {} {name} — kept: {reason}", "•".dimmed());
            }
            WorktreeAction::Failed(err) => {
                println!("    {} {name} — failed: {err}", "✗".red());
            }
        }
    }

    for verdict in &report.branches {
        match &verdict.action {
            BranchAction::Deleted => {
                println!("    {} {} — branch deleted", "✓".green(), verdict.branch);
            }
            BranchAction::WouldDelete => {
                println!(
                    "    {} {} — would delete branch",
                    "·".dimmed(),
                    verdict.branch
                );
            }
            BranchAction::Kept(reason) => {
                println!("    {} {} — kept: {reason}", "•".dimmed(), verdict.branch);
            }
            BranchAction::Failed(err) => {
                println!("    {} {} — failed: {err}", "✗".red(), verdict.branch);
            }
        }
    }

    if report.worktrees.is_empty() && report.branches.is_empty() {
        println!("    {} nothing to clean", "·".dimmed());
    }
    println!(
        "\n  {} worktree(s), {} branch(es) {}\n",
        report.reclaimed_worktrees(),
        report.reclaimed_branches(),
        if report.dry_run {
            "reclaimable"
        } else {
            "reclaimed"
        }
    );
}
