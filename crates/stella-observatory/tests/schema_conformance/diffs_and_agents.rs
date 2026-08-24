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

/// **The witness for #4624.** A delegate's tool calls are attributed against
/// the real migrated schema: `tool_calls` carries the child's id, and
/// `/api/execution` serves it on both projections so a per-child page can
/// filter without a second round trip.
///
/// Fails before this change: `ToolStart`/`ToolResult` carried no
/// `sub_agent_id`, `tool_calls` had no column for one, and the steps and tools
/// SELECTs served neither — so a child's rows sat under the parent execution
/// id indistinguishable from the lead's own, and "which tools did child X run"
/// had no answer at all.
///
/// The fixture journals the child's pair AFTER its `Finished` bracket on
/// purpose: independent delegates are dispatched concurrently, so bracket
/// order attributes nothing and the stamp is the whole mechanism.
#[test]
fn a_delegates_tool_calls_are_attributed_to_it_against_the_real_schema() {
    let workspace = real_store_workspace();
    let out: serde_json::Value =
        serde_json::from_slice(&respond(workspace.path(), "/api/execution?id=1").body)
            .expect("json");

    let tools = out["tools"].as_array().expect("tools");
    let owners: Vec<(&str, Option<&str>)> = tools
        .iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap_or("?"),
                t["sub_agent_id"].as_str(),
            )
        })
        .collect();
    assert_eq!(
        owners,
        vec![("read_file", None), ("search", Some("search-1"))],
        "NULL is the lead's own call, and the delegate's names itself: {out}"
    );

    // The metering side of the same question is served on the same payload,
    // so one page can join spend to activity per child (#4383 + #4624).
    let steps = out["steps"].as_array().expect("steps");
    assert!(
        steps
            .iter()
            .any(|s| s["sub_agent_id"].as_str() == Some("search-1")),
        "the delegate's metered call is attributed too: {out}"
    );
    assert!(
        steps.iter().any(|s| s["sub_agent_id"].is_null()),
        "and the lead's own reads null rather than being omitted: {out}"
    );
}

/// **The witness for #4627.** The transcript itself places the bracket where
/// it happened, against the real migrated schema.
///
/// Fails before this change: `sub_agent` was absent from the journal route's
/// event-type allowlist, so `/api/execution-journal` returned neither edge and
/// a turn that fanned out delegates read as the parent doing everything
/// itself, with an unexplained wall-clock gap between two tool rows. The
/// sub-agents panel above lists the same children, but a list beside the
/// timeline cannot say *when* in the turn each one ran.
#[test]
fn the_transcript_journal_places_the_sub_agent_bracket_where_it_happened() {
    let workspace = real_store_workspace();
    let journal: serde_json::Value =
        serde_json::from_slice(&respond(workspace.path(), "/api/execution-journal?id=1").body)
            .expect("json");
    let brackets: Vec<&serde_json::Value> = journal
        .as_array()
        .expect("journal")
        .iter()
        .filter(|e| e["type"] == "sub_agent")
        .collect();
    assert_eq!(brackets.len(), 2, "both edges, in order: {journal}");

    let started = brackets[0];
    assert_eq!(started["phase"], "started", "{started}");
    assert_eq!(started["agent_id"], "search-1", "{started}");
    assert_eq!(
        started["instruction_preview"], "find the retry policy",
        "{started}"
    );
    assert_eq!(started["effort"], "high", "{started}");
    assert_eq!(started["budget_usd"], 0.25, "{started}");
    assert_eq!(started["depth"], 1, "{started}");

    let finished = brackets[1];
    assert_eq!(finished["phase"], "finished", "{finished}");
    assert_eq!(finished["status"], "completed", "{finished}");
    assert_eq!(finished["cost_usd"], 0.004, "{finished}");
    assert_eq!(finished["steps"], 1, "{finished}");
    assert_eq!(finished["absorbed_messages"], 5, "{finished}");
    // The child's report is the one piece of its transcript that crosses the
    // boundary, and it arrives as this row's body.
    assert_eq!(
        finished["body"], "retry policy lives in retry.rs",
        "{finished}"
    );
    // Two clips, two keys: `report_truncated` is the child's own clamp to the
    // spec's character cap, `truncated` is this transport's body clip.
    assert_eq!(finished["report_truncated"], false, "{finished}");
    assert_eq!(finished["truncated"], false, "{finished}");
}
