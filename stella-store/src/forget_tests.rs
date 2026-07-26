//! Round-trip tests for the explicit-tombstone table.
//!
//! The behaviour under test is the one that motivated the feature: a
//! forgotten lesson must stay forgotten even when the reflection loop
//! re-mines it under a brand-new id, and it must be recoverable when the
//! user changes their mind.

use crate::{ContextSurface, Store, forget};

/// The two real memories from this project's store — the same lesson mined
/// twice, two days apart, both naming a file that had already been deleted.
const RELEARNED_A: &str = "In stella-cli witness tests (slash_models_witness.rs), prefer updating \
     assertions to match the live renderer output (abbreviated column headers like \
     'in'/'out'/'cached-i', actual seeded numeric formatting, and observed strings like 'thinks') \
     rather than changing core CLI behavior.";
const RELEARNED_B: &str = "In stella-cli witness tests (slash_models_witness.rs), prefer updating \
     assertions to match the live renderer output (abbreviated column headers like \
     'in'/'out'/'cached-i', actual seed values, etc.) rather than changing the renderer.";

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
        forget::is_suppressed(RELEARNED_B, forgotten.iter().map(String::as_str)),
        "the re-mined paraphrase must be caught by the tombstone"
    );
    assert!(
        !forget::is_suppressed(
            "For cargo test filters like --exact, place them after the -- separator.",
            forgotten.iter().map(String::as_str)
        ),
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
