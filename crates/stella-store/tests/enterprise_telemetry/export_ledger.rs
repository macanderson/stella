//! The export-ledger half of `enterprise_telemetry.rs`'s coverage: identity
//! persistence, the per-sink export nonce, and the pending/skip/backfill
//! ledger. Split out because the parent file sat close to the 1500-line
//! ratchet (AGENTS.md § "God files") — a pure move, `use super::*` carries
//! over the shared fixtures (`rollup`, `context`, `SINK_A`/`SINK_B`).

use super::*;

#[test]
fn installation_and_store_identities_persist_and_reset_on_their_real_boundaries() {
    let dir = tempfile::tempdir().unwrap();
    let host_a = dir.path().join("host-a");
    let host_b = dir.path().join("host-b");
    let install_a = load_or_create_installation_uuid(&host_a).unwrap();
    assert_eq!(
        install_a,
        load_or_create_installation_uuid(&host_a).unwrap()
    );
    assert_ne!(
        install_a,
        load_or_create_installation_uuid(&host_b).unwrap()
    );

    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = stella_store::Store::open(&workspace).unwrap();
    let first = store.enterprise_store_uuid().unwrap();
    assert_eq!(first, store.enterprise_store_uuid().unwrap());
    drop(store);
    let reopened = stella_store::Store::open(&workspace).unwrap();
    assert_eq!(first, reopened.enterprise_store_uuid().unwrap());
    drop(reopened);

    let db = workspace.join(".stella/private/store.db");
    std::fs::remove_file(&db).unwrap();
    let reset = stella_store::Store::open(&workspace).unwrap();
    assert_ne!(first, reset.enterprise_store_uuid().unwrap());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(host_a.join("installation-id"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn cloned_store_uses_a_fresh_persistent_export_nonce_per_ledger_row() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    let store = Store::open(&source).unwrap();
    store.begin_enterprise_enrollment(SINK_A).unwrap();
    let execution = store
        .begin_execution("run", "same", "anthropic", "model")
        .unwrap();
    store.finish_execution(execution, "completed", 0.0).unwrap();
    let store_uuid = store.enterprise_store_uuid().unwrap();
    drop(store);

    let source_db = source.join(".stella/private/store.db");
    let clone_a = dir.path().join("clone-a");
    let clone_b = dir.path().join("clone-b");
    let db_a = stella_store::workspace_private_sqlite_path(&clone_a, "store.db").unwrap();
    let db_b = stella_store::workspace_private_sqlite_path(&clone_b, "store.db").unwrap();
    std::fs::copy(&source_db, &db_a).unwrap();
    std::fs::copy(&source_db, &db_b).unwrap();

    let a = Store::open(&clone_a).unwrap();
    let b = Store::open(&clone_b).unwrap();
    assert_eq!(a.enterprise_store_uuid().unwrap(), store_uuid);
    assert_eq!(b.enterprise_store_uuid().unwrap(), store_uuid);
    let nonce_a = a
        .mark_enterprise_export_pending(SINK_A, execution)
        .unwrap()
        .unwrap();
    let nonce_b = b
        .mark_enterprise_export_pending(SINK_A, execution)
        .unwrap()
        .unwrap();
    assert_ne!(
        nonce_a, nonce_b,
        "clones must not derive the same export id"
    );
    assert_eq!(
        a.mark_enterprise_export_pending(SINK_A, execution)
            .unwrap()
            .unwrap(),
        nonce_a,
        "retrying one ledger row must reuse its persisted nonce"
    );

    let identity =
        OperationalIdentity::new("11111111-1111-4111-8111-111111111111", &store_uuid).unwrap();
    let event = |nonce: &str| {
        let context = OperationalEventContext::new(
            "enroll_01",
            "org_01",
            "workspace_01",
            identity.clone(),
            nonce,
            [ManagedModelDimension::new("anthropic", "anthropic/claude-sonnet-4").unwrap()],
        )
        .unwrap();
        StellaOperationalEventV1::from_finalized_rollup(&context, &rollup(execution)).unwrap()
    };
    assert_ne!(event(&nonce_a).event_id(), event(&nonce_b).event_id());
}

#[test]
fn export_ledger_backfills_only_post_enrollment_pending_executions() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = stella_store::Store::open(&workspace).unwrap();
    let old = store
        .begin_execution("run", "old", "anthropic", "model")
        .unwrap();
    store.finish_execution(old, "completed", 0.0).unwrap();
    store.begin_enterprise_enrollment(SINK_A).unwrap();
    assert!(
        store
            .mark_enterprise_export_pending(SINK_A, old)
            .unwrap()
            .is_none()
    );

    let new = store
        .begin_execution("run", "new", "anthropic", "model")
        .unwrap();
    store.finish_execution(new, "completed", 0.0).unwrap();
    assert!(
        store
            .mark_enterprise_export_pending(SINK_A, new)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        store
            .pending_enterprise_export_page(SINK_A, None, 256)
            .unwrap()[0]
            .execution_id,
        new
    );
    store.mark_enterprise_export_spooled(SINK_A, new).unwrap();
    assert!(
        store
            .pending_enterprise_export_page(SINK_A, None, 256)
            .unwrap()
            .is_empty()
    );

    drop(store);
    let reopened = stella_store::Store::open(&workspace).unwrap();
    assert!(
        reopened
            .pending_enterprise_export_page(SINK_A, None, 256)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn skipped_export_is_durable_distinct_from_spooled_and_never_reenters_pending() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = stella_store::Store::open(&workspace).unwrap();
    store.begin_enterprise_enrollment(SINK_A).unwrap();
    let execution = store
        .begin_execution("run", "legacy", "anthropic", "model")
        .unwrap();
    store.finish_execution(execution, "completed", 0.0).unwrap();
    store
        .mark_enterprise_export_pending(SINK_A, execution)
        .unwrap()
        .unwrap();
    store
        .mark_enterprise_export_skipped(
            SINK_A,
            execution,
            EnterpriseExportSkipReason::MalformedNonce,
        )
        .unwrap();
    assert!(
        store
            .pending_enterprise_export_page(SINK_A, None, 256)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .mark_enterprise_export_pending(SINK_A, execution)
            .unwrap()
            .is_none(),
        "a skipped legacy row must never be represented as a retryable pending export"
    );
    let status = store.enterprise_export_ledger_status(SINK_A).unwrap();
    assert_eq!(status.skipped_rows, 1);
    assert_eq!(status.malformed_nonce_rows, 1);
    assert_eq!(status.malformed_rollup_rows, 0);
    assert_eq!(status.missing_rollup_rows, 0);

    drop(store);
    let path = stella_store::workspace_private_sqlite_path(&workspace, "store.db").unwrap();
    let inspect = rusqlite::Connection::open(&path).unwrap();
    let pending_ledger_rows: i64 = inspect
        .query_row(
            "SELECT COUNT(*) FROM enterprise_export_ledger
             WHERE sink_fingerprint = ?1 AND execution_id = ?2",
            rusqlite::params![SINK_A, execution],
            |row| row.get(0),
        )
        .unwrap();
    let skip_reason: String = inspect
        .query_row(
            "SELECT reason FROM enterprise_export_skips
             WHERE sink_fingerprint = ?1 AND execution_id = ?2",
            rusqlite::params![SINK_A, execution],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending_ledger_rows, 0);
    assert_eq!(skip_reason, "malformed_nonce");
    drop(inspect);
    let reopened = stella_store::Store::open(&workspace).unwrap();
    assert_eq!(
        reopened.enterprise_export_ledger_status(SINK_A).unwrap(),
        status
    );
}

#[test]
fn negative_export_skip_counters_fail_closed_instead_of_reporting_zero() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = Store::open(&workspace).unwrap();
    store.begin_enterprise_enrollment(SINK_A).unwrap();
    drop(store);

    let path = stella_store::workspace_private_sqlite_path(&workspace, "store.db").unwrap();
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE enterprise_export_enrollment SET skipped_rows = -1
         WHERE sink_fingerprint = ?1",
        rusqlite::params![SINK_A],
    )
    .unwrap();
    drop(raw);

    let reopened = Store::open(&workspace).unwrap();
    assert!(
        reopened.enterprise_export_ledger_status(SINK_A).is_err(),
        "corrupt aggregate counters must not be silently reported as zero"
    );
}

