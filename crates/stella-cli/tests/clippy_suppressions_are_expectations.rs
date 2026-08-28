// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Every clippy suppression in the workspace is an `#[expect(…, reason = "…")]`.
//!
//! AGENTS.md's "Code style and conventions" asks that no `#[allow]` on a
//! clippy lint land without a comment saying why the lint is wrong *here*.
//! The crate had 23 `#[allow(clippy::too_many_arguments)]`, four of which
//! carried a reason and nineteen of which carried nothing (#3698).
//!
//! The remedy is `#[expect]` rather than a commented `#[allow]`, for a reason
//! `#[allow]` cannot offer: an expectation that stops being true is a compile
//! error under `-D warnings`. That is not theoretical here — converting the
//! nineteen made clippy report `proposals_cmd::decide_in`'s suppression
//! unfulfilled, and `driver_support::service_registry_action` was carrying one
//! under a comment listing seven handles for a function that takes three. An
//! `#[allow]` would have hidden both for as long as they lasted.
//!
//! `reason` is required by the attribute's own grammar in the form used here,
//! so this test asserts only the form; whether a reason is *true* is a review
//! question, exactly as AGENTS.md states it.
//!
//! **Tree-wide since #4918.** It was scoped to this crate because that is the
//! one #3698 audited; widening it turned out to cost nothing, because the rest
//! of the workspace was already at zero: the convention held everywhere
//! without a guard, and this is what keeps it holding — the crates that never
//! had a bare allow are exactly the ones nobody would notice acquiring one.
//!
//! It stays a **test** rather than becoming a gate step, for the argument
//! `crates/stella-cli/tests/design_token_parity.rs`'s module doc makes at
//! length: `make gate` already runs `make test`, and a gate step is five
//! coupled edits plus another shared cell for two PRs to collide on.
//! `scripts/check-dead-code-allows.py` is the other tree-wide guard of this
//! shape and IS a gate step, because it carries a ratchet baseline this one
//! has no need of — there is nothing to grandfather when the count is zero.
//!
//! Living in `stella-cli/tests/` while judging every crate is the one wart.
//! The alternative is a workspace-root test crate that exists to hold one
//! file, and the parity test above already set the precedent for keeping such
//! a check in the binary crate's own suite.

use std::path::{Path, PathBuf};

/// Every crate's `src/` in the workspace.
///
/// From this crate's manifest dir rather than the current working directory,
/// which is not the workspace root under every runner.
fn crate_sources() -> Vec<PathBuf> {
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/<name> has a parent")
        .to_path_buf();
    let mut roots: Vec<PathBuf> = std::fs::read_dir(&crates_dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", crates_dir.display()))
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("src"))
        .filter(|src| src.is_dir())
        .collect();
    roots.sort();
    roots
}

/// Every `.rs` file under `src/`, recursively.
fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(Result::ok);
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_clippy_lint_is_silenced_with_a_bare_allow() {
    let roots = crate_sources();
    // The count is a floor, not a pin: a new crate must be judged by this test
    // the day it lands, and a number here would be one more shared cell for two
    // PRs to collide on. Twenty-six is what the tree had when #4918 widened it.
    assert!(
        roots.len() >= 20,
        "expected every crate's src/, found {}: {roots:?}",
        roots.len()
    );
    let mut sources = Vec::new();
    for root in &roots {
        rust_sources(root, &mut sources);
    }
    assert!(
        !sources.is_empty(),
        "no Rust sources found under crates/*/src"
    );

    let mut offenders: Vec<String> = Vec::new();
    for path in &sources {
        let source = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        for (i, line) in source.lines().enumerate() {
            if line.trim_start().starts_with("#[allow(clippy::")
                || line.trim_start().starts_with("#![allow(clippy::")
            {
                offenders.push(format!("{}:{}: {}", path.display(), i + 1, line.trim()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "clippy suppressions in this workspace are `#[expect(<lint>, reason = \"…\")]`, \
         so a suppression that stops being true fails the build instead of \
         outliving its argument (#3698). Found {}:\n  {}",
        offenders.len(),
        offenders.join("\n  "),
    );
}
