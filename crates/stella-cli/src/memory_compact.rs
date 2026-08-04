//! `stella memory compact` — reclaim the disposable half of
//! `.stella/private/context.db`.
//!
//! Its own module rather than more of `memory_cmd.rs`, which is 1,346 lines
//! against the 1500-line ratchet (`make file-size`): a new verb belongs in a new
//! file long before the file it would join needs splitting.
//!
//! # Why `stella memory`, not `stella storage` / `stella stats`
//!
//! `stella stats prune` and its `stella storage prune` alias operate on
//! `store.db` — executions, telemetry, receipts — through
//! [`stella_store::StorePrunePolicy`], and they carry a cross-database
//! obligation (reading the usage hub's replication cursor in, writing the
//! corrected one back) that has no analogue here. This verb operates on
//! `context.db`, which is the plane every other `stella memory` verb already
//! talks to: `list`, `forget`, `edit`, `restore` all resolve `nod_…` ids through
//! [`stella_context::ContextStore`]. Mounting compaction under `storage` would
//! put two different engines against two different files behind one noun, which
//! is precisely the drift #709 had to undo.
//!
//! # What it does not do
//!
//! It does not mint a rival flag vocabulary. `--dry-run` and `--vacuum` mean
//! here exactly what they mean on `prune`, and there is deliberately no
//! `--older-than`/`--max-rows` analogue: compaction has no retention window to
//! express, because it reclaims only rows whose owner is already gone. There is
//! also no interactive confirm — `--dry-run` is the preview, as it is on
//! `prune`, and a prompt would make the verb unscriptable.

use colored::Colorize;

use stella_context::{ContextCompactPolicy, ContextCompactReport, ContextStore};

/// Flags for `stella memory compact`.
///
/// Three flags, all of which exist on `stella stats prune` with the same
/// meaning, plus the one knob that is genuinely new because it is the one class
/// whose reclaim costs future work.
#[derive(clap::Args, Debug, Clone)]
pub struct CompactArgs {
    /// Also drop vectors written under an embedder other than the active one.
    /// Recall already ignores them (L-C2 forbids mixing fingerprints), so this
    /// frees space today at the cost of a full re-embed if you ever switch back
    #[arg(long)]
    pub stale_fingerprints: bool,

    /// VACUUM afterwards to hand freed pages back to the filesystem (also
    /// automatic for a large compaction)
    #[arg(long)]
    pub vacuum: bool,

    /// Report what would be reclaimed without deleting anything
    #[arg(long)]
    pub dry_run: bool,
}

/// Entry point for `stella memory compact`.
///
/// The engine ([`ContextStore::compact`]) owns the predicates and the
/// one-transaction guarantee; this function owns the two things that are
/// properly the CLI's:
///
/// 1. **Not creating the database.** `ContextStore::open` migrates and writes a
///    fingerprint row, so opening a workspace that has never indexed anything
///    would create `context.db` purely to report that it is empty. Same
///    courtesy `stella stats prune` and `stella memory list` already keep.
/// 2. **Saying what happened, including "nothing".** A compaction that reclaims
///    zero rows is a fact about the store and prints as one; a *policy* that
///    could never reclaim anything is a mistake in the invocation and is a hard
///    error from the engine. The two must not read alike.
pub fn run_memory_compact(args: &CompactArgs) -> Result<(), String> {
    let CompactArgs {
        stale_fingerprints,
        vacuum,
        dry_run,
    } = *args;

    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    let Some(context_db) =
        stella_store::existing_workspace_private_sqlite_path(&workspace_root, "context.db")
            .map_err(|e| format!("cannot resolve context store: {e}"))?
    else {
        println!("no context store in this workspace — nothing to compact");
        return Ok(());
    };
    let store =
        ContextStore::open(&context_db).map_err(|e| format!("cannot open context store: {e}"))?;

    let policy = ContextCompactPolicy {
        orphans: true,
        stale_fingerprints,
        vacuum,
        dry_run,
    };
    let report = store
        .compact(&policy)
        .map_err(|e| format!("compaction failed: {e}"))?;

    print_compact_report(&report, dry_run, &context_db);
    if !dry_run
        && let Some(mark) = store
            .compaction_watermark()
            .map_err(|e| format!("compacted, but cannot read the watermark back: {e}"))?
    {
        println!(
            "{}",
            format!(
                "compaction watermark: {} (lifetime {} row(s) reclaimed)",
                mark.compacted_at,
                mark.orphaned_embeddings
                    + mark.orphaned_node_domains
                    + mark.orphaned_edge_domains
                    + mark.stale_fingerprint_embeddings
            )
            .dimmed()
        );
    }
    // Last, so it is the line left on screen: the whole point of the verb is
    // that it is safe, and a reader who stops after the counts should still know
    // what was NOT touched.
    println!(
        "{}",
        "memories, episodes, facts and forget tombstones are untouched — compaction \
         only reclaims the derived index (L-C3)"
            .dimmed()
    );
    Ok(())
}

fn print_compact_report(report: &ContextCompactReport, dry_run: bool, db_path: &std::path::Path) {
    let verb = if dry_run {
        "would reclaim"
    } else {
        "reclaimed"
    };
    if report.orphaned_embeddings > 0 {
        println!(
            "{verb} {} orphaned embedding(s) — vectors no memory, node or episode \
             points at any more",
            report.orphaned_embeddings
        );
    }
    let junctions = report.orphaned_node_domains + report.orphaned_edge_domains;
    if junctions > 0 {
        println!(
            "{verb} {junctions} orphaned domain tag(s) ({} node, {} edge)",
            report.orphaned_node_domains, report.orphaned_edge_domains
        );
    }
    if report.stale_fingerprint_embeddings > 0 {
        println!(
            "{verb} {} embedding(s) under a non-active embedder",
            report.stale_fingerprint_embeddings
        );
        if report.stale_vectors_of_live_nodes > 0 {
            // Stated as a conditional future cost, never as work this
            // compaction created: under the ACTIVE fingerprint it is zero,
            // because warming never saw those vectors in the first place.
            println!(
                "{}",
                format!(
                    "{} live node(s) would need re-embedding if this workspace ever \
                     switches back to that embedder",
                    report.stale_vectors_of_live_nodes
                )
                .yellow()
            );
        }
    }
    if report.vacuumed {
        println!("vacuumed {}", db_path.display());
    }
    if report.total_rows() == 0 {
        println!("nothing to compact — the context index has no reclaimable rows");
    }
}
