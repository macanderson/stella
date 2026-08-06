//! The cross-process **session registry**: every running stella session
//! announces itself as one JSON file under `data_dir()/sessions/`, so any
//! session (or a future `stella sessions` CLI) can render a live "all my
//! stella sessions" view — the deck's SESSIONS overlay reads exactly this.
//!
//! Design: **one file per session, one writer per file.** The owning process
//! is the only writer of its record (atomic temp+rename), so concurrent
//! sessions never contend and there is no lock, daemon, or socket. Readers
//! sweep the directory and are tolerant: an unparsable file is skipped, and
//! a record whose process died mid-flight (pid gone while the status still
//! says in-progress/needs-input) is *presented* as [`SessionStatus::Error`]
//! ("crashed") without rewriting the dead process's file.
//!
//! Lifecycle: the deck driver upserts on session start, on every turn
//! boundary (title/summary/status), and on exit. `Archived` is a user action
//! from the SESSIONS view; archived and other terminal records stay until
//! removed there (or swept by [`SessionRegistry::prune`]).
//!
//! Each record may own a **sidecar directory** (`data_dir()/sessions/<id>/`,
//! see [`crate::journal`]) holding the durable session state that makes it
//! resumable — deleting a record deletes its sidecar with it.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::{Result, StoreError};

/// Where a session stands. Serialized in snake_case inside each record file;
/// the SESSIONS view groups by this, in this declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// A turn is running (or the session is idle between turns but alive).
    InProgress,
    /// The session is blocked on a human answer (ask-user, scope review).
    NeedsInput,
    /// Deliberately set aside with its state intact — the deck exited (or
    /// switched away) with work still pending. Not live (no pid downgrade
    /// applies), and the first thing `resume` looks for.
    Paused,
    /// The work was **ended**, not broken. Two endings share this status
    /// because they are one fact to every reader: the user interrupted it
    /// (Ctrl-C mid-turn, queue abandoned), or the run stopped itself by
    /// policy — a stuck loop escalated past its warning, the step cap, an
    /// enforced budget, a scope review the user ended (#1653).
    ///
    /// Its counterpart [`SessionStatus::Error`] therefore means only "it fell
    /// over", which is the distinction a boot-time resume sweep reads.
    Cancelled,
    /// The session ended after finishing its work.
    Complete,
    /// Tucked away by the user from the SESSIONS view; kept until removed.
    Archived,
    /// The session ended on an error — or its process died mid-flight
    /// (derived at read time from a dead pid; see [`SessionRegistry::list`]).
    Error,
}

impl SessionStatus {
    /// Grouping/order for the SESSIONS view: active work first.
    pub const ALL: [SessionStatus; 7] = [
        SessionStatus::InProgress,
        SessionStatus::NeedsInput,
        SessionStatus::Paused,
        SessionStatus::Cancelled,
        SessionStatus::Complete,
        SessionStatus::Archived,
        SessionStatus::Error,
    ];

    /// Human group heading.
    pub fn label(&self) -> &'static str {
        match self {
            SessionStatus::InProgress => "In Progress",
            SessionStatus::NeedsInput => "Needs Input",
            SessionStatus::Paused => "Paused",
            SessionStatus::Cancelled => "Cancelled",
            SessionStatus::Complete => "Complete",
            SessionStatus::Archived => "Archived",
            SessionStatus::Error => "Error",
        }
    }

    /// Whether the session still has (or awaits) live work — these states
    /// are pid-checked at read time and downgraded to `Error` if the
    /// process is gone.
    pub fn is_live(&self) -> bool {
        matches!(self, SessionStatus::InProgress | SessionStatus::NeedsInput)
    }
}

/// One session's registry record — everything the SESSIONS view shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    /// Stable id, minted at session start (`ses-<ms>-<pid>`).
    pub id: String,
    /// The owning process, for read-time liveness checks.
    pub pid: u32,
    /// Absolute workspace path (the human title shows its basename).
    pub workspace: String,
    /// Human-readable title: `<workspace basename>: <first prompt…>`.
    pub title: String,
    /// What work is involved right now — the latest prompt/goal, truncated.
    pub summary: String,
    pub status: SessionStatus,
    pub started_at_ms: u64,
    pub updated_at_ms: u64,
    /// Exploration slices this session is currently mapping (its live draft
    /// records in `.stella/explorations/`) — lets the SESSIONS view warn
    /// before a prompt that would re-map territory another live session is
    /// already on. Absent in pre-v2 records.
    #[serde(default)]
    pub exploring: Vec<String>,
    /// Present when the session runs under the CLI's supervisor rather than in
    /// the foreground of a terminal (#1552). Absent for every ordinary
    /// session, and for every record written before supervision existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervisor: Option<SupervisorInfo>,
}

