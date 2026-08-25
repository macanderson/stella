// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A mutating call's **own reading** of what it changed — the diff
//! `write_file`, `edit_file` and `delete_file` compute from the bytes they
//! read and the bytes they left.
//!
//! # Why the work-tree measurement is not enough
//!
//! The transcript's inline diff under a file call resolves through an
//! [`AgentEvent::FileChange`](stella_protocol::AgentEvent::FileChange) that
//! folded inside the call's window, and until this existed that event had one
//! producer: the per-call work-tree snapshot (`stella_tools::call_measure`,
//! bound by `stella-cli`'s `turn_files`). A snapshot is git's reading of the
//! tree, which is the right authority for everything a `bash` or an MCP server
//! moves — and it has holes no amount of care closes:
//!
//! - **A gitignored path is invisible to it.** The journal honours
//!   `.gitignore` by design, so a `write_file` into `.stella/agents/…` under a
//!   `/.stella/*` rule produced no measurement at all, and the row rendered
//!   `wrote 9214 bytes to …` with no diff and no line count — the shape of a
//!   call that changed nothing.
//! - **A workspace with no journal measures nothing.** A session whose
//!   durability never bound (no work store, a read-only home) has no snapshot
//!   to take.
//!
//! `write_file` and `edit_file` hold both sides of every change they make: the
//! content they were handed, and the content they read before replacing it.
//! `delete_file` holds one side and knows the other is empty. A diff over
//! those two is a measurement of the bytes, not an inference from the
//! arguments (the #2290 defect the counts contract forbids — a tool's
//! *arguments* are a request, and these are the bytes that landed). So the
//! tools report it, and the turn's measurer publishes it for any path the
//! tree reading did not cover. Where the snapshot *did* see the path, its
//! reading stays the one published: one change, one event.
//!
//! # Transport
//!
//! The reading rides [`ToolOutput::Ok`]'s `data` under [`DATA_KEY`] — the
//! declared seam for a result's structured half (#3285). The model never sees
//! it; `content` is untouched, so the byte-identical success strings the
//! stagnation detector keys on (#3176) are preserved.
//!
//! # Shape
//!
//! The diff text is the per-file body the journal hands the same event —
//! everything after git's `diff --git` line: `--- a/path`, `+++ b/path`, then
//! `@@` hunks — so every consumer that already renders a measured change
//! renders this one through the same parser, unchanged.

use serde::{Deserialize, Serialize};
use stella_protocol::tool::ToolOutput;

/// The key under which a call's own changes ride `ToolOutput::Ok::data`.
pub const DATA_KEY: &str = "changes";

/// Context lines on either side of a hunk — git's default, so a diff here and
/// a diff the journal measured read alike.
const CONTEXT: usize = 3;

/// What a call did to one path, as the call itself saw it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnChange {
    /// Workspace-relative when the path is inside the session root — the same
    /// spelling the work-tree measurement uses, so the two can be matched —
    /// and absolute otherwise.
    pub path: String,
    /// Whether the path existed before the call.
    pub kind: OwnChangeKind,
    /// Lines present only after the call.
    pub added: u32,
    /// Lines present only before the call.
    pub removed: u32,
    /// The unified diff, in the shape described in the module docs.
    pub diff: String,
    /// Whether `diff` is the minimal edit script, or `stella_diff` tripped
    /// `LCS_AREA_CAP` and fell back to replace-everything.
    pub minimal: bool,
}

/// Whether the call created the path, changed one that was there, or removed
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OwnChangeKind {
    Created,
    Modified,
    Deleted,
}

/// Diff `before` against `after` for `path`.
///
/// `None` for `before` means the path did not exist: the change is
/// [`OwnChangeKind::Created`] and the diff is all-green. `Some("")` is an
/// existing empty file, which is a modification — the distinction git draws
/// with `new file mode`, and the one the deck draws with `new file`.
#[must_use]
pub fn own_change(path: &str, before: Option<&str>, after: &str) -> OwnChange {
    let kind = match before {
        Some(_) => OwnChangeKind::Modified,
        None => OwnChangeKind::Created,
    };
    let diff = stella_diff::unified_diff(before.unwrap_or_default(), after, CONTEXT);
    OwnChange {
        path: path.to_string(),
        kind,
        // Counts are `usize` in the differ and `u32` on the wire; a file with
        // more than four billion lines is not a file this tool writes.
        added: u32::try_from(diff.added).unwrap_or(u32::MAX),
        removed: u32::try_from(diff.removed).unwrap_or(u32::MAX),
        minimal: diff.minimal,
        diff: patch_text(path, kind, &diff),
    }
}

