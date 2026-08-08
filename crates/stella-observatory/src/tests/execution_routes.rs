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
            "text"
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

/// `after_seq` (#1476) narrows the transcript to rows newer than the
/// drawer's last-seen `seq` — the incremental fetch a still-running
/// execution's poll uses instead of re-downloading the whole transcript
/// every tick.
#[test]
fn execution_journal_after_seq_returns_only_newer_rows() {
    let ws = seeded_workspace();
    // Seq 3 is the tool_start row; only tool_result (4) and the two text
    // rows (5, 6) — all > 3 — should come back.
    let response = respond(ws.path(), "/api/execution-journal?id=1&after_seq=3");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let seqs: Vec<i64> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].as_i64().unwrap())
        .collect();
    assert_eq!(seqs, [4, 5, 6], "only rows with seq > 3 come back");
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
        6,
        "seq 0 survives after_seq=-1"
    );
}

/// A body past [`db::JOURNAL_BODY_CLIP`] chars is clipped and flagged in the
/// default payload, and returned whole under `?full=1` — the elision must
/// announce itself, never pose as the data.
#[test]
fn execution_journal_clips_long_bodies_unless_full() {
    use crate::db::JOURNAL_BODY_CLIP;
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
/// `db::JOURNAL_BODY_CLIP` chars it is cut and flagged, and `?full=1` returns
/// the bytes. A system prompt is thousands of stable lines, so this is the
/// common case here rather than the edge one.
#[test]
fn execution_context_clips_long_messages_unless_full() {
    use crate::db::JOURNAL_BODY_CLIP;
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
