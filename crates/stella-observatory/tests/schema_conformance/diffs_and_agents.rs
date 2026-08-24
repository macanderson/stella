//! Context/session-turn diffs, and the agents panel's writer split (#4486 split of `../schema_conformance.rs`).
use super::*;

/// The #1511 witness: two calls of the same role whose system prefixes differ
/// by one line produce exactly one hunk naming that line, and a
/// byte-identical pair reports `changed: false`. Fails before the route
/// exists (404), and fails if the baseline search stops at the execution
/// boundary — the drift here is *across* turns, which is the only place a
/// byte-stable system prompt can drift.
#[test]
fn context_diff_names_the_moved_line_and_reports_identity_honestly() {
    let workspace = real_store_workspace();
    let root: &Path = workspace.path();

    // Across the turn boundary: execution 2's first worker call against
    // execution 1's — the same role, one line of drift, system scope.
    let body = respond(
        root,
        "/api/execution-context-diff?id=2&turn=0&step=1&call_seq=0&base=prev&only=system",
    )
    .body;
    let diff: serde_json::Value = serde_json::from_slice(&body).expect("diff json");
    assert_eq!(diff["found"], true, "{diff}");
    assert_eq!(diff["changed"], true, "{diff}");
    assert_eq!(diff["base"], "prev", "{diff}");
    assert_eq!(diff["minimal"], true, "{diff}");
    assert_eq!(diff["added"], 1, "one inserted line, one addition: {diff}");
    let hunks = diff["hunks"].as_array().expect("hunks");
    assert_eq!(hunks.len(), 1, "one contiguous change, one hunk: {diff}");
    let added: Vec<&str> = hunks[0]["lines"]
        .as_array()
        .expect("lines")
        .iter()
        .filter(|l| l["op"] == "add")
        .filter_map(|l| l["text"].as_str())
        .collect();
    assert_eq!(added, vec![DRIFT_LINE], "the diff names the moved line");
    assert!(
        diff["base_label"]
            .as_str()
            .unwrap_or_default()
            .contains("execution 1"),
        "a cross-turn baseline is unmistakable: {diff}"
    );

    // Inside the turn: step 2 against step 1, byte-identical by construction.
    let body = respond(
        root,
        "/api/execution-context-diff?id=2&turn=0&step=2&call_seq=0&base=prev&only=system",
    )
    .body;
    let same: serde_json::Value = serde_json::from_slice(&body).expect("diff json");
    assert_eq!(
        same["changed"], false,
        "byte-identical is not a change: {same}"
    );
    assert_eq!(same["base"], "prev", "{same}");
    assert!(
        !same["base_label"]
            .as_str()
            .unwrap_or_default()
            .contains("execution"),
        "a same-turn baseline stays terse: {same}"
    );

    // A role's first call has no predecessor: the resolved base is reported
    // as `prompt`, never silently claimed to be `prev`.
    let body = respond(
        root,
        "/api/execution-context-diff?id=1&turn=0&step=1&call_seq=0&base=prev",
    )
    .body;
    let first: serde_json::Value = serde_json::from_slice(&body).expect("diff json");
    assert_eq!(
        first["base"], "prompt",
        "prev on a first call resolves: {first}"
    );
    assert_eq!(first["base_label"], "prompt as submitted", "{first}");
}

