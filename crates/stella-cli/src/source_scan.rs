// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Reading this crate's own source, so a "these are all the sites" claim can
//! be asserted instead of believed.
//!
//! # When this shape is the right one
//!
//! Rarely. A compile-time guard is better wherever one exists — the
//! exhaustive destructuring in [`crate::settings::completeness`] is the
//! reference, because adding a field there stops the crate compiling rather
//! than reddening a test. This module is for the case that has no such
//! guard: a **security chokepoint**, where the claim is that one function is
//! the only door to something and every other door is closed by review alone.
//! `project_code_execution_trusted`'s five gated surfaces (#4426) and the
//! plugins tier's single chokepoint (#3521) are both that shape, and both had
//! a doc comment claiming an enumeration that no grep could confirm.
//!
//! # What counts as a site
//!
//! Code, never prose. A line whose first non-space characters are `//` is a
//! comment, and a doc comment naming a function in order to explain it is a
//! citation rather than a call — counting it would make every cross-reference
//! a new site to justify. Test sources are skipped for the same reason a
//! fixture is not a caller: a file under a `tests/` directory, or named
//! `tests.rs`, describes the guard rather than being guarded by it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Every production source file in this crate with a non-comment line
/// containing `needle`, as a path relative to `src/` (`plugin_cmd/roster.rs`).
///
/// Sorted and deduplicated, so a caller compares against a written-out set
/// and reads the difference directly when it fails.
pub(crate) fn production_files_mentioning(needle: &str) -> BTreeSet<String> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut found = BTreeSet::new();
    walk(&src, &src, needle, &mut found);
    found
}

fn walk(dir: &Path, src_root: &Path, needle: &str, found: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "tests" {
                walk(&path, src_root, needle, found);
            }
            continue;
        }
        // This module is the guard, never a guarded site: every needle it
        // names is a fixture for its own directions, and counting them would
        // report the scanner as the thing it is scanning for.
        if name == "tests.rs" || name == "source_scan.rs" || !name.ends_with(".rs") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let cited_in_code = text
            .lines()
            .any(|line| !line.trim_start().starts_with("//") && line.contains(needle));
        if cited_in_code && let Ok(relative) = path.strip_prefix(src_root) {
            found.insert(relative.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scanner's own directions, so a caller's failure is never the
    /// scanner's fault: code counts and comments do not.
    ///
    /// `crate::plugin_hooks` is the fixture for the negative half. Its module
    /// doc names `resolve_project_plugins_dir` — in order to say that it
    /// deliberately does not call it — and a scanner counting that mention
    /// would report the one module built to honour the chokepoint as the
    /// module breaking it.
    #[test]
    fn code_counts_and_prose_does_not() {
        let loaders = production_files_mentioning("PluginRoster::load");
        assert!(
            loaders.contains("plugin_hooks.rs"),
            "a call is a site: {loaders:?}"
        );
        let tier = production_files_mentioning("resolve_project_plugins_dir");
        assert!(
            !tier.contains("plugin_hooks.rs"),
            "and a doc comment naming the same module is not: {tier:?}"
        );
    }
}
