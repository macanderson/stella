//! `stella memory restore <id>` — lifting a suppression, and saying which of
//! three different things actually happened.
//!
//! A restore reads two planes that can disagree: the tombstone in `store.db`
//! (canonical, surface-aware, carries the reason) and the `superseded_at`
//! suppression in `context.db` (what recall actually filters on). Neither
//! lifting anything used to print one message — "was not forgotten" — for two
//! unrelated situations: a memory that is here and simply was not forgotten,
//! and an id that names nothing at all, usually a typo. The second is the one
//! a person needs told, so it is told:
//! [`ContextStore::node_exists`](stella_context::ContextStore::node_exists)
//! sees superseded rows, which every other lookup hides, and is the only way
//! to ask the question at all.

use std::path::Path;

use colored::Colorize;
use stella_store::{ContextSurface, Store};

use super::open_context;

/// What a restore did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestoreOutcome {
    /// A suppression was lifted — in `store.db`, in the context plane, or both.
    Restored,
    /// Nothing was suppressed: the id is not tombstoned, and the context plane
    /// either holds it live or is not present to be asked.
    NotSuppressed,
    /// No node carries this public id in the context plane, in any state.
    NoSuchMemory,
}

/// What the context plane knew about the id, after its own restore ran.
/// Absent entirely when the workspace has no `context.db` — a plane that
/// cannot be asked must never be read as an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaneState {
    /// A suppression was lifted here.
    Lifted,
    /// The node is here and was not suppressed.
    Present,
    /// Nothing here carries this id, superseded or not.
    Absent,
}

/// The whole decision, over owned data: which of the three outcomes the two
/// planes' answers add up to.
fn outcome(tombstone_lifted: bool, plane: Option<PlaneState>) -> RestoreOutcome {
    if tombstone_lifted || plane == Some(PlaneState::Lifted) {
        RestoreOutcome::Restored
    } else if plane == Some(PlaneState::Absent) {
        RestoreOutcome::NoSuchMemory
    } else {
        RestoreOutcome::NotSuppressed
    }
}

/// `restore` against an explicit workspace root — the seam the tests drive,
/// mirroring `promote_in`/`list_rows`. Mutates no process state.
pub(crate) fn restore_in(workspace_root: &Path, id: &str) -> Result<RestoreOutcome, String> {
    let store = Store::open(workspace_root).map_err(|e| format!("cannot open store: {e}"))?;
    let lifted = store
        .restore(ContextSurface::Memory, id)
        .map_err(|e| format!("cannot lift tombstone: {e}"))?;
    // The exact inverse of what `forget` projected. Run unconditionally rather
    // than only when `lifted`: the two writes can disagree if a forget failed
    // halfway, and a restore that refuses to fix that is a memory no command
    // can bring back.
    let plane = match open_context(workspace_root)? {
        Some(context) => {
            let lifted_here = context
                .restore_node(id)
                .map_err(|e| format!("cannot lift `{id}` in the context plane: {e}"))?;
            Some(if lifted_here {
                PlaneState::Lifted
            } else if context
                .node_exists(id)
                .map_err(|e| format!("cannot look up `{id}` in the context plane: {e}"))?
            {
                PlaneState::Present
            } else {
                PlaneState::Absent
            })
        }
        None => None,
    };
    Ok(outcome(lifted, plane))
}

/// Entry point for `stella memory restore <id>`.
pub fn run_memory_restore(id: &str) -> Result<(), String> {
    let workspace_root =
        std::env::current_dir().map_err(|e| format!("cannot determine workspace root: {e}"))?;
    match restore_in(&workspace_root, id)? {
        RestoreOutcome::Restored => println!("  {} restored {id}", "✓".green()),
        RestoreOutcome::NotSuppressed => println!(
            "  {} `{id}` was not forgotten — nothing to restore",
            "·".dimmed()
        ),
        RestoreOutcome::NoSuchMemory => println!(
            "  {} no memory `{id}` in this workspace — `stella memory list` shows what is here",
            "·".dimmed()
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plane_that_cannot_be_asked_is_never_read_as_an_answer() {
        // No context.db: "not forgotten" is all that can honestly be said,
        // and an unknown id must not be reported as one.
        assert_eq!(outcome(false, None), RestoreOutcome::NotSuppressed);
        assert_eq!(outcome(true, None), RestoreOutcome::Restored);
    }

    #[test]
    fn either_plane_lifting_is_a_restore() {
        // The disagreement case: a forget that failed halfway leaves one plane
        // suppressed and the other not, and either one is worth reporting.
        assert_eq!(
            outcome(false, Some(PlaneState::Lifted)),
            RestoreOutcome::Restored
        );
        assert_eq!(
            outcome(true, Some(PlaneState::Present)),
            RestoreOutcome::Restored
        );
    }

    #[test]
    fn a_live_memory_is_told_apart_from_an_id_that_names_nothing() {
        assert_eq!(
            outcome(false, Some(PlaneState::Present)),
            RestoreOutcome::NotSuppressed
        );
        assert_eq!(
            outcome(false, Some(PlaneState::Absent)),
            RestoreOutcome::NoSuchMemory
        );
    }

    /// The whole flow against real stores in a temp workspace: a seeded
    /// memory, an id that names nothing, and both suppression planes — the
    /// `_in` seam takes the root directly, so no cwd is mutated.
    #[tokio::test]
    async fn restore_distinguishes_unknown_suppressed_and_live_ids() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        let context_db = stella_store::workspace_private_sqlite_path(root, "context.db").unwrap();
        let context = stella_context::ContextStore::open(context_db).unwrap();
        context
            .upsert(stella_context::ContextDelta {
                memories: vec![stella_context::MemoryInput::new(
                    stella_context::MemoryKind::Note,
                    "Regenerate gen/ rather than editing it.",
                )],
                ..Default::default()
            })
            .await
            .unwrap();
        let id = context.memory_nodes().unwrap()[0].public_id.clone();
        drop(context);

        // An id that names nothing is not "was not forgotten" — it is a typo,
        // and the two were indistinguishable before `node_exists` was consulted.
        assert_eq!(
            restore_in(root, "nod_000000000000000000000000").unwrap(),
            RestoreOutcome::NoSuchMemory
        );
        // A live, never-forgotten memory: nothing to lift, but it is here.
        assert_eq!(
            restore_in(root, &id).unwrap(),
            RestoreOutcome::NotSuppressed
        );

        // Suppressed in the context plane alone (the halfway-forget shape).
        let context = stella_context::ContextStore::open(
            stella_store::workspace_private_sqlite_path(root, "context.db").unwrap(),
        )
        .unwrap();
        assert!(context.supersede_node(&id).unwrap());
        drop(context);
        assert_eq!(restore_in(root, &id).unwrap(), RestoreOutcome::Restored);

        // Tombstoned in `store.db` alone.
        let store = Store::open(root).unwrap();
        store
            .forget(ContextSurface::Memory, &id, "content", "no longer true")
            .unwrap();
        drop(store);
        assert_eq!(restore_in(root, &id).unwrap(), RestoreOutcome::Restored);
        // ...and the lift is idempotent in both planes.
        assert_eq!(
            restore_in(root, &id).unwrap(),
            RestoreOutcome::NotSuppressed
        );
    }
}
