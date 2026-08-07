//! The durable record of everything an agent did to a workspace, kept in a
//! git repository that lives in stella's own directory.
//!
//! # Why git, and why not the user's git
//!
//! Every requirement durability has, git already implements: content-addressed
//! storage, automatic dedup of unchanged files, zlib compression, `gc` as an
//! end-of-session compaction step, and `show`/`checkout` as replay. Rolling a
//! directory of JSON snapshots instead would duplicate the whole transcript per
//! step and dedup nothing.
//!
//! What is NOT reused is the *user's* repository. The git dir lives at
//! `data_dir()/work/<local-workspace-id>.git` and the workspace is attached as
//! a work tree (`--git-dir` + `--work-tree`). That buys three things at once:
//!
//! - **The workspace need not be a git repo.** Durability stops being a
//!   property of how the user set up their directory.
//! - **The user's repository is never touched.** No shared object store, no
//!   ref namespace collision, no interaction with their `gc`. Their index,
//!   `HEAD`, branch, and staged changes are all untouched, and stella's edits
//!   appear to them as ordinary unstaged changes — which is what they are.
//! - **`.gitignore` still applies**, because it is read from the work tree. A
//!   build directory the agent wrote into does not bloat the store.
//!
//! # Never `HEAD`, never a shared index
//!
//! Two sessions may work one workspace at once, so nothing here touches `HEAD`
//! or the default index. Each session commits with plumbing —
//! `read-tree` → `add` → `write-tree` → `commit-tree` → `update-ref` — onto
//! its own `refs/stella/<session>/head`, using its own `GIT_INDEX_FILE`. Two
//! sessions therefore share the object store (which is what gives cross-session
//! dedup) and contend over nothing.
//!
//! # Keyed on identity, not path
//!
//! The store is named by [`crate::workspace_local`]'s id, so moving or renaming
//! the workspace keeps its history. Keying on the canonical path — the obvious
//! choice — would orphan everything on the first `mv`.
//!
//! # Retention
//!
//! Every session leaves a permanent ref namespace behind, and `gc` is required
//! to keep whatever a ref reaches — so compaction alone makes this store
//! smaller, never smaller *forever*. [`WorkJournal::prune`] is the deletion
//! path, built to the same shape as `store.db`'s [`crate::prune`]: an age
//! cutoff plus a ceiling, both opt-in, run when a command asks rather than on
//! every exit.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{Result, StoreError};

/// A session's handle on the workspace's durable history.
#[derive(Debug, Clone)]
pub struct WorkJournal {
    git_dir: PathBuf,
    work_tree: PathBuf,
    index_file: PathBuf,
    session: String,
    /// The directory the whole store lives in — this workspace's git dir and
    /// every session's index file. Kept because [`WorkJournal::prune`] reaches
    /// *other* sessions' sidecar indexes, which are named from it.
    store_root: PathBuf,
    /// [`crate::workspace_local`]'s id for this workspace, the prefix every
    /// file in the store is named after. Held rather than re-derived from
    /// `git_dir`, which would mean parsing an id back out of a filename.
    workspace_id: String,
}

/// Where a session's commits accumulate. Per-session so concurrent sessions
/// never contend on a ref.
fn session_ref(session: &str) -> String {
    format!("refs/stella/{session}/head")
}

/// The marker a caller can replay from: `refs/stella/<session>/turn/<n>`.
fn turn_ref(session: &str, turn: u32) -> String {
    format!("refs/stella/{session}/turn/{turn}")
}

/// The reserved directory stella's own records live under, so a blob can
/// never collide with a real workspace path — and so a per-turn diff can
/// filter the records back out ([`WorkJournal::changed_paths_at_turn`]).
const JOURNAL_DIR: &str = ".stella-journal";

/// One reserved record's path under [`JOURNAL_DIR`].
fn journal_blob_path(name: &str) -> String {
    format!("{JOURNAL_DIR}/{name}")
}

/// Where one session's private git index lives, beside the store's git dir
/// and named after the same workspace id.
fn index_file_path(store_root: &Path, workspace_id: &str, session: &str) -> PathBuf {
    store_root.join(format!("{workspace_id}.{session}.index"))
}

/// A prune that removes at least this many refs runs `gc` on its own, so a
/// large reclaim hands the space back without a second command. Mirrors
/// `prune.rs`'s `LARGE_PRUNE_ROWS`, scaled to refs.
const LARGE_PRUNE_REFS: u64 = 100;

/// Retention knobs for [`WorkJournal::prune`] — the work journal's answer to
/// what [`crate::prune::StorePrunePolicy`] is for `store.db`, and deliberately
/// the same shape: an age cutoff, a hard ceiling, both opt-in, and a policy
/// carrying neither is a no-op that deletes nothing.
///
/// The unit of retention is the **session**, because that is what the ref
/// namespace is keyed on: a session owns `refs/stella/<session>/head` plus one
/// `refs/stella/<session>/turn/<n>` per turn, and dropping some of a session's
/// turns would leave a truncated history that reads as a complete one.
#[derive(Debug, Clone, Default)]
pub struct JournalPrunePolicy {
    /// Sessions whose last commit is at least this old are dropped; `None`
    /// disables age pruning, and [`Duration::ZERO`] therefore reaches every
    /// session the guard does not protect. Measured against the commit's own
    /// committer time — the store has no clock of its own, and the tip commit
    /// is the last moment the session demonstrably did anything.
    pub older_than: Option<Duration>,
    /// Hard ceiling on retained sessions; the oldest are evicted until at or
    /// under it. `None` disables the ceiling.
    pub max_sessions: Option<usize>,
    /// Reclaim the space immediately with `gc --prune=now` (also happens
    /// automatically for a large prune). Without it the deleted sessions'
    /// objects are merely unreachable, and the next `gc` collects them on
    /// git's own expiry schedule.
    pub gc: bool,
    /// Compute the report without deleting anything.
    pub dry_run: bool,
}

impl JournalPrunePolicy {
    /// Whether this policy would do nothing at all — the caller turns this
    /// into a helpful error rather than a silent success.
    pub fn is_noop(&self) -> bool {
        self.older_than.is_none() && self.max_sessions.is_none()
    }
}

