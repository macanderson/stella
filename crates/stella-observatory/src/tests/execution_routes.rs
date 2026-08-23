//! The /api/execution* routes: detail, journal replay and the rebuilt
//! context a single call was sent. Split out of the parent module, which
//! sat at exactly the 1500-line ratchet with no baseline entry.

use super::*;

#[test]
fn execution_detail_includes_steps_tools_files() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/api/execution?id=1");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(v["id"], 1);
    assert_eq!(v["steps"].as_array().unwrap().len(), 1);
    assert_eq!(v["tools"].as_array().unwrap().len(), 2);
    assert_eq!(v["files"][0]["path"], "src/lib.rs");
    assert_eq!(v["reflection"]["self_rating"], 8);
    let none = respond(ws.path(), "/api/execution?id=2");
    let v: serde_json::Value = serde_json::from_slice(&none.body).unwrap();
    assert_eq!(
        v["reflection"],
        serde_json::Value::Null,
        "unreflected runs stay null"
    );
}

/// The execution detail route names the session that owns the turn.
///
/// The transcript is a page with an address (`#transcript/<execution>`), so it
/// is routinely reached by reload or by a pasted link, with no session list in
/// memory to have come from. Without this field such an arrival can say
/// neither which session the turn belongs to nor which turns sit either side
/// of it, and the page's breadcrumb and prev/next controls are dead. NULL is a
/// real answer — a run recorded before schema v8 stamped `session_id` has no
/// session — and must survive as `null` rather than as a missing key, because
/// the page distinguishes "no session" from "field not served".
#[test]
fn execution_detail_carries_its_session_id() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/api/execution?id=1");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(v["session_id"], "ses-1");

    let unstamped = respond(ws.path(), "/api/execution?id=2");
    let v: serde_json::Value = serde_json::from_slice(&unstamped.body).unwrap();
    assert!(
        v.get("session_id").is_some(),
        "the key must be served even when the value is null"
    );
    assert_eq!(v["session_id"], serde_json::Value::Null);
}

/// The transcript replay (#1461): the journal route folds an execution's
/// `events` slice into an ordered transcript, drops `text_delta` fragments
/// (the `text` event is the authoritative answer), and surfaces tool
/// arguments and outputs — the content the telemetry projections
/// deliberately do not carry.
#[test]
fn execution_journal_replays_transcript_without_deltas() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/api/execution-journal?id=1");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let types: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        [
            "stage",
            "reasoning",
            "tool_start",
            "tool_result",
            "text",
            "text",
            "file_change",
            "step_usage"
        ],
        "seq order, text_delta excluded"
    );
    assert_eq!(v[0]["label"], "build");
    assert_eq!(v[1]["body"], "plan the edit");
    assert_eq!(v[2]["name"], "read_file");
    assert!(v[2]["body"].as_str().unwrap().contains("src/lib.rs"));
    assert_eq!(v[3]["ok"], true);
    assert_eq!(v[3]["body"], "fn a() {}");
    assert_eq!(v[3]["duration_ms"], 12);
    // Seq 5 is the pre-#1886 spelling (`delta`), seq 6 the current one
    // (`text`) — the transcript reads both generations of journal row.
    assert_eq!(v[4]["body"], "added the function");
    assert_eq!(v[4]["truncated"], false);
    assert_eq!(v[5]["body"], "and named it well");
    // No execution 2 events were seeded — an empty transcript, not an error.
    let none = respond(ws.path(), "/api/execution-journal?id=2");
    let v: serde_json::Value = serde_json::from_slice(&none.body).unwrap();
    assert_eq!(v, serde_json::json!([]));
}

