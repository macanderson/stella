//! Tests for the observatory HTTP surface, request routing and the
//! filesystem/database views behind it. Split out of `lib.rs` to keep that
//! file under the 1500-line ratchet (`scripts/check-file-size.sh`).

use rusqlite::Connection;
use tempfile::TempDir;

use super::*;

#[test]
fn host_header_gates_dns_rebinding() {
    let local = [
        "GET /api/executions HTTP/1.1\r\nHost: 127.0.0.1:7787\r\n\r\n",
        "GET / HTTP/1.1\r\nHost: localhost:7787\r\n\r\n",
        "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        "GET / HTTP/1.1\r\nhost: [::1]:7787\r\n\r\n",
        "GET / HTTP/1.1\r\n\r\n", // no Host (raw socket) — allowed
    ];
    for h in local {
        assert!(host_is_local(h), "should allow: {h:?}");
    }
    let remote = [
        "GET / HTTP/1.1\r\nHost: attacker.example:7787\r\n\r\n",
        "GET / HTTP/1.1\r\nHost: evil.com\r\n\r\n",
        "GET / HTTP/1.1\r\nHost: 127evil.com\r\n\r\n",
        // A registrable name that a prefix test on "127." waves through.
        "GET / HTTP/1.1\r\nHost: 127.0.0.1.attacker.example\r\n\r\n",
        "GET / HTTP/1.1\r\nHost: 127.0.0.1.attacker.example:7787\r\n\r\n",
    ];
    for h in remote {
        assert!(!host_is_local(h), "should refuse: {h:?}");
    }
}

