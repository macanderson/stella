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
        // Raw SQL rather than `insert_memory`: a fixture for
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

// ── v13: drop episode.salience / memory.salience ──────────────────────────

/// A store still carrying both dead `salience` columns must migrate through
/// v13 with the columns gone and every other value untouched. This is the
/// witness: at schema version 12, before `migrate_v13` existed, both
/// columns are still there after `ContextStore::open` and this test fails.
/// With the migration in place it passes.
#[test]
fn v13_drops_salience_and_keeps_everything_else() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    {
        // v7 is the last shape before the episode lifecycle columns landed.
        // Its `episode`/`memory` tables are exactly V1/V2, salience column
        // included.
        let conn = open_legacy(&path, 7);
        conn.execute(
            "INSERT INTO episode (public_id, summary, files_touched, outcome, salience,
                                  started_at, ended_at, recorded_at)
             VALUES ('epi_v13', 'renamed a module', '[\"src/lib.rs\"]', 'success',
                     0.8, '2026-01-01T00:00:00Z', '2026-01-01T01:00:00Z',
                     '2026-01-01T01:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory (public_id, kind, content, salience, recorded_at)
             VALUES ('mem_v13', 'note', 'prefer fd over find', 0.9,
                     '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        assert!(
            table_has_column(&conn, "episode", "salience"),
            "fixture really carries the column before open"
        );
        assert!(
            table_has_column(&conn, "memory", "salience"),
            "fixture really carries the column before open"
        );
    }

    let store = ContextStore::open(&path).unwrap();
    store.integrity_check().unwrap();
    let conn = store.conn();

    assert!(
        !table_has_column(&conn, "episode", "salience"),
        "v13 must drop episode.salience"
    );
    assert!(
        !table_has_column(&conn, "memory", "salience"),
        "v13 must drop memory.salience"
    );

    // SQLite's `DROP COLUMN` rewrites the whole table, so check that every
    // other column on the row still holds its value.
    let (summary, outcome): (String, String) = conn
        .query_row(
            "SELECT summary, outcome FROM episode WHERE public_id = 'epi_v13'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(summary, "renamed a module");
    assert_eq!(outcome, "success");
    let (kind, content): (String, String) = conn
        .query_row(
            "SELECT kind, content FROM memory WHERE public_id = 'mem_v13'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(kind, "note");
    assert_eq!(content, "prefer fd over find");
    drop(conn);

    // Rewind `user_version` to 12 and reopen: `migrate_v13` must re-run over
    // a column already gone without failing, the same idempotence contract
    // every other migration in this file carries.
    drop(store);
    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 12i64).unwrap();
    drop(conn);
    let store2 = ContextStore::open(&path).unwrap();
    let v: i64 = store2
        .conn()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        v, SCHEMA_VERSION,
        "re-running v13 over an already-dropped column must not fail"
    );
}

/// Whether `table` currently has a column named `column`, via
/// `PRAGMA table_info` — the same idempotence check `migrate_v13` itself runs.
fn table_has_column(conn: &Connection, table: &str, column: &str) -> bool {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>("name"))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    names.iter().any(|name| name == column)
}

// ── v14: the export views ─────────────────────────────────────────────────

/// The column names of a table or view, in order.
fn column_names(conn: &Connection, relation: &str) -> Vec<String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({relation})"))
        .unwrap();
    let names: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>("name"))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    names
}

/// Append one `context_use` record naming `record_id`, shaped the way the
/// `stella-cli` extractor writes it.
fn append_use(store: &ContextStore, id: &str, record_id: &str, trace: &str, task: &str, at: &str) {
    let body = serde_json::json!({
        "use_kind": "rendered",
        "context_record_id": record_id,
        "use_trace_id": trace,
        "task_id": task,
        "influence_stage": "none",
        "observed_at": at,
    })
    .to_string();
    let hash = format!("sha256:{id}");
    store
        .append_record(crate::LedgerAppend {
            record_id: id,
            lineage_id: id,
            record_kind: "context_use",
            record_hash: &hash,
            schema_version: "1",
            body: &body,
            observed_at: at,
            supersedes: None,
        })
        .unwrap();
}

/// The column lists are the contract a reader outside Stella builds on. A
/// change to either one has to ship a `_v2` view instead, and this test is
/// what says so.
#[test]
fn v14_export_view_columns_are_the_documented_contract() {
    let (_dir, store) = tmp_store();
    let conn = store.conn();
    assert_eq!(
        column_names(&conn, "export_memories_v1"),
        ["lineage", "revision", "kind", "content", "recorded_at"],
    );
    assert_eq!(
        column_names(&conn, "export_memory_uses_v1"),
        [
            "seq",
            "use_id",
            "lineage",
            "use_kind",
            "thread_id",
            "execution_id",
            "used_at"
        ],
    );
}