/// What [`WorkJournal::prune`] did (or, under `dry_run`, would do).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JournalPruneReport {
    /// Sessions removed by the age cutoff.
    pub aged_out: u64,
    /// Sessions evicted to satisfy the ceiling.
    pub ceiling_evicted: u64,
    /// Refs deleted across both phases — one `head` per session plus its turn
    /// marks. This is the number that matters: a session's objects stay
    /// reachable, and so unreclaimable, until its last ref is gone.
    pub refs_deleted: u64,
    /// Sidecar index files removed, one per pruned session that had one.
    pub indexes_removed: u64,
    /// Sessions still above the ceiling afterwards, because the only ones left
    /// to evict were protected (the running session is never pruned).
    pub still_over_ceiling: u64,
    /// Whether `gc` ran.
    pub gc_ran: bool,
}

impl JournalPruneReport {
    /// Total sessions removed across both phases.
    pub fn total_sessions(&self) -> u64 {
        self.aged_out + self.ceiling_evicted
    }
}

/// The reserved blob holding the in-flight turn's resume point.
///
/// The checkpoint lives *here*, in the same commit graph as the work it
/// describes, and not in a transient file beside it. A resume point and the
/// file changes it is a resume point *for* are one fact; splitting them across
/// two stores means a recovery path has to decide which one to believe when
/// they disagree — and they will, because nothing can update both atomically.
pub const CHECKPOINT_BLOB: &str = "checkpoint.json";

/// The reserved blob holding the session's staleness map — `path → digest this
/// session last saw`, the state behind the no-clobber guarantee.
///
/// Written in the same commit as [`CHECKPOINT_BLOB`] and **not** retracted with
/// it. The two have different lifetimes, and confusing them would quietly
/// disarm the guard: a checkpoint describes one turn and must not outlive it,
/// while the staleness map describes what this *session* has seen and has to
/// outlive every turn in it. A session's second turn is exactly as entitled to
/// the guarantee as its first.
pub const OBSERVED_BLOB: &str = "observed.json";

/// The reserved blob naming the staged pipeline the in-flight turn is running
/// *inside*.
///
/// A deliberately separate blob rather than a widened [`CHECKPOINT_BLOB`]:
/// the checkpoint is the engine's own shape and the engine must not learn
/// pipeline shapes (architecture invariant 1). It shares the checkpoint's
/// lifetime instead of the staleness map's — a frame describes one turn's
/// staging and is retracted by [`WorkJournal::clear_checkpoint`] along with
/// the resume point, because a frame that outlived its turn would tell the
/// next resume it is re-entering a pipeline that already finished.
pub const PIPELINE_BLOB: &str = "pipeline.json";

impl WorkJournal {
    /// Open (creating on first use) the durable history for `workspace_root`.
    ///
    /// `session` must be filesystem- and ref-safe; session ids (`ses-…`) are.
    pub fn open(workspace_root: &Path, session: &str) -> Result<Self> {
        Self::open_in(
            &crate::usage::data_dir().join("work"),
            workspace_root,
            session,
        )
    }

    /// [`Self::open`] against an explicit store root.
    ///
    /// The root is a parameter rather than always `data_dir()` so this type
    /// has no dependency on process-global state. That is not only for tests:
    /// `data_dir()` reads `STELLA_HOME`, and a type that reads it internally
    /// cannot be exercised by two concurrent callers wanting different stores.
    pub fn open_in(store_root: &Path, workspace_root: &Path, session: &str) -> Result<Self> {
        let identity = crate::workspace_local::resolve(workspace_root)?;
        let root = store_root.to_path_buf();
        std::fs::create_dir_all(&root)
            .map_err(|e| StoreError(format!("cannot create work-journal root: {e}")))?;
        let journal = Self {
            git_dir: root.join(format!("{}.git", identity.id)),
            work_tree: identity.path,
            index_file: index_file_path(&root, &identity.id, session),
            session: session.to_string(),
            store_root: root,
            workspace_id: identity.id,
        };
        journal.ensure_repo()?;
        Ok(journal)
    }

    fn ensure_repo(&self) -> Result<()> {
        if self.git_dir.join("HEAD").exists() {
            return Ok(());
        }
        run(Command::new("git").args(["init", "-q", "--bare", &self.git_dir.to_string_lossy()]))?;
        // A bare repo refuses a work tree. Everything else about bare — no
        // checkout of its own, no branch to confuse — is exactly what is
        // wanted, so flip only this one bit.
        self.git(&["config", "core.bare", "false"])?;
        // The store is stella's own; it must never depend on whether the user
        // has configured a git identity.
        self.git(&["config", "user.name", "stella"])?;
        self.git(&["config", "user.email", "stella@localhost"])?;
        Ok(())
    }

