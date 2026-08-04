//! The SQL schema gate: a write that would create a table the storage
//! index already knows about is refused unless the caller declares intent.
//!
//! Shares `seeded_snapshot` with the parent module, which also lends it to
//! `registry::gate_batch_tests` — so the fixture stays there and this module
//! reaches it through `use super::*`.

use super::*;

#[tokio::test]
async fn schema_gate_blocks_duplicate_table_on_write() {
    let dir = std::env::temp_dir().join(format!("stella_gate_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let reg = ToolRegistry::with_issue_backend(dir.clone(), None);
    reg.update_storage_index(seeded_snapshot(&["users"]));

    // Attempt to write a SQL file that creates `users` again.
    let result = reg
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "migrations/002.sql",
                "content": "CREATE TABLE users (id INT);\n"
            }),
        )
        .await;
    assert!(result.is_error());
    match result {
        ToolOutput::Error { message } => {
            assert!(
                message.contains("Table `users` already exists"),
                "{message}"
            );
            assert!(message.contains("ALTER"), "{message}");
            // Gate v2: the conflict cites the canonical address and the
            // existing columns, so the model can decide without a read.
            assert!(message.contains("store://sql/default/users"), "{message}");
            assert!(message.contains("columns: id INT"), "{message}");
        }
        _ => panic!("expected error"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn schema_gate_allows_new_table_on_write() {
    let dir = std::env::temp_dir().join(format!("stella_gate_ok_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let reg = ToolRegistry::with_issue_backend(dir.clone(), None);
    reg.update_storage_index(seeded_snapshot(&["users"]));

    // Write a SQL file with a genuinely new, dissimilar table.
    let result = reg
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "migrations/003.sql",
                "content": "CREATE TABLE orders (id INT);\n"
            }),
        )
        .await;
    assert!(!result.is_error(), "new table should pass the gate");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn schema_gate_blocks_normalized_duplicate_and_column_dup() {
    let dir = std::env::temp_dir().join(format!("stella_gate_norm_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let reg = ToolRegistry::with_issue_backend(dir.clone(), None);
    reg.update_storage_index(seeded_snapshot(&["user_profiles"]));

    // `UserProfile` is the same relation as `user_profiles` after
    // normalization + plural folding.
    let camel = reg
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "migrations/004.sql",
                "content": "CREATE TABLE UserProfile (id INT);\n"
            }),
        )
        .await;
    assert!(camel.is_error(), "normalized duplicate must be blocked");

    // Adding a column that already exists (id) is column-level drift.
    let column = reg
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "migrations/005.sql",
                "content": "ALTER TABLE user_profiles ADD COLUMN Id BIGINT;\n"
            }),
        )
        .await;
    assert!(column.is_error(), "duplicate column must be blocked");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn schema_gate_challenge_passes_with_declared_intent_and_records_it() {
    let dir = std::env::temp_dir().join(format!("stella_gate_intent_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let reg = ToolRegistry::with_issue_backend(dir.clone(), None);
    reg.update_storage_index(seeded_snapshot(&["payments"]));

    // `payment_records` is not a name duplicate, but it resembles
    // `payments` — withheld once with the evidence.
    let call = serde_json::json!({
        "path": "migrations/006.sql",
        "content": "CREATE TABLE payment_records (id INT);\n"
    });
    let challenged = reg.execute("write_file", &call).await;
    match &challenged {
        ToolOutput::Error { message } => {
            assert!(message.contains("write withheld"), "{message}");
            assert!(message.contains("storage_intent"), "{message}");
        }
        _ => panic!("expected the similarity challenge"),
    }

    // Retrying with a declared intent passes, lands the file, and
    // records the sentence in stella.storage.toml (origin `declared`).
    let mut with_intent = call.clone();
    with_intent["storage_intent"] = serde_json::json!(
        "Immutable ledger of imported legacy charges; payments holds live charges."
    );
    let passed = reg.execute("write_file", &with_intent).await;
    assert!(!passed.is_error(), "declared intent must pass: {passed:?}");
    let manifest = std::fs::read_to_string(dir.join("stella.storage.toml")).unwrap();
    assert!(
        manifest.contains("sql/default/payment_records"),
        "{manifest}"
    );
    assert!(manifest.contains("Immutable ledger"), "{manifest}");
    assert!(manifest.contains("origin = \"declared\""), "{manifest}");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn schema_gate_exempts_rewriting_a_files_own_objects() {
    let dir = std::env::temp_dir().join(format!("stella_gate_own_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("migrations")).unwrap();
    std::fs::write(
        dir.join("migrations/001.sql"),
        "CREATE TABLE users (id INT);\n",
    )
    .unwrap();
    let reg = ToolRegistry::with_issue_backend(dir.clone(), None);
    reg.update_storage_index(seeded_snapshot(&["users"]));

    // Rewriting the file that defines `users` is an in-place change to
    // the existing object, not a duplicate creation.
    let result = reg
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "migrations/001.sql",
                "content": "CREATE TABLE users (id INT, email TEXT);\n"
            }),
        )
        .await;
    assert!(!result.is_error(), "same-file rewrite must pass the gate");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn schema_gate_grows_the_index_after_a_successful_write() {
    let dir = std::env::temp_dir().join(format!("stella_gate_grow_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let reg = ToolRegistry::with_issue_backend(dir.clone(), None);

    // Empty index: the first migration passes and registers `users`.
    let first = reg
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "migrations/001.sql",
                "content": "CREATE TABLE users (id INT);\n"
            }),
        )
        .await;
    assert!(!first.is_error());

    // A second file re-creating `users` now conflicts — caught by the
    // in-session index growth, no graph re-index needed.
    let second = reg
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "migrations/002.sql",
                "content": "CREATE TABLE users (id INT);\n"
            }),
        )
        .await;
    assert!(second.is_error(), "in-session duplicate must be caught");
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn schema_gate_checks_edit_file_new_string() {
    let dir = std::env::temp_dir().join(format!("stella_gate_edit_{}", std::process::id()));
    std::fs::create_dir_all(dir.join("migrations")).unwrap();
    std::fs::write(dir.join("migrations/002.sql"), "-- add payments\n").unwrap();
    let reg = ToolRegistry::with_issue_backend(dir.clone(), None);
    reg.update_storage_index(seeded_snapshot(&["users"]));

    // An edit whose replacement introduces a duplicate CREATE is gated
    // exactly like a write.
    let result = reg
        .execute(
            "edit_file",
            &serde_json::json!({
                "path": "migrations/002.sql",
                "old_string": "-- add payments",
                "new_string": "CREATE TABLE users (id INT);"
            }),
        )
        .await;
    assert!(
        result.is_error(),
        "edit introducing a duplicate must be gated"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn schema_gate_allows_non_sql_write() {
    let dir = std::env::temp_dir().join(format!("stella_gate_nosql_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let reg = ToolRegistry::with_issue_backend(dir.clone(), None);
    reg.update_storage_index(seeded_snapshot(&["users"]));

    // Writing a Rust file should never trigger the schema gate.
    let result = reg
        .execute(
            "write_file",
            &serde_json::json!({
                "path": "src/main.rs",
                "content": "fn main() {}\n"
            }),
        )
        .await;
    assert!(!result.is_error());
    std::fs::remove_dir_all(&dir).ok();
}