/// A streamed run of `reasoning` fragments is one block of thought, and the
/// journal route must serve it as one entry.
///
/// The defect this pins: `AgentEvent::Reasoning` carries a *delta*, so the
/// store holds one row per fragment and the transcript drew a separate titled,
/// boxed fold around each — `Let`, `me look`, `at the issue. I` — on a real
/// execution 39,778 entries long. Three properties make the merge safe rather
/// than merely tidier, and each is asserted here because each has its own way
/// of going wrong:
///
/// - the fragments concatenate **in order and without a joiner**, since the
///   provider already split mid-word;
/// - the merged entry carries the run's **last** `seq`, because the page sends
///   that back as `after_seq` and a first-seq cursor would re-fetch the run on
///   every poll, forever;
/// - a tool call **between** two runs keeps them apart, because that boundary
///   is a fact about the turn rather than a rendering artifact.
#[test]
fn streamed_reasoning_fragments_fold_into_one_block_per_run() {
    let ws = seeded_workspace();
    // Execution 2 carries no seeded events, so this run is the whole journal.
    // Two runs of fragments with a tool call between them — the shape a real
    // turn writes, mid-word splits included.
    let conn = Connection::open(ws.path().join(".stella/private/store.db")).unwrap();
    conn.execute_batch(
        r#"INSERT INTO events (execution_id, seq, event_type, payload) VALUES
             (2, 0, 'reasoning', '{"type":"reasoning","delta":"Let"}'),
             (2, 1, 'reasoning', '{"type":"reasoning","delta":" me look"}'),
             (2, 2, 'reasoning', '{"type":"reasoning","delta":" at the issue."}'),
             (2, 3, 'tool_start', '{"type":"tool_start","call":{"call_id":"c9","name":"read_file","input":{"path":"a.rs"}}}'),
             (2, 4, 'tool_result', '{"type":"tool_result","call_id":"c9","output":{"ok":{"content":"fn a() {}"}},"duration_ms":3,"speculated":false}'),
             (2, 5, 'reasoning', '{"type":"reasoning","delta":"Now"}'),
             (2, 6, 'reasoning', '{"type":"reasoning","delta":" verify it."}');"#,
    )
    .unwrap();
    drop(conn);

    let response = respond(ws.path(), "/api/execution-journal?id=2");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let entries = v.as_array().unwrap();

    let types: Vec<&str> = entries
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(
        types,
        ["reasoning", "tool_start", "tool_result", "reasoning"],
        "seven rows must serve as four entries, not seven:\n{v:#}"
    );

    let reasoning: Vec<&serde_json::Value> = entries
        .iter()
        .filter(|e| e["type"] == "reasoning")
        .collect();
    assert_eq!(
        reasoning[0]["body"], "Let me look at the issue.",
        "fragments join in order, verbatim, with no inserted joiner"
    );
    assert_eq!(
        reasoning[1]["body"], "Now verify it.",
        "a tool call between two runs must not fuse them"
    );
    assert_eq!(
        reasoning[0]["seq"], 2,
        "the cursor must be the run's LAST seq or the poll re-fetches it"
    );
    assert_eq!(reasoning[1]["seq"], 6);

    // And the incremental poll the page actually makes: asking after the
    // first run's cursor returns the rest exactly once.
    let response = respond(ws.path(), "/api/execution-journal?id=2&after_seq=2");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let types: Vec<&str> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["type"].as_str().unwrap())
        .collect();
    assert_eq!(types, ["tool_start", "tool_result", "reasoning"]);
}

/// `/api/transcript-html` renders the journal through `stella-transcript` —
/// the same code the TUI draws from — rather than through the page's own
/// JavaScript, as a fragment the turn page embeds in a shadow root.
///
/// The assertions are about the *structural* fixes, not about pixels: a tool
/// call and its result are one node, and the call's command is stated once. A
/// golden of the markup would break on every styling change and prove neither.
#[test]
fn transcript_route_renders_one_node_per_call_stating_the_command_once() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/api/transcript-html?id=1");
    assert_eq!(response.status, "200 OK");
    assert_eq!(response.content_type, "text/html; charset=utf-8");
    let html = String::from_utf8(response.body).unwrap();

    // A fragment for embedding, not a standalone document: the styles live
    // at /assets/transcript.css and the host page injects them.
    assert!(html.starts_with("<div class=\"frame\""), "{html}");
    assert!(!html.contains("<!DOCTYPE html>"));

    // One step, carrying both halves of the call.
    assert_eq!(
        html.matches("class=\"step").count(),
        1,
        "the call and its result did not fold into one node:\n{html}"
    );
    assert!(html.contains("read_file"));
    assert!(html.contains("fn a() {}"), "the result body is present");

    // The path is the header object, and it is stated once.
    assert_eq!(
        html.matches("src/lib.rs").count(),
        1,
        "the invocation was repeated:\n{html}"
    );

    // The reasoning and the answer became prose and answer blocks.
    assert!(html.contains("plan the edit"));
    assert!(html.contains("and named it well"));
}

