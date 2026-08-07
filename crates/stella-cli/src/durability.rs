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
///
/// # There is no unbind, deliberately
///
/// A finished session leaves the handle bound, and nothing clears it. That is
/// safe because the only thing a binding does is answer where the *next* turn
/// checkpoints, and after a session finishes there is no next turn: the deck
/// re-binds on a switch, and the one-shot drivers exit. A stale binding cannot
/// reach a child either — sub-agent and isolated-candidate engines carry no sink
/// at all (see `Engine::run_sub_agent`), so nothing inherits it downward.
///
/// An unbind would also cost something real: it would open a window where a
/// turn still in flight finds `sink()` empty and silently stops checkpointing,
/// which is the failure this whole module exists to prevent.
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
    /// The staged-pipeline frame every checkpoint of this session rides with,
    /// or `None` while the session is running plain engine turns.
    ///
    /// Held here, beside the journal, rather than written once when the
    /// pipeline starts: a pipeline runs *several* turns (worker, verifier,
    /// revision), and every turn that ends discards its checkpoint — which
    /// retracts the frame with it. A frame written once would therefore be
    /// gone by the second turn, and the turn most likely to be killed is not
    /// the first one.
    pipeline: Arc<RwLock<Option<String>>>,
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
            pipeline: Arc::new(RwLock::new(None)),
        });
    }

    /// Declare that this session's turns are running inside a staged pipeline,
    /// so every checkpoint from here on carries the frame describing it
    /// ([`stella_store::work_journal::PIPELINE_BLOB`]).
    ///
    /// Called once per pipeline run, before the first stage. Silently ignored
    /// on an unbound handle, like everything else here: a session with no
    /// durable record has no checkpoint for a frame to ride on either.
    pub fn set_pipeline_frame(&self, json: String) {
        let bound = self.bound.read().unwrap_or_else(|p| p.into_inner()).clone();
        if let Some(bound) = bound {
            *bound.pipeline.write().unwrap_or_else(|p| p.into_inner()) = Some(json);
        }
    }

    /// The staged-pipeline frame the interrupted turn was running inside, or
    /// `None` when it was a plain engine turn.
    ///
    /// The read side of [`Self::set_pipeline_frame`], and the reason
    /// `stella daemon resume` can tell an operator which stages a resumed run
    /// is *not* getting back (#1615) instead of quietly finishing as a bare
    /// turn.
    pub fn pipeline_frame(&self) -> Option<String> {
        self.journal()?.pipeline_frame()
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

    /// The bound session's durable record, or `None` while unbound.
    fn journal(&self) -> Option<Arc<WorkJournal>> {
        Some(
            self.bound
                .read()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()?
                .journal
                .clone(),
        )
    }

    /// The resume point an interrupted turn left behind, or `None` when this
    /// session has none — which, because every terminal path discards, means
    /// no turn of it was interrupted.
    ///
    /// This is the read side of [`Self::sink`]. It exists so the deck's resume
    /// path can prefer a step-boundary transcript over the turn-boundary one in
    /// the sidecar; see [`crate::session_persist::restore_conversation`] for
    /// which wins and why.
    pub fn checkpoint(&self) -> Option<String> {
        self.journal()?.checkpoint()
    }

    /// Mark the state at the end of a turn, so the work can later be read back
    /// by turn number rather than by commit id
    /// ([`WorkJournal::read_at_turn`]).
    ///
    /// The turn number comes from the ref namespace's own high-water mark
    /// rather than from a counter this process keeps, because a counter is
    /// exactly the thing a resume resets: a session that restarts and numbers
    /// its next turn 3 would overwrite the turn 3 that ran before the
    /// interruption, and the ref would then name work from a different turn
    /// than its name claims. The refs survive the restart, so asking them costs
    /// one `for-each-ref` and cannot drift.
    ///
    /// Best-effort and silent, like everything else here: a turn that ended is
    /// not made less ended by an unwritable marker.
    ///
    /// Marking is also the moment the turn's workspace diff is precomputed
    /// and persisted (#1870, [`crate::turn_diff`]) — the boundary ruling that
    /// keeps the observatory a pure artifact reader lives there. `store` is
    /// the workspace store when one is open (claim-mode trials run without
    /// one), `session_id` the registry id the row is keyed under, and
    /// `execution_id` the execution the turn ran as — the join the Sessions
    /// view needs, since the journal's turn ordinal has no other persisted
    /// correspondence to `executions.id`.
    pub fn mark_turn_end(
        &self,
        store: &Option<Arc<stella_store::Store>>,
        session_id: &str,
        execution_id: Option<i64>,
    ) {
        let Some(journal) = self.journal() else {
            return;
        };
        // Nothing recorded means nothing to name. A session whose turns only
        // read files never commits, and marking a turn at no commit at all is
        // not a fact worth writing down.
        let Some(tip) = journal.session_tip() else {
            return;
        };
        let turn = journal.last_marked_turn().unwrap_or(0).saturating_add(1);
        if journal.mark_turn(turn, &tip).is_err() {
            return;
        }
        if let Some(store) = store.as_deref() {
            crate::turn_diff::record_turn_diff(&journal, store, session_id, execution_id, turn);
        }
    }

    /// Compact the durable record — the end-of-session step.
    ///
    /// Every step of every turn writes a commit, a tree and a blob or two, all
    /// as loose objects. Without this a long-lived workspace accumulates them
    /// without bound, which is a leak rather than an untidiness. `git gc
    /// --auto` returns immediately when the loose-object count is under git's
    /// own threshold, so calling it at every exit costs one short-lived
    /// subprocess on the sessions that do not need it and packs the ones that
    /// do.
    ///
    /// Best-effort and never on the critical path of an exit, by the same
    /// reasoning as the sink: a session that has already finished its work must
    /// not be held up — or failed — by housekeeping.
    pub fn compact(&self) {
        if let Some(journal) = self.journal() {
            let _ = journal.compact();
        }
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
        let pipeline = self
            .bound
            .pipeline
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let _ =
            self.bound
                .journal
                .record_checkpoint(json, observed.as_deref(), pipeline.as_deref());
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
            bind_opened(durability, registry, journal, workspace_root);
            None
        }
        Err(e) => Some(format!(
            "durable work record unavailable ({e}) — this session's turns and file changes will \
             not be recoverable from stella's own history"
        )),
    }
}

