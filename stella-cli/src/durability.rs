//! Where this session's turns write their resume point.
//!
//! [`stella_core::step::CheckpointSink`] says *when* a checkpoint is written —
//! at the one step boundary where the transcript is guaranteed well-paired —
//! and deliberately says nothing about where. This module is the CLI's answer
//! to *where*, and it has to live in a crate that depends on both
//! `stella-core` (which declares the trait) and `stella-store` (which owns the
//! durable record). `stella-store` cannot implement it: it depends only on
//! `stella-protocol` and has never heard of the engine.
//!
//! # One store, not two
//!
//! The resume point goes into the work journal's
//! [`stella_store::work_journal::CHECKPOINT_BLOB`], the same commit graph as
//! the file changes it is a resume point *for* — not into a transient file
//! beside it. Two stores would need a recovery path that decides which to
//! believe when they disagree, and they would disagree, because nothing can
//! update both atomically. A crash between two such writes is not a rare case
//! here; it is the exact case the whole feature exists to survive.
//!
//! # Why a shared cell rather than a plain field
//!
//! A session's durable identity does not exist when its
//! [`crate::config::Config`] is built. The deck receives `&Config` and only
//! then resolves — or mints — the [`stella_store::SessionRecord`] the record is
//! keyed on, and it re-keys that on every in-deck session switch. A plain field
//! could be filled in by neither of those moments.
//!
//! So `Config` carries a [`SessionDurability`] handle instead: cheap to clone,
//! empty until a driver binds it, and read afresh by every engine the session
//! builds. `agent::engine_config_for` runs per turn, so a session switch is
//! picked up by the next turn with nothing to re-thread.
//!
//! # Why `sink()` may answer `None`
//!
//! [`stella_core::Engine::persist_checkpoint`] serializes the whole transcript
//! *before* it hands anything to the sink. An always-attached sink that happens
//! to have nowhere to write would therefore pay full JSON encoding on every
//! step of every turn to throw the bytes away. An unbound handle yields `None`
//! and the engine skips the encode entirely.

use std::path::Path;
use std::sync::{Arc, RwLock};

use stella_core::step::CheckpointSink;
use stella_store::work_journal::WorkJournal;

/// A handle on this session's durable record.
///
/// Cloned into [`crate::config::Config`] and shared by every engine the session
/// builds. Empty until a driver calls [`Self::bind`]; binding again re-points
/// it, which is what an in-deck session switch needs.
#[derive(Clone, Debug, Default)]
pub struct SessionDurability {
    /// `RwLock` rather than `OnceLock` because a deck session can be switched,
    /// and the *next* turn's checkpoint must land in the session it actually
    /// belongs to. `Arc<WorkJournal>` so `sink()` hands out a cheap share
    /// rather than re-opening the record per engine.
    journal: Arc<RwLock<Option<Arc<WorkJournal>>>>,
}

impl SessionDurability {
    /// Point this handle at `journal` — the durable record of the session that
    /// is now running.
    pub fn bind(&self, journal: WorkJournal) {
        let mut slot = self.journal.write().unwrap_or_else(|p| p.into_inner());
        *slot = Some(Arc::new(journal));
    }

    /// This session's durable record, if one is bound.
    pub fn journal(&self) -> Option<Arc<WorkJournal>> {
        self.journal
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The sink to hand [`stella_core::EngineConfig::checkpoint_sink`], or
    /// `None` while this handle is unbound.
    ///
    /// The record is read out *here*, once per engine, rather than on every
    /// `persist` — the lock stays off the per-step hot path, and a session
    /// switch is picked up because the next turn builds a new engine.
    pub fn sink(&self) -> Option<Arc<dyn CheckpointSink>> {
        let journal = self.journal()?;
        Some(Arc::new(JournalCheckpointSink { journal }))
    }
}

/// A [`CheckpointSink`] over the work journal's checkpoint blob.
#[derive(Debug)]
struct JournalCheckpointSink {
    journal: Arc<WorkJournal>,
}

impl CheckpointSink for JournalCheckpointSink {
    /// One commit on this session's ref. Dearer than an atomic file write and
    /// still cheap against what a step costs — see
    /// [`WorkJournal::record_checkpoint`].
    ///
    /// The error is dropped, not logged: this is called on every step of every
    /// turn, so a failing sink would emit one line per step, and the trait's
    /// contract is that a checkpoint which cannot be written leaves the turn
    /// exactly as recoverable as it was before the sink existed.
    fn persist(&self, json: &str) {
        let _ = self.journal.record_checkpoint(json);
    }

    /// Idempotent by [`WorkJournal::clear_checkpoint`]'s own contract, and free
    /// when there is nothing to retract — which is what lets every terminal
    /// path discard unconditionally.
    fn discard(&self) {
        let _ = self.journal.clear_checkpoint();
    }
}

/// The label a session's durable commits carry, derived from the workspace.
///
/// Not the session id: the id answers *which run*, and the record is already
/// keyed on it. This answers *whose work*, and a human reading `git log` on the
/// durable record wants the workspace name.
pub fn agent_label(workspace_root: &Path) -> String {
    workspace_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| workspace_root.display().to_string())
}