/// The seeded `step_usage` event becomes a metering note: the per-call audit
/// row (provider, model, tokens, cache traffic, latency) with the inspect
/// control the turn page wires to its prompt inspector, anchored by
/// (step, role).
#[test]
fn transcript_fragment_carries_a_metering_row_with_an_inspect_anchor() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/api/transcript-html?id=1");
    let html = String::from_utf8(response.body).unwrap();

    assert!(html.contains("note-meter"), "no metering note:\n{html}");
    assert!(
        html.contains("step 1 · worker · zai · glm-5.2"),
        "the metering summary does not lead with the binding:\n{html}"
    );
    assert!(
        html.contains("class=\"inspect\" data-step=\"1\" data-role=\"worker\""),
        "the inspect control lost its anchor:\n{html}"
    );
    // The fold detail carries the cache split — the figures a cache
    // investigation needs per call, not per turn.
    assert!(html.contains("29.1k from prompt cache"), "{html}");
    assert!(html.contains("1.2k written to cache"), "{html}");
}

/// The transcript stylesheet is served as an asset, byte-identical to the
/// renderer crate's one copy, so the embedding page and a test can both hold
/// it to the same contract.
#[test]
fn transcript_stylesheet_is_served_as_an_asset() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/assets/transcript.css");
    assert_eq!(response.status, "200 OK");
    assert_eq!(response.content_type, "text/css; charset=utf-8");
    let css = String::from_utf8(response.body).unwrap();
    assert_eq!(css, stella_transcript::html::STYLE);
    assert!(css.contains("--del-word"));
}

/// The standalone `/transcript` page is consolidated into the turn page —
/// gone, not redirected: the fragment route is an implementation detail of
/// the dashboard, and the page-level address it replaced answered the same
/// question in a second rendering.
#[test]
fn the_standalone_transcript_page_is_gone() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/transcript?id=1");
    assert_eq!(response.status, "404 Not Found");
}

/// An execution with no events is an empty transcript, not a 500.
#[test]
fn transcript_route_renders_an_execution_with_no_events() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/api/transcript-html?id=2");
    assert_eq!(response.status, "200 OK");
    let html = String::from_utf8(response.body).unwrap();
    assert!(!html.contains("class=\"step"));
}

/// A missing `?id=` is the caller's error, and says which parameter is absent.
#[test]
fn transcript_route_names_the_parameter_it_is_missing() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/api/transcript-html");
    assert_eq!(response.status, "400 Bad Request");
    let body = String::from_utf8(response.body).unwrap();
    assert!(body.contains("id"), "{body}");
}

/// **The #4538 witness.** `execution_reflection.partial_run` (schema v32,
/// #3808) reaches the panel: a cancelled run whose only reflection row came
/// from finalize — no self-review to gate on — must render the flag instead
/// of `reflection: null`, and a graded reflection must carry the flag beside
/// its grade, so the panel can say "this covers a partial run" either way.
///
/// The shared fixture predates the column on purpose (every other test in
/// this file is the pre-v32 degrade witness: the flag is silently absent and
/// nothing else changes); this one migrates its own copy forward.
#[test]
fn a_cancelled_runs_reflection_reads_partial_run_not_null() {
    let ws = seeded_workspace();
    let conn = Connection::open(ws.path().join(".stella/private/store.db")).unwrap();
    conn.execute_batch(
        "ALTER TABLE execution_reflection
           ADD COLUMN partial_run INTEGER NOT NULL DEFAULT 0;
         INSERT INTO executions
           (kind, prompt, provider, model, outcome, session_id, cost_usd)
         VALUES ('run', 'refactor the parser', 'zai', 'glm-5.2',
                 'cancelled', 'ses-1', 0.01);
         INSERT INTO execution_reflection (execution_id, partial_run)
         VALUES (3, 1);",
    )
    .unwrap();
    drop(conn);

    let cancelled = respond(ws.path(), "/api/execution?id=3");
    let v: serde_json::Value = serde_json::from_slice(&cancelled.body).unwrap();
    assert_eq!(
        v["reflection"],
        serde_json::json!({ "partial_run": true }),
        "a finalize-only row on a cancelled run is a reason, not a null: {v}"
    );

    let graded = respond(ws.path(), "/api/execution?id=1");
    let v: serde_json::Value = serde_json::from_slice(&graded.body).unwrap();
    assert_eq!(v["reflection"]["self_rating"], 8);
    assert_eq!(
        v["reflection"]["partial_run"], false,
        "a graded full-run reflection states the flag rather than omitting it: {v}"
    );
}

