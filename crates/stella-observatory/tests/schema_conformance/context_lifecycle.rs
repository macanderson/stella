//! The context.db lifecycle route, and its degraded/legacy cases (#4486 split of `../schema_conformance.rs`).
use super::*;

/// The #1871 witness: the route folds the seeded observation → proposal →
/// promotion-event lineage back out of a real-migration `context.db`. Fails
/// on main (the route is absent), and fails if any ledger or episode column
/// this crate reads is renamed in `stella-context`.
#[test]
fn context_lifecycle_returns_the_promotion_lineage() {
    let (workspace, proposal_lineage, observation_id) = real_context_workspace();

    let body = respond(workspace.path(), "/api/context-lifecycle").body;
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert!(v.get("error").is_none(), "{v}");
    assert_eq!(v["present"], true, "{v}");

    let proposal = &v["proposals"][0];
    assert_eq!(proposal["lineage_id"], proposal_lineage.as_str(), "{v}");
    assert_eq!(proposal["candidate_id"], CANDIDATE_ID, "{v}");
    assert_eq!(
        proposal["status"], "confirmed",
        "the decision standing is replayed from the event log, not stored: {v}"
    );
    assert_eq!(
        proposal["supporting_observations"][0],
        observation_id.as_str(),
        "the lineage reaches back to its evidence: {v}"
    );
    assert_eq!(
        proposal["events"][0]["action"], "confirmed",
        "the proposal carries its own slice of the audit trail: {v}"
    );

    assert_eq!(v["events"][0]["action"], "confirmed", "{v}");
    assert_eq!(
        v["events"][0]["candidate_id"], CANDIDATE_ID,
        "the timeline names the candidate its lineage points at: {v}"
    );

    assert_eq!(v["episodes"][0]["outcome"], "success", "{v}");
    assert_eq!(v["episodes"][0]["summary"], "added the parser fix", "{v}");

    let kinds: Vec<&str> = v["counts"]
        .as_array()
        .expect("counts")
        .iter()
        .filter_map(|c| c["kind"].as_str())
        .collect();
    for kind in ["observation", "record_proposal", "promotion_event"] {
        assert!(kinds.contains(&kind), "counts missing {kind}: {v}");
    }
}

/// Missing is a state: a workspace that has never built a context plane
/// answers with the full (empty) payload shape, never a 500 and never a
/// missing key.
#[test]
fn a_workspace_with_no_context_db_degrades_to_an_empty_lifecycle() {
    let workspace = real_store_workspace();
    let response = respond(workspace.path(), "/api/context-lifecycle");
    assert_eq!(
        response.status,
        "200 OK",
        "body: {}",
        String::from_utf8_lossy(&response.body)
    );
    let v: serde_json::Value = serde_json::from_slice(&response.body).expect("json");
    assert_eq!(v["present"], false, "{v}");
    for key in [
        "counts",
        "proposals",
        "events",
        "episodes",
        "selection_health",
    ] {
        assert_eq!(v[key], serde_json::json!([]), "{key} must be empty: {v}");
    }
}

/// The read-only observer never migrates, so it can be pointed at a
/// `context.db` older than the v8 lifecycle ledger — same hazard the pre-v18
/// store test above covers. The ledger sections degrade to empty and the
/// episode list (whose v8 columns are also gone) degrades with them; nothing
/// 500s, and everything fills in after the next session migrates the file.
#[test]
fn a_context_db_older_than_v8_degrades_to_empty_ledger_sections() {
    let (workspace, _, _) = real_context_workspace();
    {
        let raw = rusqlite::Connection::open(workspace.path().join(".stella/private/context.db"))
            .expect("open");
        // Rebuild the pre-v8 shape honestly: no ledger table, no lineage
        // columns on `episode`. Dropping the whole table also drops its
        // append-only triggers, exactly as a pre-v8 file never had them.
        raw.execute_batch(
            "DROP TABLE context_records;
             DROP INDEX IF EXISTS idx_episode_lineage;
             ALTER TABLE episode DROP COLUMN lineage_id;
             ALTER TABLE episode DROP COLUMN superseded_at;
             PRAGMA user_version = 7;",
        )
        .expect("roll the schema back");
    }

    let response = respond(workspace.path(), "/api/context-lifecycle");
    assert_eq!(
        response.status,
        "200 OK",
        "a pre-v8 context.db must degrade, not 500 — body: {}",
        String::from_utf8_lossy(&response.body)
    );
    let v: serde_json::Value = serde_json::from_slice(&response.body).expect("json");
    assert_eq!(v["present"], true, "the file exists and is reported: {v}");
    for key in [
        "counts",
        "proposals",
        "events",
        "episodes",
        "selection_health",
    ] {
        assert_eq!(v[key], serde_json::json!([]), "{key} must be empty: {v}");
    }
}

