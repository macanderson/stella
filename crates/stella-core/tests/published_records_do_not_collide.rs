//! No two records in `.stella/rules/` may clash.
//!
//! Those files steer every session run here. Two of them can share a
//! trigger, sit at one `precedence`, and ask for different `enforcement`.
//! The stricter one is then held back. It still loads, and its guard never
//! arms.
//!
//! `stella context validate` says so in a warning and exits 0. So the build
//! stays green while a `hard` guard is down. This test is the gate that
//! warning is not.
//!
//! To clear a failure, run `stella context amend <handle> --keywords ...`.
//! It narrows the trigger and stamps a new hash. A hand edit leaves the old
//! hash in place.

use std::path::PathBuf;

use stella_core::ingest::record::EnforcementMode;
use stella_core::records::{
    LoadedRecord, assign_handles, detect_conflicts, is_suspended, load_context_file,
};

/// The schema tag a record carries. `governance.toml` sits in the same
/// folder and has none, which is how it gets skipped.
const RECORD_SCHEMA: &str = "context-record/v0.1";

fn rules_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.stella/rules")
}

/// Every record this repo ships, loaded the way the CLI loads them.
fn published_records() -> Vec<LoadedRecord> {
    let dir = rules_dir();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|err| panic!("{} must be readable: {err}", dir.display()))
        .map(|entry| entry.expect("a readable directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    files.sort();

    let mut records = Vec::new();
    for path in files {
        let contents = std::fs::read_to_string(&path).expect("a readable record file");
        let parsed = toml::from_str::<toml::Value>(&contents)
            .unwrap_or_else(|err| panic!("{} is not valid TOML: {err}", path.display()));
        if parsed.get("schema").and_then(|value| value.as_str()) != Some(RECORD_SCHEMA) {
            continue;
        }
        let name = path
            .file_name()
            .expect("a file path names a file")
            .to_string_lossy();
        records.extend(
            load_context_file(&name, &contents).unwrap_or_else(|err| panic!("{name} loads: {err}")),
        );
    }
    assign_handles(&mut records);
    assert!(
        !records.is_empty(),
        "{} holds no published record — the test would pass over nothing",
        dir.display()
    );
    records
}

#[test]
fn no_two_published_records_collide() {
    let mut records = published_records();
    let conflicts = detect_conflicts(&mut records);
    let report: String = conflicts
        .iter()
        .map(|conflict| {
            format!(
                "\n  ^{} and ^{}: {}",
                conflict.left, conflict.right, conflict.detail
            )
        })
        .collect();
    assert!(
        conflicts.is_empty(),
        "{} record pair(s) collide:{report}\n\
         Narrow one side's trigger with `stella context amend <handle> --keywords ...`",
        conflicts.len()
    );
}

#[test]
fn every_published_hard_guard_arms() {
    let mut records = published_records();
    let conflicts = detect_conflicts(&mut records);
    let down: Vec<&str> = records
        .iter()
        .filter(|loaded| {
            loaded
                .record
                .enforcement
                .as_ref()
                .is_some_and(|enforcement| enforcement.mode == EnforcementMode::Hard)
        })
        .map(|loaded| loaded.handle.as_str())
        .filter(|handle| is_suspended(handle, &conflicts))
        .collect();
    assert!(
        down.is_empty(),
        "a conflict holds these `hard` guards down, so they block nothing: {}",
        down.join(", ")
    );
}
