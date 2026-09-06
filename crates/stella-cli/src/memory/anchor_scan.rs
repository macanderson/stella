//! The staleness scan: which anchors stopped holding, and recording it.
//!
//! Covers memory anchors (the files a lesson is about) and episode anchors
//! (the files a turn touched, #5338) on the same terms — the question it asks
//! is whether the file is still there, which does not depend on which kind of
//! record pointed at it.
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

use std::path::Path;
use std::time::{Duration, Instant};

use colored::Colorize;
use stella_context::{AnchorView, Clock, ContextStore, SystemClock};

/// Time cap for the auto sweep at mount. Each anchor costs one file
/// check, and a slow disk — not a long list — is what could make that
/// slow. So the cap is time, not a row count. Anything left unchecked
/// just waits for the next mount; an ended anchor drops off the list,
/// so no anchor can wait forever.
const MOUNT_SCAN_BUDGET: Duration = Duration::from_millis(200);

/// One anchor the scan found pointing at a file that is no longer there.
struct StaleAnchor {
    edge_id: i64,
    path: String,
    /// The anchoring record's text — a memory's content or an episode's
    /// summary — so the report can name what goes stale.
    source: String,
}

/// Open this workspace's context store, if it has one yet. `Ok(None)` is not
/// an error; it means there is no `context.db` yet. Both callers below treat
/// that the same as "a store with nothing stale" — a workspace with no
/// memories has no anchors to end.
fn open_context_store(workspace_root: &Path) -> Result<Option<ContextStore>, String> {
    let Some(context_db) =
        stella_store::existing_workspace_private_sqlite_path(workspace_root, "context.db")
            .map_err(|e| format!("cannot resolve context store: {e}"))?
    else {
        return Ok(None);
    };
    ContextStore::open(&context_db)
        .map(Some)
        .map_err(|e| format!("cannot open context store: {e}"))
}

/// How many of `anchors` a pass at `budget` could reach, one file check
/// each, oldest first — the same order [`ContextStore::open_anchors`]
/// returns and [`scan_stale_anchors_at_mount`] walks. Read-only, so
/// `stella memory validate` can report real throughput without touching
/// the store.
struct ScanCapacity {
    /// How many anchors were open when the pass started.
    total: usize,
    /// How many of them the pass reached before its deadline.
    examined: usize,
}

impl ScanCapacity {
    /// True when the backlog beat the budget. New anchors sit last in the
    /// walk, so a workspace adding them fast enough never reaches them.
    fn is_falling_behind(&self) -> bool {
        self.examined < self.total
    }
}

