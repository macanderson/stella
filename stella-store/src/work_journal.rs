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
    fn tip(&self) -> Option<String> {
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
    /// Returns the new commit id. Paths are workspace-relative and are staged
    /// individually — never `git add -A`, which would sweep in the user's own
    /// unrelated work and make the history a lie about what the agent did.
    pub fn record(
        &self,
        paths: &[String],
        blobs: &[(&str, &str)],
        message: &str,
    ) -> Result<String> {
        let parent = self.tip();

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
            let oid = self.hash_blob(content)?;
            // 100644 = a regular file. These are stella's own records, kept
            // under a reserved prefix so they can never collide with a real
            // workspace path.
            self.git(&[
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("100644,{oid},.stella-journal/{name}"),
            ])?;
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

    /// Mark `commit` as the state at the end of `turn`, so it can be replayed
    /// by turn number rather than by commit id.
    pub fn mark_turn(&self, turn: u32, commit: &str) -> Result<()> {
        self.git(&["update-ref", &turn_ref(&self.session, turn), commit])?;
        Ok(())
    }

    /// The content of `path` as it stood at the end of `turn`.
    pub fn read_at_turn(&self, turn: u32, path: &str) -> Result<String> {
        self.git(&["show", &format!("{}:{path}", turn_ref(&self.session, turn))])
    }

    /// One of the reserved journal blobs as it stood at the end of `turn`.
    pub fn blob_at_turn(&self, turn: u32, name: &str) -> Result<String> {
        self.read_at_turn(turn, &format!(".stella-journal/{name}"))
    }

    /// Compact the object store. The end-of-session step: everything stays
    /// replayable, it simply stops being loose objects.
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
                &[("checkpoint.json", r#"{"version":1,"step":3}"#)],
                "turn 7 step 3",
            )
            .unwrap();
        journal.mark_turn(7, &c1).unwrap();

        assert_eq!(
            journal.blob_at_turn(7, "checkpoint.json").unwrap(),
            r#"{"version":1,"step":3}"#
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
