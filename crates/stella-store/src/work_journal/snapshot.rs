// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What the shared work tree changed across one turn, measured (#3413).
//!
//! # Why this exists beside [`WorkJournal::record`]
//!
//! [`WorkJournal::record`] stages the paths its caller names, and its doc says
//! why: `git add -A` "would sweep in the user's own unrelated work and make the
//! history a lie about what the agent did". That ruling was correct while a
//! path-naming producer existed. It does not survive the producer's deletion.
//!
//! The 12-tool purge (#3244) removed every file-writing built-in along with the
//! file-CRUD ledger that called `record`, and `record` has had **no production
//! caller since** — including now that the file built-ins are back. Restoring
//! `write_file` / `edit_file` / `delete_file` restored tools that *name* a
//! path, but not a producer that can name every path a turn changed: `bash`
//! mutates the tree without naming anything, and so do MCP servers and custom
//! script tools. A `record` call driven by the file tools alone would stage a
//! subset while looking exhaustive, which is a worse lie than `add -A` tells.
//! So the choice is still not "named paths or `add -A`" — it is "`add -A` or
//! nothing", and nothing is what shipped: `AgentEvent::FileChange`, the
//! `files_touched` ledger and the `session_turn_diffs` projection are all
//! empty for every non-pipeline turn.
//!
//! This module takes the honest half of that trade and is careful about which
//! half it is. A tree measurement answers **"what changed during this turn"**.
//! It does not answer "what did the agent change", and nothing here or
//! downstream may claim that it does — a user editing a file in another window
//! mid-turn lands in this diff exactly as the agent's own writes do. That is
//! why `FileChange` is documented **observability, never evidence** (#2873):
//! the authority on what the agent did is the pipeline's adoption, which
//! measures a candidate against a sealed baseline and can tell the two apart.
//!
//! # Why a lineage of its own
//!
//! Snapshots take their own ref (`refs/stella/<session>/tree`) and their own
//! index file rather than joining the session head, for two independent
//! reasons:
//!
//! - **Correctness.** The session head carries the reserved `.stella-journal/`
//!   blobs (the resume checkpoint, the staleness map) interleaved with the
//!   turn's content. A whole-tree snapshot on that ref would delete every blob
//!   it did not itself write, and `record_checkpoint` would restore them on the
//!   next step — the two producers would fight over one tree.
//! - **Cost.** `record` and `record_checkpoint` seed the index with `read-tree`
//!   on every call, which discards git's stat cache. A dedicated index is never
//!   `read-tree`'d, so `git add -A` re-hashes only the files whose stat data
//!   actually moved. Without that, every turn would re-hash the whole worktree.
//!
//! It costs one `add -A` plus a `write-tree` per turn, at the turn boundary —
//! after the model has answered, off the critical path, in a process that is
//! already shelling to git at every step.

use std::process::Command;

use super::{JOURNAL_DIR, WorkJournal, run, snapshot_ref};
use crate::{Result, StoreError};

/// What happened to one path between two tree snapshots.
///
/// Deliberately store-local rather than `stella-protocol`'s `FileChangeKind`:
/// `stella-store` does not depend on the protocol crate, and this is git's
/// answer (`diff-tree --name-status`), not the event vocabulary. The caller
/// that emits events maps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalChangeKind {
    /// The path did not exist in the previous snapshot.
    Created,
    /// The path existed in both and its content differs.
    Modified,
    /// The path existed in the previous snapshot and is gone.
    Deleted,
}

/// One path's delta across a turn, carrying git's own line counts.
///
/// `added`/`removed` come from `diff-tree --numstat` and **must not** be
/// re-derived by counting `+`/`-` lines in `diff`: `diff` is a bounded
/// rendering and re-counting it is the #2290 defect that rendered real work as
/// `+0 -0`. A binary file keeps `0/0` (git reports `-`/`-`; it has no lines)
/// while still carrying its kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalChange {
    /// Workspace-relative path, as git spells it.
    pub path: String,
    pub kind: JournalChangeKind,
    pub added: u32,
    pub removed: u32,
    /// The unified diff for this path alone, when one was readable.
    pub diff: Option<String>,
}