/// Build a workspace with a seeded `.stella/private/store.db` shaped like the
/// real schema (the subset the observatory reads).
///
/// A hand-written *subset*, not a copy, and deliberately a divergent one:
/// `stella-store`'s shipped DDL also carries `executions.session_id`,
/// `usage_complete` and `usage_status`, and `telemetry.call_role` and
/// `usage_complete` — columns no query in this crate selects. The point of
/// the subset is speed and focus: these are routing and rendering tests, and
/// they should not pay for a migration run to assert that a query string
/// parses.
///
/// What used to be wrong with that is now covered elsewhere. Nothing checked
/// the two schemas against each other, so a column this crate *does* read,
/// renamed in `stella-store`, kept this suite green and 500'd the dashboard
/// at runtime (#827). `tests/schema_conformance.rs` closes that: it builds a
/// database through the real `Store::open` migration path and drives every
/// route against it, so drift fails there. This fixture is free to stay a
/// subset precisely because that gate exists.
///
/// Every INSERT below still names its columns: the fixture must survive the
/// store growing another one, not silently depend on the column order of the
/// day.
fn seeded_workspace() -> TempDir {
    let dir = TempDir::new().unwrap();
    let dot = dir.path().join(".stella");
    std::fs::create_dir_all(dot.join("private")).unwrap();
    let conn = Connection::open(dot.join("private/store.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE executions (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               kind TEXT NOT NULL, prompt TEXT NOT NULL,
               provider TEXT NOT NULL, model TEXT NOT NULL,
               started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               finished_at TEXT, outcome TEXT,
               cost_usd REAL NOT NULL DEFAULT 0);
             CREATE TABLE telemetry (
               execution_id INTEGER NOT NULL, step INTEGER NOT NULL,
               ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               provider TEXT NOT NULL, model TEXT NOT NULL,
               input_tokens INTEGER NOT NULL,
               estimated_input_tokens INTEGER NOT NULL DEFAULT 0,
               output_tokens INTEGER NOT NULL,
               cache_read_tokens INTEGER NOT NULL,
               cache_miss_tokens INTEGER NOT NULL,
               cache_write_tokens INTEGER NOT NULL DEFAULT 0,
               cost_usd REAL NOT NULL, duration_ms INTEGER NOT NULL,
               retries INTEGER NOT NULL, tool_calls INTEGER NOT NULL);
             CREATE TABLE tool_calls (
               execution_id INTEGER NOT NULL, seq INTEGER NOT NULL,
               call_id TEXT NOT NULL DEFAULT '', name TEXT NOT NULL,
               surface TEXT NOT NULL DEFAULT 'native',
               args_json TEXT NOT NULL DEFAULT '{}',
               args_digest TEXT NOT NULL DEFAULT '',
               reason TEXT NOT NULL DEFAULT '',
               ok INTEGER NOT NULL DEFAULT 1,
               error TEXT NOT NULL DEFAULT '',
               bytes_out INTEGER NOT NULL DEFAULT 0,
               duration_ms INTEGER NOT NULL DEFAULT 0,
               ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             CREATE TABLE files_touched (
               execution_id INTEGER NOT NULL, path TEXT NOT NULL,
               ops TEXT NOT NULL, lines_added INTEGER NOT NULL DEFAULT 0,
               lines_removed INTEGER NOT NULL DEFAULT 0,
               events TEXT NOT NULL DEFAULT '[]');
             INSERT INTO executions
               (kind, prompt, provider, model, outcome, cost_usd)
             VALUES
               ('run', 'add a function', 'zai', 'glm-5.2', 'completed', 0.03),
               ('goal', 'make tests pass', 'local', 'llama', 'goal_unmet', 0.0);
             INSERT INTO telemetry
               (execution_id, step, ts, provider, model, input_tokens,
                estimated_input_tokens, output_tokens, cache_read_tokens,
                cache_miss_tokens, cache_write_tokens, cost_usd, duration_ms,
                retries, tool_calls)
             VALUES
               (1, 1, CURRENT_TIMESTAMP, 'zai', 'glm-5.2',
                1000, 0, 200, 400, 600, 50, 0.03, 1500, 0, 2),
               (2, 1, CURRENT_TIMESTAMP, 'local', 'llama',
                2000, 0, 100, 0, 2000, 0, 0.0, 900, 1, 1);
             INSERT INTO tool_calls
               (execution_id, seq, name, ok, error, bytes_out, duration_ms)
             VALUES
               (1, 1, 'read_file', 1, '', 2048, 12),
               (1, 2, 'edit_file', 1, '', 64, 3),
               (2, 1, 'bash', 0, 'exit 1', 0, 40);
             INSERT INTO files_touched VALUES
               (1, 'src/lib.rs', 'RU', 4, 1, '[]');
             CREATE TABLE execution_reflection (
               execution_id INTEGER PRIMARY KEY,
               prompt TEXT NOT NULL DEFAULT '',
               delivered INTEGER, self_rating INTEGER,
               what_went_well TEXT NOT NULL DEFAULT '',
               what_to_improve TEXT NOT NULL DEFAULT '',
               critique TEXT NOT NULL DEFAULT '',
               produced_output INTEGER NOT NULL DEFAULT 0,
               wrote_files INTEGER NOT NULL DEFAULT 0,
               truncated INTEGER NOT NULL DEFAULT 0,
               recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             INSERT INTO execution_reflection
               (execution_id, delivered, self_rating, what_to_improve)
             VALUES (1, 1, 8, 'read the failing test first');
             CREATE TABLE rules (
               rule_id TEXT PRIMARY KEY, contents TEXT NOT NULL,
               source TEXT NOT NULL,
               created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             INSERT INTO rules (rule_id, contents, source)
             VALUES ('no-vacuous-fixes', 'a fix must change production code', 'reflection');",
    )
    .unwrap();
    // The events journal, seeded in its own batch: the payloads are the
    // internally-tagged `AgentEvent` JSON `Store::record_event` persists,
    // and a raw string keeps their quoting readable. The `text_delta` row is
    // there to be *excluded* — the journal route must drop live-preview
    // fragments in favor of the authoritative `text` event.
    conn.execute_batch(
        r#"CREATE TABLE events (
             execution_id INTEGER NOT NULL,
             seq INTEGER NOT NULL,
             ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
             event_type TEXT NOT NULL,
             payload TEXT NOT NULL,
             UNIQUE (execution_id, seq));
           INSERT INTO events (execution_id, seq, event_type, payload) VALUES
             (1, 0, 'stage', '{"type":"stage","name":"build"}'),
             (1, 1, 'text_delta', '{"type":"text_delta","text":"add"}'),
             (1, 2, 'reasoning', '{"type":"reasoning","delta":"plan the edit"}'),
             (1, 3, 'tool_start', '{"type":"tool_start","call":{"call_id":"c1","name":"read_file","input":{"path":"src/lib.rs"}}}'),
             (1, 4, 'tool_result', '{"type":"tool_result","call_id":"c1","output":{"ok":{"content":"fn a() {}"}},"duration_ms":12,"speculated":false}'),
             (1, 5, 'text', '{"type":"text","delta":"added the function"}');"#,
    )
    .unwrap();
    // The context receipts (#1475), also in their own batch: one recorded
    // worker call plus the overflow summarizer that shares its step, so a
    // reconstruction has to be scoped by `call_seq` rather than by step.
    //
    // The two gap blocks carry their preimage locally (`content`); the other
    // three resolve from the events above and their `content_digest` is the
    // real sha256 of the exact preimage bytes — that is what makes the
    // verification verdict a fact here rather than a fixture constant. The
    // tool_call digest covers `{"call_id":…,"name":…,"input":…}` in
    // `stella_protocol::ToolCall`'s declaration order.
    conn.execute_batch(
        "CREATE TABLE step_receipt (
             execution_id INTEGER NOT NULL, turn_instance INTEGER NOT NULL,
             step INTEGER NOT NULL, call_seq INTEGER NOT NULL DEFAULT 0,
             provider TEXT NOT NULL, model TEXT NOT NULL, call_role TEXT NOT NULL,
             effective_budget_tokens INTEGER NOT NULL,
             calibration_factor REAL NOT NULL,
             estimated_input_tokens INTEGER NOT NULL,
             compiled_frame_id TEXT, frame_hash TEXT,
             PRIMARY KEY (execution_id, turn_instance, step, call_seq));
           CREATE TABLE step_manifest (
             execution_id INTEGER NOT NULL, turn_instance INTEGER NOT NULL,
             step INTEGER NOT NULL, call_seq INTEGER NOT NULL DEFAULT 0,
             ordinal INTEGER NOT NULL, block_id TEXT NOT NULL,
             cache_zone TEXT NOT NULL, resident_since_step INTEGER NOT NULL,
             message_index INTEGER NOT NULL DEFAULT 0, call_id TEXT,
             PRIMARY KEY (execution_id, turn_instance, step, call_seq, ordinal));
           CREATE TABLE context_blocks (
             execution_id INTEGER NOT NULL, block_id TEXT NOT NULL,
             kind TEXT NOT NULL, origin_turn INTEGER NOT NULL,
             origin_step INTEGER NOT NULL, call_id TEXT, memory_id TEXT,
             token_cost INTEGER, content_digest TEXT NOT NULL,
             citation_label TEXT, content TEXT,
             first_seen_ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
             PRIMARY KEY (execution_id, block_id));
           INSERT INTO step_receipt
             (execution_id, turn_instance, step, call_seq, provider, model,
              call_role, effective_budget_tokens, calibration_factor,
              estimated_input_tokens)
           VALUES
             (1, 0, 1, 0, 'zai', 'glm-5.2', 'worker', 136363, 1.1, 203),
             (1, 0, 1, 1, 'zai', 'glm-5.2', 'overflow_summarizer', 136363, 1.1, 12);
           INSERT INTO context_blocks
             (execution_id, block_id, kind, origin_turn, origin_step, call_id,
              token_cost, content_digest, content)
           VALUES
             (1, 'blk_sys', 'system_prefix', 0, 0, NULL, 40,
              'sha256:0bf72c9a096fc5a3c095d830189524ae60b197a0e78dde2c80b914abb1e07b3d',
              'you are careful'),
             (1, 'blk_user', 'user_goal', 0, 0, NULL, 30,
              'sha256:11360c09a451a0c3a39d7fdc2697800d5b500a6b445b8120fba6ad5c0b5eb4e6',
              'add a function'),
             (1, 'blk_text', 'assistant_text', 0, 1, NULL, 20,
              'sha256:f9cbfada6f746168b93daa674d776cd0407b6e3c3777ef3aaf3ca253ba5ae07c',
              NULL),
             (1, 'blk_call', 'tool_call', 0, 1, 'c1', 25,
              'sha256:70b207f5a306abb1d8d7b4534452d37806896cd51569a8e8554f0e4e5109e4ad',
              NULL),
             (1, 'blk_res', 'tool_result', 0, 1, 'c1', 88,
              'sha256:bfd1986b0e5cdf2925326cf75fa8d40320722e0a3971274a7844b2147ac927e7',
              NULL);
           INSERT INTO step_manifest
             (execution_id, turn_instance, step, call_seq, ordinal, block_id,
              cache_zone, resident_since_step, message_index, call_id)
           VALUES
             (1, 0, 1, 0, 0, 'blk_sys',  'stable_prefix', 0, 0, NULL),
             (1, 0, 1, 0, 1, 'blk_user', 'cacheable',     0, 1, NULL),
             (1, 0, 1, 0, 2, 'blk_text', 'volatile',      1, 2, NULL),
             (1, 0, 1, 0, 3, 'blk_call', 'volatile',      1, 2, 'c1'),
             (1, 0, 1, 0, 4, 'blk_res',  'volatile',      1, 3, 'c1'),
             (1, 0, 1, 1, 0, 'blk_sys',  'stable_prefix', 0, 0, NULL);",
    )
    .unwrap();
    dir
}

/// Layer the filesystem-backed surfaces (skills, memories, rules,
/// lessons, mcp.toml, settings.json, codegraph.db) onto a seeded
/// workspace.
fn seed_fs_surfaces(dir: &TempDir) {
    let dot = dir.path().join(".stella");
    std::fs::create_dir_all(dot.join("skills/my-skill")).unwrap();
    std::fs::write(
        dot.join("skills/my-skill/SKILL.md"),
        "---\nname: my-skill\ndescription: does the thing\n---\n# body",
    )
    .unwrap();
    std::fs::create_dir_all(dot.join("skills/learned")).unwrap();
    std::fs::write(
        dot.join("skills/learned/hard-won.md"),
        "---\nname: hard-won\ndescription: extracted from a failure\n---\n# lesson",
    )
    .unwrap();
    std::fs::create_dir_all(dot.join("memories")).unwrap();
    std::fs::write(
        dot.join("memories/build-quirk.md"),
        "The build needs -p flags.",
    )
    .unwrap();
    std::fs::create_dir_all(dot.join("rules")).unwrap();
    std::fs::write(dot.join("rules/style.md"), "Prefer witness tests.").unwrap();
    std::fs::write(
            dot.join("private/reflections.jsonl"),
            r#"{"lesson":"check the test's invariant first","domains":["testing"],"occurred_at":1700000000}
{"lesson":"verify dependency chains before citing them","domains":["docs"],"occurred_at":1700000100}
"#,
        )
        .unwrap();
    std::fs::write(
        dot.join("mcp.toml"),
        "[servers.github]\ntransport = \"stdio\"\ncmd = \"gh-mcp\"\n\
             args = [\"--stdio\", \"--token=ghp_argsecret\"]\n\
             [servers.github.env]\nGITHUB_TOKEN = \"ghp_supersecret\"\n\
             [servers.search]\ntransport = \"http\"\n\
             url = \"https://mcp.example/v1?api_key=sk-live-urlsecret\"\n",
    )
    .unwrap();
    std::fs::write(
            dot.join("settings.json"),
            r#"{"providers":{"zai":{"api_key":"sk-live-topsecret","api_key_env":"ZAI_KEY"}},"agent_engine_config":{"agents":{"verifier":{"model":"glm-5.2"}}}}"#,
        )
        .unwrap();
    let graph = Connection::open(dot.join("private/codegraph.db")).unwrap();
    graph
        .execute_batch(
            "CREATE TABLE code_graph_files (
                   id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                   language TEXT NOT NULL, content_sha256 TEXT NOT NULL,
                   mtime_ns INTEGER NOT NULL, indexed_at INTEGER NOT NULL);
                 CREATE TABLE code_graph_symbols (
                   id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL,
                   name TEXT NOT NULL, kind TEXT NOT NULL,
                   start_line INTEGER NOT NULL, end_line INTEGER NOT NULL);
                 CREATE TABLE code_graph_imports (
                   id INTEGER PRIMARY KEY, from_file_id INTEGER NOT NULL,
                   specifier TEXT NOT NULL, to_path TEXT, kind TEXT NOT NULL);
                 INSERT INTO code_graph_files VALUES
                   (1, 'crate-a/src/lib.rs',  'rust', 'x', 0, 0),
                   (2, 'crate-a/src/util.rs', 'rust', 'x', 0, 0),
                   (3, 'crate-b/src/lib.rs',  'rust', 'x', 0, 0),
                   (4, 'tools/x.py', 'python', 'x', 0, 0),
                   (5, 'tools/y.py', 'python', 'x', 0, 0);
                 INSERT INTO code_graph_symbols VALUES
                   (1, 1, 'run', 'function', 1, 9),
                   (2, 2, 'Util', 'struct', 1, 5);
                 INSERT INTO code_graph_imports VALUES
                   (1, 3, 'crate_a::util::Util', NULL, 'absolute'),
                   (2, 2, 'crate::run',          NULL, 'absolute'),
                   (3, 2, 'std::fs',             NULL, 'absolute'),
                   (4, 4, './y', 'tools/y.py',   'relative');",
        )
        .unwrap();
}

