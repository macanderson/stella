//! Schema-version and migration tests for the context store: refusing a
//! store written by a newer stella, and each vN -> vN+1 migration
//! preserving the rows it must. Split out of the parent module, which sat
//! at exactly the 1500-line ratchet with no baseline entry to absorb it.

use super::*;

/// A store stamped by a *newer* stella must be refused, not opened as-is:
/// episodic memory and the fact graph are not rebuildable, so an older
/// binary writing into a schema it does not know is unrecoverable data
/// loss. The message must name the real fault — an out-of-date binary.
#[test]
fn rejects_a_context_db_written_by_a_newer_stella() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    {
        // The full current schema, stamped one version past this build —
        // exactly what a newer stella leaves behind.
        // `open_legacy` now applies V3 itself, so applying it again here
        // would fail on a duplicate index.
        open_legacy(&path, SCHEMA_VERSION + 1);
    }

    let Err(err) = ContextStore::open(&path) else {
        panic!("a store stamped by a newer stella must not open");
    };
    assert!(
        matches!(err, ContextError::SchemaTooNew(_)),
        "unexpected error: {err:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains(&(SCHEMA_VERSION + 1).to_string()), "{msg}");
    assert!(msg.contains("binary is out of date"), "{msg}");
}

#[test]
fn migrates_v1_context_db_preserving_bitemporal_edges() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let (a, b, c) = {
        let conn = open_legacy(&path, 1);
        let props = serde_json::json!({});
        let a = insert_legacy_node(&conn, NodeKind::Concept, "a", "", None, T1);
        let b = insert_legacy_node(&conn, NodeKind::Concept, "b", "", None, T1);
        let c = insert_legacy_node(&conn, NodeKind::Concept, "c", "", None, T1);
        // A belief a->b that was later corrected away (superseded at T2).
        let e_ab =
            insert_edge(&conn, "relates_to", a, b, 1.0, &props, None, None, T1, None).unwrap();
        close_edge(&conn, e_ab, T2, T2).unwrap();
        // A still-believed belief a->c whose world-validity window closed in
        // the past (valid_to = T1) — the `as_of` discriminator row.
        insert_edge(
            &conn,
            "relates_to",
            a,
            c,
            1.0,
            &props,
            Some(T0),
            Some(T1),
            T1,
            None,
        )
        .unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 1, "fixture really is a v1 db before open");
        (a, b, c)
    };

    // Open through the store: migrate v1 -> SCHEMA_VERSION.
    let store = ContextStore::open(&path).unwrap();
    store.integrity_check().unwrap();
    {
        let conn = store.conn();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION, "v1 db upgraded to current");
        // Currently believed = only a->c (a->b superseded; a->c's past
        // valid_to is ignored by transaction-time queries).
        assert_eq!(edge_pairs(&conn, None), vec![(a, c)]);
        // Both edges still physically present, reconstructable as-of T1.
        assert_eq!(edge_pairs(&conn, Some(T1)), vec![(a, b), (a, c)]);
    }

    // Re-open: replay is a no-op — same version, same data.
    drop(store);
    let store2 = ContextStore::open(&path).unwrap();
    let conn = store2.conn();
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
    assert_eq!(edge_pairs(&conn, None), vec![(a, c)]);
}

#[test]
fn migrates_v2_context_db_preserving_memories() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let mem_public = "mem_phase0fixturememory0001";
    {
        let conn = open_legacy(&path, 2);
        // Production writes a memory as a canonical `memory` row plus a
        // retrievable mirror `node` in one transaction; reproduce both.
        // Raw SQL rather than `insert_memory`, deliberately: a fixture for
        // schema version N must write version N's shape. Calling today's
        // writer would put a v5 `lineage_id` into a v2 table and test a
        // database that never existed.
        conn.execute(
            "INSERT INTO memory (public_id, kind, content, salience, recorded_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            params![mem_public, "reflection", "prefer rg over grep", 0.5, T1],
        )
        .unwrap();
        insert_legacy_node(
            &conn,
            NodeKind::Memory,
            "prefer rg over grep",
            "prefer rg over grep",
            Some(&format!("memory://{mem_public}")),
            T1,
        );
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 2, "fixture really is a v2 db before open");
    }

    let store = ContextStore::open(&path).unwrap();
    store.integrity_check().unwrap();
    {
        let conn = store.conn();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION, "v2 db upgraded to current");
        // The canonical memory row survived the migration.
        let content: String = conn
            .query_row(
                "SELECT content FROM memory WHERE public_id = ?1",
                params![mem_public],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(content, "prefer rg over grep");
    }
    // And it is still retrievable through the mirror node (the recall
    // surface behind `stella memory`).
    let mem_nodes = store.memory_nodes().unwrap();
    assert_eq!(mem_nodes.len(), 1);
    assert_eq!(mem_nodes[0].content, "prefer rg over grep");
}

