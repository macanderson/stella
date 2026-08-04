//! Witnesses for the storage gate over `apply_edits` batches (#442): the
//! transactional path is judged by the SAME schema gate as `write_file` /
//! `edit_file` — per touched schema file, on the batch's composed content —
//! instead of the old blanket refusal.

use super::tests::seeded_snapshot;
use super::*;

fn fixture(files: &[(&str, &str)]) -> (tempfile::TempDir, ToolRegistry) {
    let dir = tempfile::tempdir().unwrap();
    for (rel, content) in files {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    let reg = ToolRegistry::with_issue_backend(dir.path().to_path_buf(), None);
    reg.update_storage_index(seeded_snapshot(&["users"]));
    (dir, reg)
}

/// The old shape was a blanket refusal; the new shape is a real judgment —
/// a batch whose composed content re-creates an indexed table is blocked by
/// the gate, with nothing written.
#[tokio::test]
async fn a_batch_landing_a_duplicate_table_is_blocked_by_the_gate() {
    let original = "CREATE TABLE orders (id INT);\n";
    let (dir, reg) = fixture(&[("migrations/002.sql", original)]);

    let result = reg
        .execute(
            "apply_edits",
            &serde_json::json!({ "edits": [
                { "path": "migrations/002.sql", "old_string": "orders", "new_string": "users" },
            ]}),
        )
        .await;
    match result {
        ToolOutput::Error { message } => {
            assert!(
                message.contains("users"),
                "the gate names the duplicate: {message}"
            );
        }
        ToolOutput::Ok { content } => panic!("duplicate must be blocked: {content}"),
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("migrations/002.sql")).unwrap(),
        original,
        "a blocked batch writes nothing"
    );
}

/// Every schema file in the batch is judged, and one bad file blocks the
/// whole call — the batch stays all-or-nothing through the gate too.
#[tokio::test]
async fn one_duplicate_in_a_batch_blocks_every_file_in_it() {
    let clean = "CREATE TABLE payments_draft (id INT);\n";
    let dup = "CREATE TABLE orders (id INT);\n";
    let (dir, reg) = fixture(&[("migrations/a.sql", clean), ("migrations/b.sql", dup)]);

    let result = reg
        .execute(
            "apply_edits",
            &serde_json::json!({ "edits": [
                { "path": "migrations/a.sql", "old_string": "payments_draft", "new_string": "payments" },
                { "path": "migrations/b.sql", "old_string": "orders", "new_string": "users" },
            ]}),
        )
        .await;
    assert!(
        result.is_error(),
        "the bad half blocks the call: {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("migrations/a.sql")).unwrap(),
        clean,
        "the CLEAN half of a blocked batch must not land either"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("migrations/b.sql")).unwrap(),
        dup
    );
}

/// A `dry_run` batch is judged by the gate but writes nothing — so it must
/// not grow the session overlay either: the same edit applied FOR REAL
/// afterwards has to pass, not be rejected as a duplicate of the phantom
/// the dry run never created.
#[tokio::test]
async fn a_dry_run_batch_leaves_the_overlay_untouched() {
    let (dir, reg) = fixture(&[(
        "migrations/005.sql",
        "CREATE TABLE ledger_draft (id INT);\n",
    )]);
    let edits = serde_json::json!({ "edits": [
        { "path": "migrations/005.sql", "old_string": "ledger_draft", "new_string": "ledger" },
    ]});

    let dry = reg
        .execute(
            "apply_edits",
            &serde_json::json!({ "edits": edits["edits"], "dry_run": true }),
        )
        .await;
    assert!(!dry.is_error(), "the dry run validates cleanly: {dry:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("migrations/005.sql")).unwrap(),
        "CREATE TABLE ledger_draft (id INT);\n",
        "a dry run writes nothing"
    );

    let real = reg.execute("apply_edits", &edits).await;
    assert!(
        !real.is_error(),
        "the real batch must not collide with the dry run's phantom: {real:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("migrations/005.sql")).unwrap(),
        "CREATE TABLE ledger (id INT);\n"
    );
}

/// The other direction, which the old refusal made impossible: a legitimate
/// multi-file schema batch passes the gate, lands transactionally, and
/// grows the session overlay — a later duplicate of what it created is
/// blocked without any re-index.
#[tokio::test]
async fn a_clean_batch_applies_and_grows_the_index() {
    let (dir, reg) = fixture(&[
        (
            "migrations/003.sql",
            "CREATE TABLE invoices_draft (id INT);\n",
        ),
        ("src/billing.rs", "pub fn charge() {}\n"),
    ]);

    let result = reg
        .execute(
            "apply_edits",
            &serde_json::json!({ "edits": [
                { "path": "migrations/003.sql", "old_string": "invoices_draft", "new_string": "invoices" },
                { "path": "src/billing.rs", "old_string": "charge", "new_string": "charge_card" },
            ]}),
        )
        .await;
    assert!(!result.is_error(), "a clean batch applies: {result:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("migrations/003.sql")).unwrap(),
        "CREATE TABLE invoices (id INT);\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/billing.rs")).unwrap(),
        "pub fn charge_card() {}\n"
    );

    // The overlay grew: re-creating `invoices` elsewhere is now a duplicate.
    let dup = reg
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "migrations/004.sql",
                "content": "CREATE TABLE invoices (id INT);\n"
            }),
        )
        .await;
    assert!(
        dup.is_error(),
        "the batch's created objects must be indexed: {dup:?}"
    );
}