/// `/api/model-card` names its missing parameters; the card itself is read
/// from the user-tier catalog, which a seeded workspace deliberately does not
/// fabricate.
#[test]
fn model_card_route_names_its_missing_parameters() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/api/model-card?provider=zai");
    assert_eq!(response.status, "400 Bad Request");
    let body = String::from_utf8(response.body).unwrap();
    assert!(body.contains("slug"), "{body}");
}

/// `after_seq` (#1476) narrows the transcript to rows newer than the
/// drawer's last-seen `seq` — the incremental fetch a still-running
/// execution's poll uses instead of re-downloading the whole transcript
/// every tick.
#[test]
fn execution_journal_after_seq_returns_only_newer_rows() {
    let ws = seeded_workspace();
    // Seq 3 is the tool_start row; only tool_result (4), the two text rows
    // (5, 6), the file_change (7) and the step_usage metering row (8) — all
    // > 3 — should come back.
    let response = respond(ws.path(), "/api/execution-journal?id=1&after_seq=3");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let seqs: Vec<i64> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].as_i64().unwrap())
        .collect();
    assert_eq!(seqs, [4, 5, 6, 7, 8], "only rows with seq > 3 come back");
    // A cursor past every seeded row degrades to empty, not an error.
    let none = respond(ws.path(), "/api/execution-journal?id=1&after_seq=99");
    let v: serde_json::Value = serde_json::from_slice(&none.body).unwrap();
    assert_eq!(v, serde_json::json!([]));
    // Unset behaves exactly like `after_seq=0` would be wrong to: seq 0 (the
    // opening `stage` row) is still included.
    let all = respond(ws.path(), "/api/execution-journal?id=1&after_seq=-1");
    let v: serde_json::Value = serde_json::from_slice(&all.body).unwrap();
    assert_eq!(
        v.as_array().unwrap().len(),
        8,
        "seq 0 survives after_seq=-1"
    );
}

/// A tool result names the tool that produced it.
///
/// `ToolResult` is `{call_id, output, duration_ms, speculated}` — the name
/// lives on the `ToolStart` that opened the call, so a result row can only be
/// labelled by correlating back to it. Until it was, every result row in the
/// dashboard read `✓ result`, and a reader scrolling a fan-out of parallel
/// calls could not tell which row answered which.
#[test]
fn execution_journal_labels_a_result_with_its_tools_name() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/api/execution-journal?id=1");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let result = v
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "tool_result")
        .expect("the fixture seeds a tool_result");
    assert_eq!(result["name"], "read_file");
    assert_eq!(result["ok"], true);
    // The body is the decoded `ok.content`, never the tagged wrapper.
    assert_eq!(result["body"], "fn a() {}");
}

/// The name survives the incremental poll, which is where a batch-local index
/// would fail.
///
/// A live transcript polls with `after_seq` set to the highest row it already
/// has, so a `tool_result` routinely arrives in a page that does not contain
/// the `tool_start` it answers. Resolving names only within the returned rows
/// would label the first fetch and silently stop labelling every later one —
/// the failure mode that looks exactly like the feature working.
#[test]
fn a_result_polled_without_its_call_is_still_named() {
    let ws = seeded_workspace();
    // Seq 3 is the `tool_start`; asking for rows after it excludes the call
    // from the batch entirely.
    let response = respond(ws.path(), "/api/execution-journal?id=1&after_seq=3");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let batch = v.as_array().unwrap();
    assert!(
        !batch.iter().any(|e| e["type"] == "tool_start"),
        "the call must not be in this batch, or the test proves nothing"
    );
    let result = batch
        .iter()
        .find(|e| e["type"] == "tool_result")
        .expect("the result is in this batch");
    assert_eq!(result["name"], "read_file");
}