#[test]
fn overview_aggregates_runs_cost_and_tokens() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/api/overview");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(v["runs"], 2);
    assert_eq!(v["resolved"], 1);
    assert_eq!(v["input_tokens"], 3000);
    assert_eq!(v["output_tokens"], 300);
    assert_eq!(v["timeline"].as_array().unwrap().len(), 2);
}

#[test]
fn models_mirror_stats_semantics() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/api/models");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    // Ordered by cost desc: zai first, then local (off-grid).
    assert_eq!(rows[0]["provider"], "zai");
    assert_eq!(rows[0]["resolve_rate"], 1.0);
    assert_eq!(rows[1]["division"], "off-grid");
    assert_eq!(rows[1]["cost_per_resolved_usd"], serde_json::Value::Null);
}

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
        ["stage", "reasoning", "tool_start", "tool_result", "text"],
        "seq order, text_delta excluded"
    );
    assert_eq!(v[0]["label"], "build");
    assert_eq!(v[1]["body"], "plan the edit");
    assert_eq!(v[2]["name"], "read_file");
    assert!(v[2]["body"].as_str().unwrap().contains("src/lib.rs"));
    assert_eq!(v[3]["ok"], true);
    assert_eq!(v[3]["body"], "fn a() {}");
    assert_eq!(v[3]["duration_ms"], 12);
    assert_eq!(v[4]["body"], "added the function");
    assert_eq!(v[4]["truncated"], false);
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
    // Seq 3 is the tool_start row; only tool_result (4) and text (5) — both
    // > 3 — should come back.
    let response = respond(ws.path(), "/api/execution-journal?id=1&after_seq=3");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let seqs: Vec<i64> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["seq"].as_i64().unwrap())
        .collect();
    assert_eq!(seqs, [4, 5], "only rows with seq > 3 come back");
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
        5,
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

#[test]
fn tool_leaderboard_counts_errors() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/api/tools");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let bash = v
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "bash")
        .unwrap();
    assert_eq!(bash["calls"], 1);
    assert_eq!(bash["errors"], 1);
}

#[test]
fn empty_workspace_degrades_to_empty_payloads_not_errors() {
    let ws = TempDir::new().unwrap();
    for route in [
        "/api/meta",
        "/api/overview",
        "/api/executions",
        "/api/execution-journal?id=1",
        "/api/execution-journal?id=1&after_seq=0",
        "/api/execution-context?id=1",
        "/api/execution-context?id=1&turn=0&step=1&call_seq=0",
        "/api/sessions",
        "/api/session?id=ses-1700000000000-424242",
        "/api/execution-tendencies?id=1",
        "/api/execution-context-diff?id=1",
        "/api/models",
        "/api/tools",
        "/api/files",
        "/api/memory",
        "/api/mcp",
        "/api/fleet",
        "/api/activity",
        "/api/projects",
        "/api/hub-telemetry",
        "/api/codegraph",
        "/api/skills",
        "/api/mcp-servers",
        "/api/config",
        "/api/memories",
        "/api/explorations",
        "/api/rules",
        "/api/reflections",
    ] {
        let response = respond(ws.path(), route);
        assert_eq!(response.status, "200 OK", "route {route}");
    }
}