impl WorkJournal {
    /// Snapshot the whole work tree and return what changed since this
    /// session's previous snapshot.
    ///
    /// Returns an empty vector — having still written the snapshot — when:
    ///
    /// - this is the session's **first** snapshot, because "every file in the
    ///   workspace" is not a description of what a turn changed. The first call
    ///   establishes the baseline the second one measures against; and
    /// - the tree is byte-identical to the previous snapshot, in which case no
    ///   commit is written at all (a read-only turn leaves no history, matching
    ///   [`WorkJournal::record`]'s "nothing recorded means nothing to name").
    ///
    /// Ignored paths are excluded by `git add -A` honouring the work tree's own
    /// `.gitignore`, which is what keeps `.stella/private/` — the session's own
    /// databases, rewritten on every turn — out of the answer. The reserved
    /// `.stella-journal/` prefix is filtered too, defensively: this lineage never
    /// writes those blobs, so a hit would mean the user has a real file there.
    ///
    /// Best-effort by contract at every call site: a turn that ended is not
    /// made less ended by an unrecordable snapshot.
    pub fn snapshot_worktree(&self) -> Result<Vec<JournalChange>> {
        let parent = self.snapshot_tip();
        // No `read-tree`: the dedicated index persists across turns precisely
        // so git's stat cache survives and this call re-hashes only what moved.
        self.git_snapshot(&["add", "-A", "--"])?;
        let tree = self.git_snapshot(&["write-tree"])?.trim().to_string();
        if tree.is_empty() {
            return Err(StoreError::Other("git write-tree named no tree".into()));
        }

        let parent_tree = parent
            .as_deref()
            .and_then(|commit| self.tree_of(commit).ok());
        if parent_tree.as_deref() == Some(tree.as_str()) {
            // Identical tree: no commit, so the ref keeps naming the moment the
            // content last actually changed.
            return Ok(Vec::new());
        }

        let commit = self.commit_snapshot(&tree, parent.as_deref())?;
        self.git_snapshot(&["update-ref", &snapshot_ref(&self.session), &commit])?;

        let Some(parent_tree) = parent_tree else {
            // First snapshot of the session — baseline only.
            return Ok(Vec::new());
        };
        self.diff_trees(&parent_tree, &tree)
    }

    /// The commit this session's snapshot lineage last wrote, if any.
    fn snapshot_tip(&self) -> Option<String> {
        self.git_snapshot(&[
            "rev-parse",
            "--verify",
            "--quiet",
            &snapshot_ref(&self.session),
        ])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    }

    /// The tree a commit names.
    fn tree_of(&self, commit: &str) -> Result<String> {
        Ok(self
            .git_snapshot(&["rev-parse", "--verify", &format!("{commit}^{{tree}}")])?
            .trim()
            .to_string())
    }

