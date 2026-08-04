//! File anchors: which files a memory is *about*.
//!
//! Two callers share this, and they must agree or the feature is incoherent:
//!
//! * the write side ([`resolve_anchors`]), which turns a freshly mined lesson
//!   into `observed_in` edges at reflection time, and
//! * `stella memory validate`, which reads the same tokens out of memory text
//!   to report which memories have rotted.
//!
//! The extraction lives here rather than in the command module because the
//! write path depending on a `_cmd` module would invert the layering — the CLI
//! surface should call the memory subsystem, not the other way round.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

/// Source extensions a token must end in to count as a file reference.
const ANCHOR_EXTS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "go", "md", "sql", "toml",
];

/// At most this many anchors per memory.
///
/// A lesson that recites a dozen filenames is a summary of a turn, not a fact
/// about one file, and anchoring it to all of them would make every one of
/// those files recall it. The cap keeps a single bad lesson from polluting a
/// wide slice of the graph, and the truncation is deterministic (the extraction
/// order is the order the paths appear in the text).
const MAX_ANCHORS_PER_MEMORY: usize = 8;

/// Extract file anchors from memory text. Two shapes count:
///
/// * **Rooted paths** — `word/word[/word…]` with a recognized source
///   extension, e.g. `stella-cli/src/agent.rs`, `docs/design/adaptive-context/context-reuse.md`.
///   These resolve against the workspace root directly.
/// * **Bare filenames** — a name with a recognized extension and no
///   directory at all, e.g. `slash_models_witness.rs`, `Cargo.toml`.
///
/// Bare filenames matter because that is how reflections actually name
/// files: a lesson mined from a turn says "in `slash_models_witness.rs`",
/// not "in `stella-cli/tests/slash_models_witness.rs`". Requiring a slash
/// meant the memories most likely to rot — the ones naming a single test
/// file that later got deleted — were reported as `no-anchors` and never
/// checked, which is the failure this function exists to catch.
pub(crate) fn extract_path_anchors(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '/' || c == '.' || c == '-' || c == '_'))
        .filter_map(|tok| {
            // Trim trailing dots — a sentence like "see src/lib.rs." would
            // otherwise produce token "src/lib.rs." with extension "rs.".
            let tok = tok.trim_end_matches('.');
            if tok.len() > 256 || tok.starts_with('/') || tok.split('/').any(|seg| seg == "..") {
                return None;
            }
            let (stem, ext) = tok.rsplit_once('.')?;
            // A bare `.rs` has no stem and names nothing; `graph_snapshot`
            // has no extension. Both must stay out.
            if stem.is_empty() || !ANCHOR_EXTS.contains(&ext) {
                return None;
            }
            Some(tok.to_string())
        })
        .collect()
}

/// Directories that never hold source a memory would meaningfully anchor to,
/// and that dominate the walk cost when present.
const ANCHOR_WALK_SKIP: &[&str] = &[
    ".git",
    ".stella",
    "target",
    "node_modules",
    ".next",
    ".venv",
    "venv",
    "dist",
    "build",
    "__pycache__",
];

/// Cap on the tree walk, so a pathological or symlinked tree cannot stall a
/// turn's reflection or an inspection command.
const MAX_FILES: usize = 200_000;

/// Every file name present anywhere under the workspace, for resolving
/// bare-filename anchors — they carry no directory to join onto the root, so
/// existence is a "does the tree contain a file by this name" question.
pub(crate) fn workspace_file_names(root: &Path) -> HashSet<String> {
    workspace_file_index(root).into_keys().collect()
}

/// File name → every workspace-relative path carrying that name.
///
/// The write side needs the paths, not just the names: an anchor edge points at
/// a `file://` node and a bare name is not a location. Where a name is
/// ambiguous the list has more than one entry, and [`resolve_anchors`] declines
/// to guess.
pub(crate) fn workspace_file_index(root: &Path) -> HashMap<String, Vec<String>> {
    let mut index: HashMap<String, Vec<String>> = HashMap::new();
    let mut seen = 0usize;
    let mut queue = VecDeque::from([root.to_path_buf()]);
    while let Some(dir) = queue.pop_front() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            // `is_dir`/`is_file` on the entry type does not follow symlinks,
            // so a self-referential link cannot re-enter the walk.
            if kind.is_dir() {
                if !ANCHOR_WALK_SKIP.contains(&name.as_str()) {
                    queue.push_back(entry.path());
                }
            } else if kind.is_file() {
                let path = entry.path();
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                index.entry(name).or_default().push(rel);
                seen += 1;
                if seen >= MAX_FILES {
                    return index;
                }
            }
        }
    }
    index
}

