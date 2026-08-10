//! Tests for the lineage adapter: real files in temp trees, a temp inbox, and
//! the actual `git` binary — the pieces the pure core deliberately cannot see.

use super::*;
use stella_core::ingest::lineage::{AlertState, Lineage};
use stella_store::NotificationStore;

fn lineage(hash: &str) -> Lineage {
    Lineage {
        source_hash: hash.to_string(),
        commit: None,
        ingested_at: "2026-08-10T00:00:00Z".to_string(),
        ingest_run_id: "ing_test".to_string(),
        candidate_ids: vec!["claim-a1b2c3d4".to_string()],
        alerts: AlertState::Active,
    }
}

/// The ledger survives the disk: a dismissal written by one session is the
/// same dismissal the next session reads.
#[test]
fn the_ledger_round_trips_through_the_private_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    record_ingest(root, "notes.md", lineage("git:aaaa"), false);
    let mut ledger = read_ledger(root);
    ledger
        .set_alerts("notes.md", AlertState::Dismissed)
        .expect("notes.md is tracked");
    write_ledger(root, &ledger);

    let reread = read_ledger(root);
    assert_eq!(reread.sources["notes.md"].alerts, AlertState::Dismissed);
}

/// An unreadable ledger is an empty one — a corrupt file must not stop a
/// session from starting.
#[test]
fn a_corrupt_ledger_reads_as_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let path = root.join(".stella/private/ingest-lineages.json");
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(&path, "not json at all").expect("write");
    assert!(read_ledger(root).sources.is_empty());
}

/// The two hash paths agree: hashing the text that was read and hashing the
/// file it came from produce the same id, which is exactly what makes "the
/// file did not change" read as no drift.
#[test]
fn content_and_path_hashes_agree_for_an_unchanged_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let content = "# steering\n\nalways be kind to the linter.\n";
    std::fs::write(root.join("notes.md"), content).expect("write");

    let at_ingest = ingested_content_hash(root, "notes.md", content);
    let now = current_source_hash(root, "notes.md").expect("file exists");
    assert_eq!(at_ingest, now);
}

/// A deleted source hashes to `None` — the input the core reads as `Missing`.
#[test]
fn a_deleted_source_hashes_to_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(current_source_hash(dir.path(), "gone.md"), None);
}

/// The sha256 fallback carries its scheme, so a stored fallback hash can never
/// be confused with a git blob id that happens to share a prefix.
#[test]
fn the_fallback_hash_is_scheme_tagged() {
    assert!(sha256_of("anything").starts_with("sha256:"));
    assert_eq!(sha256_of("anything").len(), "sha256:".len() + 64);
}

/// A changed source surfaces exactly one alert, and a second sweep of the same
/// drift surfaces nothing — the dedup that keeps a known drift from nagging
/// every session start.
#[test]
fn a_changed_source_alerts_once_across_sweeps() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let inbox_dir = tempfile::tempdir().expect("inbox dir");
    let inbox = NotificationStore::open(inbox_dir.path());

    std::fs::write(root.join("notes.md"), "old text").expect("write");
    let hash = current_source_hash(root, "notes.md").expect("hashable");
    record_ingest(root, "notes.md", lineage(&hash), false);

    std::fs::write(root.join("notes.md"), "new text").expect("rewrite");
    assert_eq!(surface_stale_sources_into(root, &inbox), 1);
    assert_eq!(surface_stale_sources_into(root, &inbox), 0);

    let notes = inbox.list();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].title.contains("notes.md"), "{}", notes[0].title);
    assert!(
        notes[0]
            .body
            .contains("stella ingest alerts dismiss notes.md"),
        "the alert must carry its own off switch: {}",
        notes[0].body
    );
}

/// A further change is a different drift and earns a fresh alert — dedup keys
/// on the hashes, not the path.
#[test]
fn a_further_change_is_a_new_alert() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let inbox_dir = tempfile::tempdir().expect("inbox dir");
    let inbox = NotificationStore::open(inbox_dir.path());

    std::fs::write(root.join("notes.md"), "v1").expect("write");
    let hash = current_source_hash(root, "notes.md").expect("hashable");
    record_ingest(root, "notes.md", lineage(&hash), false);

    std::fs::write(root.join("notes.md"), "v2").expect("rewrite");
    assert_eq!(surface_stale_sources_into(root, &inbox), 1);
    std::fs::write(root.join("notes.md"), "v3").expect("rewrite again");
    assert_eq!(surface_stale_sources_into(root, &inbox), 1);
}

