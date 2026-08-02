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
    /// belongs to. `Arc` so `sink()` hands out cheap shares rather than
    /// re-opening the record per engine.
    bound: Arc<RwLock<Option<Bound>>>,
}

/// What a bound session writes through: its durable record, and the registry
/// holding the staleness map that rides along with each checkpoint.
#[derive(Clone)]
struct Bound {
    journal: Arc<WorkJournal>,
    registry: Arc<stella_tools::ToolRegistry>,
}

/// `ToolRegistry` is not `Debug` (it holds trait objects), and
/// [`CheckpointSink`] requires it of every implementor. The record is the part
/// worth printing anyway — it names the session and the store — so the registry
/// is elided rather than the whole handle losing its `Debug`.
impl std::fmt::Debug for Bound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bound")
            .field("journal", &self.journal)
            .finish_non_exhaustive()
    }
}

impl SessionDurability {
    /// Point this handle at the durable record of the session that is now
    /// running, and at the registry whose staleness map belongs with it.
    pub fn bind(&self, journal: WorkJournal, registry: Arc<stella_tools::ToolRegistry>) {
        let mut slot = self.bound.write().unwrap_or_else(|p| p.into_inner());
        *slot = Some(Bound {
            journal: Arc::new(journal),
            registry,
        });
    }

    /// The sink to hand [`stella_core::EngineConfig::checkpoint_sink`], or
    /// `None` while this handle is unbound.
    ///
    /// The record is read out *here*, once per engine, rather than on every
    /// `persist` — the lock stays off the per-step hot path, and a session
    /// switch is picked up because the next turn builds a new engine.
    pub fn sink(&self) -> Option<Arc<dyn CheckpointSink>> {
        let bound = self
            .bound
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()?;
        Some(Arc::new(JournalCheckpointSink { bound }))
    }
}

/// A [`CheckpointSink`] over the work journal's reserved blobs.
#[derive(Debug)]
struct JournalCheckpointSink {
    bound: Bound,
}

impl CheckpointSink for JournalCheckpointSink {
    /// One commit on this session's ref, carrying the resume point and the
    /// staleness map together. Dearer than an atomic file write and still cheap
    /// against what a step costs — see [`WorkJournal::record_checkpoint`].
    ///
    /// The staleness map is snapshotted *here*, at the step boundary, rather
    /// than when a file changes: it is updated by reads too, and saving it only
    /// on mutation would drop every read since the last write. That is the loss
    /// that matters, because a resumed session's transcript still says the agent
    /// read those files.
    ///
    /// The error is dropped, not logged: this is called on every step of every
    /// turn, so a failing sink would emit one line per step, and the trait's
    /// contract is that a checkpoint which cannot be written leaves the turn
    /// exactly as recoverable as it was before the sink existed.
    fn persist(&self, json: &str) {
        let observed = self.bound.registry.observed_snapshot();
        let _ = self
            .bound
            .journal
            .record_checkpoint(json, observed.as_deref());
    }

    /// Idempotent by [`WorkJournal::clear_checkpoint`]'s own contract, and free
    /// when there is nothing to retract — which is what lets every terminal
    /// path discard unconditionally.
    ///
    /// The staleness map is deliberately NOT retracted with the checkpoint. It
    /// describes what this *session* has seen, and the session's next turn is
    /// exactly as entitled to the no-clobber guarantee as the one that just
    /// ended.
    fn discard(&self) {
        let _ = self.bound.journal.clear_checkpoint();
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
/// Resuming a session also restores its staleness map, which is what carries
/// the no-clobber guarantee across the interruption.
///
/// Returns a message to show the operator when the record could not be opened,
/// and `None` on success. Best-effort throughout, by the same reasoning as the
/// sink contract: a session with no durable record is exactly as recoverable as
/// every session was before this existed, so refusing to start over it would
/// trade a working session for none.
pub fn bind_session(
    durability: &SessionDurability,
    registry: &Arc<stella_tools::ToolRegistry>,
    workspace_root: &Path,
    session_id: &str,
) -> Option<String> {
    match WorkJournal::open(workspace_root, session_id) {
        Ok(journal) => {
            // Before anything else can touch a file: a resumed session that
            // wrote before restoring would be unguarded for exactly the writes
            // its restored transcript most encourages it to make.
            if let Some(observed) = journal.observed() {
                registry.restore_observed(&observed);
            }
            registry.attach_work_journal(journal.clone(), agent_label(workspace_root));
            durability.bind(journal, registry.clone());
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

    /// A registry over `workspace`, for the staleness map the sink snapshots.
    fn registry(workspace: &Path) -> Arc<stella_tools::ToolRegistry> {
        Arc::new(stella_tools::ToolRegistry::new(
            workspace.to_path_buf(),
            stella_tools::RegistryOptions::default(),
        ))
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
        durability.bind(record.clone(), registry(ws.path()));

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
        durability.bind(record.clone(), registry(ws.path()));
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
        durability.bind(record.clone(), registry(ws.path()));
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

        durability.bind(first.clone(), registry(ws.path()));
        durability
            .sink()
            .expect("bound")
            .persist("{\"first\":true}");

        // The in-deck session switch: the next engine's sink writes to the
        // session that is now running, and the session the user left keeps its
        // resume point exactly as it stood.
        durability.bind(second.clone(), registry(ws.path()));
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
        durability.bind(record.clone(), registry(ws.path()));
        durability.sink().expect("bound").persist("{\"step\":7}");

        let tip = record.session_tip().expect("recorded");
        record.mark_turn(1, &tip).unwrap();
        assert_eq!(record.read_at_turn(1, "work.txt").unwrap(), "half-done\n");
        assert_eq!(record.checkpoint().as_deref(), Some("{\"step\":7}"));
    }

    #[tokio::test]
    async fn the_no_clobber_guard_survives_a_crash() {
        // The whole point of persisting the staleness map. Without it a resumed
        // session is *less* safe than a fresh one is honest: its restored
        // transcript still says the agent read the file, so the model acts on
        // content it believes it knows, while a forgetful guard waves the
        // overwrite through.
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("shared.txt"), "original\n").unwrap();

        // Session one reads the file, then checkpoints — which is where the
        // map it built gets written down.
        let first_registry = registry(ws.path());
        let out = first_registry
            .execute(
                "read_file",
                &serde_json::json!({ "path": "shared.txt", "reason": "planning" }),
            )
            .await;
        assert!(!out.is_error(), "{out:?}");
        let record = journal(store.path(), ws.path(), "ses-crash");
        let durability = SessionDurability::default();
        durability.bind(record.clone(), first_registry);
        durability.sink().expect("bound").persist("{\"step\":1}");

        // …and dies. Meanwhile something else edits the file.
        std::fs::write(ws.path().join("shared.txt"), "somebody else's work\n").unwrap();

        // Session two resumes: same durable record, a brand-new registry that
        // has never seen a thing.
        let resumed = registry(ws.path());
        resumed.restore_observed(&record.observed().expect("the map was persisted"));

        let out = resumed
            .execute(
                "write_file",
                &serde_json::json!({
                    "path": "shared.txt",
                    "content": "what session one intended\n",
                    "reason": "acting on what I read before the crash",
                }),
            )
            .await;

        assert!(
            out.is_error(),
            "the resumed session must be told the file moved, not silently overwrite it: {out:?}"
        );
        assert_eq!(
            std::fs::read_to_string(ws.path().join("shared.txt")).unwrap(),
            "somebody else's work\n",
            "and the other party's work is still there"
        );
    }
}