/// The route table and the embedded page must not drift apart. A route
/// nothing fetches is dead weight that still drags in its dependencies —
/// `/api/explorations` sat unconsumed for exactly that long (#640).
#[test]
fn every_served_route_is_fetched_by_the_embedded_page() {
    // Route-table arms are `"/api/<name>" => …`; the same literal
    // appearing as a call argument (tests, 404 fixtures) has no `=>`.
    let routes: Vec<&str> = include_str!("lib.rs")
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("\"/api/")?;
            let (route, tail) = rest.split_once('"')?;
            tail.trim_start().starts_with("=>").then_some(route)
        })
        .collect();
    assert!(
        routes.len() >= 19,
        "route table not recognised, only found {routes:?}"
    );
    for route in routes {
        // A bare `contains` lets a shorter route ride on a longer one that
        // starts with it: `/api/mcp` would pass on the strength of
        // `/api/mcp-servers` alone, and `/api/memory` on `/api/memories`,
        // `/api/execution` on `/api/executions`. Require an occurrence
        // whose next character actually ends the path — the page writes
        // `"/api/x"`, `` `/api/x` `` or `/api/x?…`.
        let needle = format!("/api/{route}");
        let fetched = INDEX_HTML.match_indices(needle.as_str()).any(|(at, _)| {
            INDEX_HTML[at + needle.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '-')
        });
        assert!(
            fetched,
            "/api/{route} is served but nothing in the dashboard fetches it: \
                 give it a consumer or delete the route"
        );
    }
}

/// The dashboard re-fetches these every 5 s; unbounded full-history
/// payloads made every poll cost the whole store. The row-returning
/// queries are windowed to the newest rows, while the headline aggregates
/// ("runs", spend) stay exact.
#[test]
fn execution_listing_and_timeline_are_bounded_to_the_newest_rows() {
    use crate::db::MAX_LISTED_EXECUTIONS;
    let ws = seeded_workspace();
    let conn = Connection::open(ws.path().join(".stella/private/store.db")).unwrap();
    let extra = MAX_LISTED_EXECUTIONS + 10;
    conn.execute_batch("BEGIN").unwrap();
    {
        let mut stmt = conn
            .prepare(
                "INSERT INTO executions
                       (kind, prompt, provider, model, outcome, cost_usd)
                     VALUES ('run', 'bulk', 'zai', 'glm-5.2', 'completed', 0.0)",
            )
            .unwrap();
        for _ in 0..extra {
            stmt.execute([]).unwrap();
        }
    }
    conn.execute_batch("COMMIT").unwrap();
    let seeded = 2; // seeded_workspace's own executions
    let newest = (seeded + extra) as i64;
    let oldest_kept = newest - MAX_LISTED_EXECUTIONS as i64 + 1;

    let v: serde_json::Value =
        serde_json::from_slice(&respond(ws.path(), "/api/executions").body).unwrap();
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), MAX_LISTED_EXECUTIONS);
    assert_eq!(rows[0]["id"], newest, "newest first");
    assert_eq!(rows.last().unwrap()["id"], oldest_kept);

    let v: serde_json::Value =
        serde_json::from_slice(&respond(ws.path(), "/api/overview").body).unwrap();
    let timeline = v["timeline"].as_array().unwrap();
    assert_eq!(timeline.len(), MAX_LISTED_EXECUTIONS);
    assert_eq!(timeline[0]["id"], oldest_kept, "chart stays ascending");
    assert_eq!(timeline.last().unwrap()["id"], newest);
    assert_eq!(
        v["runs"], newest,
        "the headline aggregate counts every run, not just the window"
    );
}

/// Same bound for the ratings feed: newest first out of the store, served
/// ascending for the chart's left-to-right axis.
#[test]
fn reflection_ratings_are_bounded_to_the_newest_rows() {
    use crate::db::MAX_LISTED_REFLECTIONS;
    let ws = seeded_workspace();
    let conn = Connection::open(ws.path().join(".stella/private/store.db")).unwrap();
    let extra = MAX_LISTED_REFLECTIONS + 10;
    conn.execute_batch("BEGIN").unwrap();
    {
        // Ids from 1000 so the seeded reflection (execution 1) is oldest.
        let mut stmt = conn
            .prepare(
                "INSERT INTO execution_reflection (execution_id, self_rating)
                     VALUES (?1, 5)",
            )
            .unwrap();
        for i in 0..extra {
            stmt.execute([1000 + i as i64]).unwrap();
        }
    }
    conn.execute_batch("COMMIT").unwrap();

    let v: serde_json::Value =
        serde_json::from_slice(&respond(ws.path(), "/api/reflections").body).unwrap();
    let ratings = v["ratings"].as_array().unwrap();
    assert_eq!(ratings.len(), MAX_LISTED_REFLECTIONS);
    let newest = 1000 + extra as i64 - 1;
    assert_eq!(ratings.last().unwrap()["execution_id"], newest);
    assert_eq!(
        ratings[0]["execution_id"],
        newest - MAX_LISTED_REFLECTIONS as i64 + 1,
        "the seeded oldest reflection aged out of the window"
    );
}

/// The tool leaderboard is the hottest query (every poll, highest-
/// cardinality table): it aggregates a bounded recent window, so calls
/// older than the window stop being rescanned every 5 s.
#[test]
fn tool_leaderboard_scans_a_bounded_recent_window() {
    use crate::db::TOOL_CALL_SCAN_WINDOW;
    let ws = seeded_workspace();
    let conn = Connection::open(ws.path().join(".stella/private/store.db")).unwrap();
    conn.execute_batch("BEGIN").unwrap();
    {
        let mut stmt = conn
            .prepare(
                "INSERT INTO tool_calls
                       (execution_id, seq, name, ok, error, bytes_out, duration_ms)
                     VALUES (1, ?1, ?2, 1, '', 0, 1)",
            )
            .unwrap();
        for i in 0..10_i64 {
            stmt.execute(rusqlite::params![10 + i, "ancient"]).unwrap();
        }
        for i in 0..TOOL_CALL_SCAN_WINDOW as i64 {
            stmt.execute(rusqlite::params![20 + i, "recent"]).unwrap();
        }
    }
    conn.execute_batch("COMMIT").unwrap();

    let v: serde_json::Value =
        serde_json::from_slice(&respond(ws.path(), "/api/tools").body).unwrap();
    let rows = v.as_array().unwrap();
    let recent = rows
        .iter()
        .find(|r| r["name"] == "recent")
        .expect("windowed calls are aggregated");
    assert_eq!(recent["calls"], TOOL_CALL_SCAN_WINDOW as i64);
    assert!(
        rows.iter().all(|r| r["name"] != "ancient"),
        "calls older than the window are not rescanned every poll"
    );
}

#[test]
fn activity_rolls_up_runs_tokens_and_tool_calls_by_day() {
    let ws = seeded_workspace();
    let response = respond(ws.path(), "/api/activity");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let days = v.as_array().unwrap();
    assert_eq!(days.len(), 1, "everything seeded lands on today");
    let d = &days[0];
    assert_eq!(d["runs"], 2);
    assert_eq!(d["resolved"], 1);
    assert_eq!(d["input_tokens"], 3000);
    assert_eq!(d["tool_calls"], 3);
    assert_eq!(d["tool_errors"], 1);
}

