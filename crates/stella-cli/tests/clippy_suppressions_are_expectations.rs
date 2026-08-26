// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Every clippy suppression in this crate is an `#[expect(…, reason = "…")]`.
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
//! Scoped to this crate because it is the one #3698 audited and the one that
//! now has zero. Widening it to the workspace is #4918.

use std::path::{Path, PathBuf};

fn crate_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
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
    let mut sources = Vec::new();
    rust_sources(&crate_src(), &mut sources);
    assert!(!sources.is_empty(), "no Rust sources found under src/");

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
        "clippy suppressions in this crate are `#[expect(<lint>, reason = \"…\")]`, \
         so a suppression that stops being true fails the build instead of \
         outliving its argument (#3698). Found {}:\n  {}",
        offenders.len(),
        offenders.join("\n  "),
    );
}
