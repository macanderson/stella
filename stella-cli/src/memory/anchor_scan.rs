//! The staleness scan: which memory anchors stopped holding, and recording it.
//!
//! This is the *policy* half of #775. `stella-context` owns the semantics —
//! ending world validity is not supersession, and never deletes — while the
//! question "does this file still exist" is about a filesystem, which the store
//! deliberately cannot answer. Keeping the two apart is what lets the store be
//! tested without a real tree, and lets this decide staleness without knowing
//! any SQL.
//!
//! Split out of `memory_cmd` because it is the memory subsystem's logic that
//! the command merely invokes, and because `memory_cmd` sits on the repo's
//! 1500-line file ceiling.

use colored::Colorize;
use stella_context::{Clock, ContextStore, SystemClock};

/// One anchor the scan found pointing at a file that is no longer there.
struct StaleAnchor {
    edge_id: i64,
    path: String,
    memory: String,
}

/// Walk the open `observed_in` anchors and report — or end — the ones whose
/// file has left the tree.
///
/// This is the *policy* half of #775. The store owns the semantics (ending
/// world validity is not supersession, and never deletes); deciding which
/// anchors are stale is a question about a filesystem, which the store
/// deliberately cannot answer.
///
/// Read-only unless `end_stale`. Ending an anchor is a write to history, and
/// the default for a command named `validate` must be to tell you what it
/// found, not to change the store because you ran an inspection.
pub(crate) fn scan_stale_anchors(
    workspace_root: &std::path::Path,
    end_stale: bool,
) -> Result<(), String> {
    let Some(context_db) =
        stella_store::existing_workspace_private_sqlite_path(workspace_root, "context.db")
            .map_err(|e| format!("cannot resolve context store: {e}"))?
    else {
        return Ok(());
    };
    let context =
        ContextStore::open(&context_db).map_err(|e| format!("cannot open context store: {e}"))?;
    let anchors = context
        .open_anchors()
        .map_err(|e| format!("cannot read anchors: {e}"))?;
    if anchors.is_empty() {
        return Ok(());
    }

    let stale: Vec<StaleAnchor> = anchors
        .into_iter()
        .filter(|a| !workspace_root.join(&a.path).exists())
        .map(|a| StaleAnchor {
            edge_id: a.edge_id,
            path: a.path,
            memory: a.memory,
        })
        .collect();

    if stale.is_empty() {
        println!(
            "\n  {} every memory anchor still points at a file",
            "✓".green()
        );
        return Ok(());
    }

    println!(
        "\n  {} {} anchor(s) point at files that are gone:",
        "⚠".yellow(),
        stale.len()
    );
    for a in &stale {
        println!(
            "    {} — {}",
            a.path.yellow(),
            a.memory.chars().take(60).collect::<String>().dimmed()
        );
    }

    if !end_stale {
        println!(
            "\n  run `stella memory validate --end-stale` to record that these \
             stopped holding.\n  The memories are NOT retracted — they were true, \
             and then the world changed."
        );
        return Ok(());
    }

    // One timestamp for the whole scan, so every anchor this pass ends carries
    // the same world-time boundary. Stamping each one as it is written would
    // scatter a single event across a range of instants for no reason.
    // The same clock the store writes `recorded_at` with, so the two axes are
    // rendered identically — these strings are compared lexicographically by
    // every bi-temporal range scan, and a second format would sort wrong.
    let now = SystemClock.now_rfc3339();
    let mut ended = 0usize;
    for a in &stale {
        match context.end_anchor_validity(a.edge_id, &now) {
            // `false` means a previous scan already ended it. Not an error, and
            // not counted — the date the file disappeared stays the first one
            // recorded rather than being moved forward by every later run.
            Ok(true) => ended += 1,
            Ok(false) => {}
            Err(e) => return Err(format!("cannot end anchor {}: {e}", a.edge_id)),
        }
    }
    println!(
        "\n  {} ended world validity for {ended} anchor(s) at {now}.\n  \
         Belief is untouched: `as_of` queries still report them, because they \
         were never wrong.",
        "✓".green()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// The anchor scan resolves stored `file://` uris back onto the workspace.
    ///
    /// This is the spelling risk made explicit: anchors are written
    /// workspace-relative (matching `record_taxonomy`), while the code-graph
    /// plane mints `file://` uris with an *absolute* path. If the scan joined
    /// an absolute uri onto the workspace root it would resolve nothing and
    /// report every anchor stale — deleting the whole graph on the first run.
    #[test]
    fn the_anchor_scan_ends_only_anchors_whose_file_is_gone() {
        let root = tempdir().unwrap();
        let context_db =
            stella_store::workspace_private_sqlite_path(root.path(), "context.db").unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/live.rs"), "pub fn x() {}").unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let context = stella_context::ContextStore::open(&context_db).unwrap();
            use stella_context::{ContextDelta, MemoryInput};
            let delta = ContextDelta {
                memories: vec![
                    MemoryInput::reflection("about a live file", Vec::<String>::new())
                        .with_anchors(["src/live.rs"]),
                    MemoryInput::reflection("about a file since deleted", Vec::<String>::new())
                        .with_anchors(["src/gone.rs"]),
                ],
                ..Default::default()
            };
            context.upsert(delta).await.unwrap();
            assert_eq!(context.open_anchors().unwrap().len(), 2);
        });

        // Report-only: the store must be untouched.
        scan_stale_anchors(root.path(), false).unwrap();
        let context = stella_context::ContextStore::open(&context_db).unwrap();
        assert_eq!(
            context.open_anchors().unwrap().len(),
            2,
            "a scan without --end-stale changes nothing"
        );
        drop(context);

        scan_stale_anchors(root.path(), true).unwrap();
        let context = stella_context::ContextStore::open(&context_db).unwrap();
        let open = context.open_anchors().unwrap();
        assert_eq!(open.len(), 1, "only the deleted file's anchor ends");
        assert_eq!(open[0].path, "src/live.rs");

        // The ended anchor is still believed — it was not wrong.
        assert!(
            context
                .facts_as_of(None)
                .unwrap()
                .iter()
                .filter(|f| f.predicate == stella_context::ANCHOR_REL)
                .count()
                == 2,
            "both anchors remain believed; only one stopped holding"
        );
    }
}
