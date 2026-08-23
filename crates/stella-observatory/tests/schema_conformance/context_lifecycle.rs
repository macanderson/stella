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
