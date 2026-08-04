//! `stella memory index` — build the IVF similarity index over
//! `.stella/private/context.db`.
//!
//! Its own module for the reason `memory_compact.rs` is: a new verb belongs in a
//! new file long before the file it would join needs splitting, and
//! `memory_cmd.rs` is already near the 1500-line ratchet.
//!
//! # Why this is a command and not something recall does for itself
//!
//! Building clusters every vector in the workspace, which is the one shape of
//! work the per-turn path exists to avoid. An auto-build would put a
//! whole-corpus pass on somebody's first token, and an auto-*re*build would put
//! it there again every time the store drifted — turning an opt-in accelerator
//! into an occasional several-second stall with no command to blame.
//!
//! So the index is built when a person asks, it is used only when
//! `context.retrieval.ann_enabled` is on, and when it drifts too far to be
//! honest the probe declines and recall silently goes back to being exact. The
//! word "silently" is doing no work there: `RecallResult::used_ann_index` says
//! which scan ran on every single recall.
//!
//! # What it does not do
//!
//! It mints no new flag vocabulary. `--dry-run` means what it means on `stella
//! memory compact` and `stella stats prune`. There is no `--probes`: probe width
//! is a *query* knob that lives in settings, and accepting it here would create
//! a second place for the same number with no way to tell which one a recall
//! used.

use colored::Colorize;

use stella_context::{AnnIndexPolicy, AnnIndexReport, ContextStore};

/// Flags for `stella memory index`.
#[derive(clap::Args, Debug, Clone)]
pub struct IndexArgs {
    /// Remove the index instead of building it. Recall reverts to the exact
    /// full scan, which is what it does by default anyway
    #[arg(long)]
    pub drop: bool,

    /// Report what would be built without writing anything
    #[arg(long)]
    pub dry_run: bool,
}

/// Entry point for `stella memory index`.
///
/// The engine ([`ContextStore::index_ann`]) owns the clustering and the
/// one-transaction guarantee; this function owns the two things that are
/// properly the CLI's, exactly as `stella memory compact` does:
///
/// 1. **Not creating the database.** `ContextStore::open` migrates and writes a
///    fingerprint row, so a workspace that has never indexed anything would get
///    a `context.db` created purely to report that there is nothing to index.
/// 2. **Saying what happened, including "nothing".** An empty store is a fact
///    about the store and prints as one; a policy that could never do anything
///    is a mistake in the invocation and is a hard error from the engine.
pub fn run_memory_index(args: &IndexArgs) -> Result<(), String> {
    let IndexArgs { drop, dry_run } = *args;

    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let Some(context_db) =
        stella_store::existing_workspace_private_sqlite_path(&workspace_root, "context.db")
            .map_err(|e| format!("cannot resolve context store: {e}"))?
    else {
        println!("no context store in this workspace — nothing to index");
        return Ok(());
    };
    let store =
        ContextStore::open(&context_db).map_err(|e| format!("cannot open context store: {e}"))?;

    let policy = AnnIndexPolicy {
        build: !drop,
        // A rebuild clears the previous index first, so `build` already implies
        // the removal; `drop` here is the standalone "remove and stop".
        drop,
        dry_run,
    };
    let report = store
        .index_ann(&policy)
        .map_err(|e| format!("indexing failed: {e}"))?;

    print_index_report(&report, dry_run);
    if !dry_run && !drop {
        report_state(&store)?;
    }
    // Last, so it is the line left on screen. The index does nothing until the
    // setting is on, and a user who built one and saw no change deserves to be
    // told why here rather than to go looking.
    println!(
        "{}",
        "the index only serves recalls when context.retrieval.ann_enabled is true — \
         until then every recall runs the exact scan"
            .dimmed()
    );
    Ok(())
}

fn print_index_report(report: &AnnIndexReport, dry_run: bool) {
    if report.dropped {
        let verb = if dry_run { "would remove" } else { "removed" };
        if report.postings_removed == 0 {
            println!("no index to remove — recall was already running the exact scan");
        } else {
            println!(
                "{verb} {} posting(s) — recall reverts to the exact full scan",
                report.postings_removed
            );
        }
        return;
    }
    if report.vectors_indexed == 0 {
        println!(
            "nothing to index — this workspace has no embeddings under the active \
             embedder yet (run a session, or wait for warming to finish)"
        );
        return;
    }
    let verb = if dry_run { "would index" } else { "indexed" };
    println!(
        "{verb} {} vector(s) into {} centroid(s)",
        report.vectors_indexed, report.centroids
    );
    if report.postings_removed > 0 {
        // A rebuild, not a first build. Said explicitly because the previous
        // index is gone either way — there is no partial-update mode to wonder
        // about.
        println!(
            "{}",
            format!(
                "replaced a previous index of {} posting(s)",
                report.postings_removed
            )
            .dimmed()
        );
    }
}

/// Read the watermark and the drift back, so the command's last word about the
/// index is the store's own record of it rather than the report it just printed.
fn report_state(store: &ContextStore) -> Result<(), String> {
    let Some(state) = store
        .ann_index_state()
        .map_err(|e| format!("indexed, but cannot read the index state back: {e}"))?
    else {
        return Ok(());
    };
    let drift = store
        .ann_index_drift()
        .map_err(|e| format!("indexed, but cannot measure index drift: {e}"))?;
    println!(
        "{}",
        format!(
            "index watermark: {} over {} vector(s), {} centroid(s), {drift} unindexed",
            state.built_at, state.vector_count, state.centroid_count
        )
        .dimmed()
    );
    Ok(())
}
