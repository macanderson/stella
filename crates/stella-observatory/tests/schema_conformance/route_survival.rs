//! Every `/api/*` route survives the real migrated schema (#4486 split of `../schema_conformance.rs`).
//!
//! Both the current schema and (#3396) a store one migration behind.
use super::*;

/// The gate itself: every route, against a real-migration database.
#[test]
fn every_route_survives_the_real_store_schema() {
    let workspace = real_store_workspace();
    let root: &Path = workspace.path();

    for (route, seeded_pointer) in ROUTES {
        let response = respond(root, route);
        assert_eq!(
            response.status,
            "200 OK",
            "{route} did not answer 200 — body: {}",
            String::from_utf8_lossy(&response.body)
        );
        let body: serde_json::Value =
            serde_json::from_slice(&response.body).unwrap_or_else(|e| panic!("{route}: {e}"));
        assert!(
            body.get("error").is_none(),
            "{route} returned an error payload: {body}"
        );
        let Some(pointer) = seeded_pointer else {
            continue;
        };
        assert!(
            body.pointer(pointer).is_some(),
            "{route} lost its seeded data at {pointer} — a column this crate reads \
             was very likely renamed or dropped in stella-store. Body: {body}"
        );
    }
}

/// **The other half of the drift problem, and the one that actually bit.**
///
/// The gate above proves the queries work against a store migrated to *this*
/// build's schema. But this crate opens every file `SQLITE_OPEN_READ_ONLY`
/// and therefore never runs migrations — so it is routinely pointed at a
/// store several versions *behind* the binary reading it. Upgrade `stella`,
/// open the dashboard before running a single turn, and nothing has yet had
/// any reason to open that file read-write.
///
/// Found by pointing this crate at an untouched copy of a real 54 MB store:
/// every route referencing the v18 `tool_calls.state` column 500'd. A missing
/// *table* had always degraded to an empty payload; a missing *column* did
/// not, because `rusqlite` reports the two through different error variants
/// (`SqliteFailure` vs `SqlInputError`) and only the first was matched.
///
/// So this drives every route against a database deliberately rolled back to
/// the pre-v18 shape. None may error: an older store renders with a section
/// empty, and fills in the moment a turn migrates it.
#[test]
fn every_route_survives_a_store_older_than_this_build() {
    let workspace = real_store_workspace();
    let db = workspace.path().join(".stella/private/store.db");
    {
        let raw = rusqlite::Connection::open(&db).expect("open");
        // Rebuild `tool_calls` without `state` — exactly the shape a v17
        // binary left behind. Dropping the column is the honest simulation;
        // stubbing the queries would test the stub.
        raw.execute_batch(
            // The by-state index has to go first — it references the column,
            // and SQLite refuses to leave an index pointing at nothing.
            "DROP INDEX IF EXISTS tool_calls_by_state;
             ALTER TABLE tool_calls DROP COLUMN state;
             PRAGMA user_version = 17;",
        )
        .expect("roll the schema back");
    }

    for (route, _) in ROUTES {
        let response = respond(workspace.path(), route);
        assert_eq!(
            response.status,
            "200 OK",
            "{route} failed against a pre-v18 store — a read-only observer never migrates, \
             so it must degrade rather than 500. Body: {}",
            String::from_utf8_lossy(&response.body)
        );
    }

    // And the degradation is honest: the column is gone, so nothing is
    // claimed to be running rather than a number being invented.
    let cursor: serde_json::Value =
        serde_json::from_slice(&respond(workspace.path(), "/api/v1/cursor").body).expect("json");
    assert_eq!(cursor["tool_calls_running"], 0);
    assert!(
        cursor["events"].as_i64().unwrap_or(0) > 0,
        "the columns that DO exist still report: {cursor}"
    );
}
