//! Attributing workspace mutations that no tool schema can describe.
//!
//! The file ledger (`ToolRegistry::record_touch`) is fed by
//! `ToolRegistry::classify_file_op`, which reads a tool's
//! *input* to decide what it is about to do. That works for the CRUD tools —
//! `write_file` names its path, `edit_file` names its path — and it cannot work
//! for `bash`. A shell command is an opaque string: `make`, `python build.py`,
//! `patch < fix.diff` and `echo x > f` all change the tree, and none of them
//! say so in a form a schema can read.
//!
//! The consequence was measured rather than assumed. Over a 20-task
//! Terminal-Bench run, 757 of 1,063 tool calls were `bash` and the ledger
//! recorded 131 events — one per CRUD-tool call, none from the shell. 71% of
//! the agent's activity left no trace in the ledger, in the Files tab, or in
//! the `file_change_events` channel the verification ladder reads.
//!
//! That last one is why this matters beyond cosmetics. On Terminal-Bench the
//! task directory is not a git repository, so the diff probe can only ever
//! answer "I could not look" (`LadderInputs::diff_available == false`). With
//! the diff channel dark, `file_change_events` is the *only* channel that can
//! positively prove the tree changed — and it was blind to the exact tool
//! doing nearly all of the changing.
//!
//! So this module answers the question the schema cannot: fingerprint the
//! workspace either side of an opaque call and attribute the difference. The
//! probe never guesses. Every bound it hits is recorded as a bound
//! ([`WorkspaceProbe::saturated`]) rather than shrinking the answer, because
//! the distinction between "I looked and saw nothing" and "I could not finish
//! looking" is the one whose collapse caused #973.

use std::collections::BTreeMap;
use std::path::Path;

use crate::file_touch::{FileOp, normalize_workspace_path};

/// Directories that cannot themselves be the point of a task and would
/// otherwise dominate the walk. Deliberately short: build outputs (`target`,
/// `dist`, `build`) are **not** here, because producing one is frequently the
/// whole job and a probe that hides it would recreate the blindness this
/// module exists to remove.
const SKIP_DIRS: &[&str] = &[
    ".git",
    // Stella's own per-workspace state: the code-graph SQLite database and
    // its WAL/SHM sidecars mutate on nearly every turn, and fingerprinting
    // them means reading megabytes of binary pages per walk only for
    // `record_touch` to drop the result (#1537).
    ".stella",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
];

/// The `tool` label every touch this module attributes is recorded under.
///
/// A constant rather than a literal at the call site because it is now read
/// back: [`crate::authored_diff::Provenance::for_tool`] uses it to tell a
/// change the agent *declared* from one this module merely *observed*, and
/// those two must never be compared through two copies of a string.
pub const PROBE_TOOL_LABEL: &str = "workspace_probe";

/// Entry ceiling for one walk. Past this the probe stops and says so.
const MAX_ENTRIES: usize = 20_000;
/// Depth ceiling, so a symlink cycle or a pathological tree cannot hang a turn.
const MAX_DEPTH: usize = 24;
/// Largest single file whose content is held for a line diff.
const MAX_CONTENT_BYTES: u64 = 256 * 1024;
/// Total content budget across one snapshot.
const MAX_TOTAL_CONTENT: u64 = 16 * 1024 * 1024;

/// What one snapshot recorded about one file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Fingerprint {
    len: u64,
    /// Modification time in nanoseconds since the epoch, when the platform
    /// offers one. `None` collapses the comparison onto length alone — a
    /// same-length in-place rewrite then reads as unchanged, which is a miss
    /// rather than a false report.
    mtime_nanos: Option<u128>,
    /// Pre-call content, when the file was small enough to hold within budget.
    /// `None` means the line counts for this path are not measurable, and is
    /// reported as such rather than being rendered as a whole-file rewrite.
    content: Option<String>,
}

/// A mutation the probe attributed to an opaque call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellTouch {
    /// Workspace-normalized path, the same key the ledger uses.
    pub path: String,
    pub op: FileOp,
    /// Pre-call content for a line diff, when it was captured within budget.
    pub pre_content: Option<String>,
    /// Whether line counts for this touch can be computed honestly. `false`
    /// when the file was too large (or arrived too late) to hold a
    /// pre-image — the touch still records *that* the file changed.
    pub counts_measurable: bool,
}

