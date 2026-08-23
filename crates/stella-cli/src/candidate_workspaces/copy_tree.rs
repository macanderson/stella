// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The second isolation substrate: the directory is the truth (#1383).
//!
//! [`SessionCandidateWorkspaces`](super::SessionCandidateWorkspaces) snapshots
//! a candidate as a git worktree and promotes the winner by `git diff` +
//! `git apply`. That is the right shape **when the real tree is precious** —
//! nothing lands that the user could not have written themselves, an adoption
//! that no longer fits refuses rather than resolves, and a candidate's
//! `target/` stays out of its answer because git's ignore rules decide what a
//! snapshot contains.
//!
//! Every one of those virtues inverts where the tree is **disposable**:
//!
//! - **The git snapshot is a view, not a copy.** `git worktree add` carries
//!   `HEAD`, and the working-tree delta rides as a patch — so a candidate is
//!   missing everything in `.gitignore`. In a benchmark container that state is
//!   installed by task setup and executed by the task's own tests
//!   (`node_modules/`, `.venv/`, a downloaded dataset), so the candidate solves
//!   a materially different tree than the grader inspects. In an ordinary JS or Python
//!   project it means every candidate fails the project's own test command on a
//!   missing dependency.
//! - **The patch buys a guarantee already paid for.** "Do not clobber the
//!   user's working copy" is what `adopt`'s seal and its two refusals exist
//!   for. Where the tree is a container about to be deleted, that is pure risk
//!   — a conflict class with nothing on the other side of the trade.
//!
//! So this copies the directory instead, ignored files included, and promotes
//! by replacing the target's contents with the winner's. No git query decides
//! what belongs; no patch has to fit.
//!
//! # It is chosen, never detected
//!
//! Whole-tree promotion overwrites the tree it lands on, so it is only safe
//! where somebody has said the tree is expendable — and that is a sentence a
//! person writes, not one a host infers from finding a `Dockerfile`. The
//! selector is the `candidate_isolation` setting
//! ([`CandidateIsolation`](crate::settings::CandidateIsolation)), whose default
//! is the git substrate; nothing else reaches this code.
//!
//! # What is deliberately not copied
//!
//! `.stella/` at the workspace root, whole. This substrate's own candidate
//! directory lives inside it, so copying it would recurse into the copy it is
//! making. It holds the session's live SQLite handles, and a promotion that
//! replaced `store.db` under a running process would corrupt the telemetry of
//! the run doing the promoting. And it is host state rather than the user's
//! work, which is what a candidate is asked to change. A candidate therefore
//! cannot answer *with* a change to `.stella/`, and that is the intended
//! boundary rather than an omission.
//!
//! Everything else is copied as it is found, `.git` included — which is what
//! makes a copy-tree candidate immune to the shared-ref hazard
//! [`ref_guard`](super::ref_guard) exists for: it has its own `.git`, so a
//! `git checkout` inside it reaches nothing the real tree reads.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use stella_core::subagent::SubAgentOutcome;
use stella_protocol::candidate::CandidateHandle;
use stella_runtime::wrapper::{
    CandidateFanoutError, CandidateReport, CandidateWork, CandidateWorkspace, CandidateWorkspaces,
};

use super::{CANDIDATES_DIR, dispatch_candidate_turn, record};
use crate::config::Config;
use crate::subagent::SessionSubAgents;

/// The one directory a candidate neither receives nor may promote. See the
/// module docs.
const HOST_STATE_DIR: &str = ".stella";

/// One session's copy-tree substrate over one workspace.
pub(crate) struct CopyTreeCandidateWorkspaces {
    /// The session's own config — the write-directory grant, the operator's
    /// tool policy, the engine's step cap.
    cfg: Config,
    /// The installed plugin's manifest name: the principal every candidate
    /// turn's tool calls authorize as.
    plugin: String,
    /// The session's one dispatcher, so a fan-out spends one pool and writes
    /// one ledger.
    sub_agents: Arc<SessionSubAgents>,
    /// The tree being copied, and the tree an adoption replaces.
    ///
    /// The session's working directory, not a repository top: this substrate
    /// knows nothing about git, which is the point — a workspace with no
    /// commit, or no `.git` at all, is one it can still serve.
    workspace_root: PathBuf,
    /// Handle → the copy it addresses.
    minted: Mutex<HashMap<CandidateHandle, PathBuf>>,
    /// Monotonic, so two fan-outs in one run cannot mint the same handle.
    next: AtomicU32,
}