/// A timestamp `date()` can't parse used to NULL out the group key and
/// fail the whole rollup — one bad row blanked the dashboard. The work
/// is real, so it lands in a named bucket that sorts last.
#[test]
fn activity_buckets_unparseable_timestamps_instead_of_failing() {
    let ws = seeded_workspace();
    let conn = Connection::open(ws.path().join(".stella/private/store.db")).unwrap();
    conn.execute_batch(
        "INSERT INTO executions
               (kind, prompt, provider, model, started_at, outcome, cost_usd)
             VALUES ('run', 'clock skew', 'zai', 'glm-5.2', 'sometime', 'completed', 0.5);
             INSERT INTO telemetry
               (execution_id, step, ts, provider, model, input_tokens,
                estimated_input_tokens, output_tokens, cache_read_tokens,
                cache_miss_tokens, cache_write_tokens, cost_usd, duration_ms,
                retries, tool_calls)
             VALUES (3, 1, 'sometime', 'zai', 'glm-5.2', 7, 0, 9, 0, 0, 0, 0.5, 11, 0, 1);
             INSERT INTO tool_calls
               (execution_id, seq, name, ok, error, bytes_out, duration_ms, ts)
             VALUES (3, 1, 'bash', 0, 'boom', 0, 4, 'sometime');",
    )
    .unwrap();
    let response = respond(ws.path(), "/api/activity");
    assert_eq!(response.status, "200 OK");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let days = v.as_array().unwrap();
    assert_eq!(days.len(), 2, "today plus the undated bucket");
    let undated = days.last().unwrap();
    assert_eq!(undated["day"], crate::db::UNDATED);
    assert_eq!(undated["runs"], 1);
    assert_eq!(undated["resolved"], 1);
    assert_eq!(undated["input_tokens"], 7);
    assert_eq!(undated["tool_calls"], 1);
    assert_eq!(undated["tool_errors"], 1);
}

#[test]
fn codegraph_resolves_rust_specifiers_and_relative_imports() {
    let ws = seeded_workspace();
    seed_fs_surfaces(&ws);
    let response = respond(ws.path(), "/api/codegraph");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let nodes = v["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 5);
    assert_eq!(v["total_symbols"], 2);
    let path_of = |i: usize| nodes[i]["path"].as_str().unwrap();
    let edges: Vec<(usize, usize)> = v["edges"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e[0].as_u64().unwrap() as usize,
                e[1].as_u64().unwrap() as usize,
            )
        })
        .collect();
    let has = |from: &str, to: &str| {
        edges
            .iter()
            .any(|&(a, b)| path_of(a) == from && path_of(b) == to)
    };
    // Cross-crate `crate_a::util::Util`, crate-local `crate::run`, and
    // the indexer-resolved Python relative import.
    assert!(has("crate-b/src/lib.rs", "crate-a/src/util.rs"));
    assert!(has("crate-a/src/util.rs", "crate-a/src/lib.rs"));
    assert!(has("tools/x.py", "tools/y.py"));
    // `std::fs` resolves to nothing.
    assert_eq!(edges.len(), 3);
}

#[test]
fn skills_list_project_scope_and_learned_flags() {
    let ws = seeded_workspace();
    seed_fs_surfaces(&ws);
    let response = respond(ws.path(), "/api/skills");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let rows = v.as_array().unwrap();
    let find = |name: &str| rows.iter().find(|r| r["name"] == name);
    let skill = find("my-skill").expect("project skill listed");
    assert_eq!(skill["scope"], "project");
    assert_eq!(skill["learned"], false);
    assert_eq!(skill["description"], "does the thing");
    let learned = find("hard-won").expect("learned skill listed");
    assert_eq!(learned["learned"], true);
}

#[test]
fn explorations_route_reports_per_map_freshness() {
    let ws = seeded_workspace();
    let dir = ws.path().join(".stella/explorations");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(ws.path().join("covered.rs"), "fn mapped() {}").unwrap();
    // sha256("fn mapped() {}") — computed with the same encoding the
    // producer uses; freshness must verify against the live file.
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"fn mapped() {}");
    let fresh_hash: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    std::fs::write(
        dir.join("zone.json"),
        serde_json::json!({
            "slice": "zone", "title": "Zone", "summary": "s", "content": "body",
            "files": ["covered.rs"], "created_at_ms": 2u64,
            "manifest": { "covered.rs": fresh_hash, "gone.rs": "0000" },
            "status": "complete", "pid": 42u32
        })
        .to_string(),
    )
    .unwrap();
    // A pre-manifest (v1) record must report "unknown", not crash.
    std::fs::write(
        dir.join("legacy.json"),
        serde_json::json!({
            "slice": "legacy", "title": "Old", "summary": "s", "content": "b",
            "files": [], "created_at_ms": 1u64
        })
        .to_string(),
    )
    .unwrap();

    let response = respond(ws.path(), "/api/explorations");
    let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
    let rows = v.as_array().unwrap();
    assert_eq!(rows.len(), 2);
    // Newest first.
    assert_eq!(rows[0]["slice"], "zone");
    assert_eq!(rows[0]["freshness"], "drifted");
    assert_eq!(rows[0]["missing"][0], "gone.rs");
    assert!(rows[0]["changed"].as_array().unwrap().is_empty());
    assert_eq!(rows[0]["manifest_files"], 2);
    assert_eq!(rows[1]["slice"], "legacy");
    assert_eq!(rows[1]["freshness"], "unknown");
    // Bodies are sized, never served in the listing.
    assert!(rows[0]["content"].is_null());
    assert_eq!(rows[0]["content_chars"], 4);
}

#[test]
fn mcp_servers_expose_names_but_never_credential_values() {
    let ws = seeded_workspace();
    seed_fs_surfaces(&ws);
    let response = respond(ws.path(), "/api/mcp-servers");
    let body = String::from_utf8(response.body).unwrap();
    assert!(body.contains("github"));
    assert!(body.contains("GITHUB_TOKEN"), "env var names are shown");
    assert!(
        !body.contains("ghp_supersecret"),
        "env var values must never be served"
    );
    // The target field is served verbatim to the browser, so args and URL
    // query strings — the two places a credential rides along — are out.
    assert!(
        !body.contains("ghp_argsecret"),
        "argument values must never be served"
    );
    assert!(
        body.contains("gh-mcp"),
        "the command name keeps the row legible"
    );
    assert!(body.contains("2 args"), "…as does the argument count");
    assert!(
        !body.contains("sk-live-urlsecret"),
        "URL query strings must never be served"
    );
    assert!(
        body.contains("https://mcp.example/v1"),
        "scheme://host/path survives for the http row"
    );
}