/// A v13 store gains both views when it opens. A store rewound to v13 that
/// already has them opens too, because the views are `IF NOT EXISTS`.
#[test]
fn v14_adds_the_export_views_and_reruns_cleanly() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("context.db");
    drop(ContextStore::open(&path).unwrap());
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP VIEW export_memories_v1;
             DROP VIEW export_memory_uses_v1;",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 13i64).unwrap();
    }

    let store = ContextStore::open(&path).unwrap();
    store.integrity_check().unwrap();
    for view in ["export_memories_v1", "export_memory_uses_v1"] {
        let n: i64 = store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'view' AND name = ?1",
                [view],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "{view} is created by v14");
    }
    drop(store);

    let conn = Connection::open(&path).unwrap();
    conn.pragma_update(None, "user_version", 13i64).unwrap();
    drop(conn);
    let store = ContextStore::open(&path).unwrap();
    let v: i64 = store
        .conn()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        v, SCHEMA_VERSION,
        "re-running v14 over its own views must not fail"
    );
}

/// The memory view holds each live memory once, with its current text. The
/// use view holds each memory use in ledger order, and nothing else. Before
/// v14 neither view exists, and every query here fails.
#[tokio::test]
async fn v14_export_views_hold_live_memories_and_their_uses() {
    let (_dir, store) = tmp_store();
    let first = store
        .upsert(crate::writeback::ContextDelta::new().with_memory(
            crate::writeback::MemoryInput::reflection("prefer rg over grep", Vec::<String>::new()),
        ))
        .await
        .unwrap();
    let kept_node = first.memory_node_ids[0].clone();
    let kept = store.memory_lineage(&kept_node).unwrap().expect("lineage");
    // An edit: the same lineage, new words.
    store
        .upsert(
            crate::writeback::ContextDelta::new().with_memory(
                crate::writeback::MemoryInput::reflection(
                    "prefer rg over grep, and fd over find",
                    Vec::<String>::new(),
                )
                .revises(&kept),
            ),
        )
        .await
        .unwrap();
    // A memory a person then forgot.
    let second = store
        .upsert(crate::writeback::ContextDelta::new().with_memory(
            crate::writeback::MemoryInput::reflection("keep commits small", Vec::<String>::new()),
        ))
        .await
        .unwrap();
    let forgot_node = second.memory_node_ids[0].clone();
    let forgot = store
        .memory_lineage(&forgot_node)
        .unwrap()
        .expect("lineage");
    assert!(store.supersede_node(&forgot_node).unwrap());

    append_use(
        &store,
        "cu_one",
        &kept_node,
        "ut_7",
        "session:ses-1789972711780-2168",
        "2026-09-21 09:03:08",
    );
    append_use(
        &store,
        "cu_two",
        &forgot_node,
        "ut_8",
        "execution:8",
        "2026-09-21T10:00:00Z",
    );
    // A use of a context record, which is no memory.
    append_use(
        &store,
        "cu_three",
        "^ctx-small-commits",
        "ut_9",
        "session:ses-1789972711780-2168",
        "2026-09-21T11:00:00Z",
    );

    let conn = store.conn();
    let memories: Vec<(String, String, String, String)> = conn
        .prepare("SELECT lineage, revision, kind, content FROM export_memories_v1")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(memories.len(), 1, "one live memory: {memories:?}");
    let (lineage, revision, kind, content) = &memories[0];
    assert_eq!(lineage, &kept);
    assert_ne!(revision, &kept, "the revision id moves with the text");
    assert_eq!(kind, "reflection");
    assert_eq!(content, "prefer rg over grep, and fd over find");

    type UseRow = (
        i64,
        String,
        String,
        String,
        Option<String>,
        Option<i64>,
        String,
    );
    let uses: Vec<UseRow> = conn
        .prepare(
            "SELECT seq, use_id, lineage, use_kind, thread_id, execution_id, used_at
               FROM export_memory_uses_v1 ORDER BY seq",
        )
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(uses.len(), 2, "the record use is left out: {uses:?}");
    assert_eq!(uses[0].1, "cu_one");
    assert_eq!(uses[0].2, kept);
    assert_eq!(uses[0].3, "rendered");
    assert_eq!(uses[0].4.as_deref(), Some("ses-1789972711780-2168"));
    assert_eq!(uses[0].5, Some(7));
    assert_eq!(
        uses[0].6, "2026-09-21T09:03:08Z",
        "the store's stamp is rewritten"
    );
    // A forgotten memory's past uses still happened.
    assert_eq!(uses[1].2, forgot);
    assert_eq!(uses[1].4, None, "a turn with no thread has no thread id");
    assert_eq!(uses[1].5, Some(8));
    assert_eq!(uses[1].6, "2026-09-21T10:00:00Z");
    assert!(uses[0].0 < uses[1].0, "seq grows in ledger order");

    let after: Vec<String> = conn
        .prepare("SELECT use_id FROM export_memory_uses_v1 WHERE seq > ?1 ORDER BY seq")
        .unwrap()
        .query_map([uses[0].0], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(
        after,
        ["cu_two"],
        "a reader resumes past the last seq it saw"
    );
}
