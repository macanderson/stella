//! The v10 `edge.public_id` hardening witness (#617) — its own module
//! because `store/tests.rs` sits at the file-size ceiling.

use crate::ContextStore;

/// #617: `edge.public_id` uniqueness is a schema fact since v10 — duplicates
/// already on disk are re-minted (earliest keeps the id) and a fresh
/// collision is a loud constraint error, never two silently coexisting rows.
#[tokio::test]
async fn v10_remints_duplicate_edge_ids_and_enforces_uniqueness() {
    let dir = tempfile::tempdir().unwrap();
    let store = ContextStore::open(dir.path().join("context.db")).unwrap();
    {
        // Rebuild the pre-v10 shape: no unique index, duplicate ids on disk.
        let conn = store.conn();
        conn.execute_batch(
            "DROP INDEX idx_edge_public_id;
             INSERT INTO node (public_id, kind, display_name, content, content_hash, recorded_at)
             VALUES ('nod_a', 'symbol', 'a', '', 'h', '2026-07-28T00:00:00Z'),
                    ('nod_b', 'symbol', 'b', '', 'h', '2026-07-28T00:00:00Z');
             INSERT INTO edge (public_id, rel, src_id, dst_id, weight, properties, recorded_at)
             VALUES ('edg_dup', 'refines', 1, 2, 1.0, '{}', '2026-07-28T00:00:00Z'),
                    ('edg_dup', 'refines', 2, 1, 1.0, '{}', '2026-07-28T00:00:01Z');",
        )
        .unwrap();
        conn.pragma_update(None, "user_version", 9i64).unwrap();
        super::schema::migrate(&conn).unwrap();

        let ids: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT public_id FROM edge ORDER BY id")
                .unwrap();
            let rows = stmt.query_map([], |r| r.get(0)).unwrap();
            rows.collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(ids[0], "edg_dup", "the earliest row keeps its id");
        assert_ne!(ids[1], "edg_dup", "the later collider is re-minted");

        let clash = conn.execute(
            "INSERT INTO edge (public_id, rel, src_id, dst_id, weight, properties, recorded_at)
             VALUES ('edg_dup', 'refines', 1, 2, 1.0, '{}', '2026-07-28T00:00:02Z')",
            [],
        );
        assert!(clash.is_err(), "a fresh duplicate is a constraint error");
    }
}