    fn git(&self, args: &[&str]) -> Result<String> {
        let mut cmd = Command::new("git");
        cmd.arg("--git-dir")
            .arg(&self.git_dir)
            .arg("--work-tree")
            .arg(&self.work_tree)
            .args(args)
            .env("GIT_INDEX_FILE", &self.index_file)
            // The work tree is the user's; a hook or config of theirs must
            // never run as a side effect of stella recording history.
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &self.git_dir);
        run(&mut cmd)
    }

    /// The commit this session last recorded, if any.
    ///
    /// Public because marking a turn needs it: a caller that records several
    /// mutations in a turn marks the turn from wherever the session ended up,
    /// rather than threading the last commit id back out of every write.
    pub fn session_tip(&self) -> Option<String> {
        self.git(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &session_ref(&self.session),
        ])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    }

    /// Record `paths` as they stand on disk, plus any `blobs` (in-memory
    /// content that has no file — the turn checkpoint, the staleness map),
    /// as one commit on this session's ref.
    ///
    /// A blob whose content is `None` is *removed* from the record. That is
    /// what lets a turn ending retract its resume point in the same substrate
    /// that wrote it, rather than needing a second store with its own delete.
    ///
    /// Returns the new commit id. Paths are workspace-relative and are staged
    /// individually — never `git add -A`, which would sweep in the user's own
    /// unrelated work and make the history a lie about what the agent did.
    pub fn record(
        &self,
        paths: &[String],
        blobs: &[(&str, Option<&str>)],
        message: &str,
    ) -> Result<String> {
        let parent = self.session_tip();

        // Seed the index from the parent so this commit is the parent's tree
        // plus these changes, not just these changes.
        match &parent {
            Some(p) => self.git(&["read-tree", p])?,
            None => self.git(&["read-tree", "--empty"])?,
        };

        for path in paths {
            // `git add` on an ignored path is a hard error, not a skip. An
            // agent that writes into a build directory would otherwise fail
            // the whole record — so ignored paths are filtered deliberately
            // rather than discovered by exception.
            if self.is_ignored(path) {
                continue;
            }
            // A path that vanished (the agent deleted it) is staged as a
            // removal; `--ignore-unmatch` keeps that from being an error.
            if self.work_tree.join(path).exists() {
                self.git(&["add", "--", path])?;
            } else {
                self.git(&["rm", "--cached", "--ignore-unmatch", "-q", "--", path])?;
            }
        }

        for (name, content) in blobs {
            let entry = journal_blob_path(name);
            match content {
                Some(content) => {
                    let oid = self.hash_blob(content)?;
                    // 100644 = a regular file. These are stella's own records,
                    // kept under a reserved prefix so they can never collide
                    // with a real workspace path.
                    self.git(&[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &format!("100644,{oid},{entry}"),
                    ])?;
                }
                // `--force-remove` drops the entry whether or not the file
                // exists in the work tree — and it never does for these, which
                // is the whole point of a blob: it has no file.
                None => {
                    self.git(&["update-index", "--force-remove", "--", &entry])?;
                }
            }
        }

        let tree = self.git(&["write-tree"])?.trim().to_string();
        let mut args: Vec<String> = vec!["commit-tree".into(), tree, "-m".into(), message.into()];
        if let Some(p) = &parent {
            args.push("-p".into());
            args.push(p.clone());
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let commit = self.git(&refs)?.trim().to_string();
        self.git(&["update-ref", &session_ref(&self.session), &commit])?;
        Ok(commit)
    }

    /// Whether the work tree's own ignore rules exclude `path`.
    ///
    /// `check-ignore` exits 1 for "not ignored", which [`run`] reports as an
    /// error — so this reads the exit status directly rather than going
    /// through it. Unreadable or ambiguous cases answer "not ignored": the
    /// cost of storing one extra file is trivial next to silently dropping a
    /// real one from the durable record.
    fn is_ignored(&self, path: &str) -> bool {
        Command::new("git")
            .arg("--git-dir")
            .arg(&self.git_dir)
            .arg("--work-tree")
            .arg(&self.work_tree)
            .args(["check-ignore", "-q", "--", path])
            .env("GIT_INDEX_FILE", &self.index_file)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &self.git_dir)
            .current_dir(&self.work_tree)
            .status()
            .is_ok_and(|s| s.success())
    }

    fn hash_blob(&self, content: &str) -> Result<String> {
        let mut cmd = Command::new("git");
        cmd.arg("--git-dir")
            .arg(&self.git_dir)
            .args(["hash-object", "-w", "--stdin"])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| StoreError(format!("cannot run git hash-object: {e}")))?;
        {
            use std::io::Write as _;
            let stdin = child.stdin.as_mut().expect("piped");
            stdin
                .write_all(content.as_bytes())
                .map_err(|e| StoreError(format!("cannot write blob: {e}")))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| StoreError(format!("git hash-object failed: {e}")))?;
        if !out.status.success() {
            return Err(StoreError(format!(
                "git hash-object failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Write the in-flight turn's resume point, replacing any earlier one, and
    /// the staleness map that belongs with it.
    ///
    /// One commit per step boundary. Measurably dearer than an atomic file
    /// write — about 27ms against 8ms for a transcript-sized snapshot — and
    /// the right trade anyway: a step is a model call, so the difference is
    /// noise beside it, and what it buys is that the resume point and the file
    /// changes it describes share one commit graph instead of needing to be
    /// reconciled across two stores.
    ///
    /// `observed` rides along rather than getting a commit of its own, which is
    /// what makes persisting it free. A step boundary is also the *right*
    /// moment for it: the map is updated by reads as well as writes, and a
    /// scheme that saved it only when a file changed would drop every read
    /// since the last mutation — the exact loss that leaves a resumed session
    /// acting on content it believes it knows with the guard no longer watching
    /// (see `ToolRegistry::restore_observed`). `None` leaves whatever was last
    /// written in place.
    ///
    /// `pipeline` rides along for the same reason and with the *checkpoint's*
    /// lifetime rather than the map's: it names the staged pipeline this turn
    /// is running inside ([`PIPELINE_BLOB`]), and a resume that read a frame
    /// from one commit and a transcript from another could restore a pipeline
    /// the transcript was never part of. `None` writes no frame, which is
    /// exactly what a plain engine turn is.
    pub fn record_checkpoint(
        &self,
        json: &str,
        observed: Option<&str>,
        pipeline: Option<&str>,
    ) -> Result<String> {
        let mut blobs: Vec<(&str, Option<&str>)> = vec![(CHECKPOINT_BLOB, Some(json))];
        if let Some(observed) = observed {
            blobs.push((OBSERVED_BLOB, Some(observed)));
        }
        if let Some(pipeline) = pipeline {
            blobs.push((PIPELINE_BLOB, Some(pipeline)));
        }
        self.record(&[], &blobs, "stella: turn checkpoint")
    }

    /// The session's staleness map as of this session's tip, or `None` when
    /// this session never observed a file.
    pub fn observed(&self) -> Option<String> {
        self.blob_at_tip(OBSERVED_BLOB)
    }

    /// The staged-pipeline frame the interrupted turn was running inside, or
    /// `None` when the turn was a plain engine turn (or the record is
    /// unreadable — both mean "resume it as a bare turn").
    pub fn pipeline_frame(&self) -> Option<String> {
        self.blob_at_tip(PIPELINE_BLOB)
    }

    /// Retract the resume point — the turn it described reached a terminal
    /// outcome, and a checkpoint that outlives its turn invites a resume that
    /// replays work the caller already saw finish.
    ///
    /// The [`PIPELINE_BLOB`] frame goes with it, in the same commit: the two
    /// describe the same turn, and a frame left behind by a turn that ended
    /// would tell the next resume it is re-entering a pipeline nobody is
    /// running.
    ///
    /// Idempotent, and cheap when there is nothing to retract: every terminal
    /// path discards unconditionally, including turns that ended before their
    /// first step boundary, so the common case must not cost a commit. Two
    /// `cat-file -e`s answer that.
    pub fn clear_checkpoint(&self) -> Result<()> {
        let mut retract: Vec<(&str, Option<&str>)> = Vec::new();
        if self.checkpoint().is_some() {
            retract.push((CHECKPOINT_BLOB, None));
        }
        if self.pipeline_frame().is_some() {
            retract.push((PIPELINE_BLOB, None));
        }
        if retract.is_empty() {
            return Ok(());
        }
        self.record(&[], &retract, "stella: turn checkpoint retired")?;
        Ok(())
    }

    /// The in-flight turn's resume point as of this session's tip, or `None`
    /// when no turn was interrupted.
    ///
    /// An unreadable record answers `None` for the same reason a missing one
    /// does: both mean "nothing to resume from", and the recovery path is
    /// identical — re-run the prompt.
    pub fn checkpoint(&self) -> Option<String> {
        self.blob_at_tip(CHECKPOINT_BLOB)
    }

    /// One reserved blob as of this session's tip.
    fn blob_at_tip(&self, name: &str) -> Option<String> {
        let tip = self.session_tip()?;
        self.git(&["show", &format!("{tip}:{}", journal_blob_path(name))])
            .ok()
    }

    /// Mark `commit` as the state at the end of `turn`, so it can be replayed
    /// by turn number rather than by commit id.
    ///
    /// Called at every deck turn's end, through
    /// `stella_cli::durability::SessionDurability::mark_turn_end`, which
    /// derives `turn` from [`Self::last_marked_turn`] rather than from a
    /// counter — see there for why that distinction has teeth across a resume.
    pub fn mark_turn(&self, turn: u32, commit: &str) -> Result<()> {
        self.git(&["update-ref", &turn_ref(&self.session, turn), commit])?;
        Ok(())
    }

    /// The highest turn this session has marked, or `None` when it has marked
    /// none.
    ///
    /// The point of asking the refs rather than keeping a counter: a counter
    /// lives in one process and a session outlives its processes. A resumed
    /// session that started counting again at 1 would re-mark turn 3 over the
    /// turn 3 that ran before the interruption — silently, and destroying the
    /// only record of the earlier one. The namespace already knows, so this
    /// reads it.
    ///
    /// A ref whose trailing segment is not a number is skipped rather than
    /// treated as an error: this answers "what is the largest turn safely past"
    /// and an unparseable neighbour does not change that answer.
    pub fn last_marked_turn(&self) -> Option<u32> {
        let prefix = format!("refs/stella/{}/turn/", self.session);
        let pattern = format!("{prefix}*");
        let out = self
            .git(&["for-each-ref", "--format=%(refname)", &pattern])
            .ok()?;
        out.lines()
            .filter_map(|line| line.trim().strip_prefix(prefix.as_str()))
            .filter_map(|turn| turn.parse::<u32>().ok())
            .max()
    }

    /// The content of `path` as it stood at the end of `turn`.
    ///
    /// The refs this reads through are written by every real session as of
    /// the change that wired [`Self::mark_turn`]. Its production consumer is
    /// the per-turn diff (#1870): `stella-cli`'s `turn_diff` module pairs
    /// [`Self::changed_paths_at_turn`] with this on each side of the boundary
    /// to shape the hunks `session_turn_diffs` persists.
    pub fn read_at_turn(&self, turn: u32, path: &str) -> Result<String> {
        self.git(&["show", &format!("{}:{path}", turn_ref(&self.session, turn))])
    }

    /// One of the reserved journal blobs as it stood at the end of `turn`.
    ///
    /// Unlike [`Self::read_at_turn`] this still has no production consumer:
    /// the per-turn diff deliberately filters the reserved records out. Kept
    /// because the marks are written anyway and a historical checkpoint read
    /// is the obvious next replay surface; if that never lands, delete this
    /// alone.
    pub fn blob_at_turn(&self, turn: u32, name: &str) -> Result<String> {
        self.read_at_turn(turn, &journal_blob_path(name))
    }

    /// Workspace paths whose recorded content differs between the end of
    /// `turn - 1` and the end of `turn` — the name half of a per-turn diff
    /// (#1870); the content halves come from [`Self::read_at_turn`] on each
    /// side. This is [`Self::read_at_turn`]'s first production consumer,
    /// closing the "writers but no reader" standing its doc records.
    ///
    /// A turn whose predecessor was never marked (the session's first, or a
    /// mark lost to pruning) diffs against nothing: every path in its tree is
    /// new. Reserved journal records ([`JOURNAL_DIR`]) are never workspace
    /// paths and are filtered out.
    ///
    /// **Read-only by construction.** Both shapes are tree-to-tree plumbing
    /// (`diff-tree`, `ls-tree`) that touches neither an index nor the work
    /// tree, so a live session committing to this shared store is never
    /// blocked or altered by a reader computing a diff — the observatory-side
    /// constraint #1870 states, discharged here at the only layer that opens
    /// the repo at all.
    pub fn changed_paths_at_turn(&self, turn: u32) -> Result<Vec<String>> {
        let target = turn_ref(&self.session, turn);
        let base = turn
            .checked_sub(1)
            .filter(|&prev| prev > 0)
            .map(|prev| turn_ref(&self.session, prev))
            .filter(|name| self.ref_exists(name));
        let listing = match base {
            Some(base) => self.git(&["diff-tree", "-r", "--name-only", &base, &target])?,
            None => self.git(&["ls-tree", "-r", "--name-only", &target])?,
        };
        // The prefix carries its slash: a real workspace file that merely
        // starts with the reserved name (`.stella-journal-notes`) is the
        // agent's work and must survive the filter.
        let reserved = format!("{JOURNAL_DIR}/");
        Ok(listing
            .lines()
            .map(str::trim)
            .filter(|path| !path.is_empty() && !path.starts_with(reserved.as_str()))
            .map(String::from)
            .collect())
    }

    /// Whether `name` currently resolves to a commit.
    fn ref_exists(&self, name: &str) -> bool {
        self.git(&["rev-parse", "--verify", "--quiet", name])
            .is_ok_and(|out| !out.trim().is_empty())
    }

    /// Compact the object store. The end-of-session step: everything stays
    /// replayable, it simply stops being loose objects.
    ///
    /// Called from the deck's exit path and from the one-shot drivers'
    /// `SessionPresence::finish`, both through
    /// `stella_cli::durability::SessionDurability::compact`, and best-effort at
    /// both — housekeeping must never delay or fail an exit.
    ///
    /// `--auto` is what makes calling it unconditionally reasonable: git checks
    /// the loose-object count against its own `gc.auto` threshold and returns
    /// immediately when it is under, so a short session pays one subprocess and
    /// a long-lived workspace gets packed without anyone having to decide when.
    ///
    /// Compaction is not retention and cannot stand in for it: `gc` reclaims
    /// nothing that a ref still reaches, and every session this store has ever
    /// held leaves refs behind. [`Self::prune`] is the path that removes them.
    pub fn compact(&self) -> Result<()> {
        self.git(&["gc", "--quiet", "--auto"])?;
        Ok(())
    }

    /// Bound the store's growth per a [`JournalPrunePolicy`]. Runs, in order:
    /// age cutoff → session ceiling, then (optionally) `gc --prune=now`.
    ///
    /// # Why deleting refs is the whole job
    ///
    /// Every commit this store holds is reachable from
    /// `refs/stella/<session>/head` or one of that session's turn marks, and a
    /// reachable object is one `gc` is *required* to keep. So the retention
    /// path is not a compaction knob — it is `update-ref -d` over a session's
    /// entire namespace, after which its objects are unreachable and any `gc`,
    /// including the automatic one [`Self::compact`] runs, can collect them.
    /// A session's refs go in one transactional batch: half a namespace left
    /// standing keeps the whole history reachable, which would report a prune
    /// that reclaimed nothing.
    ///
    /// **The running session is never pruned**, whatever the policy says —
    /// this is the analogue of `store.db`'s un-replicated-telemetry guard, and
    /// like it, a ceiling the guard cannot satisfy is reported as
    /// [`JournalPruneReport::still_over_ceiling`] rather than forced. Nothing
    /// here can tell whether *another* session is live, so that is what the
    /// age cutoff is for; a policy that keeps a day of history cannot reach a
    /// session that committed a minute ago.
    ///
    /// `dry_run` reports what would go without touching a ref.
    pub fn prune(&self, policy: &JournalPrunePolicy) -> Result<JournalPruneReport> {
        let mut report = JournalPruneReport::default();
        if policy.is_noop() {
            return Ok(report);
        }
        let mut retained = self.recorded_sessions()?;
        let mut doomed: Vec<String> = Vec::new();

        if let Some(older_than) = policy.older_than {
            let cutoff = unix_now().saturating_sub(older_than.as_secs() as i64);
            retained.retain(|(session, at)| {
                if *at <= cutoff && *session != self.session {
                    doomed.push(session.clone());
                    return false;
                }
                true
            });
            report.aged_out = doomed.len() as u64;
        }

        if let Some(max_sessions) = policy.max_sessions
            && retained.len() > max_sessions
        {
            let mut excess = retained.len() - max_sessions;
            // `recorded_sessions` is oldest-first, so this evicts in the same
            // order `store.db`'s ceiling does.
            retained.retain(|(session, _)| {
                if excess > 0 && *session != self.session {
                    excess -= 1;
                    doomed.push(session.clone());
                    report.ceiling_evicted += 1;
                    return false;
                }
                true
            });
            report.still_over_ceiling = retained.len().saturating_sub(max_sessions) as u64;
        }

        for session in &doomed {
            let refs = self.session_refs(session)?;
            report.refs_deleted += refs.len() as u64;
            if policy.dry_run {
                continue;
            }
            self.delete_refs(&refs)?;
            let index = index_file_path(&self.store_root, &self.workspace_id, session);
            if std::fs::remove_file(&index).is_ok() {
                report.indexes_removed += 1;
            }
        }

        if !policy.dry_run
            && report.refs_deleted > 0
            && (policy.gc || report.refs_deleted >= LARGE_PRUNE_REFS)
        {
            self.git(&["gc", "--quiet", "--prune=now"])?;
            report.gc_ran = true;
        }
        Ok(report)
    }

    /// Every session this store holds history for, oldest tip first, paired
    /// with that tip's committer time.
    ///
    /// Read off `head` rather than the turn marks: a turn mark points at an
    /// ancestor, so the head is the session's most recent moment, and a
    /// session whose refname does not have the shape this module writes is
    /// skipped rather than guessed at.
    fn recorded_sessions(&self) -> Result<Vec<(String, i64)>> {
        let listing = self.git(&[
            "for-each-ref",
            "--format=%(committerdate:unix) %(refname)",
            "refs/stella/",
        ])?;
        let mut sessions: Vec<(String, i64)> = Vec::new();
        for line in listing.lines() {
            let Some((at, refname)) = line.trim().split_once(' ') else {
                continue;
            };
            let Some(session) = refname
                .strip_prefix("refs/stella/")
                .and_then(|rest| rest.strip_suffix("/head"))
                .filter(|session| !session.is_empty() && !session.contains('/'))
            else {
                continue;
            };
            let Ok(at) = at.parse::<i64>() else {
                continue;
            };
            sessions.push((session.to_string(), at));
        }
        sessions.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        Ok(sessions)
    }

    /// Every ref in one session's namespace — its head and all of its turn
    /// marks.
    fn session_refs(&self, session: &str) -> Result<Vec<String>> {
        let listing = self.git(&[
            "for-each-ref",
            "--format=%(refname)",
            &format!("refs/stella/{session}/"),
        ])?;
        Ok(listing
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect())
    }

    /// Delete `refs` in one `update-ref --stdin` transaction: they either all
    /// go or none do, so a failure can never leave a session's objects
    /// reachable through a surviving turn mark.
    fn delete_refs(&self, refs: &[String]) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        let mut input = String::new();
        for name in refs {
            input.push_str("delete ");
            input.push_str(name);
            input.push('\n');
        }
        let mut cmd = Command::new("git");
        cmd.arg("--git-dir")
            .arg(&self.git_dir)
            .args(["update-ref", "--stdin"])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &self.git_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| StoreError(format!("cannot run git update-ref: {e}")))?;
        {
            use std::io::Write as _;
            let stdin = child.stdin.as_mut().expect("piped");
            stdin
                .write_all(input.as_bytes())
                .map_err(|e| StoreError(format!("cannot write ref deletions: {e}")))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| StoreError(format!("git update-ref failed: {e}")))?;
        if !out.status.success() {
            return Err(StoreError(format!(
                "git update-ref failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }
}

/// Wall-clock seconds since the epoch, the same unit `git` reports a
/// committer time in.
fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn run(cmd: &mut Command) -> Result<String> {
    let out = cmd
        .output()
        .map_err(|e| StoreError(format!("cannot run git: {e}")))?;
    if !out.status.success() {
        return Err(StoreError(format!(
            "git {:?} failed: {}",
            cmd.get_args().collect::<Vec<_>>(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch workspace and its own store root. No environment variable
    /// is touched, so these run correctly in parallel with everything else —
    /// an earlier version set `STELLA_HOME` and the tests raced each other.
    fn scratch() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let guard = tempfile::tempdir().unwrap();
        let ws = guard.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let store = guard.path().join("store");
        (guard, ws, store)
    }

    #[test]
    fn a_non_git_workspace_still_gets_durable_replayable_history() {
        // The whole point: durability must not depend on the user having run
        // `git init`.
        let (_guard, ws, store) = scratch();
        assert!(!ws.join(".git").exists(), "the workspace is not a git repo");
        std::fs::write(ws.join("a.txt"), "v1\n").unwrap();

        let journal = WorkJournal::open_in(&store, &ws, "ses-test").unwrap();
        let c1 = journal
            .record(&["a.txt".into()], &[], "stella(lead): create a.txt")
            .unwrap();
        journal.mark_turn(1, &c1).unwrap();

        std::fs::write(ws.join("a.txt"), "v2\n").unwrap();
        let c2 = journal
            .record(&["a.txt".into()], &[], "stella(lead): update a.txt")
            .unwrap();
        journal.mark_turn(2, &c2).unwrap();

        assert_eq!(journal.read_at_turn(1, "a.txt").unwrap(), "v1\n");
        assert_eq!(journal.read_at_turn(2, "a.txt").unwrap(), "v2\n");
        assert!(
            !ws.join(".git").exists(),
            "and stella still created no repo in the user's workspace"
        );
    }

    #[test]
    fn a_checkpoint_blob_rides_along_and_replays_per_turn() {
        // Turn state and workspace state land in ONE object, so "restart turn
        // 7" restores both halves from a single consistent point rather than
        // two stores that can disagree.
        let (_guard, ws, store) = scratch();
        std::fs::write(ws.join("a.txt"), "code\n").unwrap();
        let journal = WorkJournal::open_in(&store, &ws, "ses-blob").unwrap();

        let c1 = journal
            .record(
                &["a.txt".into()],
                &[(CHECKPOINT_BLOB, Some(r#"{"version":1,"step":3}"#))],
                "turn 7 step 3",
            )
            .unwrap();
        journal.mark_turn(7, &c1).unwrap();

        assert_eq!(
            journal.blob_at_turn(7, CHECKPOINT_BLOB).unwrap(),
            r#"{"version":1,"step":3}"#
        );
    }

    #[test]
    fn the_turn_namespace_is_its_own_high_water_mark_across_a_restart() {
        // Why the turn ordinal is read from the refs and not from a counter: a
        // counter belongs to a process and a session outlives its processes.
        // The second handle below is the resumed session — a different
        // `WorkJournal` over the same store and session id, exactly as a
        // restart produces — and it must not re-mark a turn the first one
        // already used.
        let (_guard, ws, store) = scratch();
        let before = WorkJournal::open_in(&store, &ws, "ses-restart").unwrap();
        assert_eq!(
            before.last_marked_turn(),
            None,
            "a session that has marked nothing is at no turn"
        );

        std::fs::write(ws.join("a.txt"), "turn one\n").unwrap();
        let c1 = before.record(&["a.txt".into()], &[], "turn 1").unwrap();
        before.mark_turn(1, &c1).unwrap();
        std::fs::write(ws.join("a.txt"), "turn two\n").unwrap();
        let c2 = before.record(&["a.txt".into()], &[], "turn 2").unwrap();
        before.mark_turn(2, &c2).unwrap();
        assert_eq!(before.last_marked_turn(), Some(2));

        // …the process dies here, and the session is resumed.
        let after = WorkJournal::open_in(&store, &ws, "ses-restart").unwrap();
        assert_eq!(
            after.last_marked_turn(),
            Some(2),
            "the resumed session picks up where the marks left off"
        );
        std::fs::write(ws.join("a.txt"), "turn three\n").unwrap();
        let c3 = after.record(&["a.txt".into()], &[], "turn 3").unwrap();
        after
            .mark_turn(after.last_marked_turn().unwrap() + 1, &c3)
            .unwrap();

        assert_eq!(after.last_marked_turn(), Some(3));
        assert_eq!(
            after.read_at_turn(2, "a.txt").unwrap(),
            "turn two\n",
            "and turn 2 still names turn 2's work, rather than having been overwritten"
        );
        assert_eq!(after.read_at_turn(3, "a.txt").unwrap(), "turn three\n");
    }

    #[test]
    fn another_sessions_turn_marks_are_not_this_ones() {
        // The ordinal is per-session by construction (`refs/stella/<session>/
        // turn/<n>`), and two sessions share one workspace store. A high-water
        // mark that saw across sessions would make one session's turn numbering
        // jump whenever a neighbour ran.
        let (_guard, ws, store) = scratch();
        std::fs::write(ws.join("a.txt"), "shared\n").unwrap();
        let theirs = WorkJournal::open_in(&store, &ws, "ses-neighbour").unwrap();
        let c = theirs.record(&["a.txt".into()], &[], "their turn").unwrap();
        theirs.mark_turn(9, &c).unwrap();

        let mine = WorkJournal::open_in(&store, &ws, "ses-mine").unwrap();
        assert_eq!(
            mine.last_marked_turn(),
            None,
            "a neighbour's turn 9 is not this session's turn 9"
        );
    }

    #[test]
    fn a_checkpoint_survives_verbatim_and_clearing_is_idempotent() {
        // Moved here from the sidecar journal when the checkpoint folded into
        // this store. The bytes must come back EXACTLY as written: they are a
        // `Checkpoint::to_json` payload whose version gate lives in
        // stella-core, and any re-encoding (storing the `String` through a
        // serializer, say) would hand that gate an escaped JSON string literal
        // instead of a checkpoint.
        let (_guard, ws, store) = scratch();
        let journal = WorkJournal::open_in(&store, &ws, "ses-verbatim").unwrap();

        assert_eq!(
            journal.checkpoint(),
            None,
            "no interrupted turn means no resume point"
        );
        // Clearing before anything was ever written is success: a turn that
        // ended on its first step discards without having persisted.
        journal
            .clear_checkpoint()
            .expect("clearing an absent checkpoint is not an error");

        let json = r#"{"version":1,"step":3,"messages":[],"nested":{"quote":"\"x\""}}"#;
        journal.record_checkpoint(json, None, None).unwrap();
        assert_eq!(
            journal.checkpoint().as_deref(),
            Some(json),
            "stored verbatim — not re-serialized"
        );

        // Rewriting replaces rather than appends: one live turn, one point.
        let second = r#"{"version":1,"step":4}"#;
        journal.record_checkpoint(second, None, None).unwrap();
        assert_eq!(journal.checkpoint().as_deref(), Some(second));

        journal.clear_checkpoint().unwrap();
        assert_eq!(
            journal.checkpoint(),
            None,
            "a finished turn leaves nothing for a later resume to replay"
        );
        journal
            .clear_checkpoint()
            .expect("clearing twice is still success");
    }

    #[test]
    fn retracting_a_checkpoint_leaves_the_workspace_files_alone() {
        // `--force-remove` drops an index entry; the fear it raises is that it
        // reaches the work tree too. It must not: the agent's files are the
        // whole point of the record, and a turn ending is not a reason to
        // forget them.
        let (_guard, ws, store) = scratch();
        std::fs::write(ws.join("kept.txt"), "real work\n").unwrap();
        let journal = WorkJournal::open_in(&store, &ws, "ses-retract").unwrap();
        journal
            .record(&["kept.txt".into()], &[], "the agent's write")
            .unwrap();
        journal
            .record_checkpoint(r#"{"step":2}"#, None, None)
            .unwrap();

        journal.clear_checkpoint().unwrap();

        let tip = journal.session_tip().unwrap();
        journal.mark_turn(1, &tip).unwrap();
        assert_eq!(journal.read_at_turn(1, "kept.txt").unwrap(), "real work\n");
        assert!(
            ws.join("kept.txt").exists(),
            "the file on disk is untouched by a checkpoint retraction"
        );
    }

    #[test]
    fn changed_paths_name_exactly_what_moved_between_turns() {
        // The #1870 name half: turn 2 changed a.txt and created c.txt, so
        // those two — and only those two — are its changed paths. b.txt was
        // recorded in turn 1 and untouched since; the checkpoint blob rides
        // in the same commits and must never surface as a workspace path.
        let (_guard, ws, store) = scratch();
        std::fs::write(ws.join("a.txt"), "v1\n").unwrap();
        std::fs::write(ws.join("b.txt"), "stable\n").unwrap();
        let journal = WorkJournal::open_in(&store, &ws, "ses-diff").unwrap();
        let c1 = journal
            .record(
                &["a.txt".into(), "b.txt".into()],
                &[(CHECKPOINT_BLOB, Some(r#"{"step":1}"#))],
                "turn 1",
            )
            .unwrap();
        journal.mark_turn(1, &c1).unwrap();

        std::fs::write(ws.join("a.txt"), "v2\n").unwrap();
        std::fs::write(ws.join("c.txt"), "new\n").unwrap();
        let c2 = journal
            .record(
                &["a.txt".into(), "c.txt".into()],
                &[(CHECKPOINT_BLOB, Some(r#"{"step":2}"#))],
                "turn 2",
            )
            .unwrap();
        journal.mark_turn(2, &c2).unwrap();

        assert_eq!(
            journal.changed_paths_at_turn(2).unwrap(),
            vec!["a.txt".to_string(), "c.txt".into()],
            "the untouched file and the reserved records stay out"
        );
        assert_eq!(
            journal.changed_paths_at_turn(1).unwrap(),
            vec!["a.txt".to_string(), "b.txt".into()],
            "a first turn diffs against nothing: its whole tree is new"
        );
        // The content halves of the diff pair, on each side of the boundary.
        assert_eq!(journal.read_at_turn(1, "a.txt").unwrap(), "v1\n");
        assert_eq!(journal.read_at_turn(2, "a.txt").unwrap(), "v2\n");
        assert!(
            journal.read_at_turn(1, "c.txt").is_err(),
            "a path born in turn 2 has no turn-1 side — the caller maps this to empty"
        );
    }

    #[test]
    fn computing_a_diff_neither_blocks_nor_mutates_a_live_writer() {
        // The #1870 no-lock constraint, made a test instead of a sentence:
        // the diff reads are tree-to-tree plumbing against refs, so another
        // session committing into the same shared store is not blocked, and
        // the reader leaves every ref of both sessions exactly where it was.
        let (_guard, ws, store) = scratch();
        std::fs::write(ws.join("a.txt"), "v1\n").unwrap();
        let reader = WorkJournal::open_in(&store, &ws, "ses-reader").unwrap();
        let c1 = reader.record(&["a.txt".into()], &[], "turn 1").unwrap();
        reader.mark_turn(1, &c1).unwrap();

        // A live neighbour session mid-write: it has recorded (index seeded,
        // ref moved) and not yet marked a turn.
        let writer = WorkJournal::open_in(&store, &ws, "ses-writer").unwrap();
        std::fs::write(ws.join("b.txt"), "theirs\n").unwrap();
        writer.record(&["b.txt".into()], &[], "their step").unwrap();
        let writer_tip_before = writer.session_tip();

        let refs_before = reader.git(&["for-each-ref", "refs/stella/"]).unwrap();
        assert_eq!(
            reader.changed_paths_at_turn(1).unwrap(),
            vec!["a.txt".to_string()],
            "the reader answers while the neighbour is mid-write"
        );
        assert_eq!(
            reader.git(&["for-each-ref", "refs/stella/"]).unwrap(),
            refs_before,
            "and moved no ref of either session"
        );

        // The writer was never blocked: its next write succeeds and lands on
        // top of the tip it had.
        std::fs::write(ws.join("b.txt"), "theirs v2\n").unwrap();
        let c2 = writer.record(&["b.txt".into()], &[], "their next").unwrap();
        assert_ne!(Some(c2), writer_tip_before, "the writer kept moving");
    }

    #[test]
    fn history_accumulates_rather_than_replacing() {
        // Each commit is the parent's tree plus this change — a file touched
        // in turn 1 and never again must still be present at turn 2.
        let (_guard, ws, store) = scratch();
        std::fs::write(ws.join("first.txt"), "one\n").unwrap();
        let journal = WorkJournal::open_in(&store, &ws, "ses-acc").unwrap();
        let c1 = journal.record(&["first.txt".into()], &[], "first").unwrap();
        journal.mark_turn(1, &c1).unwrap();

        std::fs::write(ws.join("second.txt"), "two\n").unwrap();
        let c2 = journal
            .record(&["second.txt".into()], &[], "second")
            .unwrap();
        journal.mark_turn(2, &c2).unwrap();

        assert_eq!(journal.read_at_turn(2, "first.txt").unwrap(), "one\n");
        assert_eq!(journal.read_at_turn(2, "second.txt").unwrap(), "two\n");
    }

    /// Whether the store still holds `oid` at all — `cat-file -e` is the
    /// cheapest way to ask, and the only assertion that distinguishes "the ref
    /// is gone" from "the bytes are gone".
    fn object_exists(journal: &WorkJournal, oid: &str) -> bool {
        journal.git(&["cat-file", "-e", oid]).is_ok()
    }

    #[test]
    fn pruning_a_session_unreaches_its_objects_so_gc_can_reclaim_them() {
        // The whole point of retention here: deleting the ref is what makes
        // the objects collectable. `gc` on its own is required to KEEP
        // anything a ref reaches, so a store with no ref deletion grows
        // forever no matter how often it compacts.
        let (_guard, ws, store) = scratch();
        std::fs::write(ws.join("a.txt"), "theirs\n").unwrap();
        let theirs = WorkJournal::open_in(&store, &ws, "ses-old").unwrap();
        let dropped = theirs.record(&["a.txt".into()], &[], "their work").unwrap();
        theirs.mark_turn(1, &dropped).unwrap();

        std::fs::write(ws.join("b.txt"), "mine\n").unwrap();
        let mine = WorkJournal::open_in(&store, &ws, "ses-live").unwrap();
        let kept = mine.record(&["b.txt".into()], &[], "my work").unwrap();

        let report = mine
            .prune(&JournalPrunePolicy {
                older_than: Some(Duration::ZERO),
                gc: true,
                ..Default::default()
            })
            .unwrap();

        assert_eq!(report.aged_out, 1, "the running session is never pruned");
        assert_eq!(report.total_sessions(), 1);
        assert_eq!(report.refs_deleted, 2, "the head plus its one turn mark");
        assert_eq!(report.indexes_removed, 1, "and the sidecar index with it");
        assert!(report.gc_ran);

        assert!(
            theirs.session_tip().is_none(),
            "the pruned session's head is gone"
        );
        assert!(
            theirs.read_at_turn(1, "a.txt").is_err(),
            "and so is its turn mark"
        );
        assert!(
            !index_file_path(&mine.store_root, &mine.workspace_id, "ses-old").exists(),
            "the pruned session's index file is gone"
        );
        assert_eq!(
            mine.session_tip().as_deref(),
            Some(kept.as_str()),
            "the live session is untouched"
        );

        assert!(object_exists(&mine, &kept), "a retained commit stays");
        assert!(
            !object_exists(&mine, &dropped),
            "the pruned commit's objects were actually reclaimed"
        );
    }

    #[test]
    fn the_session_ceiling_evicts_the_oldest_and_reports_what_the_guard_blocks() {
        let (_guard, ws, store) = scratch();
        std::fs::write(ws.join("a.txt"), "shared\n").unwrap();
        // Same second for all three, so the tiebreak is the session name and
        // the eviction order is deterministic: ses-1 (this handle's, and
        // therefore protected), then ses-2, then ses-3.
        for session in ["ses-1", "ses-2", "ses-3"] {
            let journal = WorkJournal::open_in(&store, &ws, session).unwrap();
            journal.record(&["a.txt".into()], &[], session).unwrap();
        }
        let mine = WorkJournal::open_in(&store, &ws, "ses-1").unwrap();

        let report = mine
            .prune(&JournalPrunePolicy {
                max_sessions: Some(1),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.aged_out, 0, "no age window means no age phase");
        assert_eq!(report.ceiling_evicted, 2);
        assert_eq!(report.still_over_ceiling, 0);
        assert_eq!(mine.recorded_sessions().unwrap().len(), 1);

        // A ceiling the guard cannot satisfy is reported, never forced: the
        // running session outranks any policy.
        let report = mine
            .prune(&JournalPrunePolicy {
                max_sessions: Some(0),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.ceiling_evicted, 0);
        assert_eq!(report.still_over_ceiling, 1);
        assert!(mine.session_tip().is_some());
    }

    #[test]
    fn an_empty_policy_is_a_no_op_and_a_dry_run_deletes_nothing() {
        let (_guard, ws, store) = scratch();
        std::fs::write(ws.join("a.txt"), "theirs\n").unwrap();
        let theirs = WorkJournal::open_in(&store, &ws, "ses-old").unwrap();
        let tip = theirs.record(&["a.txt".into()], &[], "their work").unwrap();
        theirs.mark_turn(1, &tip).unwrap();
        let mine = WorkJournal::open_in(&store, &ws, "ses-live").unwrap();
        mine.record(&["a.txt".into()], &[], "my work").unwrap();

        assert_eq!(
            mine.prune(&JournalPrunePolicy::default()).unwrap(),
            JournalPruneReport::default(),
            "a policy with neither knob set deletes nothing"
        );

        let report = mine
            .prune(&JournalPrunePolicy {
                older_than: Some(Duration::ZERO),
                gc: true,
                dry_run: true,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(report.aged_out, 1, "a dry run reports what it would drop");
        assert_eq!(report.refs_deleted, 2);
        assert_eq!(report.indexes_removed, 0);
        assert!(!report.gc_ran, "and never reclaims");
        assert_eq!(
            theirs.session_tip().as_deref(),
            Some(tip.as_str()),
            "the session it named is still there"
        );
    }

    #[test]
    fn gitignored_paths_never_enter_the_store() {
        // An agent that writes build output must not bloat durable history
        // with it. `.gitignore` is read from the work tree, so this works even
        // though the git dir lives elsewhere.
        let (_guard, ws, store) = scratch();
        std::fs::write(ws.join(".gitignore"), "build/\n").unwrap();
        std::fs::create_dir_all(ws.join("build")).unwrap();
        std::fs::write(ws.join("build/out.o"), "junk\n").unwrap();
        std::fs::write(ws.join("kept.txt"), "real\n").unwrap();

        let journal = WorkJournal::open_in(&store, &ws, "ses-ignore").unwrap();
        let c = journal
            .record(
                &["kept.txt".into(), "build/out.o".into()],
                &[],
                "one real file, one ignored",
            )
            .unwrap();
        journal.mark_turn(1, &c).unwrap();

        assert_eq!(journal.read_at_turn(1, "kept.txt").unwrap(), "real\n");
        assert!(
            journal.read_at_turn(1, "build/out.o").is_err(),
            "the ignored artifact must not be stored"
        );
    }
}