/// A bounded fingerprint of the workspace, taken either side of a call whose
/// effects cannot be read from its input.
#[derive(Debug, Clone, Default)]
pub struct WorkspaceProbe {
    entries: BTreeMap<String, Fingerprint>,
    /// The walk hit [`MAX_ENTRIES`] or [`MAX_DEPTH`]. The snapshot is a lower
    /// bound on the tree, so a path's *absence* from it proves nothing.
    saturated: bool,
}

impl WorkspaceProbe {
    /// Fingerprint `root`, holding content for files within budget.
    ///
    /// Errors are absences, not failures: an unreadable directory is simply
    /// not described. A probe that refused to return on a permission error
    /// would turn a partially-visible workspace into no workspace at all.
    pub fn capture(root: &Path) -> Self {
        let mut probe = Self::default();
        let mut budget = MAX_TOTAL_CONTENT;
        let mut stack = vec![(root.to_path_buf(), 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            if depth > MAX_DEPTH {
                probe.saturated = true;
                continue;
            }
            let Ok(reader) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in reader.flatten() {
                if probe.entries.len() >= MAX_ENTRIES {
                    probe.saturated = true;
                    return probe;
                }
                let path = entry.path();
                let Ok(meta) = entry.metadata() else { continue };
                if meta.is_dir() {
                    let name = entry.file_name();
                    let name = name.to_string_lossy();
                    if SKIP_DIRS.contains(&name.as_ref()) {
                        continue;
                    }
                    stack.push((path, depth + 1));
                    continue;
                }
                // Symlinks are not followed: the target is either inside the
                // root (and walked on its own) or outside it (and not ours).
                if !meta.is_file() {
                    continue;
                }
                let Some(normalized) = path
                    .to_str()
                    .and_then(|raw| normalize_workspace_path(root, raw))
                else {
                    continue;
                };
                let len = meta.len();
                let content = if len <= MAX_CONTENT_BYTES && len <= budget {
                    match std::fs::read(&path) {
                        // Lossy so a binary still yields a deterministic
                        // (if approximate) line count, matching how the
                        // CRUD path reads pre-images.
                        Ok(bytes) => {
                            budget = budget.saturating_sub(len);
                            Some(String::from_utf8_lossy(&bytes).into_owned())
                        }
                        Err(_) => None,
                    }
                } else {
                    None
                };
                probe.entries.insert(
                    normalized,
                    Fingerprint {
                        len,
                        mtime_nanos: meta.modified().ok().and_then(|t| {
                            t.duration_since(std::time::UNIX_EPOCH)
                                .ok()
                                .map(|d| d.as_nanos())
                        }),
                        content,
                    },
                );
            }
        }
        probe
    }

    /// Whether this snapshot is a lower bound on the tree rather than a
    /// complete description of it.
    pub fn saturated(&self) -> bool {
        self.saturated
    }

    /// Attribute the difference between this (pre) snapshot and `post`.
    ///
    /// Deletions are reported **only** when both snapshots are complete. If
    /// either walk saturated, a path missing from `post` may simply be a path
    /// the second walk never reached, and reporting that as a deletion would
    /// invent a mutation that never happened — the failure mode this module
    /// exists to avoid, pointed the other way.
    pub fn diff(&self, post: &Self) -> Vec<ShellTouch> {
        let mut touches = Vec::new();
        for (path, after) in &post.entries {
            match self.entries.get(path) {
                None => touches.push(ShellTouch {
                    path: path.clone(),
                    op: FileOp::Create,
                    pre_content: None,
                    counts_measurable: true,
                }),
                Some(before)
                    if before.len != after.len || before.mtime_nanos != after.mtime_nanos =>
                {
                    touches.push(ShellTouch {
                        path: path.clone(),
                        op: FileOp::Update,
                        pre_content: before.content.clone(),
                        counts_measurable: before.content.is_some(),
                    })
                }
                Some(_) => {}
            }
        }
        if !self.saturated && !post.saturated {
            for (path, before) in &self.entries {
                if !post.entries.contains_key(path) {
                    touches.push(ShellTouch {
                        path: path.clone(),
                        op: FileOp::Delete,
                        pre_content: before.content.clone(),
                        counts_measurable: before.content.is_some(),
                    });
                }
            }
        }
        touches
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, body).unwrap();
    }

