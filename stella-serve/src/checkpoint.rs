// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where a served turn's resume point lives, and how it is read back.
//!
//! [`stella_core::step::CheckpointSink`] says *when* a checkpoint is written —
//! at the one step boundary where the transcript is guaranteed well-paired —
//! and deliberately says nothing about where. `crate::session::drive_turn`
//! already reaches both of its seams, calling the same
//! `Engine::persist_checkpoint` / `discard_checkpoint` the CLI driver calls.
//! What was missing was everything on the other side of the seam: nothing set
//! `EngineConfig::checkpoint_sink`, and a [`SessionSpec`] carried no durable
//! identity to key a store on. A served turn therefore died with the process,
//! while a CLI turn did not.
//!
//! This module is the answer to *where*, for the server.
//!
//! # A store, not just a sink
//!
//! A [`CheckpointSink`] is write-only: `persist` and `discard`, no read. That
//! is the right shape for the engine — a turn never reads its own resume point
//! — and the wrong shape for a server, because the whole value of a resume
//! point is realized by something that outlived the turn. So the port here is
//! a keyed **store** ([`CheckpointStore`]), and the sink the engine sees is a
//! thin adapter over one key of it (`TurnCheckpoint::sink`).
//!
//! # Why the server may own this record when it may not own the workspace
//!
//! `stella-serve` deliberately has no `stella-store` dependency and no
//! workspace root: every tool call is answered by the host (see
//! `crate::remote`), so the tree lives on the host's side and a server that
//! invented a filesystem location for *the work* would be writing against a
//! tree it does not own.
//!
//! A checkpoint is not the work. It is the engine's own turn state —
//! transcript, step index, money meter — which the server *does* own, because
//! the server is what is orchestrating the turn. Keeping it in a server-owned
//! location asserts nothing about the host's filesystem, which is why this can
//! be a store here while the work journal cannot.
//!
//! # Resume is host-initiated, and that is not a shortcut
//!
//! Nothing here re-drives a turn on restart, and a future version should not
//! either without deciding this on purpose. A resumed turn's very first act is
//! a reverse request — a model call the host has to answer — so a server that
//! "resumed" a turn with no host attached would produce a thread parked on a
//! request nobody is listening for, then time it out. The recoverable unit is
//! the checkpoint, not the running turn: the server's job is to not lose it
//! and to hand it back when asked (`GET /v1/sessions/{id}/checkpoint`), and
//! the host — which owns the tools, the credentials and the decision to spend
//! more money — chooses whether to continue.
//!
//! # What is retained, and for how long
//!
//! A turn that reaches any terminal outcome discards its own resume point, so
//! the steady-state contents of a store are exactly *the turns that died
//! without ending*: a `SIGKILL`, a panic, a power loss. That is a set bounded
//! by crash frequency rather than by traffic, but it is not self-emptying —
//! nothing else deletes those keys, because nothing else can tell an
//! abandoned resume point from one whose host is about to come back for it.
//! `DELETE /v1/sessions/{id}/checkpoint` is how a host that has finished
//! recovering says so.
//!
//! [`SessionSpec`]: crate::SessionSpec

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use stella_core::step::CheckpointSink;

use crate::observe::SharedObserver;
use crate::observe::event::{CheckpointOp, ServeEvent, TurnRef};

/// Ceiling on a [`CheckpointKey`], in bytes.
///
/// Server-minted ids are 40 characters (`session-` plus 32 hex), so this is
/// generous for every key this crate produces and still short enough that a
/// key is always a legal filename on every filesystem a
/// [`FileCheckpointStore`] might sit on.
pub const MAX_CHECKPOINT_KEY_LEN: usize = 128;

/// The durable name of one resume point — validated on construction, so a
/// store implementation never has to defend itself against its own key.
///
/// This is a newtype rather than a `&str` for one reason: a
/// [`FileCheckpointStore`] turns a key into a path, and `../../etc/passwd` is
/// a perfectly good `&str`. Validating in the constructor means traversal is
/// impossible *by construction* rather than by each implementor remembering to
/// check — including implementors outside this crate, which is the case that
/// cannot be fixed later.
///
/// The accepted alphabet is ASCII alphanumerics, `-` and `_`: exactly what the
/// server's own ids use, with nothing left over that means something to a
/// filesystem, a shell, or a URL.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CheckpointKey(String);

