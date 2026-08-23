// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a search is allowed to see, and what it must not.
//!
//! A sibling of the ladder tests rather than part of them: `tests.rs` sits
//! against the 1500-line ceiling, and this is its own subject — not whether a
//! rung ranks well, but which files reach a rung at all.
//!
//! Every assertion here is two-sided by construction. A test that only proves
//! a file *is* found would pass just as happily if the walk had stopped
//! excluding anything, so each fixture plants a decoy carrying the same query
//! terms in the place that must stay unreachable.

use std::fs;

use crate::search::scan;

/// Witness for #3162. `.stella/` was skipped wholesale, so the published
/// context records were invisible to every rung — including the one that
/// needs no index at all. They are the one part of `.stella/` tracked in Git,
/// because a record only steers a teammate's session if it travels with the
/// repository, and a retrieval tool that cannot see the repository's own
/// steering policy is the hole the issue names.
///
/// The other half is what must stay invisible, and the fixture asserts both
/// in one walk: `.stella/private/` holds SQLite state and OAuth tokens, and
/// `.stella/settings.json` sits directly on the path the walk now crosses to
/// reach the records.
#[test]
fn the_scan_rung_sees_context_records_and_still_never_sees_private_state() {
    let workspace = tempfile::tempdir().expect("tempdir");
    let root = workspace.path().canonicalize().expect("canonicalize");
    fs::create_dir_all(root.join(".stella").join("rules")).expect("mkdir rules");
    fs::create_dir_all(root.join(".stella").join("private")).expect("mkdir private");
    fs::write(
        root.join(".stella").join("rules").join("ctx.demo.toml"),
        "[[record]]\nstatement = \"a provider adapter never reaches into the engine\"\n",
    )
    .expect("write the record");
    // Both of these mention the query, so their absence from the answer is a
    // refusal rather than a coincidence.
    fs::write(
        root.join(".stella").join("settings.json"),
        "{\"note\": \"provider adapter\"}",
    )
    .expect("write settings");
    fs::write(
        root.join(".stella").join("private").join("notes.jsonl"),
        "{\"lesson\": \"provider adapter\"}",
    )
    .expect("write private state");

    let outcome = scan::scan_hits_bounded(&root, "provider adapter", 10, 100);
    let paths: Vec<&str> = outcome.hits.iter().map(|hit| hit.path.as_str()).collect();
    assert!(
        paths.contains(&".stella/rules/ctx.demo.toml"),
        "a context record must be reachable by the scan rung (#3162): {paths:?}"
    );
    assert!(
        !paths
            .iter()
            .any(|path| path.starts_with(".stella/private/")),
        "`.stella/private/` must stay unsearchable: {paths:?}"
    );
    assert!(
        !paths.contains(&".stella/settings.json"),
        "crossing `.stella` to reach the records must not scan `.stella` itself: {paths:?}"
    );
}