/// A body past [`crate::journal::JOURNAL_BODY_CLIP`] chars is clipped and flagged in the
/// default payload, and returned whole under `?full=1` — the elision must
/// announce itself, never pose as the data.
#[test]
fn execution_journal_clips_long_bodies_unless_full() {
    use crate::journal::JOURNAL_BODY_CLIP;
    let ws = seeded_workspace();
    let long = "x".repeat(JOURNAL_BODY_CLIP + 100);
    Connection::open(ws.path().join(".stella/private/store.db"))
        .unwrap()
        .execute(
            "INSERT INTO events (execution_id, seq, event_type, payload)
             VALUES (2, 0, 'text', ?1)",
            [serde_json::json!({"type": "text", "delta": long}).to_string()],
        )
        .unwrap();
    let response = respond(ws.path(), "/api/execution-journal?id=2");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(v[0]["truncated"], true);
    assert!(v[0]["body"].as_str().unwrap().chars().count() <= JOURNAL_BODY_CLIP + 1);
    let full = respond(ws.path(), "/api/execution-journal?id=2&full=1");
    let v: serde_json::Value = serde_json::from_slice(&full.body).unwrap();
    assert_eq!(v[0]["truncated"], false);
    assert_eq!(v[0]["body"].as_str().unwrap().chars().count(), long.len());
}