/// The same fixture at v3 — one per schema version, because the lineage
/// backfill is the only Phase-1 change that rewrites existing rows and each
/// version reaches it by a different path.
#[test]
fn migrates_v3_context_db_preserving_memories() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let mem_public = "mem_v3fixturememory000001";
    {
        let conn = open_legacy(&path, 3);
        conn.execute(
            "INSERT INTO memory (public_id, kind, content, salience, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                mem_public,
                "note",
                "the deploy needs the migration first",
                0.0,
                T1
            ],
        )
        .unwrap();
        insert_legacy_node(
            &conn,
            NodeKind::Memory,
            "the deploy needs the migration first",
            "the deploy needs the migration first",
            Some(&format!("memory://{mem_public}")),
            T1,
        );
    }

    let store = ContextStore::open(&path).unwrap();
    store.integrity_check().unwrap();
    let stats = store.memory_lineage_stats().unwrap();
    assert_eq!(stats.lineages, 1);
    assert_eq!(stats.live, 1);
    assert_eq!(stats.superseded, 0);
    assert_eq!(
        store.memory_revisions(mem_public).unwrap().len(),
        1,
        "a migrated memory is a lineage with exactly one revision"
    );
}

/// The migration must be safe to run twice — reopening a store is the normal
/// case, and a backfill that is not idempotent corrupts on the second open.
#[test]
fn the_lineage_migration_is_idempotent_across_reopens() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let mem_public = "mem_idempotencefixture001";
    {
        let conn = open_legacy(&path, 2);
        conn.execute(
            "INSERT INTO memory (public_id, kind, content, salience, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![mem_public, "note", "a lesson", 0.0, T1],
        )
        .unwrap();
    }
    let first = {
        let store = ContextStore::open(&path).unwrap();
        store.memory_lineage_stats().unwrap()
    };
    let second = {
        let store = ContextStore::open(&path).unwrap();
        store.memory_lineage_stats().unwrap()
    };
    assert_eq!(first, second, "reopening must not change anything");
    assert_eq!(first.lineages, 1);
    assert_eq!(first.live, 1);
}
/// A v4 store — the last shape before memory lineage — must migrate all the way
/// to the compaction schema, and reopening must not re-run anything.
#[test]
fn a_v4_context_db_migrates_and_the_compaction_migration_is_idempotent() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    let mem_public = "mem_v4fixturememory000001";
    {
        let conn = open_legacy(&path, 4);
        conn.execute(
            "INSERT INTO memory (public_id, kind, content, salience, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![mem_public, "note", "a v4 lesson", 0.0, T1],
        )
        .unwrap();
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 4, "fixture really is a v4 db before open");
    }

    let first = {
        let store = ContextStore::open(&path).unwrap();
        store.integrity_check().unwrap();
        let v: i64 = store
            .conn()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION, "v4 db upgraded to current");
        assert!(
            store.compaction_watermark().unwrap().is_none(),
            "the table lands empty — a migration must not invent a compaction \
             that never ran"
        );
        store
            .compact(&ContextCompactPolicy::orphans_only())
            .unwrap();
        store.compaction_watermark().unwrap().expect("stamped")
    };

    // Reopen: the ladder replays as a no-op and the watermark is untouched.
    let store = ContextStore::open(&path).unwrap();
    store.integrity_check().unwrap();
    assert_eq!(
        store.compaction_watermark().unwrap().as_ref(),
        Some(&first),
        "reopening must not re-run the migration or clear the watermark"
    );
    assert_eq!(
        store.memory_lineage_stats().unwrap().lineages,
        1,
        "and the v4 memory still made it through the lineage backfill"
    );
}

