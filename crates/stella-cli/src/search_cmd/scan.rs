//! The strategy that needs no index at all: walk the working tree and rank
//! files by how many query terms appear in their path and their contents.
//!
//! # This is not an edge case
//!
//! `stella init` is best-effort by design, and a workspace whose language has
//! no tree-sitter grammar in `stella-graph` produces an empty index. Without
//! this rung, a search would answer "nothing found" on exactly those
//! workspaces.
//!
//! # Why it still is not a match list
//!
//! It ranks *files*, and it says how many terms and where they matched. It
//! deliberately does not emit `path:line: text` rows: that output shape is
//! what `grep -n` already produces. The path is a pointer; the follow-up is
//! opening the file.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use super::engine::Hit;

/// Files opened in one scan. A bound, not a tuning knob: the walk is
/// synchronous and someone is waiting on it, and a repository large enough
/// to exceed this has an index worth building instead. Reaching it is
/// disclosed in the answer's note — a capped scan whose miss reads as
/// absence is the failure the whole module argues against.
pub(crate) const MAX_FILES_SCANNED: usize = 4_000;

/// Bytes read from each file. Enough to cover the imports, the type
/// declarations and the head of a typical module.
const MAX_BYTES_PER_FILE: usize = 64 * 1024;

/// Directories never descended into. Every entry is either machine-generated
/// or another tool's private state; a term matched inside one of them points
/// at nothing the reader can edit.
const SKIPPED_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".stella",
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
    ".mypy_cache",
    ".pytest_cache",
    ".next",
    ".cargo",
];

/// A term found in a path is worth this many found in contents.
///
/// A file *named* for the thing is a stronger signal than a file that
/// mentions it: `retry.rs` is about retries; a file that says "retry" twice
/// in a comment is not. Three is judgement, not measurement — it is enough to
/// float a well-named file over an incidental mention without letting a lucky
/// path beat a file that is genuinely dense in the term.
const PATH_TERM_WEIGHT: usize = 3;

/// What one scan produced, and what it could not see.
#[derive(Debug)]
pub(crate) struct ScanOutcome {
    /// The ranked hits, best first, at most the caller's limit.
    pub(crate) hits: Vec<Hit>,
    /// How many files matched at all — the caller must disclose a cut list.
    pub(crate) matched: usize,
    /// The walk stopped at [`MAX_FILES_SCANNED`] with the tree unfinished,
    /// so a miss is inconclusive and the caller must say so.
    pub(crate) exhausted: bool,
}

/// Rank files under `root` against `query`, best first.
///
/// Deterministic: score descending, then path ascending, and the walk itself
/// sorts each directory's entries — a filesystem's readdir order is not
/// stable across machines, and two runs of one query must rank identically
/// (invariant 7).
pub(crate) fn scan_hits(root: &Path, query: &str, limit: usize) -> ScanOutcome {
    scan_hits_bounded(root, query, limit, MAX_FILES_SCANNED)
}

/// [`scan_hits`] with the file cap injected, so a test can prove the
/// exhaustion disclosure without writing four thousand files.
pub(crate) fn scan_hits_bounded(
    root: &Path,
    query: &str,
    limit: usize,
    max_files: usize,
) -> ScanOutcome {
    let terms = super::enrich::terms_of(query);
    if terms.is_empty() {
        return ScanOutcome {
            hits: Vec::new(),
            matched: 0,
            exhausted: false,
        };
    }

    let mut scored: Vec<(usize, usize, usize, String)> = Vec::new();
    let mut budget = max_files;
    for file in walk(root, &mut budget) {
        let Ok(relative) = file.strip_prefix(root) else {
            continue;
        };
        let path = relative.to_string_lossy().replace('\\', "/");
        let lowered_path = path.to_lowercase();
        let in_path = terms
            .iter()
            .filter(|term| lowered_path.contains(*term))
            .count();

        let contents = read_head(&file).unwrap_or_default().to_lowercase();
        let in_body = terms.iter().filter(|term| contents.contains(*term)).count();

        let score = in_path * PATH_TERM_WEIGHT + in_body;
        if score > 0 {
            scored.push((score, in_path, in_body, path));
        }
    }

    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.3.cmp(&b.3)));
    let matched = scored.len();
    scored.truncate(limit);
    let hits = scored
        .into_iter()
        .map(|(_, in_path, in_body, path)| Hit {
            why: format!(
                "matched {in_path} query term(s) in its PATH and {in_body} in its contents, out \
                 of {} (no index was available, so this is a term match, not a meaning match)",
                terms.len()
            ),
            path,
            focus: None,
        })
        .collect();
    ScanOutcome {
        hits,
        matched,
        exhausted: budget == 0,
    }
}

/// Every regular file under `root`, shallowest directories first and sorted
/// within each, skipping [`SKIPPED_DIRS`] and dotted entries, stopping once
/// `budget` files have been yielded.
///
/// Breadth-first deliberately, because the order decides **which** files a
/// capped walk ever sees: a stack here walked the alphabetically *last*
/// subtree first, so on a tree bigger than the cap the files most likely to
/// matter — the root's own files, `src/` — were exactly the ones the budget
/// never reached. Shallow-first keeps the cap's cut biased toward the top of
/// the tree, and stays deterministic across machines.
///
/// Iterative rather than recursive: a symlink loop or a pathological tree
/// must cost a bounded walk, never the process's stack.
fn walk(root: &Path, budget: &mut usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut queue = VecDeque::from([root.to_path_buf()]);
    while let Some(directory) = queue.pop_front() {
        if *budget == 0 {
            break;
        }
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        let mut children: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        children.sort();
        for child in children {
            if *budget == 0 {
                break;
            }
            let Some(name) = child.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            // `symlink_metadata`, not `metadata`: a symlink pointing at a
            // parent directory would otherwise make this walk unbounded, and
            // a symlink out of the workspace would read files the search has
            // no business reading.
            let Ok(metadata) = fs::symlink_metadata(&child) else {
                continue;
            };
            if metadata.is_dir() {
                if !name.starts_with('.') && !SKIPPED_DIRS.contains(&name) {
                    queue.push_back(child);
                }
            } else if metadata.is_file() && !name.starts_with('.') {
                *budget -= 1;
                found.push(child);
            }
        }
    }
    found
}

/// The first [`MAX_BYTES_PER_FILE`] of `path` as text, or `None` when it is
/// binary or unreadable.
fn read_head(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let head = &bytes[..bytes.len().min(MAX_BYTES_PER_FILE)];
    // A NUL byte in the head is the same binary sniff `grep` uses. Cheaper
    // than a UTF-8 validation over a large blob and it fails in the right
    // direction: a text file with a stray NUL is skipped, never mis-decoded.
    if head.contains(&0) {
        return None;
    }
    Some(String::from_utf8_lossy(head).into_owned())
}
