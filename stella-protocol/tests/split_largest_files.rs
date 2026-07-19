//! Witness test: the four largest source files in the workspace must be
//! split up into logically organized, smaller modules.
//!
//! At the time this test was written the four largest files were:
//!   stella-tui/src/deck_ui.rs      6629 lines
//!   stella-cli/src/command_deck.rs 5199 lines
//!   stella-store/src/lib.rs        4582 lines
//!   stella-cli/src/agent.rs        4262 lines
//!
//! A file counts as "split" when the module still exists (as the file
//! itself, and/or a sibling module directory with the same stem, e.g.
//! `deck_ui.rs` -> `deck_ui/…`) AND every Rust file belonging to that
//! module is at or below MAX_LINES lines. Deleting the module outright
//! does not pass.

use std::fs;
use std::path::{Path, PathBuf};

/// Each of the four monoliths currently has 4200+ lines; after a real
/// split every resulting piece must fit under this ceiling.
const MAX_LINES: usize = 2500;

const TARGETS: &[&str] = &[
    "stella-tui/src/deck_ui.rs",
    "stella-cli/src/command_deck.rs",
    "stella-store/src/lib.rs",
    "stella-cli/src/agent.rs",
];

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <root>/stella-protocol
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("stella-protocol should live inside the workspace root")
        .to_path_buf()
}

fn line_count(path: &Path) -> usize {
    fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
        .lines()
        .count()
}

/// Collect every .rs file that belongs to the module rooted at `target`:
/// the file itself (if it still exists) plus any .rs files inside a
/// sibling directory with the same stem (recursive).
fn module_files(root: &Path, target: &str) -> Vec<PathBuf> {
    let file = root.join(target);
    let mut found = Vec::new();
    if file.is_file() {
        found.push(file.clone());
    }
    let dir = file.with_extension("");
    if dir.is_dir() {
        collect_rs(&dir, &mut found);
    }
    // For a lib.rs target the "module directory" is its parent src/ dir;
    // splitting lib.rs necessarily creates sibling files, which are fine —
    // we only require lib.rs itself to shrink, so nothing extra to collect.
    found
}

fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display())) {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn four_largest_files_are_split_into_smaller_modules() {
    let root = workspace_root();
    let mut violations = Vec::new();

    for target in TARGETS {
        let files = module_files(&root, target);
        assert!(
            !files.is_empty(),
            "{target} was removed entirely instead of being split into modules \
             (expected the file, or a module directory with the same stem, to exist)"
        );
        for file in files {
            let lines = line_count(&file);
            if lines > MAX_LINES {
                violations.push(format!(
                    "{} has {lines} lines (max allowed: {MAX_LINES}) — split it into smaller modules",
                    file.strip_prefix(&root).unwrap_or(&file).display()
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "the largest files have not been split up:\n  {}",
        violations.join("\n  ")
    );
}
