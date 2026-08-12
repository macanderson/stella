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
//! the agent's activity left no trace in the ledger or in the Files tab.
//!
//! It once left no trace in the verification ladder either, and that framing is
//! now wrong in a way worth stating: a wire-carried tally of tool-declared
//! touches (`LadderInputs::file_change_events`) was read by three ladder
//! predicates until #2873, read by none of them after, and removed from the
//! wire entirely in #2934. A tally of tool-declared touches cannot be an
//! authority on whether the tree changed — this module exists because it is
//! not one — so the ladder takes that answer from git and nowhere else. What
//! this module buys is
//! **observability**: the ledger, the Files tab, and the authored-diff channel
//! see the tool that does nearly all of the changing.
//!
//! So this module answers the question the schema cannot: fingerprint the
//! workspace either side of an opaque call and attribute the difference. The
//! probe never guesses. Every bound it hits is recorded as a bound
//! ([`WorkspaceProbe::saturated`]) rather than shrinking the answer, because
//! the distinction between "I looked and saw nothing" and "I could not finish
//! looking" is the one whose collapse caused #973.

use std::collections::BTreeMap;
use std::path::Path;

use stella_graph::workspace_ignore::WorkspaceIgnore;

use crate::file_touch::{FileOp, normalize_workspace_path};

/// Directories that cannot themselves be the point of a task and would
/// otherwise dominate the walk. Deliberately short: build outputs (`target`,
/// `dist`, `build`) are **not** here, because producing one is frequently the
/// whole job and a probe that hides it would recreate the blindness this
/// module exists to remove. Inside a git repository the repository's own
/// ignore rules take that role instead — see [`IgnorePolicy`] — which keeps
/// this list about what can *never* matter rather than about what usually
/// doesn't.
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

/// How many individual touches one probe delta may record before the rest are
/// reported as a count.
///
/// The walk was already bounded ([`MAX_ENTRIES`]); recording what it found was
/// not, and that is where a trial's budget went. Measured on Terminal-Bench
/// `sqlite-with-gcov`, 2026-08-08: one `tar xzf` of the vendored SQLite
/// tarball produced 2,211 touches, and recording them took **659 seconds of a
/// 900-second task budget** — 70% of the trial spent watching itself extract
/// an archive. All six trials of that arm timed out, none of them because the
/// model was slow: model time was 45–164s against 660–790s inside that one
/// tool call. The cost is per touch and mostly not the diff — each one journals
/// a mutation, and a bound work journal spends several `git` invocations doing
/// it.
///
/// 256 keeps every ordinary edit intact (the largest real refactor in this
/// repository's history touches well under it) while capping the pathological
/// case — an extraction, a `npm install`, a build that writes an object tree —
/// at a bounded cost. Past it the delta is still *reported*, as a count rather
/// than as files: the bound is disclosed, never silently applied, which is the
/// same discipline [`WorkspaceProbe::saturated`] already follows for the walk.
pub(crate) const MAX_RECORDED_TOUCHES: usize = 256;

