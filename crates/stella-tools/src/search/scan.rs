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
//!
//! # One ignore answer, not two
//!
//! The walk resolves [`stella_graph::workspace_ignore::WorkspaceIgnore`]
//! once per [`scan_hits`] call. That is the same rule set `stella-graph`'s
//! own index walk uses — it honours `.gitignore`, negations, and
//! `.git/info/exclude`. The walk also skips the build and vendor caches in
//! the shared [`stella_graph::DENY_DIRS`] list, instead of keeping a second
//! copy. A path the repository owner excludes stays out of the fallback
//! scan, the same as it stays out of the index.

use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};

use stella_graph::DENY_DIRS;
use stella_graph::admitted::{Admits, admission};
use stella_graph::workspace_ignore::WorkspaceIgnore;

use super::engine::Hit;

/// Files opened in one scan. A bound, not a tuning knob: the walk is
/// synchronous and someone is waiting on it, and a repository large enough
/// to exceed this has an index worth building instead. Reaching it is
/// disclosed in the answer's note — a capped scan whose miss reads as
/// absence is the failure the whole module argues against.
pub const MAX_FILES_SCANNED: usize = 4_000;

/// Bytes read from each file. Enough to cover the imports, the type
/// declarations and the head of a typical module.
const MAX_BYTES_PER_FILE: usize = 64 * 1024;

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
pub struct ScanOutcome {
    /// The ranked hits, best first, at most the caller's limit.
    pub hits: Vec<Hit>,
    /// How many files matched at all — the caller must disclose a cut list.
    pub matched: usize,
    /// The walk stopped at [`MAX_FILES_SCANNED`] with the tree unfinished,
    /// so a miss is inconclusive and the caller must say so.
    pub exhausted: bool,
}

/// Rank files under `root` against `query`, best first.
///
/// Deterministic: score descending, then path ascending, and the walk itself
/// sorts each directory's entries — a filesystem's readdir order is not
/// stable across machines, and two runs of one query must rank identically
/// (invariant 7).
pub fn scan_hits(root: &Path, query: &str, limit: usize) -> ScanOutcome {
    scan_hits_bounded(root, query, limit, MAX_FILES_SCANNED)
}

/// [`scan_hits`] with the file cap injected, so a test can prove the
/// exhaustion disclosure without writing four thousand files.
pub fn scan_hits_bounded(root: &Path, query: &str, limit: usize, max_files: usize) -> ScanOutcome {
    let terms = super::names::terms_of(query);
    if terms.is_empty() {
        return ScanOutcome {
            hits: Vec::new(),
            matched: 0,
            exhausted: false,
        };
    }

    // One `git ls-files` per scan, not per directory — resolved here and
    // threaded down the walk, the same discipline `stella-graph`'s own
    // build-time walk uses for its index passes.
    let ignore = WorkspaceIgnore::resolve(root);

    let mut scored: Vec<(usize, usize, usize, String)> = Vec::new();
    let mut budget = max_files;
    for file in walk(root, &mut budget, &ignore) {
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
/// within each. Skips [`stella_graph::DENY_DIRS`], dotted entries, and
/// anything `ignore` excludes — except for the
/// [`stella_graph::admitted::ADMITTED_SUBTREES`] carve-out. Stops once
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
fn walk(root: &Path, budget: &mut usize, ignore: &WorkspaceIgnore) -> Vec<PathBuf> {
    let mut found = Vec::new();
    // The root-relative path rides with each directory because the carve-out
    // is a fact about a *path* (`.stella/rules`) and the basename alone
    // cannot tell it from `.stella/private`.
    let mut queue = VecDeque::from([(root.to_path_buf(), String::new(), Admits::Files)]);
    while let Some((directory, prefix, admits)) = queue.pop_front() {
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
            let rel = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            // `symlink_metadata`, not `metadata`: a symlink pointing at a
            // parent directory would otherwise make this walk unbounded, and
            // a symlink out of the workspace would read files the search has
            // no business reading.
            let Ok(metadata) = fs::symlink_metadata(&child) else {
                continue;
            };
            if metadata.is_dir() {
                let ordinary =
                    admits == Admits::Files && !name.starts_with('.') && !DENY_DIRS.contains(&name);
                let carved = (!ordinary).then(|| admission(&rel)).flatten();
                // The repository's own rules win over the carve-out, exactly
                // as they do in `stella-graph`'s build-time walk: a project
                // that gitignores `.stella/` has no records to publish.
                if (ordinary || carved.is_some()) && !ignore.excludes_dir(&rel) {
                    queue.push_back((child, rel, carved.unwrap_or(Admits::Files)));
                }
            } else if metadata.is_file()
                && admits == Admits::Files
                && !name.starts_with('.')
                && !ignore.excludes(&rel)
            {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace's `.gitignore` can exclude a directory. The fallback scan
    /// must never show a file from it. A hit is not just a rank: it is a
    /// path plus a claim that the path matched — enough to open the file
    /// next.
    #[test]
    fn a_gitignored_directory_is_never_offered_by_the_fallback_scan() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(".gitignore"), "secret/\n").unwrap();
        fs::create_dir_all(root.path().join("secret")).unwrap();
        fs::write(root.path().join("secret/token.txt"), "zqxfrobnicate9k\n").unwrap();

        let outcome = scan_hits(root.path(), "zqxfrobnicate9k", 10);
        assert!(
            outcome.hits.is_empty(),
            "a term unique to a gitignored file must not surface a hit: {:?}",
            outcome.hits
        );
        assert_eq!(outcome.matched, 0);
    }

    /// A negated pattern proves the walk uses git's real rules. A plain
    /// prefix check could not un-ignore one file inside a broader `*.log`
    /// rule, but `!keep.log` can.
    #[test]
    fn a_negated_gitignore_pattern_still_surfaces_its_own_file() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(".gitignore"), "*.log\n!keep.log\n").unwrap();
        fs::write(root.path().join("keep.log"), "kept9k\n").unwrap();
        fs::write(root.path().join("drop.log"), "kept9k\n").unwrap();

        let outcome = scan_hits(root.path(), "kept9k", 10);
        let paths: Vec<&str> = outcome.hits.iter().map(|hit| hit.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["keep.log"],
            "the negated file surfaces and the still-ignored sibling does not"
        );
    }
}
