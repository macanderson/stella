// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The task board is not engine code (AGENTS.md rule 12).
//!
//! `TaskBoard` sat in `stella-core` for as long as the crate existed.
//! `scripts/check-core-reachability.py` read it as engine code the whole
//! time. The cause was `RunningTask`, a closure the host installs on the
//! event sender, sharing its file. That guard walks whole modules. One
//! reached item carries the rest of its module in with it. It is the shape
//! the record plane got in through: one call to `record_hash` towing a
//! fourteen-thousand-line plane.
//!
//! Splitting the file makes the guard's answer match what the engine reaches.
//! This test pins the split. Nothing fails to compile when the board comes
//! back. A later author who adds a `pub mod tasks;` beside `driver` re-makes
//! the old condition, and the guard still reports it reached.
//!
//! A source scan, not a type-level check, because Rust cannot say "this
//! crate does not export that". The scanner is checked first, so a read that
//! found nothing fails here rather than passing.

use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` file under `src/`, as (path relative to `src/`, contents).
fn sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, String)>) {
        let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {dir:?}: {e}"));
        for entry in entries {
            let path = entry.expect("a directory entry").path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(root)
                    .expect("every hit is under src/")
                    .display()
                    .to_string();
                let body = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{rel}: {e}"));
                out.push((rel, body));
            }
        }
    }

    let root = src_dir();
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out
}

/// The scanner reads real files and finds the port that must still be here.
///
/// Delete `RunningTask`, or break the walk so it returns nothing, and the
/// test below would pass over an empty list. This is what catches that.
#[test]
fn the_scanner_sees_the_crate_it_is_scanning() {
    let files = sources();
    assert!(
        files.len() > 20,
        "the engine's crate is larger than this; the walk found only {}",
        files.len()
    );
    assert!(
        files
            .iter()
            .any(|(rel, _)| rel == "running_task.rs" || rel == "running_task/mod.rs"),
        "the port the engine does reach must be here: {:?}",
        files.iter().map(|(rel, _)| rel).collect::<Vec<_>>()
    );
    assert!(
        files
            .iter()
            .any(|(rel, body)| rel == "event_sender.rs" && body.contains("RunningTask")),
        "the event sender is what installs the port"
    );
}

/// The board's own words appear nowhere in the engine's crate.
///
/// `RunningTask` is all the engine needs. It asks which task runs and stamps
/// the answer. The rules, the spawn queue and their error type belong to the
/// six `task_*` tools that move them. They live with those tools, in
/// `stella_tools::tasks::board`.
#[test]
fn the_engines_crate_names_no_board_type() {
    let forbidden = ["TaskBoard", "TaskBoardError", "SpawnRequest"];
    let offenders: Vec<String> = sources()
        .into_iter()
        .flat_map(|(rel, body)| {
            forbidden
                .iter()
                .filter(|name| body.contains(**name))
                .map(|name| format!("{rel} names {name}"))
                .collect::<Vec<_>>()
        })
        .collect();

    assert!(
        offenders.is_empty(),
        "the task board left stella-core and must not return — put board \
         logic in stella_tools::tasks::board, and reach the running task \
         through stella_core::RunningTask:\n  {}",
        offenders.join("\n  ")
    );
}