/// [`bind_session`] over an already-opened record.
///
/// Split out because [`bind_session`] resolves the record through
/// [`WorkJournal::open`], which reads `STELLA_HOME` — so a test exercising the
/// binding itself would have to reach for a process-global and race every
/// sibling. This half takes the record as a parameter, the same trade
/// [`WorkJournal::open_in`] makes for the same reason.
fn bind_opened(
    durability: &SessionDurability,
    registry: &Arc<stella_tools::ToolRegistry>,
    journal: WorkJournal,
    workspace_root: &Path,
) {
    // Before anything else can touch a file: a resumed session that wrote
    // before restoring would be unguarded for exactly the writes its restored
    // transcript most encourages it to make.
    //
    // A session with no map of its own is restored to an EMPTY one, not left
    // holding whatever was there. The deck reuses a single registry across an
    // in-deck session switch, so "nothing to restore, leave it alone" would
    // hand the arriving session the departing session's belief about the tree
    // — and it would then refuse the arriving session's writes to files only
    // the departing one ever read. That is the inherited guard
    // `restore_observed`'s replace-don't-merge semantics exist to prevent,
    // arriving through the one door those semantics cannot see.
    registry.restore_observed(journal.observed().as_deref().unwrap_or("{}"));
    registry.attach_work_journal(journal.clone(), agent_label(workspace_root));
    durability.bind(journal, registry.clone());
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

    /// **The #1615 witness (capture half).** A session running staged-pipeline
    /// turns carries the frame describing them on *every* checkpoint commit,
    /// and the frame is retracted with the checkpoint — so the next turn of
    /// the same pipeline re-declares it and a turn that ended leaves nothing
    /// claiming a pipeline is still running.
    ///
    /// The second half is the one with teeth: a pipeline drives several turns
    /// (worker, verifier, revision) and each terminal path discards, so a
    /// frame written once at the start would be gone by the second turn — and
    /// the turn most likely to be killed is not the first one.
    #[test]
    fn a_pipeline_frame_rides_every_checkpoint_and_retires_with_it() {
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let record = journal(store.path(), ws.path(), "ses-frame");
        let durability = SessionDurability::default();
        durability.bind(record.clone(), registry(ws.path()));
        durability.set_pipeline_frame(r#"{"version":1}"#.to_string());
        let sink = durability.sink().expect("bound");

        sink.persist(r#"{"step":1}"#);
        assert_eq!(
            durability.pipeline_frame().as_deref(),
            Some(r#"{"version":1}"#)
        );
        sink.persist(r#"{"step":2}"#);
        assert_eq!(
            durability.pipeline_frame().as_deref(),
            Some(r#"{"version":1}"#),
            "the frame is re-stated at every step boundary, not only the first"
        );

        sink.discard();
        assert!(record.checkpoint().is_none());
        assert!(
            durability.pipeline_frame().is_none(),
            "a frame that outlived its turn would tell the next resume it is \
             re-entering a pipeline nobody is running"
        );

        // The pipeline's next turn checkpoints, and the frame is back — this
        // is what a frame written once could not do.
        sink.persist(r#"{"step":1,"turn":2}"#);
        assert_eq!(
            durability.pipeline_frame().as_deref(),
            Some(r#"{"version":1}"#)
        );
    }

    /// A plain engine turn declares no frame, so nothing beside its checkpoint
    /// claims it lost a pipeline. The delta the witness above is measured
    /// against.
    #[test]
    fn a_bare_turn_leaves_no_pipeline_frame() {
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        let record = journal(store.path(), ws.path(), "ses-bare");
        let durability = SessionDurability::default();
        durability.bind(record.clone(), registry(ws.path()));

        durability.sink().expect("bound").persist(r#"{"step":1}"#);

        assert!(record.checkpoint().is_some());
        assert_eq!(durability.pipeline_frame(), None);
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
    async fn switching_to_a_session_with_no_map_does_not_inherit_the_last_ones() {
        // The deck reuses ONE registry across an in-deck session switch. A
        // session that never observed a file must not arrive holding the
        // departing session's observations, or it will be refused writes to
        // files it has never read — a false positive in a guard whose whole
        // claim is that it has none.
        let store = tempfile::tempdir().unwrap();
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(ws.path().join("shared.txt"), "original\n").unwrap();
        let registry = registry(ws.path());
        let durability = SessionDurability::default();

        // Session one reads the file and checkpoints, so its map is durable.
        let out = registry
            .execute(
                "read_file",
                &serde_json::json!({ "path": "shared.txt", "reason": "planning" }),
            )
            .await;
        assert!(!out.is_error(), "{out:?}");
        let first = journal(store.path(), ws.path(), "ses-departing");
        durability.bind(first.clone(), registry.clone());
        durability.sink().expect("bound").persist("{\"step\":1}");
        assert!(first.observed().is_some(), "the map was persisted");

        // Something else edits the file, and the user switches to a session
        // that has never seen it.
        std::fs::write(ws.path().join("shared.txt"), "somebody else's work\n").unwrap();
        let second = journal(store.path(), ws.path(), "ses-arriving");
        assert!(
            second.observed().is_none(),
            "the arriving session has no map"
        );
        bind_opened(&durability, &registry, second, ws.path());

        let out = registry
            .execute(
                "write_file",
                &serde_json::json!({
                    "path": "shared.txt",
                    "content": "the arriving session's work\n",
                    "reason": "this session never read that file",
                }),
            )
            .await;
        assert!(
            !out.is_error(),
            "a session that never read the file must not be held to the last session's \
             observation of it: {out:?}"
        );
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
