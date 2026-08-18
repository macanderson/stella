// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The system temp directories every session may write to.
//!
//! # Why the confinement stops short of here
//!
//! [`stella_core::workspace_scope`] confines writes because a write outside
//! the session's tree is what turns a bad turn into lost work. The system temp
//! directory is the one place on the machine where that reason does not hold:
//! nothing the user owns lives there, the operating system reclaims it, and
//! every other program on the machine already treats it as common scratch
//! space. Refusing it protects nothing and costs a turn.
//!
//! This reverses a deliberate earlier decision — `/tmp` was exempt in
//! `crate::bash`, was removed on the argument that a session has
//! `STELLA_SCRATCH` for exactly this, and is now readmitted. Both halves of
//! that argument were true; what settled it is that the model does not reach
//! for `STELLA_SCRATCH`, it reaches for `/tmp`, because that is what shell
//! convention is. `cargo clippy … | tee /tmp/out.txt` is an ordinary correct
//! command, and a boundary that refuses ordinary correct commands is
//! discovered as a bug rather than obeyed as a policy. `STELLA_SCRATCH` is
//! still the better place — it is per-session, so it cannot collide with a
//! sibling run, and it is cleaned up with the session — and it stays the one
//! the prompt recommends. This makes the conventional spelling work too.
//!
//! # What is granted, exactly
//!
//! `TMPDIR` (via [`std::env::temp_dir`]) and `/tmp`. On macOS those are
//! different directories — `TMPDIR` is a per-user `/var/folders/…` path while
//! `/tmp` symlinks to `/private/tmp` — and a model writes to either, so both
//! are granted. Each is canonicalized, because a scope's roots must be
//! canonical for anything to be compared against them (see
//! [`crate::ctx::ToolCtx::resolve_for_write`]); one that does not resolve is
//! dropped rather than kept, because unlike an operator's `--allow-dir` typo
//! there is no intent here to honour — a temp directory that is not there is
//! simply not this platform's temp directory.

use std::path::{Path, PathBuf};

/// The canonical system temp directories, in a stable order, deduplicated.
///
/// Deliberately not cached: it is called once per `write_scope()` and the two
/// `canonicalize` calls are cheap next to the tool dispatch that follows.
pub fn system_temp_roots() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for candidate in [std::env::temp_dir(), PathBuf::from("/tmp")] {
        let Ok(canonical) = candidate.canonicalize() else {
            continue;
        };
        if !is_usable_root(&canonical) || out.contains(&canonical) {
            continue;
        }
        out.push(canonical);
    }
    out
}

/// Whether a resolved candidate is a directory small enough to be a grant
/// rather than a surrender.
///
/// A filesystem root is refused. `TMPDIR` is environment the session inherits,
/// so `TMPDIR=/` would otherwise hand the whole machine write access through a
/// path meant to widen it by one directory — the shape where a permissive
/// reading of untrusted configuration turns a confinement check off entirely.
fn is_usable_root(path: &Path) -> bool {
    path.parent().is_some() && path.is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_temp_directory_is_granted_and_canonical() {
        let roots = system_temp_roots();
        assert!(!roots.is_empty(), "every supported platform has a temp dir");
        for root in &roots {
            assert!(root.is_absolute(), "{root:?} must be absolute");
            assert_eq!(
                &root.canonicalize().expect("resolved once already"),
                root,
                "{root:?} must already be canonical"
            );
        }
    }

    /// On macOS `TMPDIR` and `/tmp` are different directories and both are
    /// granted; on a platform where they coincide the duplicate collapses.
    #[test]
    fn the_roots_are_deduplicated() {
        let roots = system_temp_roots();
        let mut seen = roots.clone();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), roots.len(), "{roots:?}");
    }

    #[test]
    fn a_filesystem_root_is_never_usable() {
        assert!(!is_usable_root(Path::new("/")));
        assert!(is_usable_root(
            &std::env::temp_dir().canonicalize().unwrap()
        ));
    }
}
