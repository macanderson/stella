//! Tests for reversible suppression — both the lexical matcher and the
//! tombstone table it feeds.
//!
//! The behaviour under test is the one that motivated the feature: a
//! forgotten lesson must stay forgotten even when the reflection loop
//! re-mines it under a brand-new id, and it must be recoverable when the
//! user changes their mind.

use super::*;
use crate::Store;

/// The two real memories from this project's store — the same lesson mined
/// twice, two days apart, both naming a file that had already been deleted.
const RELEARNED_A: &str = "In stella-cli witness tests (slash_models_witness.rs), prefer updating \
     assertions to match the live renderer output (abbreviated column headers like \
     'in'/'out'/'cached-i', actual seeded numeric formatting, and observed strings like 'thinks') \
     rather than changing core CLI behavior.";
const RELEARNED_B: &str = "In stella-cli witness tests (slash_models_witness.rs), prefer updating \
     assertions to match the live renderer output (abbreviated column headers like \
     'in'/'out'/'cached-i', actual seed values, etc.) rather than changing the renderer.";
/// A lesson from the same corpus that shares scaffolding but no subject —
/// the noise side of every margin assertion below.
const UNRELATED: &str = "For cargo test filters like --exact, always place them after the -- \
     separator (cargo test -- --exact) to pass args to the test binary instead of Cargo.";

#[test]
fn forget_then_read_back_by_surface() {
    let store = Store::in_memory().expect("in-memory store");
    store
        .forget(ContextSurface::Memory, "nod_aaa", RELEARNED_A, "stale file")
        .expect("forget");

    let ids = store.forgotten_ids(ContextSurface::Memory).expect("ids");
    assert!(ids.contains("nod_aaa"));

    // Surfaces are independent namespaces — a forgotten memory must not
    // suppress a rule that happens to share an id.
    assert!(
        store
            .forgotten_ids(ContextSurface::Rule)
            .expect("rule ids")
            .is_empty()
    );
}

#[test]
fn forgetting_twice_updates_instead_of_erroring() {
    let store = Store::in_memory().expect("in-memory store");
    store
        .forget(ContextSurface::Memory, "nod_aaa", RELEARNED_A, "first")
        .expect("first forget");
    store
        .forget(ContextSurface::Memory, "nod_aaa", RELEARNED_B, "second")
        .expect("re-forget must be idempotent, not an error");

    let rows = store.forgotten_rows().expect("rows");
    assert_eq!(rows.len(), 1, "one verdict per item, not a duplicate");
    assert_eq!(rows[0].reason, "second");
    assert_eq!(rows[0].content, RELEARNED_B, "content refreshed");
}

#[test]
fn restore_reports_whether_anything_was_lifted() {
    let store = Store::in_memory().expect("in-memory store");
    store
        .forget(ContextSurface::Skill, "witness-tests", "some text", "")
        .expect("forget");

    assert!(
        store
            .restore(ContextSurface::Skill, "witness-tests")
            .expect("restore"),
        "restoring a live tombstone reports true"
    );
    assert!(
        !store
            .restore(ContextSurface::Skill, "witness-tests")
            .expect("restore again"),
        "restoring something never forgotten reports false, not success"
    );
    assert!(
        store
            .forgotten_ids(ContextSurface::Skill)
            .expect("ids")
            .is_empty()
    );
}

/// The point of the whole design: forgetting memory `nod_aaa` must also
/// suppress the paraphrase the miner writes back under `nod_bbb`.
#[test]
fn a_forgotten_lesson_suppresses_its_relearned_paraphrase() {
    let store = Store::in_memory().expect("in-memory store");
    store
        .forget(ContextSurface::Memory, "nod_aaa", RELEARNED_A, "stale")
        .expect("forget");

    let forgotten = store
        .forgotten_texts(ContextSurface::Memory)
        .expect("forgotten texts");

    assert!(
        is_suppressed(RELEARNED_B, forgotten.iter().map(String::as_str)),
        "the re-mined paraphrase must be caught by the tombstone"
    );
    assert!(
        !is_suppressed(UNRELATED, forgotten.iter().map(String::as_str)),
        "an unrelated lesson must still be allowed through"
    );
}

#[test]
fn forgotten_texts_skips_rows_with_no_recorded_content() {
    let store = Store::in_memory().expect("in-memory store");
    // A surface whose id IS its whole identity (a domain name) may carry no
    // body; such a row must not contribute an empty string to the
    // restatement check, or it would match nothing and cost a comparison.
    store
        .forget(ContextSurface::Domain, "stella-cli", "", "")
        .expect("forget");
    assert!(
        store
            .forgotten_texts(ContextSurface::Domain)
            .expect("texts")
            .is_empty()
    );
    assert!(
        store
            .forgotten_ids(ContextSurface::Domain)
            .expect("ids")
            .contains("stella-cli"),
        "the id is still tombstoned even with no body"
    );
}