/// The witness for sent-context inspection (#1475): the reconstructed message
/// array carries the system prompt the receipt recorded, in wire order, and
/// says so verifiably.
///
/// This is the question the dashboard could not answer — the transcript shows
/// what a run *did*, never what a call was *sent* — and the system prompt is
/// the part of it that exists nowhere else in the store.
#[test]
fn execution_context_rebuilds_the_message_array_a_call_was_sent() {
    let ws = seeded_workspace();
    let response = respond(
        ws.path(),
        "/api/execution-context?id=1&turn=0&step=1&call_seq=0",
    );
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(v["receipts_recorded"], true, "{v}");
    // The index the drawer drills from: both calls that shared step 1.
    let calls = v["calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2, "{v}");
    assert_eq!(calls[0]["call_role"], "worker");
    assert_eq!(calls[0]["estimated_input_tokens"], 203);
    assert_eq!(calls[1]["call_role"], "overflow_summarizer");
    // Frame identity is absent for a receipt written with the lifecycle off,
    // and null must read as "not computed", never as "computed and empty".
    assert!(calls[0]["compiled_frame_id"].is_null());

    let context = &v["context"];
    assert_eq!(context["found"], true, "{v}");
    let messages = context["messages"].as_array().unwrap();
    let roles: Vec<&str> = messages
        .iter()
        .map(|m| m["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["system", "user", "assistant", "tool"], "{v}");
    assert_eq!(
        messages[0]["body"], "you are careful",
        "the system prompt the model actually saw"
    );
    assert_eq!(messages[1]["body"], "add a function");
    // One message, several blocks: the assistant text and the tool call it
    // carried regroup by `message_index`, in manifest order.
    assert_eq!(
        messages[2]["body"],
        "added the function\n→ tool_call c1 read_file {\"path\":\"src/lib.rs\"}"
    );
    assert_eq!(messages[3]["body"], "← tool_result c1\nfn a() {}");
    // Verified means the journal-resolved blocks re-hashed to the digests the
    // receipt recorded — a fact re-derived here, not a stored verdict.
    assert_eq!(context["verified"], true, "{v}");
    assert_eq!(context["unresolved"], serde_json::json!([]));
    assert_eq!(context["digest_mismatches"], serde_json::json!([]));
    // Nothing mismatched, so there is no severity to report — and the era
    // reads as the pre-rewrite one because this fixture's `executions` table
    // predates the column, which is exactly how a store written by an older
    // build behaves (#1981).
    assert_eq!(context["digest_mismatch_severity"], "none");
    assert_eq!(context["journal_era"], "compaction_unjournaled");
    // A gap block's preimage is stored locally, so its check is tautological
    // and is deliberately not reported as evidence.
    assert!(messages[0]["blocks"][0]["digest_verified"].is_null());
    assert_eq!(messages[3]["blocks"][0]["digest_verified"], true);
    assert_eq!(messages[3]["blocks"][0]["cache_zone"], "volatile");
    assert_eq!(messages[3]["blocks"][0]["token_cost"], 88);
}

/// A step can hold several model calls, and each was sent its own context.
/// Addressing one must never fold in the other's blocks.
#[test]
fn execution_context_is_scoped_to_one_call_seq_not_the_whole_step() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/api/execution-context?id=1&step=1&call_seq=1");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let messages = v["context"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1, "the summarizer's own manifest: {v}");
    assert_eq!(messages[0]["role"], "system");
    // Coordinates with no receipt are answered, not errored.
    let none = respond(ws.path(), "/api/execution-context?id=1&step=9");
    let v: serde_json::Value = serde_json::from_slice(&none.body).unwrap();
    assert_eq!(none.status, "200 OK");
    assert_eq!(v["context"]["found"], false, "{v}");
    assert!(
        v["context"]["note"].as_str().unwrap().contains("step 9"),
        "{v}"
    );
}

/// An execution recorded with `context.lifecycle` off has no receipts, and
/// that is a state with its own answer — the wording `stella inspect` uses —
/// not a 500 and not an empty array pretending nothing was sent.
#[test]
fn execution_context_without_receipts_says_so_instead_of_erroring() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/api/execution-context?id=2&step=1");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(response.status, "200 OK");
    assert_eq!(v["receipts_recorded"], false, "{v}");
    assert_eq!(v["calls"], serde_json::json!([]));
    assert!(v["context"].is_null());
    assert!(
        v["note"]
            .as_str()
            .unwrap()
            .contains("has no recorded receipts"),
        "{v}"
    );
}

/// A reconstructed message obeys the transcript route's clip policy: past
/// `journal::JOURNAL_BODY_CLIP` chars it is cut and flagged, and `?full=1` returns
/// the bytes. A system prompt is thousands of stable lines, so this is the
/// common case here rather than the edge one.
#[test]
fn execution_context_clips_long_messages_unless_full() {
    use crate::journal::JOURNAL_BODY_CLIP;
    let ws = seeded_workspace();
    let long = "x".repeat(JOURNAL_BODY_CLIP + 100);
    Connection::open(ws.path().join(".stella/private/store.db"))
        .unwrap()
        .execute(
            "UPDATE context_blocks SET content = ?1 WHERE block_id = 'blk_sys'",
            [long.as_str()],
        )
        .unwrap();
    let clipped = respond(ws.path(), "/api/execution-context?id=1&step=1");
    let v: serde_json::Value = serde_json::from_slice(&clipped.body).unwrap();
    assert_eq!(v["context"]["messages"][0]["truncated"], true, "{v}");
    let full = respond(ws.path(), "/api/execution-context?id=1&step=1&full=1");
    let v: serde_json::Value = serde_json::from_slice(&full.body).unwrap();
    assert_eq!(v["context"]["messages"][0]["truncated"], false);
    assert_eq!(
        v["context"]["messages"][0]["body"].as_str().unwrap().len(),
        long.len()
    );
}

/// A file change reaches the transcript **with its diff**, as hunks.
///
/// The transcript used to stop at "some files were touched" — the paths were
/// listed in a side panel with their line counts, and nothing on the page
/// said *what* changed, which is the question a transcript is opened to
/// answer. The deck and the plain surface have shown the diff inline since
/// #2421; this is the same row.
///
/// The event carries git's `-p` patch as TEXT (it is measured by shelling to
/// git at the turn boundary), and the page has exactly one diff renderer,
/// which draws hunks — so the projection parses. Fails on main: the route's
/// `event_type IN (…)` filter did not select `file_change` at all.
#[test]
fn execution_journal_carries_a_file_changes_diff_as_hunks() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/api/execution-journal?id=1");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let change = v
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["type"] == "file_change")
        .expect("the fixture seeds a file_change");
    assert_eq!(change["path"], "src/lib.rs");
    assert_eq!(change["kind"], "modified");
    // The counts ride the event from git's numstat. They are NOT recounted
    // from the diff, which is a capped rendering — re-deriving would report
    // the size of the view as the size of the change.
    assert_eq!(change["added"], 2);
    assert_eq!(change["removed"], 1);

    let hunks = change["hunks"].as_array().expect("hunks");
    assert_eq!(hunks.len(), 1, "{change}");
    assert_eq!(hunks[0]["old_start"], 3, "positioned in the file, not at 1");
    let ops: Vec<&str> = hunks[0]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["op"].as_str().unwrap())
        .collect();
    assert_eq!(ops, ["equal", "remove", "add", "add"]);
    assert_eq!(hunks[0]["lines"][1]["text"], "fn a() {}");
    // Nothing was elided here, so the fold field says so rather than naming
    // index 0 — which is a real position a renderer would draw a marker at.
    assert_eq!(change["elided"], 0);
    assert!(change["fold_before"].is_null(), "{change}");
}