/// What a **supervised** session's sidecar directory holds, beyond the durable
/// journal every session gets. The names live here, beside
/// [`SessionRegistry::sidecar_dir`], because the sidecar's layout has one owner
/// and a second copy of these strings in the CLI is a rename away from
/// pointing at a file nothing writes.
pub mod supervised {
    /// The child's stdout, verbatim.
    ///
    /// Two files rather than one merged console: `stella run --output-format
    /// json` is piped into `jq`, and a warning folded into stdout breaks the
    /// parse on the *caller's* side, where it is unattributable. Attaching
    /// re-splits them onto the same two streams they were written from.
    pub const STDOUT_LOG: &str = "stdout.log";
    /// The child's stderr, verbatim. See [`STDOUT_LOG`].
    pub const STDERR_LOG: &str = "stderr.log";
    /// The prompt, handed to the child as its stdin.
    ///
    /// Not an argv element: an argv-borne prompt is visible to every user on
    /// the machine in `ps`, and a long one hits `ARG_MAX`. A file in a `0700`
    /// directory is neither.
    pub const STDIN: &str = "stdin";
    /// The advisory lock the child holds open for its whole life.
    ///
    /// The kernel releases it when the last holder dies — crash, `SIGKILL`
    /// and power loss included — so "is the lock free?" answers "is this run
    /// over?" without trusting a pid. See [`super::SupervisorInfo::pgid`] for
    /// why that distinction is load-bearing rather than fastidious.
    ///
    /// *Last holder*, not *the process*: a `flock` belongs to an open file
    /// description, and `fork`+`exec` inherits one. Keeping the answer equal
    /// to "is the run over?" is therefore a duty of whoever takes the lock —
    /// they must stop it being inherited further, or a background process the
    /// run spawned will go on answering yes after the run has ended.
    pub const LOCK: &str = "supervisor.lock";
    /// A scope-review proposal parked by the child, awaiting a human (#1585).
    ///
    /// Its own file — never folded into either console — because the two
    /// console files are byte-verbatim stdout/stderr and a prompt inside
    /// either would corrupt what a machine-format caller is parsing. Present
    /// exactly while the run's record reads `NeedsInput`; the child removes it
    /// once answered.
    pub const APPROVAL_REQUEST: &str = "approval-request.json";
    /// The answer to [`APPROVAL_REQUEST`], written atomically by whichever
    /// attached terminal took the question. The child polls for it, applies
    /// it, and removes both files.
    pub const APPROVAL_ANSWER: &str = "approval-answer.json";
}

/// How to reach a supervised session's work: it is a detached child in its own
/// process group, its console is two files in the sidecar, and it outlives the
/// terminal that started it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorInfo {
    /// The process **group** to signal, which is also the child's pid: the
    /// child `setsid`s before `exec`, so it leads its own session.
    ///
    /// Stored as a group rather than derived from [`SessionRecord::pid`] at
    /// use because the two are only equal while that invariant holds, and the
    /// cost of being wrong is asymmetric — signalling a group that is not ours
    /// takes down a stranger's processes. A reader that wants to signal must
    /// still confirm the run is live through [`supervised::LOCK`]: a pid (and
    /// a pgid) is recycled by the kernel, an open lock is not.
    pub pgid: i32,
}

/// A `<id>.json` that is present but unusable — the state
/// [`SessionRegistry::list`] silently drops.
///
/// It is reported rather than repaired because the record cannot be rebuilt:
/// `id`, `pid` and `started_at_ms` are recoverable from the filename (ids are
/// self-minted `ses-<ms>-<pid>`), but `workspace` is not held anywhere else on
/// disk — not in `journal.jsonl`, not in `history.json` — and resuming needs
/// it. So the honest outcome is to surface the session as damaged, keep its
/// sidecar, and let a human decide, instead of silently hiding it (today) or
/// silently deleting it (what #617 item 12 proposed).
#[derive(Debug, Clone)]
pub struct DamagedRecord {
    /// The record's id, taken from the filename.
    pub id: String,
    /// The unreadable record file.
    pub path: PathBuf,
    /// Why it could not be used — zero-length, or the parse error.
    pub reason: String,
    /// Whether a sidecar directory sits beside it. When true there is very
    /// likely a recoverable `history.json` inside, which is what makes
    /// deleting this session unacceptable.
    pub has_sidecar: bool,
}