// ── v8: the lifecycle ledger, and episode.lineage_id (#714) ──────────────

/// ADR 0010 point 6 (ratified 2026-07-26) says `lineage_id` lands on `memory`
/// **and `episode`**, and the plan document records Phase 1 as having delivered
/// exactly that. It did not — `migrate_v5` altered `memory` alone. v8 closes the
/// gap in favor of the ratified ADR rather than amending the ADR down to the
/// code.
///
/// The backfill is the same lossless one v5 used: every existing row is its own
/// lineage's first revision, so `lineage_id = public_id`. This asserts that on a
/// row written by a **pre-v6** binary, which is the only case where the backfill
/// does any work.
#[test]
fn v8_backfills_episode_lineage_from_public_id_without_touching_content() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    {
        let conn = open_legacy(&path, 3);
        conn.execute(
            "INSERT INTO episode (public_id, summary, files_touched, outcome, salience,
                                  started_at, ended_at, recorded_at)
             VALUES ('epi_legacy', 'fixed the tenancy leak', '[\"src/db.rs\"]', 'success',
                     0.5, '2026-01-01T00:00:00Z', '2026-01-01T01:00:00Z',
                     '2026-01-01T01:00:00Z')",
            [],
        )
        .unwrap();
    }

    let _store = ContextStore::open(&path).unwrap();
    let conn = Connection::open(&path).unwrap();

    let (lineage, summary, superseded): (String, String, Option<String>) = conn
        .query_row(
            "SELECT lineage_id, summary, superseded_at FROM episode WHERE public_id = 'epi_legacy'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        lineage, "epi_legacy",
        "an existing episode is its own lineage's first revision"
    );
    assert_eq!(
        summary, "fixed the tenancy leak",
        "the backfill read no content and changed none"
    );
    assert!(
        superseded.is_none(),
        "a migrated episode is live, not superseded"
    );
}

/// The migration is statement-level idempotent, not merely gated by the version
/// ladder — a rewound `user_version` is how the fixtures above are built and
/// what a partial restore looks like, and SQLite has no `ADD COLUMN IF NOT
/// EXISTS` to fall back on.
#[test]
fn v8_is_idempotent_across_a_rewound_user_version() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    ContextStore::open(&path).unwrap();
    {
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "user_version", 5i64).unwrap();
    }
    // Re-running v6 over a database that already has every v6 object must not
    // fail on a duplicate column, index, table, or trigger.
    ContextStore::open(&path).unwrap();
    let conn = Connection::open(&path).unwrap();
    let v: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, SCHEMA_VERSION);
}

/// The ledger's append-only guarantee survives a migration, because it is
/// installed as triggers rather than enforced by the writing code.
#[test]
fn v8_installs_the_append_only_triggers_on_a_migrated_store() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    drop(open_legacy(&path, 3));
    let store = ContextStore::open(&path).unwrap();
    store
        .append_record(crate::LedgerAppend {
            record_id: "obs_1",
            lineage_id: "obs_1",
            record_kind: "observation",
            record_hash: "sha256:aa",
            schema_version: "1.0-draft",
            body: "{}",
            observed_at: "2026-07-26T12:00:00Z",
            supersedes: None,
        })
        .unwrap();
    drop(store);

    let conn = Connection::open(&path).unwrap();
    assert!(
        conn.execute("DELETE FROM context_records", []).is_err(),
        "a migrated store's ledger is rewritable"
    );
}

/// Not one legacy row is rewritten by v8 (ADR 0010: new kinds are born
/// canonical and have no migration). The only writes are the additive episode
/// columns and their backfill.
#[test]
fn v8_creates_the_ledger_empty() {
    let dir = TempDir::new().unwrap();
    let store = ContextStore::open(dir.path().join("context.db")).unwrap();
    assert!(
        store.record_counts().unwrap().is_empty(),
        "a fresh ledger holds nothing — records are born, never migrated in"
    );
}