/// A diff longer than the view's budget shows its beginning and its end, and
/// states the size of the middle it dropped.
///
/// Only the changed lines, never the file — and never more of them than
/// `stella_diff::view::VIEW_CAP`, because this payload is re-shipped to the
/// browser on every dashboard poll. `?full=1` lifts the cap, the same escape
/// hatch that lifts the body clip.
#[test]
fn a_long_file_change_diff_is_elided_from_the_middle_and_says_so() {
    let ws = seeded_workspace();
    let adds = stella_diff::view::VIEW_CAP * 2;
    let body: String = (1..=adds).map(|i| format!("+line {i}\n")).collect();
    let diff = format!("--- a/big.rs\n+++ b/big.rs\n@@ -0,0 +1,{adds} @@\n{body}");
    let payload = serde_json::json!({
        "type": "file_change",
        "path": "big.rs",
        "kind": "created",
        "added": adds,
        "removed": 0,
        "diff": diff,
    });
    let conn = rusqlite::Connection::open(ws.path().join(".stella/private/store.db")).unwrap();
    conn.execute(
        "INSERT INTO events (execution_id, seq, event_type, payload)
         VALUES (1, 9, 'file_change', ?1)",
        [payload.to_string()],
    )
    .unwrap();
    drop(conn);

    let response = respond(ws.path(), "/api/execution-journal?id=1&after_seq=8");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let change = &v.as_array().unwrap()[0];
    let hunks = change["hunks"].as_array().expect("hunks");
    let shown: usize = hunks
        .iter()
        .map(|h| h["lines"].as_array().map_or(0, Vec::len))
        .sum();
    assert!(
        shown <= stella_diff::view::VIEW_CAP,
        "{shown} lines shipped against a cap of {}",
        stella_diff::view::VIEW_CAP
    );
    assert_eq!(
        change["elided"],
        adds - shown,
        "the drop is counted: {shown}"
    );
    // Both ends survive, and the tail's header names the file's real lines
    // rather than restarting the gutter from 1.
    assert_eq!(hunks[0]["lines"][0]["text"], "line 1");
    let tail = hunks.last().unwrap();
    let last = tail["lines"].as_array().unwrap().last().unwrap();
    assert_eq!(last["text"], format!("line {adds}"));
    assert!(
        hunks.len() >= 2 && change["fold_before"].as_u64() == Some(1),
        "the marker belongs between the two ends: {change}"
    );
    assert_eq!(
        tail["new_start"].as_u64().map(|n| n as usize),
        Some(adds + 1 - tail["lines"].as_array().unwrap().len())
    );

    // `?full=1` is the reader who asked for everything.
    let full = respond(ws.path(), "/api/execution-journal?id=1&after_seq=8&full=1");
    let v: serde_json::Value = serde_json::from_slice(&full.body).unwrap();
    let change = &v.as_array().unwrap()[0];
    assert_eq!(change["elided"], 0, "nothing withheld under ?full=1");
    let shown: usize = change["hunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["lines"].as_array().map_or(0, Vec::len))
        .sum();
    assert_eq!(shown, adds);
}