impl std::fmt::Debug for CopyTreeCandidateWorkspaces {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CopyTreeCandidateWorkspaces")
            .field("plugin", &self.plugin)
            .field("workspace_root", &self.workspace_root)
            .field("minted", &self.minted.lock().map(|m| m.len()).unwrap_or(0))
            .finish_non_exhaustive()
    }
}

impl CopyTreeCandidateWorkspaces {
    /// Build the substrate for one installed plugin over this session's tree.
    pub(crate) fn new(cfg: &Config, plugin: &str, sub_agents: Arc<SessionSubAgents>) -> Self {
        Self {
            cfg: cfg.clone(),
            plugin: plugin.to_string(),
            sub_agents,
            workspace_root: cfg.workspace_root.clone(),
            minted: Mutex::new(HashMap::new()),
            next: AtomicU32::new(0),
        }
    }

    /// Where this substrate's copies live — the same directory the git
    /// substrate keeps its checkouts in, so one sweep sees both.
    fn candidates_dir(&self) -> PathBuf {
        self.workspace_root.join(CANDIDATES_DIR)
    }

    /// The copy a handle addresses.
    fn copy_of(&self, handle: &CandidateHandle) -> Result<PathBuf, CandidateFanoutError> {
        self.minted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(handle)
            .cloned()
            .ok_or_else(|| CandidateFanoutError::NotAdopted {
                handle: handle.clone(),
                reason: "this host minted no workspace under that handle".to_string(),
            })
    }

    /// What earlier runs in this workspace left behind, one line each (#2813).
    pub(crate) fn orphaned_candidates(&self) -> Vec<String> {
        record::report(&self.candidates_dir())
    }

    /// Keep every candidate still live, and say where (#2651).
    ///
    /// The preserved artifact is the **tree**, not a patch: this substrate's
    /// candidate is already a whole directory, and writing a diff of it would
    /// be inventing a git question about a workspace this adapter exists
    /// precisely to serve without one. Taking the handle out of the table is
    /// what keeps it: the sweep that follows finds nothing under it and
    /// answers success, which is `remove`'s already-gone contract.
    pub(crate) fn preserve_unscored(&self) -> Vec<String> {
        let kept: Vec<(CandidateHandle, PathBuf)> = std::mem::take(
            &mut *self
                .minted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
        .into_iter()
        .collect();
        kept.into_iter()
            .map(|(handle, dir)| {
                format!(
                    "the run ended before anything scored candidate {handle}; its tree is \
                     kept whole at {}",
                    dir.display()
                )
            })
            .collect()
    }
}

#[async_trait]
impl CandidateWorkspaces for CopyTreeCandidateWorkspaces {
    async fn create(&self, label: &str) -> Result<CandidateWorkspace, CandidateFanoutError> {
        let ordinal = self.next.fetch_add(1, Ordering::Relaxed);
        let handle = CandidateHandle::new(format!("candidate-{ordinal}"));
        let dir = self
            .candidates_dir()
            .join(slug(label, ordinal, std::process::id()));
        // The copy is the whole cost of this substrate, and it is paid here so
        // that nothing downstream has to ask git what belongs.
        copy_tree(&self.workspace_root, &dir, true).map_err(|error| {
            CandidateFanoutError::NotCreated {
                reason: format!(
                    "the workspace could not be copied to {}: {error}",
                    dir.display()
                ),
            }
        })?;
        let _ = record::CandidateRecord::of_directory(handle.as_str(), &dir).write();
        self.minted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(handle.clone(), dir.clone());
        // Canonical, because the plugin is handed this path to read and test
        // in, and a symlinked spelling is one a plugin's own path handling may
        // resolve differently than this host would.
        let root = dir.canonicalize().unwrap_or(dir);
        Ok(CandidateWorkspace {
            handle,
            root: root.to_string_lossy().into_owned(),
        })
    }

    async fn work(
        &self,
        workspace: &CandidateWorkspace,
        work: CandidateWork,
    ) -> Result<CandidateReport, CandidateFanoutError> {
        let outcome = dispatch_candidate_turn(
            &self.cfg,
            &self.plugin,
            &self.sub_agents,
            PathBuf::from(&workspace.root),
            work,
        )
        .await?;

        // Measured from the tree, never from what the turn said it did — the
        // git substrate's rule, asked of two directories instead of an index.
        let copy = self.copy_of(&workspace.handle)?;
        let (files_changed, lines_changed) = tree_delta(&self.workspace_root, &copy);

        Ok(CandidateReport {
            report: outcome.summary().to_string(),
            completed: matches!(outcome, SubAgentOutcome::Completed(_)),
            cost_usd: outcome.cost_usd(),
            files_changed,
            lines_changed,
        })
    }