/// What [`SessionRegistry::scan`] found, with healthy, damaged, and genuinely
/// orphaned state kept apart. See that method for why the distinction exists.
#[derive(Debug, Default)]
pub struct RegistryScan {
    /// Records that parsed, newest-started first, liveness downgrade applied.
    pub healthy: Vec<SessionRecord>,
    /// Records present on disk but unusable. Their sidecars are intact.
    pub damaged: Vec<DamagedRecord>,
    /// Sidecar directory names with no `<id>.json` beside them at all — the
    /// only state that is safe to reclaim.
    pub orphan_sidecars: Vec<String>,
}

impl SessionRecord {
    /// A fresh in-progress record for this process, timestamped now.
    pub fn new(workspace: impl Into<String>, title: impl Into<String>) -> Self {
        let now = now_ms();
        let pid = std::process::id();
        Self {
            id: format!("ses-{now}-{pid}"),
            pid,
            workspace: workspace.into(),
            title: title.into(),
            summary: String::new(),
            status: SessionStatus::InProgress,
            started_at_ms: now,
            updated_at_ms: now,
            exploring: Vec::new(),
            supervisor: None,
        }
    }
}

/// The registry directory handle. Cheap to construct; every operation is a
/// direct filesystem op (no cached state to go stale across processes).
#[derive(Debug, Clone)]
pub struct SessionRegistry {
    dir: PathBuf,
}

impl SessionRegistry {
    /// The standard registry at `data_dir()/sessions`.
    pub fn open_default() -> Self {
        Self::open(crate::usage::data_dir().join("sessions"))
    }

