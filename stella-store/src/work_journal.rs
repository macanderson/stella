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

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Result, StoreError};

/// A session's handle on the workspace's durable history.
#[derive(Debug, Clone)]
pub struct WorkJournal {
    git_dir: PathBuf,
    work_tree: PathBuf,
    index_file: PathBuf,
    session: String,
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

/// Stella's own records live under a reserved prefix, so a blob can never
/// collide with a real workspace path.
fn journal_blob_path(name: &str) -> String {
    format!(".stella-journal/{name}")
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
        let git_dir = root.join(format!("{}.git", identity.id));
        let journal = Self {
            git_dir,
            work_tree: identity.path,
            index_file: root.join(format!("{}.{session}.index", identity.id)),
            session: session.to_string(),
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
    pub fn record_checkpoint(&self, json: &str, observed: Option<&str>) -> Result<String> {
        let mut blobs: Vec<(&str, Option<&str>)> = vec![(CHECKPOINT_BLOB, Some(json))];
        if let Some(observed) = observed {
            blobs.push((OBSERVED_BLOB, Some(observed)));
        }
        self.record(&[], &blobs, "stella: turn checkpoint")
    }

    /// The session's staleness map as of this session's tip, or `None` when
    /// this session never observed a file.
    pub fn observed(&self) -> Option<String> {
        self.blob_at_tip(OBSERVED_BLOB)
    }

    /// Retract the resume point — the turn it described reached a terminal
    /// outcome, and a checkpoint that outlives its turn invites a resume that
    /// replays work the caller already saw finish.
    ///
    /// Idempotent, and cheap when there is nothing to retract: every terminal
    /// path discards unconditionally, including turns that ended before their
    /// first step boundary, so the common case must not cost a commit. One
    /// `cat-file -e` answers that.
    pub fn clear_checkpoint(&self) -> Result<()> {
        if self.checkpoint().is_none() {
            return Ok(());
        }
        self.record(
            &[],
            &[(CHECKPOINT_BLOB, None)],
            "stella: turn checkpoint retired",
        )?;
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
    /// # No production caller yet, and why it is still here
    ///
    /// The refs this reads through are written by every real session as of the
    /// change that wired [`Self::mark_turn`] — before that they existed only in
    /// this module's own tests, which made this genuinely unreachable API.
    /// It now has data; what it does not yet have is a *consumer*. Answering
    /// "show me the workspace as it stood three turns ago" needs a surface to
    /// ask it from, and neither a CLI verb nor the cross-machine transport work
    /// that would want this granularity is built.
    ///
    /// Kept rather than deleted because the write side is now load-bearing and
    /// costs one `update-ref` per turn: retiring the reader would leave the
    /// marks being written for nothing, which is the worse of the two shapes.
    /// If the transport work is dropped, delete the pair together.
    pub fn read_at_turn(&self, turn: u32, path: &str) -> Result<String> {
        self.git(&["show", &format!("{}:{path}", turn_ref(&self.session, turn))])
    }

    /// One of the reserved journal blobs as it stood at the end of `turn`.
    ///
    /// Same standing as [`Self::read_at_turn`]: real refs, no consumer yet.
    pub fn blob_at_turn(&self, turn: u32, name: &str) -> Result<String> {
        self.read_at_turn(turn, &format!(".stella-journal/{name}"))
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
    pub fn compact(&self) -> Result<()> {
        self.git(&["gc", "--quiet", "--auto"])?;
        Ok(())
    }
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
        journal.record_checkpoint(json, None).unwrap();
        assert_eq!(
            journal.checkpoint().as_deref(),
            Some(json),
            "stored verbatim — not re-serialized"
        );

        // Rewriting replaces rather than appends: one live turn, one point.
        let second = r#"{"version":1,"step":4}"#;
        journal.record_checkpoint(second, None).unwrap();
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
        journal.record_checkpoint(r#"{"step":2}"#, None).unwrap();

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
