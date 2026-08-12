// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The strategy that needs no index at all: walk the working tree and rank
//! files by how many query terms appear in their path and their contents.
//!
//! # This is not an edge case
//!
//! `stella init` is best-effort by design, and a workspace whose language has
//! no tree-sitter grammar in `stella-graph` produces an empty index — which
//! is the *normal* case on Terminal-Bench, where the task tree is frequently
//! a handful of shell scripts, config and a language nobody indexed. Without
//! this rung, `search` would answer "nothing found" on exactly the workspaces
//! the bench measures, and the fused-tool experiment would fail for a reason
//! that has nothing to do with fusion.
//!
//! # Why it still is not a match list
//!
//! It ranks *files*, and it says how many terms and where they matched. It
//! deliberately does not emit `path:line: text` rows: that output shape is
//! what `grep -n` already produces, and a tool that produces it competes with
//! the shell on the shell's ground (see the parent module's docs). The path
//! is a pointer; the follow-up is `read_file`.

use std::fs;
use std::path::{Path, PathBuf};

use super::Hit;

/// Files opened in one scan. A bound, not a tuning knob: the walk is
/// synchronous and an agent is waiting on it, and a repository large enough
/// to exceed this has an index worth building instead.
const MAX_FILES_SCANNED: usize = 4_000;

/// Bytes read from each file. Enough to cover the imports, the type
/// declarations and the head of a typical module.
const MAX_BYTES_PER_FILE: usize = 64 * 1024;

/// Directories never descended into. Every entry is either machine-generated
/// or another tool's private state; a term matched inside one of them points
/// at nothing the agent can edit.
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

/// Rank files under `root` against `query`, best first.
///
/// Deterministic: score descending, then path ascending, and the walk itself
/// sorts each directory's entries — a filesystem's readdir order is not
/// stable across machines, and two runs of one query must rank identically
/// (invariant 7).
pub(crate) fn scan_hits(root: &Path, query: &str, limit: usize) -> Vec<Hit> {
    let terms = super::enrich::terms_of(query);
    if terms.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(usize, usize, usize, String)> = Vec::new();
    let mut budget = MAX_FILES_SCANNED;
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
    scored.truncate(limit);
    scored
        .into_iter()
        .map(|(_, in_path, in_body, path)| Hit {
            why: format!(
                "matched {in_path} query term(s) in its PATH and {in_body} in its contents, out \
                 of {} (no index was available, so this is a term match, not a meaning match)",
                terms.len()
            ),
            path,
        })
        .collect()
}

/// Every regular file under `root`, sorted, skipping [`SKIPPED_DIRS`] and
/// dotted entries, stopping once `budget` files have been yielded.
///
/// Iterative rather than recursive: a symlink loop or a pathological tree
/// must cost a bounded walk, never the process's stack.
fn walk(root: &Path, budget: &mut usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut queue = vec![root.to_path_buf()];
    while let Some(directory) = queue.pop() {
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
            // a symlink out of the workspace would read files the tool has no
            // business reading.
            let Ok(metadata) = fs::symlink_metadata(&child) else {
                continue;
            };
            if metadata.is_dir() {
                if !name.starts_with('.') && !SKIPPED_DIRS.contains(&name) {
                    queue.push(child);
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