    fn commit_snapshot(&self, tree: &str, parent: Option<&str>) -> Result<String> {
        let mut args: Vec<String> = vec![
            "commit-tree".into(),
            tree.into(),
            "-m".into(),
            "turn snapshot".into(),
        ];
        if let Some(parent) = parent {
            args.push("-p".into());
            args.push(parent.to_string());
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        Ok(self.git_snapshot(&args)?.trim().to_string())
    }

    /// The three readings that make up one measured delta: kinds, counts, and
    /// display text. Three plumbing reads rather than one, because git has no
    /// single format carrying all three, and the alternative — deriving counts
    /// or kinds from the patch — is exactly the re-derivation that broke.
    fn diff_trees(&self, before: &str, after: &str) -> Result<Vec<JournalChange>> {
        let name_status = self.git_snapshot(&[
            "diff-tree",
            "-r",
            "--name-status",
            "--no-renames",
            "-z",
            before,
            after,
        ])?;
        let mut changes = parse_name_status(&name_status);
        changes.retain(|change| !is_reserved(&change.path));
        if changes.is_empty() {
            return Ok(changes);
        }

        // Counts and patch are best-effort *relative to the kinds*: a delta
        // whose numstat read failed is still a real delta and must still be
        // reported, with the `0/0` that says "not measured" rather than
        // vanishing from the answer.
        if let Ok(numstat) = self.git_snapshot(&[
            "diff-tree",
            "-r",
            "--numstat",
            "--no-renames",
            "-z",
            before,
            after,
        ]) {
            attach_numstat(&mut changes, &numstat);
        }
        if let Ok(patch) = self.git_snapshot(&[
            "diff-tree",
            "-r",
            "-p",
            "--no-renames",
            "--no-color",
            before,
            after,
        ]) {
            attach_diffs(&mut changes, &patch);
        }
        Ok(changes)
    }

    /// One git command against the snapshot index.
    ///
    /// Same isolation as [`WorkJournal::git`] — the work tree is the user's, so
    /// neither their git config nor their hooks may run as a side effect of
    /// stella measuring it — but pointed at the dedicated index file this
    /// module owns.
    fn git_snapshot(&self, args: &[&str]) -> Result<String> {
        let mut cmd = Command::new("git");
        cmd.arg("--git-dir")
            .arg(&self.git_dir)
            .arg("--work-tree")
            .arg(&self.work_tree)
            .args(args)
            .env("GIT_INDEX_FILE", &self.snapshot_index)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", &self.git_dir);
        run(&mut cmd)
    }
}

/// Whether a path belongs to stella's own reserved records rather than the
/// user's work. The prefix carries its slash so a real file merely *starting*
/// with the reserved name survives — the same rule
/// [`WorkJournal::changed_paths_at_turn`] applies.
fn is_reserved(path: &str) -> bool {
    path.starts_with(&format!("{JOURNAL_DIR}/"))
}

/// Parse `diff-tree -r --name-status --no-renames -z` — `S\0path\0` records.
/// Renames are disabled, so no record carries two paths.
fn parse_name_status(raw: &str) -> Vec<JournalChange> {
    let mut parts = raw.split('\0').filter(|s| !s.is_empty());
    let mut out = Vec::new();
    while let (Some(status), Some(path)) = (parts.next(), parts.next()) {
        let kind = match status.chars().next() {
            Some('A') => JournalChangeKind::Created,
            Some('D') => JournalChangeKind::Deleted,
            // Anything else git can emit here (M, T, C, U) is a modification
            // as far as an observer is concerned. Guessing "modified" is safe;
            // dropping the record would under-report a real change.
            _ => JournalChangeKind::Modified,
        };
        out.push(JournalChange {
            path: path.to_string(),
            kind,
            added: 0,
            removed: 0,
            diff: None,
        });
    }
    out
}

/// Attach git's own line counts from `diff-tree --numstat -z`:
/// `added\tremoved\tpath\0` per file, where a binary file reports `-\t-`.
fn attach_numstat(changes: &mut [JournalChange], numstat: &str) {
    for record in numstat.split('\0').filter(|s| !s.is_empty()) {
        let mut fields = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if let Some(change) = changes.iter_mut().find(|c| c.path == path) {
            change.added = added.parse().unwrap_or(0);
            change.removed = removed.parse().unwrap_or(0);
        }
    }
}

/// Cut a multi-file patch into per-path slices and hang each on its change.
fn attach_diffs(changes: &mut [JournalChange], patch: &str) {
    for (path, diff) in split_patch_per_file(patch) {
        if let Some(change) = changes.iter_mut().find(|c| c.path == path) {
            change.diff = Some(diff);
        }
    }
}

/// Cut a multi-file patch into `(path, diff-text)` pairs, keyed by the `b/`
/// side (the post-image, which is the path every consumer names) and falling
/// back to the `a/` side for a deletion, whose `b/` side is `/dev/null`.
fn split_patch_per_file(patch: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in patch.lines() {
        // Column zero only: an indented or `+`/`-`-prefixed occurrence is hunk
        // *content* that happens to look like a header.
        if let Some(header) = line.strip_prefix("diff --git ") {
            if let Some(done) = current.take() {
                out.push(done);
            }
            current = header_path(header).map(|path| (path, String::new()));
            continue;
        }
        if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some(done) = current.take() {
        out.push(done);
    }
    out
}

/// The path a `diff --git a/x b/x` header describes. Git quotes paths
/// containing spaces, so the halves split on the ` b/` boundary rather than on
/// whitespace, and the `b/` side wins unless it is `/dev/null`.
fn header_path(header: &str) -> Option<String> {
    let (a, b) = match header.split_once(" b/") {
        Some((a, b)) => (a.trim_start_matches("a/"), b),
        // Not a shape we understand — better no diff than a wrong one.
        None => return None,
    };
    if b == "/dev/null" || b.is_empty() {
        let a = a.trim();
        return (!a.is_empty()).then(|| a.to_string());
    }
    Some(b.to_string())
}

#[cfg(test)]
mod tests;