/// The removal of `path`, whose content was `before`.
///
/// Its own constructor rather than a `None` for `after`, because the two ends
/// are not symmetric: a deletion always has a before and never has an after,
/// so [`own_change`]'s `Option` would have to grow a second one that no
/// caller could pass both of. The diff is all-red, headed `--- a/path` /
/// `+++ /dev/null` — the git spelling every consumer keys "deleted file" on.
#[must_use]
pub fn own_delete(path: &str, before: &str) -> OwnChange {
    let kind = OwnChangeKind::Deleted;
    let diff = stella_diff::unified_diff(before, "", CONTEXT);
    OwnChange {
        path: path.to_string(),
        kind,
        added: u32::try_from(diff.added).unwrap_or(u32::MAX),
        removed: u32::try_from(diff.removed).unwrap_or(u32::MAX),
        minimal: diff.minimal,
        diff: patch_text(path, kind, &diff),
    }
}

/// The workspace-relative spelling of a path the write scope resolved.
///
/// [`crate::ctx::ToolCtx::resolve_for_write`] hands back a path relative to
/// whichever allowed root it landed in, which is the session root for every
/// ordinary call and a nested or sibling root for a scoped one. The
/// measurement names paths relative to the session root, so a change has to be
/// spelled the same way to be matched against it; a path outside the session
/// root keeps its absolute form, which the measurement can never name anyway.
///
/// The scope's roots are canonical (`ToolRegistry::write_scope`) and the
/// session root is as the host gave it, so on a machine where the two spell
/// one directory differently — macOS's `/var` → `/private/var` — the prefix
/// is tried in both spellings. Failing that match would leave every path
/// absolute, which the measurement could never match and the deck would
/// render as a file outside the workspace.
#[must_use]
pub fn workspace_path(root: &std::path::Path, scope_root: &std::path::Path, rel: &str) -> String {
    let absolute = scope_root.join(rel);
    if let Ok(inside) = absolute.strip_prefix(root) {
        return inside.to_string_lossy().into_owned();
    }
    if let Ok(canonical) = root.canonicalize()
        && let Ok(inside) = absolute.strip_prefix(&canonical)
    {
        return inside.to_string_lossy().into_owned();
    }
    absolute.to_string_lossy().into_owned()
}

/// The per-file patch body, in git's shape minus the `diff --git` line the
/// journal strips before attaching (`work_journal::snapshot::attach_diffs`).
fn patch_text(path: &str, kind: OwnChangeKind, diff: &stella_diff::Diff) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    match kind {
        OwnChangeKind::Created => out.push_str("--- /dev/null\n"),
        OwnChangeKind::Modified | OwnChangeKind::Deleted => {
            let _ = writeln!(out, "--- a/{path}");
        }
    }
    match kind {
        OwnChangeKind::Deleted => out.push_str("+++ /dev/null\n"),
        OwnChangeKind::Created | OwnChangeKind::Modified => {
            let _ = writeln!(out, "+++ b/{path}");
        }
    }
    for hunk in &diff.hunks {
        out.push_str(&hunk.header());
        out.push('\n');
        for line in &hunk.lines {
            out.push(line.op.sigil());
            out.push_str(&line.text);
            out.push('\n');
        }
    }
    out
}

