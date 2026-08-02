//! Where this session's turns write their resume point.
//!
//! [`stella_core::step::CheckpointSink`] says *when* a checkpoint is written —
//! at the one step boundary where the transcript is guaranteed well-paired —
//! and deliberately says nothing about where. This module is the CLI's answer
//! to *where*, and it has to live in a crate that depends on both
//! `stella-core` (which declares the trait) and `stella-store` (which owns the
//! session sidecar). `stella-store` cannot implement it: it depends only on
//! `stella-protocol` and has never heard of the engine.
//!
//! # Why a shared cell rather than a plain field
//!
//! A session's durable identity does not exist when its [`crate::config::Config`] is
//! built. The deck receives `&Config` and only then resolves — or mints — the
//! [`stella_store::SessionRecord`] whose sidecar the checkpoint belongs in, and
//! it re-points that sidecar mid-run every time the user switches sessions. A
//! `PathBuf` field could be filled in by neither of those moments.
//!
//! So `Config` carries a [`SessionDurability`] handle instead: cheap to clone,
//! empty until a driver binds it, and read afresh by every engine the session
//! builds. `agent::engine_config_for` runs per turn, so an in-deck session
//! switch is picked up by the next turn with nothing to re-thread.
//!
//! # Why `sink()` may answer `None`
//!
//! [`stella_core::Engine::persist_checkpoint`] serializes the whole transcript
//! *before* it hands anything to the sink. An always-attached sink that happens
//! to have nowhere to write would therefore pay full JSON encoding on every
//! step of every turn to throw the bytes away. An unbound handle yields `None`
//! and the engine skips the encode entirely.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use stella_core::step::CheckpointSink;

/// A handle on where this session's resume point is written.
///
/// Cloned into [`crate::config::Config`] and shared by every engine the session
/// builds. Empty until a driver calls [`Self::bind`]; binding again re-points
/// it, which is what an in-deck session switch needs.
#[derive(Clone, Debug, Default)]
pub struct SessionDurability {
    /// The bound session's sidecar directory. `RwLock` rather than `OnceLock`
    /// because a deck session can be switched, and the *next* turn's checkpoint
    /// must land in the session it actually belongs to.
    sidecar: Arc<RwLock<Option<PathBuf>>>,
}

impl SessionDurability {
    /// Point this handle at `sidecar_dir` — the directory
    /// [`stella_store::SessionRegistry::sidecar_dir`] gives for the session
    /// that is now running.
    pub fn bind(&self, sidecar_dir: PathBuf) {
        let mut slot = self.sidecar.write().unwrap_or_else(|p| p.into_inner());
        *slot = Some(sidecar_dir);
    }

    /// The sink to hand [`stella_core::EngineConfig::checkpoint_sink`], or
    /// `None` while this handle is unbound.
    ///
    /// The bound path is read out *here*, once per engine, rather than on every
    /// `persist` — the lock stays off the per-step hot path, and a session
    /// switch is picked up because the next turn builds a new engine.
    pub fn sink(&self) -> Option<Arc<dyn CheckpointSink>> {
        let dir = self
            .sidecar
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()?;
        Some(Arc::new(SidecarCheckpointSink { dir }))
    }
}

/// A [`CheckpointSink`] over the session sidecar's `checkpoint.json`.
#[derive(Debug)]
struct SidecarCheckpointSink {
    dir: PathBuf,
}

impl CheckpointSink for SidecarCheckpointSink {
    /// One atomic write (temp + fsync + rename), as the trait's cost note
    /// requires — this runs on the turn's own task and blocks it.
    ///
    /// The error is dropped, not logged: this is called on every step of every
    /// turn, so a failing sink would emit one line per step, and the trait's
    /// contract is that a checkpoint which cannot be written leaves the turn
    /// exactly as recoverable as it was before the sink existed.
    fn persist(&self, json: &str) {
        let _ = stella_store::journal::write_checkpoint(&self.dir, json);
    }

    /// Idempotent by [`stella_store::journal::clear_checkpoint`]'s own
    /// contract — a missing file is success, which is what lets every terminal
    /// path discard unconditionally.
    fn discard(&self) {
        let _ = stella_store::journal::clear_checkpoint(&self.dir);
    }
}

/// The label a session's durable commits carry, derived from the workspace.
///
/// Not the session id: the id answers *which run*, and the work journal is
/// already keyed on it. This answers *whose work*, and a human reading
/// `git log` on the durable record wants the workspace name.
pub fn agent_label(workspace_root: &Path) -> String {
    workspace_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| workspace_root.display().to_string())
}

/// Bind both halves of this session's durability: where its turns write a
/// resume point, and where its file mutations commit.
///
/// One call rather than two because the halves answer one question — *which
/// session owns the work happening now* — and a driver that re-keyed only one
/// of them would checkpoint into the session it just left, or commit there.
/// The deck calls this at startup and again on every in-deck session switch;
/// the one-shot drivers call it once.
///
/// Returns a message to show the operator when the durable record could not be
/// opened, and `None` on success. Best-effort throughout, by the same reasoning
/// as the sink contract: a session with no durable record is exactly as
/// recoverable as every session was before this existed, so refusing to start
/// over it would trade a working session for none.
pub fn bind_session(
    durability: &SessionDurability,
    registry: &stella_tools::ToolRegistry,
    workspace_root: &Path,
    sidecar_dir: PathBuf,
    session_id: &str,
) -> Option<String> {
    durability.bind(sidecar_dir);
    match stella_store::work_journal::WorkJournal::open(workspace_root, session_id) {
        Ok(journal) => {
            registry.attach_work_journal(journal, agent_label(workspace_root));
            None
        }
        Err(e) => Some(format!(
            "durable work record unavailable ({e}) — this session's file changes will not be \
             recoverable from stella's own history"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unbound_handle_offers_no_sink() {
        assert!(SessionDurability::default().sink().is_none());
    }

    #[test]
    fn a_bound_handle_round_trips_a_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let durability = SessionDurability::default();
        durability.bind(dir.path().to_path_buf());

        let sink = durability.sink().expect("bound");
        sink.persist("{\"version\":1}");
        assert_eq!(
            stella_store::journal::read_checkpoint(dir.path()).as_deref(),
            Some("{\"version\":1}"),
        );

        sink.discard();
        assert!(stella_store::journal::read_checkpoint(dir.path()).is_none());
    }

    #[test]
    fn discard_is_idempotent_on_a_turn_that_never_checkpointed() {
        let dir = tempfile::tempdir().unwrap();
        let durability = SessionDurability::default();
        durability.bind(dir.path().to_path_buf());
        let sink = durability.sink().expect("bound");
        // Twice, having never persisted: both must be silent.
        sink.discard();
        sink.discard();
        assert!(stella_store::journal::read_checkpoint(dir.path()).is_none());
    }

    #[test]
    fn rebinding_re_points_the_next_sink() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let durability = SessionDurability::default();

        durability.bind(first.path().to_path_buf());
        durability
            .sink()
            .expect("bound")
            .persist("{\"first\":true}");

        // The in-deck session switch: the next engine's sink writes to the
        // session that is now running, and the previous session's resume point
        // is left exactly as it stood.
        durability.bind(second.path().to_path_buf());
        durability
            .sink()
            .expect("bound")
            .persist("{\"second\":true}");

        assert_eq!(
            stella_store::journal::read_checkpoint(first.path()).as_deref(),
            Some("{\"first\":true}"),
        );
        assert_eq!(
            stella_store::journal::read_checkpoint(second.path()).as_deref(),
            Some("{\"second\":true}"),
        );
    }
}