/// Same posture as [`Store::quarantined_memory_ids`]: a missing table is an
/// error the caller must handle, never a silent empty set that would read as
/// "nothing is forgotten" and surface everything.
#[test]
fn a_missing_tombstone_table_is_an_error_not_an_empty_set() {
    let store = Store::in_memory().expect("in-memory store");
    store
        .lock()
        .execute("DROP TABLE forgotten", [])
        .expect("remove tombstone table");
    assert!(store.forgotten_ids(ContextSurface::Memory).is_err());
    assert!(store.forgotten_texts(ContextSurface::Memory).is_err());
    assert!(store.forgotten_rows().is_err());
}

/// An existing database must gain the table by migration, not only fresh
/// ones via `create_latest_schema`.
#[test]
fn the_table_arrives_by_migration_on_an_existing_database() {
    let root = tempfile::tempdir().expect("tempdir");
    let store = Store::open(root.path()).expect("open");
    // Simulate a pre-v14 file: drop the table and wind the version back.
    store
        .lock()
        .execute("DROP TABLE forgotten", [])
        .expect("drop");
    store
        .lock()
        .pragma_update(None, "user_version", 13i64)
        .expect("rewind version");
    drop(store);

    let store = Store::open(root.path()).expect("reopen runs migrations");
    store
        .forget(ContextSurface::Memory, "nod_aaa", RELEARNED_A, "")
        .expect("table exists after migration");
    assert!(
        store
            .forgotten_ids(ContextSurface::Memory)
            .expect("ids")
            .contains("nod_aaa")
    );
}

// ---- the lexical matcher itself ----------------------------------------
//
// The tests above exercise the tombstone table end-to-end; these pin the
// pure functions it is built on, where the thresholds are chosen.

#[test]
fn a_relearned_lesson_is_recognized_as_a_restatement() {
    assert!(
        is_restatement(RELEARNED_B, RELEARNED_A),
        "similarity was {}",
        similarity(RELEARNED_B, RELEARNED_A)
    );
}

#[test]
fn unrelated_lessons_are_not_restatements() {
    assert!(!is_restatement(UNRELATED, RELEARNED_A));
    assert!(!is_restatement(RELEARNED_A, UNRELATED));
}

/// The threshold is only defensible if signal and noise are far apart —
/// pin the margin so a future tweak that collapses it fails loudly.
#[test]
fn signal_and_noise_stay_far_apart() {
    let signal = similarity(RELEARNED_A, RELEARNED_B);
    let noise = similarity(RELEARNED_A, UNRELATED);
    assert!(signal > 0.5, "re-learned pair scored {signal}");
    assert!(noise < 0.2, "unrelated pair scored {noise}");
    assert!(
        signal > noise * 3.0,
        "margin collapsed: signal {signal}, noise {noise}"
    );
}

#[test]
fn identifiers_and_paths_stay_whole() {
    // Two lessons about *different* files must not look alike just
    // because they share sentence scaffolding.
    let about_a = "In witness tests (alpha_witness.rs), prefer updating assertions.";
    let about_b = "In witness tests (beta_witness.rs), prefer updating assertions.";
    assert!(tokens(about_a).contains("alpha_witness.rs"));
    // The filename stayed whole, so its parts are not tokens of their own.
    assert!(!tokens(about_a).contains("alpha"));
    assert!(
        similarity(about_a, about_b) < 1.0,
        "the differing filename must register"
    );
}

#[test]
fn empty_text_matches_nothing() {
    assert_eq!(similarity("", ""), 0.0);
    assert_eq!(similarity("", RELEARNED_A), 0.0);
    assert!(!is_restatement("", ""));
}

#[test]
fn suppression_scans_every_tombstone() {
    let forgotten = [UNRELATED, RELEARNED_A];
    assert!(is_suppressed(RELEARNED_B, forgotten));
    assert!(!is_suppressed(
        "An entirely new observation about graph queries.",
        forgotten
    ));
}

#[test]
fn surface_spellings_round_trip() {
    for surface in ContextSurface::all() {
        assert_eq!(ContextSurface::parse(surface.as_str()), Some(surface));
    }
    assert_eq!(ContextSurface::parse("nonsense"), None);
}