#[test]
fn config_serves_scope_chain_with_secrets_redacted() {
    let ws = seeded_workspace();
    seed_fs_surfaces(&ws);
    let response = respond(ws.path(), "/api/config");
    let body = String::from_utf8(response.body).unwrap();
    assert!(!body.contains("sk-live-topsecret"), "api keys are redacted");
    assert!(body.contains("ZAI_KEY"), "env var *names* survive");
    assert!(body.contains("glm-5.2"), "engine config is visible");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    let scopes = v["scopes"].as_array().unwrap();
    assert_eq!(scopes.len(), 3);
    let project = scopes.iter().find(|s| s["scope"] == "project").unwrap();
    assert_eq!(project["exists"], true);
}

#[test]
fn memories_rules_and_reflections_come_from_disk_and_db() {
    let ws = seeded_workspace();
    seed_fs_surfaces(&ws);
    let memories: serde_json::Value =
        serde_json::from_slice(&respond(ws.path(), "/api/memories").body).unwrap();
    assert_eq!(memories[0]["name"], "build-quirk");
    let rules: serde_json::Value =
        serde_json::from_slice(&respond(ws.path(), "/api/rules").body).unwrap();
    assert_eq!(rules["db"][0]["rule_id"], "no-vacuous-fixes");
    assert_eq!(rules["files"][0]["name"], "style");
    let refl: serde_json::Value =
        serde_json::from_slice(&respond(ws.path(), "/api/reflections").body).unwrap();
    assert_eq!(refl["lessons"].as_array().unwrap().len(), 2);
    let ratings = refl["ratings"].as_array().unwrap();
    assert_eq!(ratings.len(), 1);
    assert_eq!(ratings[0]["self_rating"], 8);
    assert_eq!(ratings[0]["what_to_improve"], "read the failing test first");
}

#[test]
fn project_param_drills_into_another_registered_workspace() {
    // `crate::test_env::lock` rather than a mutex private to this file: the
    // hazard is the shared process environment, and fsview.rs mutates it too
    // (#1137). A second private lock would serialize nothing against that one.
    let _env = crate::test_env::lock();
    let _restore = crate::test_env::EnvRestore::capture(&["STELLA_DATA_DIR"]);
    // Two workspaces: `home` is empty, `other` is seeded. A usage.db in a
    // private STELLA_DATA_DIR registers `other`; ?project= must re-point
    // the request at it, and unknown ids must fall back to `home`.
    let home = TempDir::new().unwrap();
    let other = seeded_workspace();
    let data = TempDir::new().unwrap();
    let usage = Connection::open(data.path().join("usage.db")).unwrap();
    usage
        .execute_batch(&format!(
            "CREATE TABLE projects (
                   project_id TEXT PRIMARY KEY, name TEXT NOT NULL,
                   root_path TEXT NOT NULL, first_seen_at TEXT NOT NULL,
                   last_seen_at TEXT NOT NULL);
                 CREATE TABLE execution_rollup (
                   project_id TEXT NOT NULL, execution_id INTEGER NOT NULL,
                   kind TEXT NOT NULL, prompt_digest TEXT NOT NULL,
                   prompt_preview TEXT NOT NULL DEFAULT '',
                   model TEXT NOT NULL, provider TEXT NOT NULL,
                   outcome TEXT NOT NULL, cost_usd REAL NOT NULL,
                   input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
                   duration_ms INTEGER NOT NULL, tool_calls INTEGER NOT NULL,
                   files_written INTEGER NOT NULL, produced_output INTEGER NOT NULL,
                   self_rating INTEGER, started_at TEXT NOT NULL,
                   PRIMARY KEY (project_id, execution_id));
                 INSERT INTO projects VALUES
                   ('feedbeef00000001', 'other', '{}', '2026-01-01', '2026-01-02');",
            other.path().display()
        ))
        .unwrap();
    // SAFETY: the env lock is held for the whole test, and `_restore` undoes
    // this on drop — including if one of the `respond` calls below panics,
    // which would otherwise leak a path into a TempDir about to be deleted.
    unsafe { std::env::set_var("STELLA_DATA_DIR", data.path()) };
    let drilled = respond(home.path(), "/api/overview?project=feedbeef00000001");
    let unknown = respond(home.path(), "/api/overview?project=doesnotexist");
    let listed = respond(home.path(), "/api/projects");
    let v: serde_json::Value = serde_json::from_slice(&drilled.body).unwrap();
    assert_eq!(v["runs"], 2, "?project= reads the other workspace");
    let v: serde_json::Value = serde_json::from_slice(&unknown.body).unwrap();
    assert_eq!(v["runs"], 0, "unknown ids fall back to the serving root");
    let v: serde_json::Value = serde_json::from_slice(&listed.body).unwrap();
    assert_eq!(v["available"], true);
    assert_eq!(v["projects"][0]["name"], "other");
    assert_eq!(v["projects"][0]["has_store"], true);
}