/// Whether one workspace walk consults the repository's own ignore rules.
///
/// The default is [`IgnorePolicy::SkipIgnored`] — the `ignore_gitignore`
/// setting, which ships on: paths the workspace's own `.gitignore` excludes
/// are never walked, fingerprinted, or recorded as touches, because the
/// repository has already declared them uninteresting and their churn is what
/// used to drown the walk (77% of recorded changes in a measured session were
/// `target/` artifacts, and the walk saturated inside them before reaching
/// real source). `"ignore_gitignore": "off"` in settings selects
/// [`IgnorePolicy::WalkAll`], the unfiltered walk.
///
/// Outside a git repository the policy is inert — nothing is ignored, so the
/// probe stays fully sighted. That preserves this module's stated posture on
/// Terminal-Bench, where the task directory is not a repository and producing
/// build output is frequently the whole job.
///
/// A **process-free** registry always resolves to [`IgnorePolicy::WalkAll`],
/// whatever the setting says: consulting the rules means spawning `git`, and
/// that isolation exists precisely to promise no child process runs. Losing
/// the filter there costs walk time, which `MAX_RECORDED_TOUCHES` already
/// bounds; honoring the setting instead would cost the guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IgnorePolicy {
    /// Skip what the repository's own ignore rules exclude (the default).
    #[default]
    SkipIgnored,
    /// Walk everything the filesystem shows, ignore rules notwithstanding.
    WalkAll,
}

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
    /// What this walk's ignore rules excluded, resolved once by
    /// [`WorkspaceIgnore`]. Carried on the snapshot because [`Self::diff`]
    /// needs it: a path absent from one side may be *pruned* rather than
    /// *gone*, and the two must not be confused when `.gitignore` itself
    /// changes between the walks.
    ignored: WorkspaceIgnore,
}

impl WorkspaceProbe {
    /// Fingerprint `root`, holding content for files within budget, under the
    /// default ignore policy ([`IgnorePolicy::SkipIgnored`]).
    ///
    /// Errors are absences, not failures: an unreadable directory is simply
    /// not described. A probe that refused to return on a permission error
    /// would turn a partially-visible workspace into no workspace at all.
    pub fn capture(root: &Path) -> Self {
        Self::capture_with(root, IgnorePolicy::default())
    }