/// One reading as the wire event every transcript surface folds.
///
/// The counts and the diff are carried, never recounted from the text — the
/// #2290 contract at this seam, the same one `turn_files::file_change` holds
/// for a measured change.
#[must_use]
pub fn file_change(change: OwnChange) -> stella_protocol::AgentEvent {
    stella_protocol::AgentEvent::FileChange {
        path: change.path,
        kind: match change.kind {
            OwnChangeKind::Created => stella_protocol::FileChangeKind::Created,
            OwnChangeKind::Modified => stella_protocol::FileChangeKind::Modified,
            OwnChangeKind::Deleted => stella_protocol::FileChangeKind::Deleted,
        },
        added: change.added,
        removed: change.removed,
        diff: Some(change.diff),
        minimal: change.minimal,
    }
}

/// Attach `changes` to a successful output under [`DATA_KEY`].
///
/// An error output is returned untouched: a failed call reports no change,
/// whatever it may have left behind (the next measurement still sees that).
/// Other keys already in `data` are kept.
#[must_use]
pub fn attach(output: ToolOutput, changes: &[OwnChange]) -> ToolOutput {
    let ToolOutput::Ok { content, data } = output else {
        return output;
    };
    let mut data = match data {
        Some(serde_json::Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    // Serializing a plain struct of strings and integers cannot fail; the
    // fallback keeps the signature infallible rather than hiding a panic.
    let value = serde_json::to_value(changes).unwrap_or(serde_json::Value::Array(Vec::new()));
    data.insert(DATA_KEY.to_string(), value);
    ToolOutput::Ok {
        content,
        data: Some(serde_json::Value::Object(data)),
    }
}

/// The changes a successful output carries under [`DATA_KEY`], or none.
///
/// Lenient on purpose: a tool that carries no reading, or a reading in a shape
/// this version does not parse, is a call with nothing to report — never an
/// error at the seam that publishes it.
#[must_use]
pub fn from_output(output: &ToolOutput) -> Vec<OwnChange> {
    let ToolOutput::Ok {
        data: Some(data), ..
    } = output
    else {
        return Vec::new();
    };
    data.get(DATA_KEY)
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::AgentEvent;

    /// A created file is all-green, counted, and headed `/dev/null` on the old
    /// side — the git spelling every consumer keys "new file" on.
    #[test]
    fn a_created_file_is_an_all_green_diff_from_dev_null() {
        let change = own_change("notes/spec.md", None, "# Spec\n\nbody\n");
        assert_eq!(change.kind, OwnChangeKind::Created);
        assert_eq!((change.added, change.removed), (3, 0));
        assert!(
            change
                .diff
                .starts_with("--- /dev/null\n+++ b/notes/spec.md\n@@ -0,0 +1,3 @@\n")
        );
        assert!(change.diff.contains("+# Spec\n"));
        assert!(
            !change.diff.contains("\n-"),
            "nothing removed: {}",
            change.diff
        );
    }

    /// A modification names both sides and frames the change with git's three
    /// lines of context.
    #[test]
    fn a_modification_is_a_hunk_over_both_sides() {
        let before = "a\nb\nc\nd\ne\nf\ng\n";
        let after = "a\nb\nc\nD\ne\nf\ng\n";
        let change = own_change("src/lib.rs", Some(before), after);
        assert_eq!(change.kind, OwnChangeKind::Modified);
        assert_eq!((change.added, change.removed), (1, 1));
        assert!(
            change
                .diff
                .starts_with("--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,7 +1,7 @@\n")
        );
        assert!(change.diff.contains("\n-d\n+D\n"), "{}", change.diff);
        assert!(change.minimal, "seven lines never trips the area cap");
    }

    /// **The witness for #4696.** An edit large enough to trip
    /// `stella_diff::LCS_AREA_CAP` reports `minimal: false` on the call's own
    /// reading — the flag [`file_change`] must carry onto the wire event so a
    /// transcript surface can tell a real rewrite from a degraded diff.
    #[test]
    fn a_blunt_fallback_edit_reports_itself_as_non_minimal() {
        // 2001 * 2001 = 4,004,001, just over LCS_AREA_CAP (4,000,000) —
        // mirrors `stella_diff`'s own witness for the fallback threshold.
        let before: String = (0..2001).map(|i| format!("old-{i}\n")).collect();
        let after: String = (0..2001).map(|i| format!("new-{i}\n")).collect();

        let change = own_change("big.rs", Some(&before), &after);
        assert!(!change.minimal, "the area cap must have tripped");

        let event = file_change(change);
        let AgentEvent::FileChange { minimal, .. } = event else {
            panic!("file_change must produce a FileChange event");
        };
        assert!(!minimal, "the wire event must carry the same reading");
    }

    /// A deletion is all-red and headed `/dev/null` on the NEW side — the
    /// mirror of a creation, and the git spelling every consumer keys
    /// "deleted file" on.
    #[test]
    fn a_deleted_file_is_an_all_red_diff_to_dev_null() {
        let change = own_delete("notes/spec.md", "# Spec\n\nbody\n");
        assert_eq!(change.kind, OwnChangeKind::Deleted);
        assert_eq!((change.added, change.removed), (0, 3));
        assert!(
            change
                .diff
                .starts_with("--- a/notes/spec.md\n+++ /dev/null\n@@ -1,3 +0,0 @@\n")
        );
        assert!(change.diff.contains("-# Spec\n"));
        // Past the two header lines — `+++ /dev/null` is one of them — no
        // line may be an addition.
        assert!(
            !change.diff.lines().skip(2).any(|l| l.starts_with('+')),
            "nothing added: {}",
            change.diff
        );
    }

    /// An existing empty file is modified, not created: the distinction the
    /// deck renders as `new file`, so it must not be guessed from content.
    #[test]
    fn an_empty_existing_file_is_a_modification() {
        let change = own_change("x", Some(""), "one\n");
        assert_eq!(change.kind, OwnChangeKind::Modified);
        assert!(change.diff.starts_with("--- a/x\n"));
    }

    /// The reading round-trips through `data` untouched, beside whatever the
    /// tool already put there, and never touches `content`.
    #[test]
    fn attach_then_from_output_round_trips_beside_existing_data() {
        let change = own_change("a.txt", None, "hi\n");
        let output =
            ToolOutput::ok_with_data("wrote 3 bytes to a.txt", serde_json::json!({ "keep": 1 }));
        let output = attach(output, std::slice::from_ref(&change));
        let ToolOutput::Ok { content, data } = &output else {
            panic!("attach must keep the Ok arm");
        };
        assert_eq!(content, "wrote 3 bytes to a.txt");
        assert_eq!(data.as_ref().unwrap()["keep"], 1);
        assert_eq!(from_output(&output), vec![change]);
    }

    /// A failed call carries no reading, and an output without one reads as
    /// empty rather than as an error.
    #[test]
    fn errors_and_readingless_outputs_carry_nothing() {
        let change = own_change("a.txt", None, "hi\n");
        let err = attach(ToolOutput::error("no"), &[change]);
        assert!(err.is_error());
        assert!(from_output(&err).is_empty());
        assert!(from_output(&ToolOutput::ok("plain")).is_empty());
    }

    /// A scoped root's relative path is re-spelled against the session root,
    /// and a path outside it stays absolute.
    #[test]
    fn workspace_path_is_session_relative_inside_and_absolute_outside() {
        let root = std::path::Path::new("/ws");
        assert_eq!(workspace_path(root, root, "src/a.rs"), "src/a.rs");
        assert_eq!(
            workspace_path(root, std::path::Path::new("/ws/nested"), "b.rs"),
            "nested/b.rs"
        );
        assert_eq!(
            workspace_path(root, std::path::Path::new("/elsewhere"), "c.rs"),
            "/elsewhere/c.rs"
        );
    }

    /// The scope root is canonical and the session root is as given; where
    /// the two spell one directory differently the path is still relative.
    /// On macOS a tempdir is the live case (`/var` → `/private/var`); on a
    /// filesystem with no such alias both spellings agree and this is the
    /// plain case again.
    #[test]
    fn workspace_path_matches_a_canonical_scope_root_to_the_given_session_root() {
        let dir = tempfile::tempdir().unwrap();
        let given = dir.path();
        let canonical = given.canonicalize().unwrap();
        assert_eq!(workspace_path(given, &canonical, "src/a.rs"), "src/a.rs");
    }
}