/// A deleted source alerts with deletion wording — and says the records live on.
#[test]
fn a_deleted_source_alerts_with_deletion_wording() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let inbox_dir = tempfile::tempdir().expect("inbox dir");
    let inbox = NotificationStore::open(inbox_dir.path());

    std::fs::write(root.join("scaffold.md"), "records are the product").expect("write");
    let hash = current_source_hash(root, "scaffold.md").expect("hashable");
    record_ingest(root, "scaffold.md", lineage(&hash), false);
    std::fs::remove_file(root.join("scaffold.md")).expect("delete");

    assert_eq!(surface_stale_sources_into(root, &inbox), 1);
    let notes = inbox.list();
    assert!(notes[0].title.contains("deleted"), "{}", notes[0].title);
    assert!(
        notes[0].body.contains("records remain live"),
        "{}",
        notes[0].body
    );
}

/// The dismissal contract, end to end: a dismissed lineage surfaces nothing —
/// not for change, not for deletion — while its neighbour still alerts.
#[test]
fn a_dismissed_lineage_is_silent_and_scoped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let inbox_dir = tempfile::tempdir().expect("inbox dir");
    let inbox = NotificationStore::open(inbox_dir.path());

    for name in ["a.md", "b.md"] {
        std::fs::write(root.join(name), "original").expect("write");
        let hash = current_source_hash(root, name).expect("hashable");
        record_ingest(root, name, lineage(&hash), false);
    }
    run_set_alerts(root, std::path::Path::new("a.md"), AlertState::Dismissed)
        .expect("a.md is tracked");

    std::fs::remove_file(root.join("a.md")).expect("delete a");
    std::fs::write(root.join("b.md"), "changed").expect("change b");

    assert_eq!(surface_stale_sources_into(root, &inbox), 1);
    let notes = inbox.list();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].title.contains("b.md"), "{}", notes[0].title);
}

/// Dismissing a never-ingested path fails loudly and names what is tracked —
/// a silent no-op would leave the user believing they were covered.
#[test]
fn dismissing_an_untracked_path_names_the_tracked_ones() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    record_ingest(root, "tracked.md", lineage("git:aaaa"), false);

    let err = run_set_alerts(
        root,
        std::path::Path::new("untracked.md"),
        AlertState::Dismissed,
    )
    .expect_err("untracked.md was never ingested");
    assert!(err.contains("untracked.md"), "{err}");
    assert!(err.contains("tracked.md"), "{err}");
}

/// The ledger key is one derivation, everywhere: `display_path` normalizes
/// backslashes even on the out-of-workspace fallback, so a lineage recorded
/// on Windows is the same string a later `alerts dismiss` computes. Ingest
/// used a raw fallback here once, and the two spellings could never meet.
#[test]
fn ledger_keys_normalize_backslashes_on_the_fallback_path() {
    let root = std::path::Path::new("/ws");
    assert_eq!(
        display_path(root, std::path::Path::new("C:\\notes\\steering.md")),
        "C:/notes/steering.md"
    );
    assert_eq!(
        display_path(root, std::path::Path::new("/ws/docs/notes.md")),
        "docs/notes.md"
    );
}

/// Re-ingesting a dismissed file re-arms it by default; `--keep-dismissed`
/// carries the dismissal forward. The core owns the rule — this pins that the
/// flag actually reaches it through the persistence layer.
#[test]
fn re_ingest_re_arms_unless_told_otherwise() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    record_ingest(root, "notes.md", lineage("git:aaaa"), false);
    let mut ledger = read_ledger(root);
    ledger
        .set_alerts("notes.md", AlertState::Dismissed)
        .expect("tracked");
    write_ledger(root, &ledger);

    record_ingest(root, "notes.md", lineage("git:bbbb"), true);
    assert_eq!(
        read_ledger(root).sources["notes.md"].alerts,
        AlertState::Dismissed
    );

    record_ingest(root, "notes.md", lineage("git:cccc"), false);
    assert_eq!(
        read_ledger(root).sources["notes.md"].alerts,
        AlertState::Active
    );
}