    /// Replace the workspace's contents with this candidate's.
    ///
    /// There is no seal, no patch and no conflict class, which is the whole
    /// trade: where the tree is disposable the only question is which
    /// candidate won, and the answer is delivered by making the tree be that
    /// candidate.
    ///
    /// Ordered deletions-then-copies rather than "empty it and copy it back",
    /// so there is never a moment at which the workspace is not a workspace. A
    /// failure part way leaves a tree that is a mixture of the two, which is
    /// the honest cost of a promotion that cannot be atomic and is stated
    /// rather than hidden — the reason the git substrate remains the default.
    ///
    /// # Errors
    ///
    /// [`CandidateFanoutError::NotAdopted`] naming the path that would not be
    /// written.
    async fn adopt(&self, workspace: &CandidateWorkspace) -> Result<(), CandidateFanoutError> {
        let copy = self.copy_of(&workspace.handle)?;
        let fail = |error: std::io::Error| CandidateFanoutError::NotAdopted {
            handle: workspace.handle.clone(),
            reason: format!("the winner's tree could not replace the workspace: {error}"),
        };
        remove_absent(&copy, &self.workspace_root, true).map_err(fail)?;
        copy_tree(&copy, &self.workspace_root, true).map_err(fail)?;
        Ok(())
    }

    async fn remove(&self, workspace: &CandidateWorkspace) -> Result<(), CandidateFanoutError> {
        let Some(dir) = self
            .minted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&workspace.handle)
        else {
            // Already gone, and that is success rather than an error — the
            // sweep removes everything, including what adoption already took,
            // and what `preserve_unscored` deliberately kept.
            return Ok(());
        };
        // A removal that failed and left nothing behind is success: the
        // directory may have gone with a `TempDir`, or with the container.
        // What is reported is disk still standing under a name only this table
        // knew.
        if let Err(error) = std::fs::remove_dir_all(&dir)
            && dir.exists()
        {
            return Err(CandidateFanoutError::NotRemoved {
                handle: workspace.handle.clone(),
                reason: format!("{}: {error}", dir.display()),
            });
        }
        record::forget(&dir);
        Ok(())
    }
}

/// The directory name a candidate's copy is made under.
///
/// The process id is in it for the reason the git substrate hashes a run scope
/// into its slugs: two `stella` processes fanning out in one workspace derive
/// the same ordinals, and the second copy would land inside the first's tree.
fn slug(label: &str, ordinal: u32, pid: u32) -> String {
    let safe: String = label
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .take(48)
        .collect();
    format!("{safe}-{ordinal}-{pid}")
}

/// Whether `entry`, directly under a tree's root, is host state a candidate
/// neither receives nor promotes.
fn is_host_state(name: &std::ffi::OsStr) -> bool {
    name == HOST_STATE_DIR
}