/// The context-records routes against a **real-migration** `context.db`: a
/// memory written through `ContextStore::upsert` — which mints the `nod_…`
/// mirror node, the `memory` row it points at, and its domain rows — then a
/// `context_use` naming that node, exactly as `stella`'s own extractor
/// writes one. The routes must find the node by the ledger's id and read its
/// words, its memory row and its domains back.
///
/// This is the half `tests/context_records.rs`'s hand-written DDL cannot
/// prove: that `node`, `memory`, `domain` and `node_domains` still have the
/// columns this crate's SQL names. A rename in `stella-context` fails here,
/// at `cargo test`, rather than as an empty panel on a user's dashboard —
/// which is what `is_missing_schema`'s degradation would otherwise turn it
/// into.
#[test]
fn context_records_join_a_real_memory_node() {
    let (workspace, _, _) = real_context_workspace();
    let store = ContextStore::open(workspace.path().join(".stella/private/context.db"))
        .expect("reopen through the real migrations");
    let receipt = tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(async {
            store
                .upsert(
                    ContextDelta::new().with_memory(
                        MemoryInput::new(
                            MemoryKind::Reflection,
                            "Run the cheap control before any bisect. One trial settles it.",
                        )
                        .with_domains(["agent-engine"]),
                    ),
                )
                .await
                .expect("memory upsert")
        });
    let node_id = receipt.memory_node_ids[0].clone();
    assert!(node_id.starts_with("nod_"), "{node_id}");

    let use_body = serde_json::json!({
        "use_kind": "rendered",
        "context_record_id": node_id,
        "use_trace_id": "ut_1",
        "task_id": "session:ses-1",
        "influence_stage": "none",
        "observed_at": "2026-09-01T10:00:00Z",
    })
    .to_string();
    append_lifecycle(
        &store,
        "context_use",
        "cu_1",
        "cu_1",
        "sha256:cu_1",
        "1.0-draft",
        &use_body,
        "2026-09-01T10:00:00Z",
    );

    let body = respond(workspace.path(), "/api/context-records").body;
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert!(v.get("error").is_none(), "{v}");
    let row = v["records"]
        .as_array()
        .expect("records")
        .iter()
        .find(|r| r["id"] == node_id.as_str())
        .unwrap_or_else(|| panic!("the used node is listed: {v}"));
    assert_eq!(row["plane"], "recall", "{row}");
    assert_eq!(row["kind"], "memory", "{row}");
    assert_eq!(
        row["title"], "Run the cheap control before any bisect",
        "{row}"
    );
    assert_eq!(row["health"]["uses"], 1, "{row}");
    assert_eq!(row["standing"], "unassessed", "{row}");

    let body = respond(
        workspace.path(),
        &format!("/api/context-record?id={node_id}"),
    )
    .body;
    let d: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(d["found"], true, "{d}");
    assert_eq!(
        d["record"]["domains"],
        serde_json::json!(["agent-engine"]),
        "domains come from node_domains ⋈ domain: {d}"
    );
    assert_eq!(d["source"]["kind"], "recall", "{d}");
    assert_eq!(
        d["source"]["memory"]["kind"], "reflection",
        "the memory row behind the mirror node: {d}"
    );
    assert_eq!(d["uses"][0]["task_id"], "session:ses-1", "{d}");
}