    /// A registry rooted at `dir` (tests point this at a temp dir).
    pub fn open(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Ids are self-minted (`ses-<ms>-<pid>`), but never trust a name to
    /// stay a single path component.
    fn safe_id(id: &str) -> String {
        id.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{}.json", Self::safe_id(id)))
    }

    /// The session's sidecar directory (journal + snapshots — see
    /// [`crate::journal`]), beside its record file. `list` only reads
    /// `.json` files, so the directory never shadows a record.
    pub fn sidecar_dir(&self, id: &str) -> PathBuf {
        self.dir.join(Self::safe_id(id))
    }

    /// Create `id`'s sidecar directory owner-only, and answer its path.
    ///
    /// Every other writer of a sidecar creates it on its first write. A
    /// supervised session needs it to exist *before* the session does: its
    /// console files and its liveness lock ([`supervised`]) are opened into
    /// the directory as the child is spawned, and the mode those files inherit
    /// is not the caller's to reinvent.
    pub fn prepare_sidecar(&self, id: &str) -> Result<PathBuf> {
        let dir = self.sidecar_dir(id);
        crate::ensure_private_dir(&dir)?;
        Ok(dir)
    }

    /// Write (create or replace) `record` atomically, stamping
    /// `updated_at_ms`. Only the owning session should call this for its own
    /// record — except for [`SessionRegistry::set_status`]'s
    /// archive/cleanup writes from the viewer.
    pub fn upsert(&self, record: &SessionRecord) -> Result<()> {
        crate::ensure_private_dir(&self.dir)?;
        let mut stamped = record.clone();
        stamped.updated_at_ms = now_ms();
        let json = serde_json::to_string_pretty(&stamped)
            .map_err(|e| StoreError(format!("cannot serialize session record: {e}")))?;
        let path = self.path_for(&record.id);
        // sync = true: an unfsynced rename can publish a directory entry whose
        // bytes never left the page cache, so a power cut leaves a
        // zero-length `<id>.json`. `list` skips the unparsable record, which
        // makes the session invisible to `resumable`/`latest_resumable` AND
        // strands its sidecar (journal + history.json) because `prune`
        // iterates `list`. Upserts happen a handful of times per turn, not per
        // event, so the fsync is cheap against what is lost — the same
        // reasoning `journal::write_snapshot` already documents.
        crate::private::write_private_atomic(&path, json.as_bytes())
    }

    /// All records, newest-started first, with dead-process downgrade
    /// applied: a live-status record whose pid is gone is shown as `Error`
    /// (the session crashed without writing a terminal status). Unreadable
    /// files are skipped — one corrupt record never hides the rest.
    ///
    /// A skipped record is *invisible*, not *absent*: its `<id>.json` is still
    /// on disk. Anything deciding whether state may be deleted must use
    /// [`Self::scan`], which keeps the two apart.
    pub fn list(&self) -> Vec<SessionRecord> {
        self.scan().healthy
    }

    /// The registry directory as it actually is on disk, with the three states
    /// [`Self::list`] flattens into one kept apart.
    ///
    /// `list` reads `<id>.json` and drops anything that fails to parse, so a
    /// damaged record and a missing record are indistinguishable through it.
    /// That conflation is load-bearing for anything destructive: `upsert`'s own
    /// comment records that a power cut leaves a **zero-length `<id>.json`**,
    /// which `list` skips — so a sweep that deletes "sidecars `list` doesn't
    /// account for" deletes the conversation of a session whose record was
    /// merely truncated, and `history.json` is the only continuable copy
    /// (#617 item 12).
    ///
    /// So the orphan test here is "is there a `<id>.json` at all", answered
    /// from the directory entry and never from a parse.
    pub fn scan(&self) -> RegistryScan {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return RegistryScan::default();
        };

        let mut scan = RegistryScan::default();
        let mut record_names: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut sidecar_names: Vec<String> = Vec::new();

        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    sidecar_names.push(name.to_string());
                }
                continue;
            }
            if !file_type.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(stem) = path
                .file_stem()
                .and_then(|s| s.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            // Recorded BEFORE the parse: the file's existence is what makes a
            // sidecar non-orphaned, whether or not its bytes are usable.
            record_names.insert(stem.clone());

            match crate::read_private_to_string(&path)
                .map_err(|error| error.to_string())
                .and_then(|text| {
                    serde_json::from_str::<SessionRecord>(&text).map_err(|error| {
                        if text.is_empty() {
                            "record is zero-length (an interrupted write)".to_string()
                        } else {
                            error.to_string()
                        }
                    })
                }) {
                Ok(mut record) => {
                    record.status = Self::presented_status(&record);
                    scan.healthy.push(record);
                }
                Err(reason) => scan.damaged.push(DamagedRecord {
                    id: stem,
                    has_sidecar: path.with_extension("").is_dir(),
                    reason,
                    path,
                }),
            }
        }

        scan.orphan_sidecars = sidecar_names
            .into_iter()
            .filter(|name| !record_names.contains(name))
            .collect();

        scan.healthy
            .sort_by_key(|r| std::cmp::Reverse(r.started_at_ms));
        scan.damaged.sort_by(|a, b| a.id.cmp(&b.id));
        scan.orphan_sidecars.sort();
        scan
    }

    /// Delete sidecar directories that have no `<id>.json` beside them at all.
    ///
    /// The narrow definition is the safety property: a directory whose record
    /// exists but cannot be parsed is **not** an orphan and is never touched
    /// here (see [`Self::scan`]). Returns the ids removed.
    ///
    /// This reclaims the case [`Self::upsert`]'s comment calls "strands its
    /// sidecar" — a record deleted while its directory survived — without
    /// being able to reach a session whose record is merely damaged.
    pub fn prune_orphan_sidecars(&self) -> Result<Vec<String>> {
        let mut removed = Vec::new();
        for id in self.scan().orphan_sidecars {
            let dir = self.dir.join(&id);
            // Re-check under the same name we are about to delete: a session
            // that started between the scan and here has written its record.
            if self.dir.join(format!("{id}.json")).exists() {
                continue;
            }
            std::fs::remove_dir_all(&dir).map_err(|error| {
                StoreError(format!(
                    "cannot remove orphan session sidecar {}: {error}",
                    dir.display()
                ))
            })?;
            removed.push(id);
        }
        Ok(removed)
    }

    /// Read one record (no liveness downgrade — the raw stored state).
    pub fn get(&self, id: &str) -> Option<SessionRecord> {
        let text = crate::read_private_to_string(&self.path_for(id)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Rewrite `id`'s status (the viewer's archive action, and the owner's
    /// terminal transitions). Returns whether the record existed.
    pub fn set_status(&self, id: &str, status: SessionStatus) -> Result<bool> {
        let Some(mut record) = self.get(id) else {
            return Ok(false);
        };
        record.status = status;
        self.upsert(&record)?;
        Ok(true)
    }

    /// Delete `id`'s record — and its sidecar state with it (a deleted
    /// session must not leave an orphaned journal behind); returns whether
    /// the record existed.
    pub fn remove(&self, id: &str) -> Result<bool> {
        let existed = match std::fs::remove_file(self.path_for(id)) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(StoreError(format!("cannot remove session record: {e}"))),
        };
        if let Err(e) = std::fs::remove_dir_all(self.sidecar_dir(id))
            && e.kind() != std::io::ErrorKind::NotFound
        {
            return Err(StoreError(format!("cannot remove session state: {e}")));
        }
        Ok(existed)
    }

    /// `record`'s status as presented to viewers: a live status whose owning
    /// process is gone reads as `Error` ("crashed") without rewriting the
    /// dead process's file. Every other status is returned as stored.
    pub fn presented_status(record: &SessionRecord) -> SessionStatus {
        if record.status.is_live() && !pid_alive(record.pid) {
            SessionStatus::Error
        } else {
            record.status
        }
    }

    /// Whether `id` can be reopened: its record exists, no live process owns
    /// it, and there is durable state on disk to restore ([`crate::journal`]).
    pub fn resumable(&self, id: &str) -> bool {
        self.get(id).is_some_and(|r| {
            !Self::presented_status(&r).is_live()
                && crate::journal::has_state(&self.sidecar_dir(&r.id))
        })
    }

    /// The most recently *active* resumable session for `workspace` — what a
    /// bare `stella resume` reopens.
    ///
    /// Applies [`Self::resumable`]'s predicate inline rather than calling it:
    /// [`Self::list`] has already read and downgraded every record, and
    /// `presented_status` is idempotent, so re-reading each candidate's file
    /// through `get` would only pay the registry's IO twice.
    pub fn latest_resumable(&self, workspace: &str) -> Option<SessionRecord> {
        self.list()
            .into_iter()
            .filter(|r| {
                r.workspace == workspace
                    && !r.status.is_live()
                    && crate::journal::has_state(&self.sidecar_dir(&r.id))
            })
            .max_by_key(|r| r.updated_at_ms)
    }

    /// Sweep terminal records older than `max_age_ms` (registry hygiene —
    /// called opportunistically by the deck driver at startup).
    ///
    /// Terminal is judged on the *presented* status ([`Self::list`]), not the
    /// stored one, so a record still stored `InProgress` whose owning process
    /// died reads as crashed and is in scope — deliberately, since nothing
    /// will ever move it to a terminal state otherwise. Note what that
    /// implies: sweeping it also deletes its sidecar, so a crashed session
    /// stops being resumable once it ages past the cutoff. Two shapes are
    /// spared: records whose presented status is still live, and `Paused`
    /// ones — a pause is a deliberate promise that the state is kept for
    /// `resume` (its documented contract), so it must not age out from under
    /// the user who made it.
    pub fn prune(&self, max_age_ms: u64) -> Result<usize> {
        let cutoff = now_ms().saturating_sub(max_age_ms);
        let mut removed = 0;
        for record in self.list() {
            let terminal = !record.status.is_live() && record.status != SessionStatus::Paused;
            if terminal && record.updated_at_ms < cutoff {
                removed += usize::from(self.remove(&record.id)?);
            }
        }
        Ok(removed)
    }
}

