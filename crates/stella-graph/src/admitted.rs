// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Which subtrees of `.stella/` a retrieval surface may read, and which it
//! must refuse by name.
//!
//! `.stella/` is the agent's own private state and every retrieval walk skips
//! it wholesale — except for `.stella/rules/`, which holds the published
//! **context records**: the one part of `.stella/` deliberately tracked in Git,
//! because a record only steers a teammate's session if it travels with the
//! repository (AGENTS.md § "The `.stella/` directory"). A retrieval tool that
//! cannot see the repository's own steering policy is the hole #3162 names.
//!
//! # Why the list lives here rather than beside each walk
//!
//! Two walks apply it: the code-graph walk (this crate's `walk`, #4492) and the
//! search tool's index-free scan (`stella-tools`, #4456). They are in
//! different crates and neither can see the other's constant, so a second copy
//! is how one of them comes to admit `.stella/private/` — SQLite state, OAuth
//! tokens, mined reflections — after somebody edits the other. This crate is
//! the lower of the two, so the policy lives here and the scan reads it.
//!
//! The exclusion of `.stella/private/` holds because it is **not on this
//! list**, rather than as a side effect of skipping everything above it. That
//! is the difference #3162 asked for: the thing that must stay unreadable is
//! refused by name, in one place.

use std::path::{Component, Path};

/// Subtrees a retrieval walk enters despite the hidden-directory rule and
/// whatever deny-list it applies, named by their path relative to the walk
/// root with `/` separators on every platform.
pub const ADMITTED_SUBTREES: &[&str] = &[".stella/rules"];

/// Whether a directory a walk descends into contributes its own files, or is
/// only being crossed to reach a subtree that does.
///
/// The distinction exists because an admitted subtree sits *under* a skipped
/// one: reaching `.stella/rules` means entering `.stella`, and entering
/// `.stella` must not index `.stella/settings.json` or `.stella/memories/` on
/// the way past.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admits {
    /// An ordinary directory: its own files are read.
    Files,
    /// Crossed only because an [`ADMITTED_SUBTREES`] entry sits underneath.
    /// Its own files are not read, and its other children are refused unless
    /// they are themselves on the way to — or inside — an admitted subtree.
    Passage,
}

/// What `rel` is to [`ADMITTED_SUBTREES`]: inside one, on the way to one, or
/// neither. `None` means the walk has no business there.
///
/// `rel` is a walk-relative path with `/` separators, exactly as the two
/// walks build it.
pub fn admission(rel: &str) -> Option<Admits> {
    for allowed in ADMITTED_SUBTREES {
        if rel == *allowed || rel.starts_with(&format!("{allowed}/")) {
            return Some(Admits::Files);
        }
        if allowed.starts_with(&format!("{rel}/")) {
            return Some(Admits::Passage);
        }
    }
    None
}

/// Whether `path` names a file inside a `.stella/rules` directory — the one
/// place a `.toml` file is a published context record rather than a build
/// manifest.
///
/// This is what keeps `Cargo.toml`, `deny.toml` and `rust-toolchain.toml` out
/// of the index once [`crate::Language::Toml`] exists (#4492): an extension
/// alone would make every manifest in the tree an indexable document, which
/// buys a corpus of dependency version tables and dilutes the records the
/// language was added for.
///
/// Component-wise rather than substring-wise, and it reads any prefix, so it
/// answers the same for the absolute path the walk carries and for the
/// workspace-relative path stored in the index.
pub fn is_context_record(path: &Path) -> bool {
    let mut components = path.components();
    while let Some(component) = components.next() {
        if !matches!(component, Component::Normal(name) if name == ".stella") {
            continue;
        }
        if matches!(components.clone().next(), Some(Component::Normal(next)) if next == "rules") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_records_directory_is_admitted_and_crossed_to() {
        assert_eq!(admission(".stella/rules"), Some(Admits::Files));
        assert_eq!(
            admission(".stella/rules/ctx.demo.toml"),
            Some(Admits::Files)
        );
        assert_eq!(admission(".stella"), Some(Admits::Passage));
    }

    /// The half that matters: every other child of `.stella` is refused, and
    /// refused *by name* rather than by the dot rule that used to hide it.
    #[test]
    fn private_state_and_the_rest_of_stella_are_refused() {
        for refused in [
            ".stella/private",
            ".stella/private/store.db",
            ".stella/memories",
            ".stella/skills/x",
            ".stella/tools",
            ".stella/settings.json",
            "src",
            ".git",
        ] {
            assert_eq!(admission(refused), None, "`{refused}` must be refused");
        }
    }

    /// A prefix match on the string would admit `.stella/rules-backup/`; the
    /// separator is what makes the entry name a directory rather than a stem.
    #[test]
    fn a_sibling_sharing_the_prefix_is_not_the_admitted_subtree() {
        assert_eq!(admission(".stella/rules-backup"), None);
        assert_eq!(admission(".stella/rules-backup/x.toml"), None);
    }

    #[test]
    fn a_record_is_recognised_under_any_prefix() {
        assert!(is_context_record(Path::new(".stella/rules/ctx.demo.toml")));
        assert!(is_context_record(Path::new(
            "/home/a/proj/.stella/rules/nested/ctx.demo.toml"
        )));
    }

    #[test]
    fn a_manifest_outside_the_records_directory_is_not_a_record() {
        for manifest in [
            "Cargo.toml",
            "crates/stella-graph/Cargo.toml",
            "deny.toml",
            ".stella/rules.toml",
            ".stella/tools/mine.toml",
            ".stella/private/rules/leaked.toml",
        ] {
            assert!(
                !is_context_record(Path::new(manifest)),
                "`{manifest}` must not read as a context record"
            );
        }
    }
}