fn mount_scan_capacity(
    anchors: &[AnchorView],
    workspace_root: &Path,
    budget: Duration,
) -> ScanCapacity {
    let deadline = Instant::now() + budget;
    let mut examined = 0usize;
    for a in anchors {
        if Instant::now() >= deadline {
            break;
        }
        let _ = workspace_root.join(&a.path).exists();
        examined += 1;
    }
    ScanCapacity {
        total: anchors.len(),
        examined,
    }
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
pub(crate) fn scan_stale_anchors(workspace_root: &Path, end_stale: bool) -> Result<(), String> {
    let Some(context) = open_context_store(workspace_root)? else {
        return Ok(());
    };
    let anchors = context
        .open_anchors()
        .map_err(|e| format!("cannot read anchors: {e}"))?;
    if anchors.is_empty() {
        return Ok(());
    }

    let capacity = mount_scan_capacity(&anchors, workspace_root, MOUNT_SCAN_BUDGET);
    if capacity.is_falling_behind() {
        println!(
            "\n  {} the mount sweep can check only {}/{} open anchor(s) in one pass \
             at the current {}ms budget — the newest may never be reached; run \
             `stella memory validate --end-stale` to catch up.",
            "⚠".yellow(),
            capacity.examined,
            capacity.total,
            MOUNT_SCAN_BUDGET.as_millis()
        );
    }

    let stale: Vec<StaleAnchor> = anchors
        .into_iter()
        .filter(|a| !workspace_root.join(&a.path).exists())
        .map(|a| StaleAnchor {
            edge_id: a.edge_id,
            path: a.path,
            source: a.source,
        })
        .collect();

    if stale.is_empty() {
        println!("\n  {} every anchor still points at a file", "✓".green());
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
            a.source.chars().take(60).collect::<String>().dimmed()
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

/// Runs [`scan_stale_anchors`] on its own, at every session mount, right
/// next to warm. It uses the store warm already opened. So a deleted
/// file's anchor stops feeding graph links, with no need to run
/// `stella memory validate --end-stale` by hand.
///
/// Stays quiet on failure and on success — this is upkeep the session
/// does for itself, not a report. `stella memory validate` is the
/// command for that.
pub(crate) fn scan_stale_anchors_at_mount(context: &ContextStore, workspace_root: &Path) {
    let deadline = Instant::now() + MOUNT_SCAN_BUDGET;
    let _ = end_stale_anchors_within_deadline(context, workspace_root, deadline);
}

/// The bound at work: ends world validity for each open anchor whose
/// file is gone, checked before `deadline`. Split out so a test can
/// pass a deadline already past, and prove the cap really stops the
/// walk.
fn end_stale_anchors_within_deadline(
    context: &ContextStore,
    workspace_root: &Path,
    deadline: Instant,
) -> Result<usize, String> {
    let anchors = context
        .open_anchors()
        .map_err(|e| format!("cannot read anchors: {e}"))?;
    if anchors.is_empty() {
        return Ok(0);
    }

    let stale: Vec<StaleAnchor> = anchors
        .into_iter()
        .take_while(|_| Instant::now() < deadline)
        .filter(|a| !workspace_root.join(&a.path).exists())
        .map(|a| StaleAnchor {
            edge_id: a.edge_id,
            path: a.path,
            source: a.source,
        })
        .collect();
    if stale.is_empty() {
        return Ok(0);
    }

    let now = SystemClock.now_rfc3339();
    let mut ended = 0usize;
    for a in &stale {
        match context.end_anchor_validity(a.edge_id, &now) {
            Ok(true) => ended += 1,
            Ok(false) => {}
            Err(e) => return Err(format!("cannot end anchor {}: {e}", a.edge_id)),
        }
    }
    Ok(ended)
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

    /// The mount sweep alone — no `--end-stale` command — ends world
    /// validity for an anchor whose file is gone, using the real time
    /// cap.
    #[test]
    fn mount_scan_ends_a_stale_anchor_under_the_production_budget() {
        let root = tempdir().unwrap();
        let context_db =
            stella_store::workspace_private_sqlite_path(root.path(), "context.db").unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let context = stella_context::ContextStore::open(&context_db).unwrap();
        rt.block_on(async {
            use stella_context::{ContextDelta, MemoryInput};
            context
                .upsert(ContextDelta {
                    memories: vec![
                        MemoryInput::reflection("about a file since deleted", Vec::<String>::new())
                            .with_anchors(["src/gone.rs"]),
                    ],
                    ..Default::default()
                })
                .await
                .unwrap();
        });
        assert_eq!(context.open_anchors().unwrap().len(), 1);

        scan_stale_anchors_at_mount(&context, root.path());

        assert_eq!(
            context.open_anchors().unwrap().len(),
            0,
            "the mount sweep ends world validity for the gone file's anchor, unasked"
        );
    }

    /// The time cap is real, not just assumed. A deadline already past
    /// must stop the walk before it checks the one anchor there is. It
    /// stays open for the next pass.
    #[test]
    fn mount_scan_stops_at_an_already_expired_deadline() {
        let root = tempdir().unwrap();
        let context_db =
            stella_store::workspace_private_sqlite_path(root.path(), "context.db").unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let context = stella_context::ContextStore::open(&context_db).unwrap();
        rt.block_on(async {
            use stella_context::{ContextDelta, MemoryInput};
            context
                .upsert(ContextDelta {
                    memories: vec![
                        MemoryInput::reflection("about a file since deleted", Vec::<String>::new())
                            .with_anchors(["src/gone.rs"]),
                    ],
                    ..Default::default()
                })
                .await
                .unwrap();
        });

        // Sleep past a saved `Instant` so the deadline is already past.
        // Subtracting from `Instant::now()` could panic this early on.
        let deadline = Instant::now();
        std::thread::sleep(Duration::from_millis(5));

        let ended = end_stale_anchors_within_deadline(&context, root.path(), deadline).unwrap();
        assert_eq!(ended, 0, "an expired deadline must not process any anchor");
        assert_eq!(
            context.open_anchors().unwrap().len(),
            1,
            "the anchor is untouched when the budget is already spent"
        );
    }

    /// A zero budget models a backlog too big for one pass, no slow disk
    /// needed. `scan_stale_anchors_at_mount` stays quiet either way, so
    /// this is the only place the gap shows.
    #[test]
    fn mount_scan_capacity_flags_a_backlog_the_budget_cannot_clear() {
        let root = tempdir().unwrap();
        let context_db =
            stella_store::workspace_private_sqlite_path(root.path(), "context.db").unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let context = stella_context::ContextStore::open(&context_db).unwrap();
        rt.block_on(async {
            use stella_context::{ContextDelta, MemoryInput};
            context
                .upsert(ContextDelta {
                    memories: vec![
                        MemoryInput::reflection("about file one", Vec::<String>::new())
                            .with_anchors(["src/one.rs"]),
                        MemoryInput::reflection("about file two", Vec::<String>::new())
                            .with_anchors(["src/two.rs"]),
                        MemoryInput::reflection("about file three", Vec::<String>::new())
                            .with_anchors(["src/three.rs"]),
                    ],
                    ..Default::default()
                })
                .await
                .unwrap();
        });
        let anchors = context.open_anchors().unwrap();
        assert_eq!(anchors.len(), 3);

        let capacity = mount_scan_capacity(&anchors, root.path(), Duration::ZERO);
        assert_eq!(capacity.total, 3);
        assert_eq!(
            capacity.examined, 0,
            "a zero budget reaches its deadline before the first check"
        );
        assert!(
            capacity.is_falling_behind(),
            "3 open anchors against a pass that can examine none is exactly \
             the falling-behind backlog this signal exists to catch"
        );
    }

    /// The other side: a budget that covers the backlog is not falling
    /// behind, so the report line stays quiet on an ordinary workspace.
    #[test]
    fn mount_scan_capacity_is_current_when_the_budget_covers_the_backlog() {
        let root = tempdir().unwrap();
        let context_db =
            stella_store::workspace_private_sqlite_path(root.path(), "context.db").unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let context = stella_context::ContextStore::open(&context_db).unwrap();
        rt.block_on(async {
            use stella_context::{ContextDelta, MemoryInput};
            context
                .upsert(ContextDelta {
                    memories: vec![
                        MemoryInput::reflection("about file one", Vec::<String>::new())
                            .with_anchors(["src/one.rs"]),
                    ],
                    ..Default::default()
                })
                .await
                .unwrap();
        });
        let anchors = context.open_anchors().unwrap();

        let capacity = mount_scan_capacity(&anchors, root.path(), Duration::from_secs(1));
        assert_eq!(capacity.examined, capacity.total);
        assert!(!capacity.is_falling_behind());
    }
}