/// Workspace-relative paths a memory should anchor to, resolved against the
/// tree as it is right now.
///
/// Only files that **exist** are anchored. An anchor to a path that was never
/// there is not a fact that went stale, it is a hallucinated filename, and
/// writing it would make the staleness scan's first pass immediately "end" an
/// anchor that never held — recording a fiction in the world-time axis.
///
/// A bare filename matching **several** files is skipped rather than guessed.
/// Anchoring `mod.rs` to whichever copy the walk happened to reach first would
/// attach the lesson to an arbitrary file, and a confidently wrong edge is
/// worse here than a missing one: the missing edge costs recall a little
/// precision, the wrong one teaches the graph something false.
pub(crate) fn resolve_anchors(root: &Path, text: &str) -> Vec<String> {
    let tokens = extract_path_anchors(text);
    if tokens.is_empty() {
        return Vec::new();
    }
    // Only pay for the tree walk if a bare name actually needs resolving.
    let needs_index = tokens.iter().any(|t| !t.contains('/'));
    let index = if needs_index {
        workspace_file_index(root)
    } else {
        HashMap::new()
    };

    let mut out: Vec<String> = Vec::new();
    for tok in tokens {
        let resolved = if tok.contains('/') {
            root.join(&tok).is_file().then_some(tok)
        } else {
            match index.get(&tok).map(Vec::as_slice) {
                Some([only]) => Some(only.clone()),
                _ => None,
            }
        };
        if let Some(path) = resolved
            && !out.contains(&path)
        {
            out.push(path);
            if out.len() >= MAX_ANCHORS_PER_MEMORY {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn extract_path_anchors_finds_source_paths() {
        let text = "graph_snapshot() in stella-cli/src/agent.rs computes the \
                    neighborhood. See also docs/hooks.md and src/lib.rs.";
        let anchors = extract_path_anchors(text);
        assert!(anchors.contains(&"stella-cli/src/agent.rs".to_string()));
        assert!(anchors.contains(&"docs/hooks.md".to_string()));
        assert!(anchors.contains(&"src/lib.rs".to_string()));
        // No false positives from bare words.
        assert!(!anchors.iter().any(|a| a == "graph_snapshot"));
    }

    #[test]
    fn extract_path_anchors_finds_bare_filenames() {
        // The shape reflections actually use: a file named with no directory.
        let text = "In stella-cli witness tests (slash_models_witness.rs), prefer \
                    updating assertions rather than changing the renderer.";
        let anchors = extract_path_anchors(text);
        assert!(
            anchors.contains(&"slash_models_witness.rs".to_string()),
            "bare filenames must anchor: {anchors:?}"
        );
    }

    #[test]
    fn extract_path_anchors_rejects_non_files() {
        // A bare extension has no stem, version strings have no source
        // extension, and prose words have no extension at all.
        let anchors = extract_path_anchors("the .rs files in v0.4.47 use cargo test -- --exact");
        assert!(anchors.is_empty(), "no false anchors: {anchors:?}");
    }

    #[test]
    fn resolve_anchors_keeps_only_files_that_exist() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/real.rs"), "").unwrap();

        let got = resolve_anchors(
            root.path(),
            "the fix is in src/real.rs, not src/imaginary.rs",
        );
        assert_eq!(got, vec!["src/real.rs".to_string()]);
    }

    #[test]
    fn resolve_anchors_resolves_an_unambiguous_bare_filename_to_its_path() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("a/b")).unwrap();
        std::fs::write(root.path().join("a/b/only.rs"), "").unwrap();

        let got = resolve_anchors(root.path(), "see only.rs for the registry");
        assert_eq!(got, vec!["a/b/only.rs".to_string()]);
    }

    #[test]
    fn resolve_anchors_declines_to_guess_an_ambiguous_bare_filename() {
        // Two files named `mod.rs`. Picking either would attach the lesson to
        // an arbitrary one — a confidently wrong edge, which is worse than none.
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("a")).unwrap();
        std::fs::create_dir_all(root.path().join("b")).unwrap();
        std::fs::write(root.path().join("a/mod.rs"), "").unwrap();
        std::fs::write(root.path().join("b/mod.rs"), "").unwrap();

        assert!(resolve_anchors(root.path(), "as mod.rs shows").is_empty());
    }

    #[test]
    fn resolve_anchors_caps_a_lesson_that_recites_the_whole_tree() {
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        let mut text = String::new();
        for i in 0..(MAX_ANCHORS_PER_MEMORY + 5) {
            std::fs::write(root.path().join(format!("src/f{i}.rs")), "").unwrap();
            text.push_str(&format!("src/f{i}.rs "));
        }
        assert_eq!(
            resolve_anchors(root.path(), &text).len(),
            MAX_ANCHORS_PER_MEMORY
        );
    }

    #[test]
    fn resolve_anchors_skips_the_tree_walk_when_no_bare_name_needs_it() {
        // A rooted path resolves by `join`, so a workspace with no readable
        // tree still anchors it. Guards the `needs_index` shortcut.
        let root = tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/one.rs"), "").unwrap();
        assert_eq!(
            resolve_anchors(root.path(), "src/one.rs is the place"),
            vec!["src/one.rs".to_string()]
        );
    }
}