#[test]
fn hub_telemetry_aggregates_scope_and_drills_to_project() {
    let _env = crate::test_env::lock();
    let _restore = crate::test_env::EnvRestore::capture(&["STELLA_DATA_DIR"]);
    let home = TempDir::new().unwrap();

    // A hub dir with no usage.db is an empty state, never an error.
    let empty = TempDir::new().unwrap();
    // SAFETY: the env lock is held for the whole test, and `_restore` undoes
    // this on drop — including if the fixture `unwrap`s below panic first.
    unsafe { std::env::set_var("STELLA_DATA_DIR", empty.path()) };
    let none = respond(home.path(), "/api/hub-telemetry");

    // A populated hub spanning an org AND the common never-cloud-registered
    // case (NULL org/workspace, '' repo), plus a row with no recorded_at.
    let data = TempDir::new().unwrap();
    let usage = Connection::open(data.path().join("usage.db")).unwrap();
    usage
            .execute_batch(
                "CREATE TABLE projects (
                   project_id TEXT PRIMARY KEY, name TEXT NOT NULL,
                   root_path TEXT NOT NULL, first_seen_at TEXT NOT NULL,
                   last_seen_at TEXT NOT NULL);
                 CREATE TABLE telemetry (
                   project_id TEXT NOT NULL, source_rowid INTEGER NOT NULL,
                   org_id TEXT, workspace_id TEXT, repo_id TEXT NOT NULL DEFAULT '',
                   execution_id INTEGER NOT NULL, step INTEGER NOT NULL,
                   recorded_at TEXT NOT NULL DEFAULT '',
                   provider TEXT NOT NULL, call_role TEXT NOT NULL, model TEXT NOT NULL,
                   input_tokens INTEGER NOT NULL, estimated_input_tokens INTEGER NOT NULL,
                   output_tokens INTEGER NOT NULL, cache_read_tokens INTEGER NOT NULL,
                   cache_miss_tokens INTEGER NOT NULL, cache_write_tokens INTEGER NOT NULL,
                   cost_usd REAL NOT NULL, duration_ms INTEGER NOT NULL,
                   retries INTEGER NOT NULL, tool_calls INTEGER NOT NULL,
                   usage_complete INTEGER NOT NULL,
                   PRIMARY KEY (project_id, source_rowid));
                 INSERT INTO projects VALUES
                   ('p1','proj-one','/tmp/p1','2026-07-20','2026-07-21'),
                   ('p2','proj-two','/tmp/p2','2026-07-20','2026-07-20'),
                   ('p3','proj-three','/tmp/p3','2026-07-22','2026-07-22');
                 INSERT INTO telemetry
                   (project_id,source_rowid,org_id,workspace_id,repo_id,execution_id,step,
                    recorded_at,provider,call_role,model,input_tokens,estimated_input_tokens,
                    output_tokens,cache_read_tokens,cache_miss_tokens,cache_write_tokens,
                    cost_usd,duration_ms,retries,tool_calls,usage_complete)
                 VALUES
                   ('p1',1,'acme','ws1','r1',100,0,'2026-07-20 10:00:00','anthropic','worker','claude-x',1000,1000,200,800,200,0,0.50,1200,0,3,1),
                   ('p1',2,'acme','ws1','r1',101,0,'2026-07-21 10:00:00','anthropic','worker','claude-x',500,500,100,400,100,0,0.25,900,0,1,1),
                   ('p2',1,'acme','ws1','r2',200,0,'2026-07-20 12:00:00','openai','worker','gpt-y',800,800,300,0,800,0,0.40,1500,0,2,1),
                   ('p3',1,NULL,NULL,'',300,0,'2026-07-22 09:00:00','anthropic','worker','claude-x',300,300,50,100,200,0,0.10,600,0,0,1),
                   ('p3',2,NULL,NULL,'',301,0,'','anthropic','worker','claude-x',100,100,20,0,100,0,0.05,400,0,0,1);",
            )
            .unwrap();
    drop(usage);
    unsafe { std::env::set_var("STELLA_DATA_DIR", data.path()) };

    let all = respond(home.path(), "/api/hub-telemetry");
    let acme = respond(home.path(), "/api/hub-telemetry?org=acme");
    let local = respond(home.path(), "/api/hub-telemetry?org=(local)");
    let repos = respond(home.path(), "/api/hub-telemetry?org=acme&workspace=ws1");
    let proj = respond(
        home.path(),
        "/api/hub-telemetry?org=acme&workspace=ws1&repo=r1",
    );

    // Empty hub: available:false, zeroed, 200 OK.
    assert_eq!(none.status, "200 OK");
    let v: serde_json::Value = serde_json::from_slice(&none.body).unwrap();
    assert_eq!(v["available"], false);
    assert_eq!(v["totals"]["calls"], 0);
    assert!(v["drill"]["rows"].as_array().unwrap().is_empty());

    // Unscoped totals + org drill.
    let v: serde_json::Value = serde_json::from_slice(&all.body).unwrap();
    assert_eq!(v["available"], true);
    assert_eq!(v["totals"]["calls"], 5);
    assert_eq!(v["totals"]["projects"], 3);
    assert_eq!(v["totals"]["orgs"], 1, "NULL org is not a distinct org");
    assert!((v["totals"]["cost_usd"].as_f64().unwrap() - 1.30).abs() < 1e-9);
    assert_eq!(v["drill"]["level"], "org");
    let orgs = v["drill"]["rows"].as_array().unwrap();
    let acme_row = orgs.iter().find(|r| r["key"] == "acme").unwrap();
    assert!((acme_row["cost_usd"].as_f64().unwrap() - 1.15).abs() < 1e-9);
    assert_eq!(acme_row["projects"], 2);
    let local_row = orgs.iter().find(|r| r["key"].is_null()).unwrap();
    assert!((local_row["cost_usd"].as_f64().unwrap() - 0.15).abs() < 1e-9);
    // provider/model mirrors `usage report`, ordered by cost, with a rate.
    let pm = v["by_provider_model"].as_array().unwrap();
    assert_eq!(pm[0]["provider"], "anthropic");
    assert_eq!(pm[0]["model"], "claude-x");
    assert_eq!(pm[0]["calls"], 4);
    assert!((pm[0]["cache_read_rate"].as_f64().unwrap() - 1300.0 / 1900.0).abs() < 1e-6);
    // Daily series excludes the row with no recorded_at.
    let days: Vec<&str> = v["by_day"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["day"].as_str().unwrap())
        .collect();
    assert_eq!(days, ["2026-07-20", "2026-07-21", "2026-07-22"]);

    // Org filter narrows totals and steps the drill to workspace.
    let v: serde_json::Value = serde_json::from_slice(&acme.body).unwrap();
    assert_eq!(v["totals"]["calls"], 3);
    assert_eq!(v["drill"]["level"], "workspace");
    assert_eq!(v["drill"]["rows"][0]["key"], "ws1");

    // The `(local)` sentinel selects the NULL-org bucket.
    let v: serde_json::Value = serde_json::from_slice(&local.body).unwrap();
    assert_eq!(v["totals"]["calls"], 2);
    assert!((v["totals"]["cost_usd"].as_f64().unwrap() - 0.15).abs() < 1e-9);
    assert_eq!(v["drill"]["level"], "workspace");
    assert!(v["drill"]["rows"][0]["key"].is_null());

    // workspace set → drill by repo.
    let v: serde_json::Value = serde_json::from_slice(&repos.body).unwrap();
    assert_eq!(v["drill"]["level"], "repo");
    let repo_keys: Vec<&str> = v["drill"]["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["key"].as_str().unwrap())
        .collect();
    assert!(repo_keys.contains(&"r1") && repo_keys.contains(&"r2"));

    // repo set → drill to the project leaf, carrying its name.
    let v: serde_json::Value = serde_json::from_slice(&proj.body).unwrap();
    assert_eq!(v["drill"]["level"], "project");
    let leaf = &v["drill"]["rows"][0];
    assert_eq!(leaf["key"], "p1");
    assert_eq!(leaf["name"], "proj-one");
    assert_eq!(leaf["calls"], 2);
    assert!((leaf["cost_usd"].as_f64().unwrap() - 0.75).abs() < 1e-9);
}

#[test]
fn unknown_route_is_404_and_missing_id_is_400() {
    let ws = TempDir::new().unwrap();
    assert_eq!(respond(ws.path(), "/api/nope").status, "404 Not Found");
    assert_eq!(
        respond(ws.path(), "/api/execution").status,
        "400 Bad Request"
    );
    assert_eq!(
        respond(ws.path(), "/api/execution?id=abc").status,
        "400 Bad Request"
    );
}

#[test]
fn index_and_mark_are_embedded() {
    let ws = TempDir::new().unwrap();
    let index = respond(ws.path(), "/");
    assert_eq!(index.content_type, "text/html; charset=utf-8");
    // Lowercase deliberately: the comet brand kit writes "stella — observatory"
    // (docs/brand/README.md — lowercase always), and this pins the masthead to it.
    assert!(
        String::from_utf8(index.body)
            .unwrap()
            .contains("stella — observatory")
    );
    let mark = respond(ws.path(), "/assets/mark.svg");
    assert_eq!(mark.content_type, "image/svg+xml");
    // The header lockup needs both cuts; a missing wordmark route would
    // render as a broken image rather than failing loudly.
    let wordmark = respond(ws.path(), "/assets/wordmark.svg");
    assert_eq!(wordmark.content_type, "image/svg+xml");
    assert!(String::from_utf8(wordmark.body).unwrap().contains("<svg"));
}

