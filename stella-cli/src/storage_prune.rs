//! `stella storage prune` — bound `store.db`'s growth.
//!
//! The counterpart to `stella usage prune`. That verb bounds the *derived*
//! cross-project hub; this one bounds the workspace's source of truth, which
//! is the tier that actually holds prompt text and event payloads and grows a
//! row per event forever.
//!
//! Deliberately conservative, because the thing being pruned is not
//! reconstructible from anywhere else: it refuses to run without an explicit
//! knob, `--dry-run` shows the exact report a real run would produce, and the
//! guards that protect in-flight turns and `forgotten` tombstones are
//! structural rather than flags (see [`stella_store::retention`]).

use clap::Args;
use colored::Colorize as _;

use stella_store::Store;
use stella_store::retention::{RetentionPolicy, RetentionReport};

/// Flags for `stella storage prune`.
///
/// Defined here rather than inline in `main.rs`'s `StorageCmd` so the knobs sit
/// next to the code that interprets them — and so adding a verb costs the
/// already-oversized `main.rs` one line instead of forty.
#[derive(Args, Debug, Clone)]
pub(crate) struct PruneArgs {
    /// Drop executions that finished longer ago than this window: N followed
    /// by d, w, h, mo, or y (e.g. `90d`)
    #[arg(long, value_name = "AGE")]
    pub older_than: Option<String>,
    /// Hard ceiling on retained executions; evict the oldest first
    #[arg(long, value_name = "N")]
    pub max_executions: Option<i64>,
    /// Also prune executions whose enterprise export is still pending (breaks
    /// that drain). Does not reach in-flight turns or tombstones.
    #[arg(long)]
    pub force: bool,
    /// `VACUUM` afterwards to return file bytes to the filesystem
    #[arg(long)]
    pub vacuum: bool,
    /// Report what would be removed without removing anything
    #[arg(long)]
    pub dry_run: bool,
}

/// Run the prune and print its report.
pub(crate) fn run(args: &PruneArgs) -> Result<(), String> {
    let PruneArgs {
        older_than,
        max_executions,
        force,
        vacuum,
        dry_run,
    } = args.clone();
    if older_than.is_none() && max_executions.is_none() {
        return Err(
            "nothing to prune — pass at least one of --older-than or --max-executions".to_string(),
        );
    }
    let older_than = older_than
        .as_deref()
        .map(crate::usage_cmd::parse_age_window)
        .transpose()?;

    let root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let store = Store::open(&root).map_err(|e| format!("cannot open store.db: {e}"))?;

    let policy = RetentionPolicy {
        older_than,
        max_executions,
        force,
        vacuum,
        dry_run,
    };
    let report = store.prune(&policy).map_err(|e| e.0)?;
    print_report(&report, dry_run);
    Ok(())
}

fn print_report(report: &RetentionReport, dry_run: bool) {
    let lead = if dry_run { "would remove" } else { "removed" };

    if report.is_empty() {
        println!(
            "{}",
            "nothing to prune — no execution matched the policy".dimmed()
        );
    } else {
        println!(
            "{} {} execution(s) and {} related row(s)",
            if dry_run { "◇" } else { "◆" }.bright_magenta(),
            report.executions_removed().to_string().bold(),
            report.child_rows_removed.to_string().bold(),
        );
        if report.aged_out > 0 {
            println!(
                "  {} {} past the age cutoff",
                lead.dimmed(),
                report.aged_out
            );
        }
        if report.ceiling_evicted > 0 {
            println!(
                "  {} {} to satisfy the ceiling",
                lead.dimmed(),
                report.ceiling_evicted
            );
        }
    }

    // Always explain what was held back — a smaller-than-expected prune with
    // no explanation is the thing that makes people reach for `--force`.
    if report.protected_unfinished > 0 {
        println!(
            "  {} {} unfinished execution(s) kept (in flight; never pruned)",
            "•".dimmed(),
            report.protected_unfinished
        );
    }
    if report.protected_pending_export > 0 {
        println!(
            "  {} {} execution(s) kept for a pending enterprise export — drain first, or \
             re-run with {}",
            "•".dimmed(),
            report.protected_pending_export,
            "--force".bold()
        );
    }
    if report.still_over_ceiling > 0 {
        println!(
            "  {} still {} over the ceiling: the rest are protected",
            "•".dimmed(),
            report.still_over_ceiling
        );
    }
    if report.vacuumed {
        println!(
            "  {} VACUUMed — file bytes returned to the filesystem",
            "•".dimmed()
        );
    }
    if dry_run {
        println!(
            "{}",
            "dry run — nothing was deleted; re-run without --dry-run to apply".dimmed()
        );
    }
}