    /// [`Self::capture`] under an explicit ignore policy.
    pub fn capture_with(root: &Path, policy: IgnorePolicy) -> Self {
        let mut probe = Self {
            ignored: match policy {
                IgnorePolicy::SkipIgnored => WorkspaceIgnore::resolve(root),
                IgnorePolicy::WalkAll => WorkspaceIgnore::none(),
            },
            ..Self::default()
        };
        let mut budget = MAX_TOTAL_CONTENT;
        let mut stack = vec![(root.to_path_buf(), String::new(), 0usize)];
        while let Some((dir, rel, depth)) = stack.pop() {
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
                // The walk-relative key, threaded down the stack rather than
                // re-derived per entry: `normalize_workspace_path`
                // canonicalizes, which would cost a syscall per directory.
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let rel_child = if rel.is_empty() {
                    name.to_string()
                } else {
                    format!("{rel}/{name}")
                };
                if meta.is_dir() {
                    if SKIP_DIRS.contains(&name.as_ref()) {
                        continue;
                    }
                    // `--directory` collapses a wholly-ignored tree to its
                    // topmost entry, so an exact match here prunes the whole
                    // subtree without a prefix scan.
                    if probe.ignored.excludes_dir(&rel_child) {
                        continue;
                    }
                    stack.push((path, rel_child, depth + 1));
                    continue;
                }
                if probe.ignored.excludes(&rel_child) {
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

    /// Whether this snapshot's walk pruned `path` under its ignore rules —
    /// either listed outright or covered by a collapsed `dir/` entry.
    fn ignores(&self, path: &str) -> bool {
        self.ignored.excludes(path)
    }

    /// Attribute the difference between this (pre) snapshot and `post`.
    ///
    /// Deletions are reported **only** when both snapshots are complete. If
    /// either walk saturated, a path missing from `post` may simply be a path
    /// the second walk never reached, and reporting that as a deletion would
    /// invent a mutation that never happened — the failure mode this module
    /// exists to avoid, pointed the other way.
    ///
    /// The same discipline covers a `.gitignore` edited between the walks:
    /// a path one side pruned and the other side saw is *unknowable*, not
    /// changed. Absent from `post` because a new rule now covers it is not a
    /// deletion; present in `post` because a rule was dropped is not a
    /// creation. Both are skipped ([`WorkspaceIgnore::excludes`]) — a miss, never a
    /// fabrication, because the probe never guesses.
    pub fn diff(&self, post: &Self) -> Vec<ShellTouch> {
        let mut touches = Vec::new();
        for (path, after) in &post.entries {
            match self.entries.get(path) {
                None if self.ignores(path) => {}
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
                if !post.entries.contains_key(path) && !post.ignores(path) {
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
    /// (A directory that merely *contains* a `.git` entry is not a repository
    /// — `git ls-files` refuses it — so the ignore consultation stays inert
    /// here too and the walk is the unfiltered one.)
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

    fn git_init(root: &Path) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["init", "--quiet"])
            .status()
            .expect("git must be runnable in the test environment");
        assert!(status.success(), "git init failed");
    }

    /// The `ignore_gitignore` default: inside a git repository the
    /// repository's own rules decide what the probe walks, so churn in
    /// ignored paths — a build tree, a scratch file — is invisible while a
    /// source edit in the same delta stays attributed.
    #[test]
    fn gitignored_churn_is_invisible_when_the_workspace_is_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        write(dir.path(), ".gitignore", "target/\n*.scratchtmp\n");
        let pre = WorkspaceProbe::capture(dir.path());
        write(dir.path(), "target/debug/app.o", "object\n");
        write(dir.path(), "probe-note.scratchtmp", "x\n");
        write(dir.path(), "src/lib.rs", "pub fn f() {}\n");
        let post = WorkspaceProbe::capture(dir.path());

        let paths: Vec<_> = pre.diff(&post).into_iter().map(|t| t.path).collect();
        assert_eq!(paths, vec!["src/lib.rs".to_string()]);
    }

    /// `"ignore_gitignore": "off"` restores the unfiltered walk.
    #[test]
    fn the_walk_all_policy_still_sees_ignored_paths() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        write(dir.path(), ".gitignore", "target/\n");
        let pre = WorkspaceProbe::capture_with(dir.path(), IgnorePolicy::WalkAll);
        write(dir.path(), "target/debug/app.o", "object\n");
        let post = WorkspaceProbe::capture_with(dir.path(), IgnorePolicy::WalkAll);

        let paths: Vec<_> = pre.diff(&post).into_iter().map(|t| t.path).collect();
        assert_eq!(paths, vec!["target/debug/app.o".to_string()]);
    }

    /// A rule *added* between the walks hides paths from the second walk;
    /// their absence is pruning, not deletion, and inventing a mass deletion
    /// here would be the probe fabricating the biggest delta of the session.
    #[test]
    fn a_gitignore_rule_added_mid_turn_does_not_fabricate_deletions() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        write(dir.path(), "target/debug/app.o", "object\n");
        let pre = WorkspaceProbe::capture(dir.path());
        write(dir.path(), ".gitignore", "target/\n");
        let post = WorkspaceProbe::capture(dir.path());

        let paths: Vec<_> = pre.diff(&post).into_iter().map(|t| t.path).collect();
        assert_eq!(paths, vec![".gitignore".to_string()]);
    }

    /// The mirror image: a rule *dropped* between the walks makes paths
    /// appear in the second walk that the first one pruned. The probe cannot
    /// tell "newly created" from "was there all along", so it says nothing —
    /// a miss, never a fabricated creation.
    #[test]
    fn a_gitignore_rule_dropped_mid_turn_does_not_fabricate_creations() {
        let dir = tempfile::tempdir().unwrap();
        git_init(dir.path());
        write(dir.path(), ".gitignore", "target/\n");
        write(dir.path(), "target/debug/app.o", "object\n");
        let pre = WorkspaceProbe::capture(dir.path());
        std::fs::remove_file(dir.path().join(".gitignore")).unwrap();
        let post = WorkspaceProbe::capture(dir.path());

        let touches = pre.diff(&post);
        let paths: Vec<_> = touches.iter().map(|t| t.path.as_str()).collect();
        assert_eq!(paths, vec![".gitignore"], "{touches:?}");
        assert_eq!(touches[0].op, FileOp::Delete);
    }
}