/// The #1870 observatory witness: a recorded turn diff replays through the
/// route with its hunks naming the changed lines, the session view stamps
/// the turn with its journal ordinal (the only persisted join between the
/// two numbering schemes), and an unrecorded turn answers `found: false`
/// with the full key set. Fails on main — the route is absent.
#[test]
fn session_turn_diff_replays_the_recorded_hunks() {
    let workspace = real_store_workspace();
    let root: &Path = workspace.path();

    let body = respond(
        root,
        "/api/session-turn-diff?id=ses-1700000000000-424242&turn=1",
    )
    .body;
    let v: serde_json::Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(v["found"], true, "{v}");
    assert_eq!(v["execution_id"], 1, "the store-side turn identity: {v}");
    assert_eq!(v["files"][0]["path"], "src/lib.rs", "{v}");
    let lines = v["files"][0]["hunks"][0]["lines"]
        .as_array()
        .expect("lines");
    let removed: Vec<&str> = lines
        .iter()
        .filter(|l| l["op"] == "remove")
        .filter_map(|l| l["text"].as_str())
        .collect();
    let added: Vec<&str> = lines
        .iter()
        .filter(|l| l["op"] == "add")
        .filter_map(|l| l["text"].as_str())
        .collect();
    assert_eq!(removed, vec!["two"], "the hunks name the old line: {v}");
    assert_eq!(added, vec!["TWO"], "and the new one: {v}");

    // The session view carries the join: the seeded execution's turn row is
    // stamped with the journal ordinal its diff was recorded under.
    let session: serde_json::Value =
        serde_json::from_slice(&respond(root, "/api/session?id=ses-1700000000000-424242").body)
            .expect("json");
    let stamped = session["turns"]
        .as_array()
        .expect("turns")
        .iter()
        .find(|t| t["id"] == 1)
        .expect("the seeded execution's turn row");
    assert_eq!(
        stamped["journal_turn"], 1,
        "the page can only offer a diff through this stamp: {session}"
    );

    // Missing is a state: an unrecorded turn, and a store without the v21
    // table at all, both answer the full (empty) shape.
    let absent: serde_json::Value = serde_json::from_slice(
        &respond(
            root,
            "/api/session-turn-diff?id=ses-1700000000000-424242&turn=99",
        )
        .body,
    )
    .expect("json");
    assert_eq!(absent["found"], false, "{absent}");
    assert_eq!(absent["files"], serde_json::json!([]), "{absent}");
}

/// The v30 `agent_uses.kind` column resolves against the real migrated schema,
/// and the two writers arrive apart (#3822). The panel's grouping rule itself
/// is witnessed in the crate's own tests; this is the schema-drift gate over
/// it.
#[test]
fn the_agents_panel_separates_the_two_writers_against_the_real_schema() {
    let workspace = real_store_workspace();
    let session: serde_json::Value = serde_json::from_slice(
        &respond(workspace.path(), "/api/session?id=ses-1700000000000-424242").body,
    )
    .expect("json");
    let kinds: Vec<&str> = session["agents"]
        .as_array()
        .expect("agents panel")
        .iter()
        .filter_map(|a| a["kind"].as_str())
        .collect();
    assert!(kinds.contains(&"definition"), "{session}");
    assert!(kinds.contains(&"delegation"), "{session}");
}

/// The turn page's sub-agents fold against the real migrated schema: the
/// `sub_agent` bracket (task, effort, status, summary, both timestamps)
/// joined with the v33 `telemetry.sub_agent_id` metering rows (model, API
/// provider, tokens). The fold's own rules are witnessed in the crate's unit
/// tests; this is the schema-drift gate over both reads.
#[test]
fn the_turn_page_folds_a_delegates_bracket_and_metering_against_the_real_schema() {
    let workspace = real_store_workspace();
    let out: serde_json::Value =
        serde_json::from_slice(&respond(workspace.path(), "/api/execution-subagents?id=1").body)
            .expect("json");
    let agents = out["agents"].as_array().expect("agents");
    assert_eq!(agents.len(), 1, "{out}");
    let a = &agents[0];
    assert_eq!(a["agent_id"], "search-1", "{out}");
    assert_eq!(a["instruction_preview"], "find the retry policy", "{out}");
    assert_eq!(a["effort"], "high", "{out}");
    assert_eq!(a["status"], "completed", "{out}");
    assert_eq!(a["summary"], "retry policy lives in retry.rs", "{out}");
    assert_eq!(a["provider"], "zai", "{out}");
    assert_eq!(a["model"], "glm-5.2", "{out}");
    assert_eq!(a["calls"], 1, "{out}");
    assert_eq!(a["tokens_in"], 300, "{out}");
    assert!(a["started_ts"].is_string(), "{out}");
    assert!(a["finished_ts"].is_string(), "{out}");
}