impl CheckpointKey {
    /// Validate `raw` as a key, or say why it is not one.
    pub fn new(raw: &str) -> Result<Self, CheckpointStoreError> {
        if raw.is_empty() || raw.len() > MAX_CHECKPOINT_KEY_LEN {
            return Err(CheckpointStoreError::InvalidKey {
                reason: format!(
                    "a checkpoint key must be 1..={MAX_CHECKPOINT_KEY_LEN} bytes, got {}",
                    raw.len()
                ),
            });
        }
        if let Some(bad) = raw
            .bytes()
            .find(|b| !(b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_'))
        {
            return Err(CheckpointStoreError::InvalidKey {
                reason: format!(
                    "a checkpoint key may only contain ASCII alphanumerics, '-' and '_'; \
                     found byte 0x{bad:02x}"
                ),
            });
        }
        Ok(Self(raw.to_string()))
    }

    /// The key as it was validated.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CheckpointKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Why a checkpoint could not be stored, read, or removed.
#[derive(Debug, thiserror::Error)]
pub enum CheckpointStoreError {
    /// The key was not a [`CheckpointKey`].
    #[error("invalid checkpoint key: {reason}")]
    InvalidKey {
        /// What was wrong with it.
        reason: String,
    },
    /// The backing filesystem refused.
    #[error("checkpoint store I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// An out-of-tree store's own failure.
    #[error("checkpoint store failed: {0}")]
    Backend(String),
}

/// Keyed, durable storage for turn resume points.
///
/// Implemented in-tree by [`MemoryCheckpointStore`] and
/// [`FileCheckpointStore`]; a host embedding this crate as a library
/// implements it over whatever it already trusts — a table, an object store, a
/// replicated log — and hands it to
/// [`ServeConfig::with_checkpoint_store`](crate::ServeConfig::with_checkpoint_store).
///
/// # Contract
///
/// - `put` **replaces**. A checkpoint is a complete snapshot, never a delta
///   (`stella_core::step::CheckpointSink::persist`), so there is no merge to
///   get wrong and a partially-written value must never be readable — see
///   [`FileCheckpointStore`] for what that costs on a filesystem.
/// - `remove` is **idempotent**. A turn that ended before its first step
///   boundary still discards on the way out, so removing what was never there
///   is the common case and is not an error.
/// - Every method is **synchronous and called from the turn's own thread**
///   (`put`/`remove`) or a connection task (`get`), so implementations must be
///   cheap. `put` runs at every step boundary of every turn.
/// - Failures are **reported, never propagated into the turn**. The adapter
///   behind [`TurnCheckpoint`] swallows the `Result` by the sink contract: a
///   turn that cannot write a resume point is exactly as recoverable as every
///   turn was before this module existed, so failing live work to report a
///   durability *improvement* would be strictly worse than not offering one.
pub trait CheckpointStore: Send + Sync + std::fmt::Debug {
    /// Record `json` under `key`, replacing any earlier value.
    fn put(&self, key: &CheckpointKey, json: &str) -> Result<(), CheckpointStoreError>;

    /// Read back what was stored under `key`, or `None` if nothing is.
    fn get(&self, key: &CheckpointKey) -> Result<Option<String>, CheckpointStoreError>;

    /// Drop `key`. Succeeds when there was nothing there.
    fn remove(&self, key: &CheckpointKey) -> Result<(), CheckpointStoreError>;
}

/// A store that lives and dies with the process.
///
/// Useful for tests and for an embedder that wants the read-back surface
/// without durability, and deliberately **not** the server's default: a store
/// that cannot survive the process would answer the exact question this module
/// exists to answer with "no".
#[derive(Debug, Default)]
pub struct MemoryCheckpointStore {
    entries: Mutex<HashMap<CheckpointKey, String>>,
}

impl MemoryCheckpointStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<CheckpointKey, String>> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl CheckpointStore for MemoryCheckpointStore {
    fn put(&self, key: &CheckpointKey, json: &str) -> Result<(), CheckpointStoreError> {
        self.entries().insert(key.clone(), json.to_string());
        Ok(())
    }

    fn get(&self, key: &CheckpointKey) -> Result<Option<String>, CheckpointStoreError> {
        Ok(self.entries().get(key).cloned())
    }

    fn remove(&self, key: &CheckpointKey) -> Result<(), CheckpointStoreError> {
        self.entries().remove(key);
        Ok(())
    }
}

/// One JSON file per key under a server-owned directory.
///
/// # Why the write is a temp-plus-rename and not a `write`
///
/// The value being written is a whole transcript — kilobytes to megabytes —
/// so a plain `fs::write` interrupted by the very crash this record exists to
/// survive leaves a truncated file, which decodes as a *corrupt* checkpoint
/// rather than a missing one. Those are not equally bad: a missing checkpoint
/// costs the turn, a corrupt one costs the turn *and* looks like a bug in the
/// engine. Writing to a temp file, `fsync`ing it, and renaming makes the
/// visible file transition atomically from the previous complete snapshot to
/// the next one, with no intermediate state a reader can observe.
///
/// The directory is `fsync`ed after the rename on a best-effort basis — that
/// is what makes the *rename itself* durable, not merely the bytes — and its
/// failure is ignored because a platform that refuses to open a directory as a
/// file (Windows) is not a platform where the fsync was going to help.
///
/// # Single-writer per key
///
/// The temp file's name is derived from the key rather than randomized, which
/// is safe because a key has exactly one writer at a time: a session admits
/// one live turn (`crate::sessions`), and a turn id is unique. A leftover temp
/// file from a crash is overwritten by the next write and never read.
#[derive(Debug)]
pub struct FileCheckpointStore {
    root: PathBuf,
}

impl FileCheckpointStore {
    /// Open (creating if needed) a store rooted at `root`.
    ///
    /// This is a server-owned directory, not part of any workspace — see the
    /// module docs for why that distinction is what makes a server-side store
    /// legitimate at all.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, CheckpointStoreError> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    /// The directory this store writes into.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn path(&self, key: &CheckpointKey) -> PathBuf {
        self.root.join(format!("{key}.json"))
    }

    fn temp_path(&self, key: &CheckpointKey) -> PathBuf {
        self.root.join(format!("{key}.json.tmp"))
    }
}

impl CheckpointStore for FileCheckpointStore {
    fn put(&self, key: &CheckpointKey, json: &str) -> Result<(), CheckpointStoreError> {
        use std::io::Write as _;

        let temp = self.temp_path(key);
        {
            let mut file = std::fs::File::create(&temp)?;
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
        }
        // A failed rename leaves the previous snapshot intact and the temp file
        // behind; both are recoverable states, which is the point of doing it
        // in this order.
        std::fs::rename(&temp, self.path(key))?;
        let _ = std::fs::File::open(&self.root).and_then(|dir| dir.sync_all());
        Ok(())
    }

    fn get(&self, key: &CheckpointKey) -> Result<Option<String>, CheckpointStoreError> {
        match std::fs::read_to_string(self.path(key)) {
            Ok(json) => Ok(Some(json)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn remove(&self, key: &CheckpointKey) -> Result<(), CheckpointStoreError> {
        match std::fs::remove_file(self.path(key)) {
            Ok(()) => Ok(()),
            // Idempotent by the sink contract: every terminal path discards,
            // including the turns that never checkpointed at all.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err.into()),
        }
    }
}

/// The durable identity of one turn: which store, and under what key.
///
/// Carried on [`SessionSpec::checkpoint`](crate::SessionSpec::checkpoint) —
/// this is the "session identity to key a store on" whose absence is why a
/// served turn used to die with the process.
///
/// Which id a turn keys on is a routing decision, not a storage one, and the
/// two answers differ on purpose:
///
/// - a turn inside a session keys on the **session id**, because that is the
///   name the host will still have after the crash, and because a session
///   admits one live turn at a time so the key has one writer;
/// - a stateless `POST /v1/turns` turn keys on its own **turn id**, the only
///   name it ever had.
#[derive(Clone, Debug)]
pub struct TurnCheckpoint {
    /// Where the resume point goes.
    pub store: Arc<dyn CheckpointStore>,
    /// What it is called there.
    pub key: CheckpointKey,
}

impl TurnCheckpoint {
    /// Build a checkpoint identity for `id`, or `None` if `id` is not a legal
    /// key.
    ///
    /// `None` rather than an error because this sits on the turn-creation path:
    /// a caller that cannot be keyed gets an ordinary non-durable turn, which
    /// is what every turn was before this existed. Refusing to *run* the turn
    /// would trade a working turn for none.
    #[must_use]
    pub fn for_id(store: &Arc<dyn CheckpointStore>, id: &str) -> Option<Self> {
        CheckpointKey::new(id).ok().map(|key| Self {
            store: Arc::clone(store),
            key,
        })
    }

    /// The [`CheckpointSink`] to hand `EngineConfig::checkpoint_sink`.
    ///
    /// `observer` and `turn` are only used to report a *failing* store, once —
    /// see [`StoreSink`].
    pub(crate) fn sink(&self, observer: SharedObserver, turn: TurnRef) -> Arc<dyn CheckpointSink> {
        Arc::new(StoreSink {
            store: Arc::clone(&self.store),
            key: self.key.clone(),
            observer,
            turn,
            reported: AtomicBool::new(false),
        })
    }
}

/// One key of a [`CheckpointStore`], seen by the engine as a
/// [`CheckpointSink`].
///
/// # Reporting a failure exactly once
///
/// The sink contract forbids returning the error and warns against logging per
/// call, because `persist` runs at every step boundary of every turn — a
/// store that is failing (full disk, revoked permission, unmounted volume)
/// would otherwise emit one line per step for as long as the server is up.
///
/// Silence is not the alternative, though. A durability feature that fails
/// quietly is worse than no durability feature: the operator believes served
/// turns are recoverable and finds out otherwise during the incident that
/// needed them. So the first failure of this turn's sink emits one
/// [`ServeEvent::CheckpointFailed`] and latches; the rest are counted by
/// nothing and said by nothing, which is the honest cost of the per-step
/// budget.
struct StoreSink {
    store: Arc<dyn CheckpointStore>,
    key: CheckpointKey,
    observer: SharedObserver,
    turn: TurnRef,
    reported: AtomicBool,
}

/// `SharedObserver` is a trait object and not `Debug`, which
/// [`CheckpointSink`] requires of every implementor. The store and the key are
/// the parts worth printing — they answer *where this turn was writing* — so
/// the observer is elided rather than the sink losing its `Debug`.
impl std::fmt::Debug for StoreSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoreSink")
            .field("store", &self.store)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl StoreSink {
    /// Report the first failure of this turn's sink and latch. Later ones are
    /// dropped — see the type's docs.
    fn report(&self, op: CheckpointOp, error: &CheckpointStoreError) {
        if self.reported.swap(true, Ordering::Relaxed) {
            return;
        }
        self.observer.emit(&ServeEvent::CheckpointFailed {
            turn: self.turn.clone(),
            op,
            error: error.to_string(),
        });
    }
}

impl CheckpointSink for StoreSink {
    fn persist(&self, json: &str) {
        if let Err(err) = self.store.put(&self.key, json) {
            self.report(CheckpointOp::Persist, &err);
        }
    }

    fn discard(&self) {
        if let Err(err) = self.store.remove(&self.key) {
            self.report(CheckpointOp::Discard, &err);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observe::null_observer;

    fn key(raw: &str) -> CheckpointKey {
        CheckpointKey::new(raw).expect("valid key")
    }

    /// The keys the server actually mints are keys.
    #[test]
    fn server_minted_ids_are_valid_keys() {
        for id in [
            "session-0123456789abcdef0123456789abcdef",
            "turn-0123456789abcdef0123456789abcdef",
            "a",
        ] {
            assert!(CheckpointKey::new(id).is_ok(), "{id} should be a key");
        }
    }

    /// The reason the newtype exists: a path-shaped key never reaches a store.
    #[test]
    fn a_key_cannot_escape_its_directory() {
        for hostile in [
            "..",
            "../../etc/passwd",
            "a/b",
            "a\\b",
            ".hidden",
            "with space",
            "nul\0byte",
            "",
        ] {
            assert!(
                CheckpointKey::new(hostile).is_err(),
                "{hostile:?} must not be a key"
            );
        }
        assert!(CheckpointKey::new(&"x".repeat(MAX_CHECKPOINT_KEY_LEN + 1)).is_err());
    }

    /// Both stores answer the same three questions the same way, so the
    /// contract is one contract and not two implementations that happen to
    /// agree.
    fn store_round_trips(store: &dyn CheckpointStore) {
        let k = key("session-abc");
        assert_eq!(store.get(&k).unwrap(), None, "nothing stored yet");

        store.put(&k, "{\"step\":1}").unwrap();
        assert_eq!(store.get(&k).unwrap().as_deref(), Some("{\"step\":1}"));

        // `put` replaces — a checkpoint is a snapshot, never a delta.
        store.put(&k, "{\"step\":2}").unwrap();
        assert_eq!(store.get(&k).unwrap().as_deref(), Some("{\"step\":2}"));

        store.remove(&k).unwrap();
        assert_eq!(store.get(&k).unwrap(), None);
        // Idempotent: every terminal path discards, including turns that never
        // wrote anything.
        store.remove(&k).unwrap();
    }

    #[test]
    fn the_memory_store_honors_the_contract() {
        store_round_trips(&MemoryCheckpointStore::new());
    }

    #[test]
    fn the_file_store_honors_the_contract() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCheckpointStore::open(dir.path()).unwrap();
        store_round_trips(&store);
    }

    /// Keys do not see each other's values — the property every multi-session
    /// server depends on.
    #[test]
    fn keys_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCheckpointStore::open(dir.path()).unwrap();
        store.put(&key("session-one"), "{\"a\":1}").unwrap();
        store.put(&key("session-two"), "{\"b\":2}").unwrap();
        store.remove(&key("session-one")).unwrap();
        assert_eq!(store.get(&key("session-one")).unwrap(), None);
        assert_eq!(
            store.get(&key("session-two")).unwrap().as_deref(),
            Some("{\"b\":2}"),
            "removing one key must not touch another"
        );
    }

    /// The whole point: what a file store wrote is still there for a *new*
    /// store object over the same directory — which is what a restarted
    /// process is.
    #[test]
    fn a_file_store_outlives_the_process_that_wrote_it() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = FileCheckpointStore::open(dir.path()).unwrap();
            store.put(&key("session-survivor"), "{\"step\":7}").unwrap();
        }
        let reopened = FileCheckpointStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.get(&key("session-survivor")).unwrap().as_deref(),
            Some("{\"step\":7}"),
            "a resume point that dies with the process is not a resume point"
        );
    }

    /// The rename is what makes a reader see either the old snapshot or the
    /// new one — never a half-written transcript. Proved by the absence of the
    /// temp file, which is the only thing a reader could otherwise trip over.
    #[test]
    fn a_completed_write_leaves_no_temp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileCheckpointStore::open(dir.path()).unwrap();
        store.put(&key("session-atomic"), "{\"step\":1}").unwrap();
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["session-atomic.json".to_string()]);
    }