/// The dashboard sends `encodeURIComponent` output, so a scope value with
/// a space, `&` or `/` arrives escaped. Comparing the still-encoded bytes
/// against the stored value silently drilled into an empty scope.
#[test]
fn query_param_percent_decodes_the_value() {
    let q = Some("org=acme%20corp&repo=owner%2Fname&workspace=a+b&empty=");
    assert_eq!(query_param(q, "org").as_deref(), Some("acme corp"));
    assert_eq!(query_param(q, "repo").as_deref(), Some("owner/name"));
    assert_eq!(query_param(q, "workspace").as_deref(), Some("a b"));
    assert_eq!(query_param(q, "empty"), None);
    assert_eq!(query_param(q, "absent"), None);
    assert_eq!(query_param(None, "org"), None);
    // `%20`/`+` decode to a real space: whitespace is a value, not an
    // absent filter.
    assert_eq!(query_param(Some("org=%20"), "org").as_deref(), Some(" "));
    assert_eq!(query_param(Some("org=+"), "org").as_deref(), Some(" "));
}

/// This parses attacker-reachable bytes, so a malformed escape must be
/// inert — never a panic, never a dropped byte. Multi-byte UTF-8 split
/// across escapes must reassemble; invalid bytes become U+FFFD.
#[test]
fn percent_decode_tolerates_malformed_escapes() {
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(percent_decode("%A"), "%A");
    assert_eq!(percent_decode("%ZZ"), "%ZZ");
    assert_eq!(percent_decode("a%%20b"), "a% b");
    // é is two bytes, escaped separately by encodeURIComponent.
    assert_eq!(percent_decode("caf%C3%A9"), "café");
    assert_eq!(percent_decode("%ff"), "\u{fffd}");
    assert_eq!(percent_decode("plain"), "plain");
}

/// The page must be fully self-contained: any http(s) URL in the HTML
/// would be an outbound fetch from the user's browser — a phone-home.
#[test]
fn dashboard_html_has_no_external_references() {
    for needle in ["http://", "https://", "//cdn", "@import", "integrity="] {
        assert!(
            !INDEX_HTML.contains(needle),
            "embedded dashboard must not reference {needle}"
        );
    }
}

#[tokio::test]
async fn serves_over_a_real_socket() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let ws = seeded_workspace();
    let root = ws.path().to_path_buf();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let _ = serve(root, 0, move |addr| {
            let _ = tx.send(addr);
        })
        .await;
    });
    let addr = rx.await.unwrap();
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        // A loopback Host: the DNS-rebinding gate (host_is_local, its
        // own unit test above) would 403 a non-local name.
        .write_all(b"GET /api/overview HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let mut body = String::new();
    stream.read_to_string(&mut body).await.unwrap();
    assert!(body.starts_with("HTTP/1.1 200 OK"));
    assert!(body.contains("\"runs\":2"));
    // The no-phone-home guarantee, enforced by the browser rather than
    // only by `dashboard_html_has_no_external_references`.
    assert!(
        body.contains("\r\nX-Content-Type-Options: nosniff\r\n"),
        "head was: {body}"
    );
    assert!(
        body.contains(&format!("\r\nContent-Security-Policy: {CSP}\r\n")),
        "head was: {body}"
    );
    server.abort();
}

/// The CSP must keep the embedded page working: it is one document with
/// inline script, an inline `<style>`, and inline `style="…"` attributes,
/// so dropping `'unsafe-inline'` from either directive would silently
/// break it. Same-origin `/assets/*.svg` needs `img-src 'self'`.
#[test]
fn csp_admits_everything_the_embedded_dashboard_actually_uses() {
    for directive in [
        "script-src 'self' 'unsafe-inline'",
        "style-src 'self' 'unsafe-inline'",
        "img-src 'self'",
        "connect-src 'self'",
        "frame-ancestors 'none'",
    ] {
        assert!(CSP.contains(directive), "CSP is missing {directive}");
    }
    // A header value may not carry a newline — it would split the head.
    assert!(!CSP.contains('\r') && !CSP.contains('\n'));
    assert!(INDEX_HTML.contains("<style>") && INDEX_HTML.contains("<script>"));
    // Every asset the page names must be same-origin *and* actually routed.
    // Asserted over whichever attribute carries it rather than a fixed
    // `src="…"`: the mark moved from an `<img src>` to a `<link rel=icon
    // href>`, which is the same same-origin fetch and the same CSP question,
    // but silently failed a literal-string check. What matters is that no
    // reference points off-origin and none points at a 404.
    let root = tempfile::tempdir().expect("temp workspace");
    for attr in ["src=\"", "href=\""] {
        for (_, rest) in INDEX_HTML
            .match_indices(attr)
            .map(|(i, m)| (i, &INDEX_HTML[i + m.len()..]))
        {
            let target = rest.split('"').next().unwrap_or_default();
            assert!(
                target.starts_with('/') || target.starts_with('#'),
                "the page must reference nothing off-origin, found {target:?}"
            );
            if target.starts_with('/') {
                assert_ne!(
                    respond(root.path(), target).status,
                    "404 Not Found",
                    "the page references {target:?}, which no route serves"
                );
            }
        }
    }
}

/// Padding the request line to the 8 KiB head cap used to be served: the
/// route still parsed out of the truncated path, while every header —
/// `Host` included — sat past the cap, and an absent Host is allowed. That
/// handed a rebound page the whole dashboard cross-origin. A head with no
/// terminator inside the cap is now refused before routing.
///
/// Sized to *exactly* the cap so the server consumes every byte the client
/// wrote: closing a socket with unread bytes still in its receive queue
/// sends an RST, which would race the client's read of the response.
#[tokio::test]
async fn unterminated_request_head_is_refused_not_routed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let ws = seeded_workspace();
    let root = ws.path().to_path_buf();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let _ = serve(root, 0, move |addr| {
            let _ = tx.send(addr);
        })
        .await;
    });
    let addr = rx.await.unwrap();
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let prefix = "GET /api/overview?pad=";
    let suffix = " HTTP/1.1\r\n";
    let pad = "a".repeat(8192 - prefix.len() - suffix.len());
    let request = format!("{prefix}{pad}{suffix}");
    assert_eq!(request.len(), 8192, "fills the cap without terminating");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut body = String::new();
    stream.read_to_string(&mut body).await.unwrap();
    assert!(
        body.starts_with("HTTP/1.1 431"),
        "an unterminated head must be refused, got: {}",
        body.lines().next().unwrap_or_default()
    );
    assert!(!body.contains("\"runs\""), "no telemetry may leak");
    server.abort();
}