/// Bind both halves of this session's durability: where its turns write a
/// resume point, and where its file mutations commit.
///
/// One call, and one opened record shared by both, because the halves answer
/// one question — *which session owns the work happening now* — and a driver
/// that re-keyed only one of them would checkpoint into the session it just
/// left, or commit there. The deck calls this at startup and again on every
/// in-deck session switch; the one-shot drivers call it once.
///
/// Returns a message to show the operator when the record could not be opened,
/// and `None` on success. Best-effort throughout, by the same reasoning as the
/// sink contract: a session with no durable record is exactly as recoverable as
/// every session was before this existed, so refusing to start over it would
/// trade a working session for none.
pub fn bind_session(
    durability: &SessionDurability,
    registry: &stella_tools::ToolRegistry,
    workspace_root: &Path,
    session_id: &str,
) -> Option<String> {
    match WorkJournal::open(workspace_root, session_id) {
        Ok(journal) => {
            registry.attach_work_journal(journal.clone(), agent_label(workspace_root));
            durability.bind(journal);
            None
        }
        Err(e) => Some(format!(
            "durable work record unavailable ({e}) — this session's turns and file changes will \
             not be recoverable from stella's own history"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record rooted in two temp dirs — never [`WorkJournal::open`], which
    /// reads `STELLA_HOME` and would make these tests fight their siblings over
    /// one process-global.
    fn journal(store: &Path, workspace: &Path, session: &str) -> WorkJournal {
        WorkJournal::open_in(store, workspace, session).unwrap()
    }

    #[test]
    fn an_unbound_handle_offers_no_sink() {
        assert!(SessionDurability::default().sink().is_none());
    }

    #[test]
    fn a_bound_handle_round_trips_a_checkpoint() {
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let record = journal(store.path(), ws.path(), "ses-round-trip");
        let durability = SessionDurability::default();
        durability.bind(record.clone());

        let sink = durability.sink().expect("bound");
        sink.persist("{\"version\":1}");
        assert_eq!(record.checkpoint().as_deref(), Some("{\"version\":1}"));

        sink.discard();
        assert!(
            record.checkpoint().is_none(),
            "a turn that ended leaves no resume point behind"
        );
    }

    #[test]
    fn a_later_checkpoint_supersedes_the_earlier_one() {
        // The sink contract: `persist` is a complete snapshot, never a delta,
        // and replaces any earlier one. In a commit graph "replaces" has to be
        // proved — the earlier blob is still reachable from an older commit.
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let record = journal(store.path(), ws.path(), "ses-supersede");
        let durability = SessionDurability::default();
        durability.bind(record.clone());
        let sink = durability.sink().expect("bound");

        sink.persist("{\"step\":1}");
        sink.persist("{\"step\":2}");
        assert_eq!(record.checkpoint().as_deref(), Some("{\"step\":2}"));
    }

    #[test]
    fn discard_is_free_and_silent_on_a_turn_that_never_checkpointed() {
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let record = journal(store.path(), ws.path(), "ses-never");
        let durability = SessionDurability::default();
        durability.bind(record.clone());
        let sink = durability.sink().expect("bound");

        sink.discard();
        sink.discard();
        assert!(record.checkpoint().is_none());
        // And it cost nothing: a turn ending before its first step boundary is
        // the common case, so it must not write a commit.
        assert!(
            record.session_tip().is_none(),
            "discarding nothing writes nothing"
        );
    }

    #[test]
    fn rebinding_re_points_the_next_sink() {
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let first = journal(store.path(), ws.path(), "ses-first");
        let second = journal(store.path(), ws.path(), "ses-second");
        let durability = SessionDurability::default();

        durability.bind(first.clone());
        durability
            .sink()
            .expect("bound")
            .persist("{\"first\":true}");

        // The in-deck session switch: the next engine's sink writes to the
        // session that is now running, and the session the user left keeps its
        // resume point exactly as it stood.
        durability.bind(second.clone());
        durability
            .sink()
            .expect("bound")
            .persist("{\"second\":true}");

        assert_eq!(first.checkpoint().as_deref(), Some("{\"first\":true}"));
        assert_eq!(second.checkpoint().as_deref(), Some("{\"second\":true}"));
    }

    #[test]
    fn a_checkpoint_and_the_work_it_describes_share_one_commit_graph() {
        // The reason there is one store and not two: a resume point is only
        // useful alongside the file state it resumes INTO, and here both are
        // reachable from the same ref rather than needing to be reconciled.
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let record = journal(store.path(), ws.path(), "ses-together");
        std::fs::write(ws.path().join("work.txt"), "half-done\n").unwrap();
        record
            .record(&["work.txt".to_string()], &[], "the agent's write")
            .unwrap();

        let durability = SessionDurability::default();
        durability.bind(record.clone());
        durability.sink().expect("bound").persist("{\"step\":7}");

        let tip = record.session_tip().expect("recorded");
        record.mark_turn(1, &tip).unwrap();
        assert_eq!(record.read_at_turn(1, "work.txt").unwrap(), "half-done\n");
        assert_eq!(record.checkpoint().as_deref(), Some("{\"step\":7}"));
    }
}