#[test]
fn pending_export_backfill_is_hard_paged_across_a_ten_thousand_row_outage() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = Store::open(&workspace).unwrap();
    store.begin_enterprise_enrollment(SINK_A).unwrap();
    for index in 0..10_050 {
        let execution = store
            .begin_execution("run", &format!("outage-{index}"), "anthropic", "model")
            .unwrap();
        store.finish_execution(execution, "completed", 0.0).unwrap();
        store
            .mark_enterprise_export_pending(SINK_A, execution)
            .unwrap()
            .unwrap();
    }

    assert!(
        store
            .pending_enterprise_export_page(SINK_A, None, 257)
            .is_err(),
        "callers cannot request an unbounded startup page"
    );
    let first = store
        .pending_enterprise_export_page(SINK_A, None, 256)
        .unwrap();
    assert_eq!(first.len(), 256);
    let second = store
        .pending_enterprise_export_page(SINK_A, Some(first.last().unwrap().execution_id), 256)
        .unwrap();
    assert_eq!(second.len(), 256);
    assert!(second[0].execution_id > first[255].execution_id);
}

#[test]
fn completed_export_ledger_compacts_with_a_durable_idempotency_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = Store::open(&workspace).unwrap();
    store.begin_enterprise_enrollment(SINK_A).unwrap();
    let mut rows = Vec::new();
    for index in 0..300 {
        let execution = store
            .begin_execution("run", &format!("done-{index}"), "anthropic", "model")
            .unwrap();
        store.finish_execution(execution, "completed", 0.0).unwrap();
        let nonce = store
            .mark_enterprise_export_pending(SINK_A, execution)
            .unwrap()
            .unwrap();
        store
            .mark_enterprise_export_spooled(SINK_A, execution)
            .unwrap();
        rows.push((execution, nonce));
    }

    assert_eq!(
        store.compact_enterprise_export_ledger(SINK_A, 32).unwrap(),
        268
    );
    assert!(
        store
            .mark_enterprise_export_pending(SINK_A, rows[0].0)
            .unwrap()
            .is_none(),
        "the compacted-through boundary prevents a new nonce for old closeout"
    );
    assert_eq!(
        store
            .mark_enterprise_export_pending(SINK_A, rows[299].0)
            .unwrap()
            .unwrap(),
        rows[299].1
    );
}

