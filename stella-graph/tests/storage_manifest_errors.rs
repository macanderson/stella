//! A `stella.storage.toml` that does not parse must degrade LOUDLY.
//!
//! Assembly stays best-effort by design — a broken manifest cannot be allowed
//! to take the whole storage map down — but the failure used to be swallowed
//! at every call site (`.ok().flatten()`), which made "this repo declares no
//! boundaries" and "this repo's boundaries silently stopped being enforced
//! because of a typo" produce byte-identical snapshots.

use std::fs;

use tempfile::tempdir;

#[test]
fn a_malformed_manifest_is_reported_not_swallowed() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("stella.storage.toml"),
        // Unterminated string: valid-looking, and exactly the kind of typo a
        // hand-edited manifest picks up.
        "version = 1\n[relations.\"sql/default/orders\"]\nintent = \"unterminated\n",
    )
    .unwrap();

    let snapshot = stella_graph::load_storage_snapshot(dir.path());
    let error = snapshot
        .manifest_error
        .as_ref()
        .expect("a manifest that does not parse must say so");
    assert!(
        error.contains("stella.storage.toml"),
        "the reason names the offending file: {error}"
    );
    // Still best-effort: the map is empty rather than an error, so every
    // storage mechanism no-ops instead of blocking the run.
    assert!(snapshot.layers.is_empty() && snapshot.relations.is_empty());
}

#[test]
fn a_well_formed_manifest_carries_no_error() {
    let dir = tempdir().unwrap();
    fs::write(
        dir.path().join("stella.storage.toml"),
        "version = 1\n[relations.\"sql/default/orders\"]\nintent = \"Orders.\"\n",
    )
    .unwrap();

    let snapshot = stella_graph::load_storage_snapshot(dir.path());
    assert_eq!(snapshot.manifest_error, None);
}

#[test]
fn an_absent_manifest_is_not_an_error() {
    let dir = tempdir().unwrap();
    let snapshot = stella_graph::load_storage_snapshot(dir.path());
    assert_eq!(
        snapshot.manifest_error, None,
        "a repo that never declared a storage map is not misconfigured"
    );
}