    /// A store keyed through the sink adapter is the same store: the engine's
    /// two verbs land on the two store verbs.
    #[test]
    fn the_sink_writes_and_clears_its_key() {
        let store: Arc<dyn CheckpointStore> = Arc::new(MemoryCheckpointStore::new());
        let checkpoint = TurnCheckpoint::for_id(&store, "session-sink").expect("keyable");
        let sink = checkpoint.sink(null_observer(), TurnRef::new("turn-abc"));

        sink.persist("{\"step\":3}");
        assert_eq!(
            store.get(&key("session-sink")).unwrap().as_deref(),
            Some("{\"step\":3}")
        );

        sink.discard();
        assert_eq!(store.get(&key("session-sink")).unwrap(), None);
    }

    /// An unkeyable id yields no checkpoint rather than an error — the turn
    /// still runs, it just is not durable.
    #[test]
    fn an_unkeyable_id_yields_no_checkpoint() {
        let store: Arc<dyn CheckpointStore> = Arc::new(MemoryCheckpointStore::new());
        assert!(TurnCheckpoint::for_id(&store, "../escape").is_none());
    }

    /// A store that always fails: the failure mode the report latch exists for.
    #[derive(Debug)]
    struct BrokenStore;

    impl CheckpointStore for BrokenStore {
        fn put(&self, _key: &CheckpointKey, _json: &str) -> Result<(), CheckpointStoreError> {
            Err(CheckpointStoreError::Backend("no room".to_string()))
        }
        fn get(&self, _key: &CheckpointKey) -> Result<Option<String>, CheckpointStoreError> {
            Err(CheckpointStoreError::Backend("no room".to_string()))
        }
        fn remove(&self, _key: &CheckpointKey) -> Result<(), CheckpointStoreError> {
            Err(CheckpointStoreError::Backend("no room".to_string()))
        }
    }

    /// A failing store is reported once and never fails the turn — both halves
    /// of the sink contract, in one test because they are one decision.
    #[test]
    fn a_failing_store_is_reported_once_and_never_fails_the_turn() {
        let capture = Arc::new(crate::observe::Capture::new());
        let observer: SharedObserver = capture.clone();
        let store: Arc<dyn CheckpointStore> = Arc::new(BrokenStore);
        let checkpoint = TurnCheckpoint::for_id(&store, "session-broken").expect("keyable");
        let sink = checkpoint.sink(observer, TurnRef::new("turn-abc"));

        // Ten steps of a turn against a dead disk. None of these panic or
        // return, because the sink cannot fail the turn that produced it.
        for _ in 0..10 {
            sink.persist("{\"step\":1}");
        }
        sink.discard();

        let failures = capture
            .events()
            .iter()
            .filter(|event| matches!(event, ServeEvent::CheckpointFailed { .. }))
            .count();
        assert_eq!(
            failures, 1,
            "a failing store is a per-step event and must be reported per turn"
        );
    }
}