#[test]
fn legacy_export_nonce_migration_is_resumable_and_startup_bounded() {
    const LEGACY_ROWS: i64 = 50_257;
    const STARTUP_ROW_BUDGET: i64 = 1_024;

    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    drop(Store::open(&workspace).unwrap());
    let path = stella_store::workspace_private_sqlite_path(&workspace, "store.db").unwrap();
    let mut raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch(
        "DROP TABLE enterprise_export_ledger;
         CREATE TABLE enterprise_export_ledger (
             sink_fingerprint TEXT NOT NULL,
             execution_id INTEGER NOT NULL,
             status TEXT NOT NULL CHECK(status IN ('pending', 'spooled')),
             PRIMARY KEY(sink_fingerprint, execution_id)
         );",
    )
    .unwrap();
    let tx = raw.transaction().unwrap();
    {
        let mut insert = tx
            .prepare(
                "INSERT INTO enterprise_export_ledger
                 (sink_fingerprint, execution_id, status) VALUES (?1, ?2, 'pending')",
            )
            .unwrap();
        for execution_id in 1..=LEGACY_ROWS {
            insert
                .execute(rusqlite::params![SINK_A, execution_id])
                .unwrap();
        }
    }
    tx.commit().unwrap();
    let ledger_rootpage_before: i64 = raw
        .query_row(
            "SELECT rootpage FROM sqlite_master
             WHERE type = 'table' AND name = 'enterprise_export_ledger'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    drop(raw);

    drop(Store::open(&workspace).unwrap());
    let inspect = rusqlite::Connection::open(&path).unwrap();
    let ledger_rootpage_after: i64 = inspect
        .query_row(
            "SELECT rootpage FROM sqlite_master
             WHERE type = 'table' AND name = 'enterprise_export_ledger'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let preserved_rows: i64 = inspect
        .query_row("SELECT COUNT(*) FROM enterprise_export_ledger", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(
        ledger_rootpage_after, ledger_rootpage_before,
        "first open must not rebuild or copy the legacy export ledger"
    );
    assert_eq!(preserved_rows, LEGACY_ROWS);
    let (migrated, batches, complete): (i64, i64, i64) = inspect
        .query_row(
            "SELECT migrated_rows, batches_completed, is_complete
             FROM enterprise_export_migration WHERE singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(migrated, STARTUP_ROW_BUDGET);
    assert_eq!(
        batches, 4,
        "startup must commit multiple fixed-size batches"
    );
    assert_eq!(complete, 0);
    assert!(
        inspect
            .query_row(
                "SELECT COUNT(*) FROM enterprise_export_ledger WHERE export_nonce = ''",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap()
            > 49_000
    );
    drop(inspect);

    let mut previous = migrated;
    for _ in 0..100 {
        drop(Store::open(&workspace).unwrap());
        let inspect = rusqlite::Connection::open(&path).unwrap();
        let (current, complete): (i64, i64) = inspect
            .query_row(
                "SELECT migrated_rows, is_complete
                 FROM enterprise_export_migration WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(current >= previous);
        assert!(current - previous <= STARTUP_ROW_BUDGET);
        previous = current;
        if complete == 1 {
            break;
        }
    }
    let inspect = rusqlite::Connection::open(&path).unwrap();
    let (rows, distinct_nonces, empty_nonces): (i64, i64, i64) = inspect
        .query_row(
            "SELECT COUNT(*), COUNT(DISTINCT export_nonce),
                    SUM(CASE WHEN export_nonce = '' THEN 1 ELSE 0 END)
             FROM enterprise_export_ledger",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(rows, LEGACY_ROWS);
    assert_eq!(distinct_nonces, LEGACY_ROWS);
    assert_eq!(empty_nonces, 0);
}

#[test]
fn first_post_upgrade_runtime_repairs_pending_nonces_beyond_the_startup_budget() {
    const LEGACY_ROWS: usize = 1_026;

    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = Store::open(&workspace).unwrap();
    store.begin_enterprise_enrollment(SINK_A).unwrap();
    let mut executions = Vec::with_capacity(LEGACY_ROWS);
    for index in 0..LEGACY_ROWS {
        let execution = store
            .begin_execution("run", &format!("legacy-{index}"), "anthropic", "model")
            .unwrap();
        store.finish_execution(execution, "completed", 0.0).unwrap();
        executions.push(execution);
    }
    drop(store);

    let path = stella_store::workspace_private_sqlite_path(&workspace, "store.db").unwrap();
    let mut raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch(
        "DROP TABLE enterprise_export_ledger;
         CREATE TABLE enterprise_export_ledger (
             sink_fingerprint TEXT NOT NULL,
             execution_id INTEGER NOT NULL,
             status TEXT NOT NULL CHECK(status IN ('pending', 'spooled')),
             PRIMARY KEY(sink_fingerprint, execution_id)
         );",
    )
    .unwrap();
    let tx = raw.transaction().unwrap();
    {
        let mut insert = tx
            .prepare(
                "INSERT INTO enterprise_export_ledger
                 (sink_fingerprint, execution_id, status) VALUES (?1, ?2, ?3)",
            )
            .unwrap();
        for (index, execution_id) in executions.iter().enumerate() {
            let status = if index < 1_024 { "spooled" } else { "pending" };
            insert
                .execute(rusqlite::params![SINK_A, execution_id, status])
                .unwrap();
        }
    }
    tx.commit().unwrap();
    drop(raw);

    let upgraded = Store::open(&workspace).unwrap();
    let repaired_by_mark = upgraded
        .mark_enterprise_export_pending(SINK_A, executions[1_024])
        .unwrap()
        .unwrap();
    assert_eq!(repaired_by_mark.len(), 32);
    assert!(
        repaired_by_mark
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );

    let pending = upgraded
        .pending_enterprise_export_page(SINK_A, None, 256)
        .unwrap();
    assert_eq!(pending.len(), 2);
    assert!(pending.iter().all(|row| {
        row.export_nonce.len() == 32
            && row
                .export_nonce
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
    }));
}