    /// The defect this module was built for: a shell redirect is attributed.
    #[test]
    fn a_file_created_outside_any_crud_tool_is_attributed() {
        let dir = tempfile::tempdir().unwrap();
        let pre = WorkspaceProbe::capture(dir.path());
        write(dir.path(), "solution.py", "print(1)\n");
        let post = WorkspaceProbe::capture(dir.path());

        let touches = pre.diff(&post);
        assert_eq!(touches.len(), 1, "{touches:?}");
        assert_eq!(touches[0].path, "solution.py");
        assert_eq!(touches[0].op, FileOp::Create);
    }

    #[test]
    fn an_in_place_rewrite_is_an_update_and_carries_its_pre_image() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "main.c", "int main(){return 1;}\n");
        let pre = WorkspaceProbe::capture(dir.path());
        // Length differs, so this is caught even where mtime has coarse
        // resolution.
        write(dir.path(), "main.c", "int main(){return 0;}\n// fixed\n");
        let post = WorkspaceProbe::capture(dir.path());

        let touches = pre.diff(&post);
        assert_eq!(touches.len(), 1, "{touches:?}");
        assert_eq!(touches[0].op, FileOp::Update);
        assert!(touches[0].counts_measurable);
        assert_eq!(
            touches[0].pre_content.as_deref(),
            Some("int main(){return 1;}\n")
        );
    }

    #[test]
    fn a_removed_file_is_a_delete_when_both_walks_were_complete() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "stale.txt", "gone\n");
        let pre = WorkspaceProbe::capture(dir.path());
        std::fs::remove_file(dir.path().join("stale.txt")).unwrap();
        let post = WorkspaceProbe::capture(dir.path());

        let touches = pre.diff(&post);
        assert_eq!(touches.len(), 1, "{touches:?}");
        assert_eq!(touches[0].op, FileOp::Delete);
    }

    /// A saturated walk is a lower bound, so absence proves nothing and must
    /// not be rendered as a deletion.
    #[test]
    fn a_saturated_walk_never_invents_a_deletion() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "kept.txt", "x\n");
        let pre = WorkspaceProbe::capture(dir.path());
        let mut post = WorkspaceProbe::capture(dir.path());
        post.entries.clear();
        post.saturated = true;

        assert!(pre.diff(&post).is_empty(), "saturation must not delete");
    }

    #[test]
    fn an_untouched_tree_reports_nothing() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.txt", "a\n");
        write(dir.path(), "nested/b.txt", "b\n");
        let pre = WorkspaceProbe::capture(dir.path());
        let post = WorkspaceProbe::capture(dir.path());

        assert!(pre.diff(&post).is_empty());
    }

    /// Stella's own state directory mutates on nearly every turn (the
    /// code-graph database and its WAL/SHM sidecars), and none of it is
    /// workspace content — the probe must neither pay to fingerprint it nor
    /// attribute its churn (#1537).
    #[test]
    fn stella_state_churn_is_invisible_to_the_probe() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/lib.rs", "pub fn f() {}\n");
        let pre = WorkspaceProbe::capture(dir.path());
        write(
            dir.path(),
            ".stella/private/codegraph.db",
            "\u{0}binary pages\u{0}\n",
        );
        write(
            dir.path(),
            ".stella/private/codegraph.db-wal",
            "\u{0}wal\u{0}\n",
        );
        write(dir.path(), "src/lib.rs", "pub fn f() -> u8 { 0 }\n");
        let post = WorkspaceProbe::capture(dir.path());

        let paths: Vec<_> = pre.diff(&post).into_iter().map(|t| t.path).collect();
        assert_eq!(paths, vec!["src/lib.rs".to_string()]);
    }

    /// Skipping `.git` is what keeps the probe affordable; skipping a build
    /// directory would hide the point of many tasks, so `target/` is walked.
    #[test]
    fn vcs_metadata_is_skipped_but_build_output_is_not() {
        let dir = tempfile::tempdir().unwrap();
        let pre = WorkspaceProbe::capture(dir.path());
        write(dir.path(), ".git/objects/ab/cdef", "blob\n");
        write(dir.path(), "target/release/app", "binary\n");
        let post = WorkspaceProbe::capture(dir.path());

        let paths: Vec<_> = pre.diff(&post).into_iter().map(|t| t.path).collect();
        assert_eq!(paths, vec!["target/release/app".to_string()]);
    }
}