/// Whether `pid` is a live process. Unix: `kill(pid, 0)` (EPERM still means
/// alive). Elsewhere: assume alive (no downgrade — better to show a stale
/// in-progress row than to mislabel a live session as crashed).
pub(crate) fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // `pid_t` is signed: a stored pid that doesn't fit (a corrupt
        // record, or a sentinel like `u32::MAX`) must read as dead — an
        // `as` cast would wrap it negative, and `kill(-N, 0)` probes
        // process GROUP N, which can spuriously report alive.
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        if pid == 0 {
            return false;
        }
        let rc = unsafe { libc::kill(pid, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_registry(tag: &str) -> (PathBuf, SessionRegistry) {
        let dir =
            std::env::temp_dir().join(format!("stella-sessions-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (dir.clone(), SessionRegistry::open(dir))
    }

    #[test]
    fn upsert_list_status_remove_roundtrip() {
        let (dir, reg) = temp_registry("roundtrip");

        let mut rec = SessionRecord::new("/w/space", "space: fix the flaky test");
        rec.summary = "fix the flaky test in stella-tui".into();
        reg.upsert(&rec).unwrap();

        let listed = reg.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, rec.id);
        // Our own pid is alive, so the live status survives the sweep.
        assert_eq!(listed[0].status, SessionStatus::InProgress);

        assert!(reg.set_status(&rec.id, SessionStatus::Archived).unwrap());
        assert_eq!(reg.get(&rec.id).unwrap().status, SessionStatus::Archived);

        assert!(reg.remove(&rec.id).unwrap());
        assert!(!reg.remove(&rec.id).unwrap());
        assert!(reg.list().is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dead_pid_downgrades_live_statuses_to_error_at_read_time() {
        let (dir, reg) = temp_registry("deadpid");

        let mut crashed = SessionRecord::new("/w/a", "a");
        crashed.pid = u32::MAX - 1; // certainly not a live pid
        reg.upsert(&crashed).unwrap();

        let mut done = SessionRecord::new("/w/b", "b");
        done.id = format!("{}-b", done.id); // distinct id even within one ms
        done.pid = u32::MAX - 1;
        done.status = SessionStatus::Complete;
        reg.upsert(&done).unwrap();

        let listed = reg.list();
        let crashed_row = listed.iter().find(|r| r.id == crashed.id).unwrap();
        let done_row = listed.iter().find(|r| r.id == done.id).unwrap();
        // Live status + dead pid → presented as Error…
        assert_eq!(crashed_row.status, SessionStatus::Error);
        // …but the stored file is untouched, and terminal statuses are kept.
        assert_eq!(
            reg.get(&crashed.id).unwrap().status,
            SessionStatus::InProgress
        );
        assert_eq!(done_row.status, SessionStatus::Complete);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_skips_corrupt_files_and_sorts_newest_first() {
        let (dir, reg) = temp_registry("corrupt");

        let mut old = SessionRecord::new("/w/old", "old");
        old.started_at_ms = 1_000;
        reg.upsert(&old).unwrap();
        let mut new = SessionRecord::new("/w/new", "new");
        new.id = format!("{}-b", new.id); // distinct id even within one ms
        new.started_at_ms = 2_000;
        reg.upsert(&new).unwrap();
        std::fs::write(dir.join("garbage.json"), "not json").unwrap();

        let listed = reg.list();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, new.id);
        assert_eq!(listed[1].id, old.id);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paused_is_not_live_and_survives_the_owners_death() {
        let (dir, reg) = temp_registry("paused");
        let mut rec = SessionRecord::new("/w/p", "p");
        rec.pid = u32::MAX - 1; // certainly not a live pid
        rec.status = SessionStatus::Paused;
        reg.upsert(&rec).unwrap();
        // A paused session is deliberate, not crashed: no Error downgrade.
        assert_eq!(reg.list()[0].status, SessionStatus::Paused);
        assert!(!SessionStatus::Paused.is_live());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remove_deletes_the_sidecar_state_with_the_record() {
        let (dir, reg) = temp_registry("sidecar");
        let rec = SessionRecord::new("/w/s", "s");
        reg.upsert(&rec).unwrap();
        let sidecar = reg.sidecar_dir(&rec.id);
        crate::journal::write_queue(&sidecar, &["pending".into()]).unwrap();
        assert!(sidecar.exists());

        assert!(reg.remove(&rec.id).unwrap());
        assert!(!sidecar.exists(), "sidecar must not outlive its record");
        // Removing a missing record (and missing sidecar) stays a clean no.
        assert!(!reg.remove(&rec.id).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn registry_directory_and_record_are_owner_only_and_existing_modes_are_repaired() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        let reg = SessionRegistry::open(&dir);
        let rec = SessionRecord::new("/private/workspace", "private prompt");
        reg.upsert(&rec).unwrap();
        let record = reg.path_for(&rec.id);
        let mode = |path: &std::path::Path| {
            std::fs::symlink_metadata(path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&record), 0o600);

        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o666)).unwrap();
        reg.upsert(&rec).unwrap();
        assert_eq!(mode(&record), 0o600, "atomic replacement remains private");
    }

    #[cfg(unix)]
    #[test]
    fn registry_rejects_a_symlink_record_without_following_it() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let reg = SessionRegistry::open(&dir);
        let rec = SessionRecord::new("/w", "private");
        let target = tmp.path().join("outside.json");
        std::fs::write(&target, "outside").unwrap();
        symlink(&target, reg.path_for(&rec.id)).unwrap();

        assert!(reg.upsert(&rec).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "outside");
    }

    /// #617 item 12, the case that made the sweep unacceptable as filed. A
    /// power cut leaves a zero-length `<id>.json`; `list` skips it, so the
    /// session looks accounted-for by nothing. The sweep must still not touch
    /// it, because `history.json` beside it is the only continuable copy of the
    /// conversation.
    #[test]
    fn a_power_cut_record_is_damaged_not_orphaned_and_survives_the_sweep() {
        let (dir, reg) = temp_registry("powercut");
        let rec = SessionRecord::new("/w", "interrupted");
        reg.upsert(&rec).unwrap();

        // The session had written a conversation snapshot.
        let sidecar = reg.sidecar_dir(&rec.id);
        std::fs::create_dir_all(&sidecar).unwrap();
        let history = sidecar.join(crate::journal::HISTORY_FILE);
        std::fs::write(&history, b"[{\"role\":\"user\"}]").unwrap();

        // Now the power cut: the record is published but zero-length.
        std::fs::write(reg.path_for(&rec.id), b"").unwrap();

        // `list` cannot see it — that is the trap.
        assert!(
            reg.list().is_empty(),
            "a zero-length record is invisible to list; that is the premise"
        );

        // `scan` calls it damaged, NOT an orphan.
        let scan = reg.scan();
        assert!(scan.healthy.is_empty());
        assert_eq!(scan.damaged.len(), 1, "the record is damaged");
        assert!(
            scan.damaged[0].reason.contains("zero-length"),
            "the reason should name the interrupted write, got: {}",
            scan.damaged[0].reason
        );
        assert!(
            scan.damaged[0].has_sidecar,
            "the damaged record must be reported as having recoverable state"
        );
        assert!(
            scan.orphan_sidecars.is_empty(),
            "a sidecar whose record exists is never an orphan, got {:?}",
            scan.orphan_sidecars
        );

        // And the sweep leaves the conversation alone.
        assert!(reg.prune_orphan_sidecars().unwrap().is_empty());
        assert!(
            history.exists(),
            "the sweep deleted a resumable conversation"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The other half: a sidecar with genuinely no record beside it is the
    /// "stranded sidecar" `upsert`'s comment names, and IS reclaimable.
    #[test]
    fn a_sidecar_with_no_record_at_all_is_swept() {
        let (dir, reg) = temp_registry("orphan");
        crate::ensure_private_dir(&dir).unwrap();

        let stranded = reg.sidecar_dir("ses-1-1");
        std::fs::create_dir_all(&stranded).unwrap();
        std::fs::write(stranded.join(crate::journal::HISTORY_FILE), b"[]").unwrap();

        // A healthy session alongside it must be untouched.
        let live = SessionRecord::new("/w", "live");
        reg.upsert(&live).unwrap();
        let live_sidecar = reg.sidecar_dir(&live.id);
        std::fs::create_dir_all(&live_sidecar).unwrap();

        let scan = reg.scan();
        assert_eq!(scan.orphan_sidecars, vec!["ses-1-1".to_string()]);
        assert!(scan.damaged.is_empty());

        assert_eq!(
            reg.prune_orphan_sidecars().unwrap(),
            vec!["ses-1-1".to_string()]
        );
        assert!(!stranded.exists(), "the orphan should be reclaimed");
        assert!(
            live_sidecar.exists(),
            "a live session's sidecar is not an orphan"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn latest_resumable_wants_matching_workspace_state_and_no_live_owner() {
        let (dir, reg) = temp_registry("resumable");

        // Live (our own pid): never resumable, even with state on disk.
        let mut live = SessionRecord::new("/w/a", "live");
        reg.upsert(&live).unwrap();
        live = reg.get(&live.id).unwrap();
        crate::journal::write_history(&reg.sidecar_dir(&live.id), &[]).unwrap();

        // Dead + state, with explicit activity stamps (bypassing upsert's
        // restamp, as the prune test does) so the winner is deterministic
        // even when every upsert lands in the same millisecond.
        let pin_updated = |mut rec: SessionRecord, updated_at_ms: u64| {
            rec.pid = u32::MAX - 1;
            rec.status = SessionStatus::Paused;
            reg.upsert(&rec).unwrap();
            rec.updated_at_ms = updated_at_ms;
            std::fs::write(
                dir.join(format!("{}.json", rec.id)),
                serde_json::to_string(&rec).unwrap(),
            )
            .unwrap();
            crate::journal::write_history(&reg.sidecar_dir(&rec.id), &[]).unwrap();
            rec
        };
        let mut old = SessionRecord::new("/w/a", "old");
        old.id = format!("{}-old", old.id);
        let old = pin_updated(old, 1_000);
        let mut newest = SessionRecord::new("/w/a", "newest");
        newest.id = format!("{}-new", newest.id);
        let newest = pin_updated(newest, 2_000);

        // Dead, right workspace, but nothing on disk to restore.
        let mut bare = SessionRecord::new("/w/a", "bare");
        bare.id = format!("{}-bare", bare.id);
        bare.pid = u32::MAX - 1;
        bare.status = SessionStatus::Complete;
        reg.upsert(&bare).unwrap();

        // Other workspace.
        let mut other = SessionRecord::new("/w/b", "other");
        other.id = format!("{}-other", other.id);
        other.pid = u32::MAX - 1;
        other.status = SessionStatus::Paused;
        reg.upsert(&other).unwrap();
        crate::journal::write_history(&reg.sidecar_dir(&other.id), &[]).unwrap();

        assert!(!reg.resumable(&live.id));
        assert!(!reg.resumable(&bare.id));
        assert!(reg.resumable(&old.id));
        let picked = reg.latest_resumable("/w/a").expect("one resumable");
        assert_eq!(picked.id, newest.id);
        assert_eq!(reg.latest_resumable("/w/none"), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_sweeps_only_old_terminal_records() {
        let (dir, reg) = temp_registry("prune");

        let mut live = SessionRecord::new("/w/live", "live");
        reg.upsert(&live).unwrap();
        live = reg.get(&live.id).unwrap();

        let mut done = SessionRecord::new("/w/done", "done");
        done.id = format!("{}-d", done.id);
        done.status = SessionStatus::Complete;
        reg.upsert(&done).unwrap();
        // Backdate the terminal record past any cutoff (bypass upsert's
        // restamping by rewriting the file directly).
        let mut old = reg.get(&done.id).unwrap();
        old.updated_at_ms = 1;
        std::fs::write(
            dir.join(format!("{}.json", old.id)),
            serde_json::to_string(&old).unwrap(),
        )
        .unwrap();

        let removed = reg.prune(60_000).unwrap();
        assert_eq!(removed, 1);
        assert!(reg.get(&done.id).is_none());
        assert_eq!(reg.get(&live.id).unwrap().id, live.id);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A paused session is a deliberate promise that its state is kept for
    /// `resume` — it must survive the age sweep however old it gets. It used
    /// not to: `Paused` is not `is_live()`, so the sweep treated it as
    /// terminal and deleted the sidecar `latest_resumable` exists to find.
    #[test]
    fn prune_spares_a_paused_session_however_old() {
        let (dir, reg) = temp_registry("prune-paused");

        let mut paused = SessionRecord::new("/w/paused", "paused");
        paused.status = SessionStatus::Paused;
        reg.upsert(&paused).unwrap();
        let mut old = reg.get(&paused.id).unwrap();
        old.updated_at_ms = 1;
        std::fs::write(
            dir.join(format!("{}.json", old.id)),
            serde_json::to_string(&old).unwrap(),
        )
        .unwrap();

        let removed = reg.prune(60_000).unwrap();
        assert_eq!(removed, 0, "a paused session must not age out");
        assert!(reg.get(&paused.id).is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Invariant 4 for the field #1552 added: a supervised record survives the
    /// write/read the registry actually performs, not just `to_string`.
    #[test]
    fn a_supervised_record_round_trips_through_the_registry() {
        let (dir, reg) = temp_registry("supervised-roundtrip");

        let mut rec = SessionRecord::new("/w/space", "space: long run");
        rec.supervisor = Some(SupervisorInfo { pgid: 4242 });
        reg.upsert(&rec).unwrap();

        let read = reg.get(&rec.id).expect("record readable");
        assert_eq!(read.supervisor, Some(SupervisorInfo { pgid: 4242 }));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every record on every machine that ever ran stella predates the
    /// `supervisor` field. They must keep parsing: `list` drops what it cannot
    /// parse, so a required field here would make every existing session
    /// vanish from the SESSIONS view on upgrade — and take `resume` with it.
    #[test]
    fn a_record_written_before_supervision_still_parses() {
        let pre_1552 = r#"{
            "id": "ses-1-2",
            "pid": 2,
            "workspace": "/w/old",
            "title": "old: a session from before",
            "summary": "",
            "status": "complete",
            "started_at_ms": 1,
            "updated_at_ms": 1
        }"#;

        let record: SessionRecord = serde_json::from_str(pre_1552).expect("legacy record parses");
        assert_eq!(record.supervisor, None);
        assert!(record.exploring.is_empty());
    }

    /// The absent case must stay absent on disk too. `list` is read by the
    /// deck on every refresh, and a `"supervisor": null` in every record is
    /// bytes and confusion bought for nothing.
    #[test]
    fn an_unsupervised_record_writes_no_supervisor_key() {
        let rec = SessionRecord::new("/w/space", "space: ordinary");
        let json = serde_json::to_string(&rec).unwrap();
        assert!(
            !json.contains("supervisor"),
            "unsupervised records must not carry the key: {json}"
        );
    }
}