/// Copy `source`'s contents into `target`, creating what is missing.
///
/// `at_root` marks the top of the walk, which is the only level where
/// [`HOST_STATE_DIR`] is skipped — a `.stella` nested inside a subproject is
/// that project's data, and dropping it would make the copy unfaithful in
/// exactly the way this substrate exists to avoid.
///
/// Symlinks are recreated as symlinks rather than followed: a tree with a
/// symlink into itself would otherwise copy forever, and a `node_modules`
/// full of workspace links would multiply into gigabytes of duplicates.
fn copy_tree(source: &Path, target: &Path, at_root: bool) -> std::io::Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if at_root && is_host_state(&name) {
            continue;
        }
        let from = entry.path();
        let to = target.join(&name);
        let kind = std::fs::symlink_metadata(&from)?;
        if kind.is_dir() {
            copy_tree(&from, &to, false)?;
            continue;
        }
        // Whatever stands there goes first, and that is not tidiness. Copying
        // onto an existing symlink would follow it and write bytes somewhere
        // this promotion never named; copying onto a read-only file — every
        // object under `.git/objects` is `0444` — fails outright; and a
        // directory that became a file cannot be written across at all.
        if let Ok(there) = std::fs::symlink_metadata(&to) {
            remove_any(&to, &there)?;
        }
        if kind.is_symlink() {
            symlink(&std::fs::read_link(&from)?, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn symlink(link: &Path, at: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(link, at)
}

#[cfg(windows)]
fn symlink(link: &Path, at: &Path) -> std::io::Result<()> {
    // A directory symlink and a file symlink are different calls on Windows,
    // and which one is right is decided by what the link points at rather than
    // by the link itself.
    if link.is_dir() {
        std::os::windows::fs::symlink_dir(link, at)
    } else {
        std::os::windows::fs::symlink_file(link, at)
    }
}

/// Delete everything in `target` that `source` does not have.
///
/// The half of a replacement that a copy cannot do: a file the winner deleted
/// is answered by its absence, and a promotion that only ever wrote would
/// leave it standing.
fn remove_absent(source: &Path, target: &Path, at_root: bool) -> std::io::Result<()> {
    for entry in std::fs::read_dir(target)? {
        let entry = entry?;
        let name = entry.file_name();
        if at_root && is_host_state(&name) {
            continue;
        }
        let here = entry.path();
        let there = source.join(&name);
        let kind = std::fs::symlink_metadata(&here)?;
        match std::fs::symlink_metadata(&there) {
            // Same name, same kind: recurse into a directory, leave a file for
            // the copy to overwrite.
            Ok(other) if other.is_dir() && kind.is_dir() => {
                remove_absent(&there, &here, false)?;
            }
            Ok(other) if other.is_dir() != kind.is_dir() => {
                // A directory became a file, or the reverse. `fs::copy` cannot
                // write across that, so the old shape goes first.
                remove_any(&here, &kind)?;
            }
            Ok(_) => {}
            Err(_) => remove_any(&here, &kind)?,
        }
    }
    Ok(())
}

fn remove_any(path: &Path, kind: &std::fs::Metadata) -> std::io::Result<()> {
    if kind.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// How much the tree at `candidate` differs from the tree at `source`:
/// `(files, lines added + removed)`.
///
/// Measuring a copy costs a full read of both trees, and that is named rather
/// than hidden: it is this substrate's trade — disk and time in exchange for a
/// faithful tree — and it is bounded by a candidate turn that already cost
/// minutes of model time.
///
/// A file whose bytes are not UTF-8 counts as a changed **file** with no
/// lines, the same honest reading `numstat` gives a binary: its size in lines
/// is not a number that exists.
fn tree_delta(source: &Path, candidate: &Path) -> (u32, u32) {
    let before = files_under(source, true);
    let after = files_under(candidate, true);
    let mut files = 0_u32;
    let mut lines = 0_u32;
    for path in before.union(&after) {
        let old = std::fs::read(source.join(path)).ok();
        let new = std::fs::read(candidate.join(path)).ok();
        if old == new {
            continue;
        }
        files = files.saturating_add(1);
        // A side that is absent is zero lines; a side that is not UTF-8 has no
        // line count at all, and one text side does not make the pair a text
        // change — counting the other half alone would report a deletion of
        // every line of a file that is still there.
        let as_text = |bytes: Option<Vec<u8>>| match bytes {
            None => Some(String::new()),
            Some(raw) => String::from_utf8(raw).ok(),
        };
        let (Some(old), Some(new)) = (as_text(old), as_text(new)) else {
            continue;
        };
        let diff = stella_diff::unified_diff(&old, &new, 0);
        lines = lines.saturating_add(u32::try_from(diff.added + diff.removed).unwrap_or(u32::MAX));
    }
    (files, lines)
}

/// Every file under `root`, as paths relative to it, following no symlink.
fn files_under(root: &Path, at_root: bool) -> BTreeSet<PathBuf> {
    let mut found = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if at_root && is_host_state(&name) {
            continue;
        }
        let path = entry.path();
        let Ok(kind) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if kind.is_dir() {
            found.extend(
                files_under(&path, false)
                    .into_iter()
                    .map(|below| PathBuf::from(&name).join(below)),
            );
        } else {
            // A symlink counts as a leaf, compared by what `fs::read` makes of
            // it — which is the target's bytes, and is the same question asked
            // of both trees.
            found.insert(PathBuf::from(&name));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(spec: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, contents) in spec {
            let at = dir.path().join(path);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(at, contents).unwrap();
        }
        dir
    }

    /// **#1383's core claim.** The git substrate's snapshot is a git *view*, so
    /// a gitignored `node_modules/` is absent from every candidate. A copy is
    /// a copy.
    #[test]
    fn a_copy_carries_what_gitignore_excludes() {
        let source = tree(&[
            ("src/main.rs", "fn main() {}\n"),
            (".gitignore", "node_modules/\n"),
            ("node_modules/left-pad/index.js", "module.exports = 1;\n"),
            (".stella/private/store.db", "not the user's work\n"),
        ]);
        let target = tempfile::tempdir().unwrap();
        let into = target.path().join("candidate-0");

        copy_tree(source.path(), &into, true).unwrap();

        assert_eq!(
            std::fs::read_to_string(into.join("node_modules/left-pad/index.js")).unwrap(),
            "module.exports = 1;\n",
            "the ignored dependency a task's tests exec must be in the candidate"
        );
        assert!(into.join("src/main.rs").exists());
        assert!(
            !into.join(HOST_STATE_DIR).exists(),
            "the host's own state is not the user's work, and the candidate \
             directory lives inside it"
        );
    }

    /// A promotion answers a deletion as well as a write: a file the winner
    /// removed must not survive in the tree it replaces.
    #[test]
    fn a_replacement_answers_a_deletion_as_well_as_a_write() {
        let target = tree(&[
            ("keep.txt", "old\n"),
            ("gone.txt", "delete me\n"),
            ("dir/nested.txt", "old\n"),
        ]);
        let winner = tree(&[("keep.txt", "new\n"), ("dir/nested.txt", "new\n")]);

        remove_absent(winner.path(), target.path(), true).unwrap();
        copy_tree(winner.path(), target.path(), true).unwrap();

        assert_eq!(
            std::fs::read_to_string(target.path().join("keep.txt")).unwrap(),
            "new\n"
        );
        assert_eq!(
            std::fs::read_to_string(target.path().join("dir/nested.txt")).unwrap(),
            "new\n"
        );
        assert!(
            !target.path().join("gone.txt").exists(),
            "a file the winner deleted is answered by its absence"
        );
    }

    /// The host's own state survives a promotion untouched — the session's
    /// live SQLite handles are in there, and the candidate directory itself.
    #[test]
    fn a_replacement_leaves_the_hosts_own_state_alone() {
        let target = tree(&[(".stella/private/store.db", "live\n"), ("a.txt", "old\n")]);
        let winner = tree(&[("a.txt", "new\n")]);

        remove_absent(winner.path(), target.path(), true).unwrap();
        copy_tree(winner.path(), target.path(), true).unwrap();

        assert_eq!(
            std::fs::read_to_string(target.path().join(".stella/private/store.db")).unwrap(),
            "live\n"
        );
    }

    /// A symlink is recreated rather than followed. A tree with a link into
    /// itself would otherwise copy until the disk ran out.
    #[cfg(unix)]
    #[test]
    fn a_symlink_into_the_tree_is_recreated_rather_than_followed() {
        let source = tree(&[("real/file.txt", "x\n")]);
        std::os::unix::fs::symlink(source.path(), source.path().join("loop")).unwrap();
        let target = tempfile::tempdir().unwrap();
        let into = target.path().join("candidate-0");

        copy_tree(source.path(), &into, true).unwrap();

        assert!(
            std::fs::symlink_metadata(into.join("loop"))
                .unwrap()
                .is_symlink(),
            "a link is copied as a link"
        );
    }

    #[test]
    fn the_delta_counts_edits_additions_and_deletions() {
        let source = tree(&[
            ("same.txt", "one\n"),
            ("edited.txt", "one\ntwo\n"),
            ("deleted.txt", "gone\n"),
        ]);
        let candidate = tree(&[
            ("same.txt", "one\n"),
            ("edited.txt", "one\nTWO\n"),
            ("added.txt", "new\n"),
        ]);

        let (files, lines) = tree_delta(source.path(), candidate.path());
        assert_eq!(files, 3, "edited, deleted, added — never the identical one");
        // edited: one line out, one in; deleted: one out; added: one in.
        assert_eq!(lines, 4);
    }

    #[test]
    fn a_binary_file_is_a_changed_file_with_no_lines() {
        let source = tree(&[("logo.png", "\u{0}")]);
        let candidate = tempfile::tempdir().unwrap();
        std::fs::write(candidate.path().join("logo.png"), [0xff_u8, 0xfe, 0x00]).unwrap();

        let (files, lines) = tree_delta(source.path(), candidate.path());
        assert_eq!(files, 1);
        assert_eq!(lines, 0, "its size in lines is not a number that exists");
    }

    #[test]
    fn a_slug_survives_a_label_full_of_path_characters() {
        let slug = slug("plugin:candidates/worker#0", 2, 4242);
        assert_eq!(slug, "plugin-candidates-worker-0-2-4242");
        assert!(!slug.contains('/'), "a slug is one path component");
    }
}
