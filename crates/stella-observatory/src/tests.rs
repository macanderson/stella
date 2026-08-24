//! Tests for the observatory HTTP surface, request routing and the
//! filesystem/database views behind it. Split out of `lib.rs` to keep that
//! file under the 1500-line ratchet (`scripts/check-file-size.sh`).

use rusqlite::Connection;
use tempfile::TempDir;

use super::*;

/// The /api/execution* route tests -- a child module so it reaches the
/// shared `seeded_workspace` fixtures through this file's own `use super::*`.
mod execution_routes;

/// The raw HTTP surface tests -- a child module for the same reason.
mod http_surface;

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
/// `stella-store`'s shipped DDL also carries `executions.usage_complete` and
/// `usage_status`, and `telemetry.call_role` and `usage_complete` — columns
/// no query in this crate selects. The point of the subset is speed and
/// focus: these are routing and rendering tests, and they should not pay for
/// a migration run to assert that a query string parses.
///
/// `executions.session_id` used to sit in that list and no longer does: the
/// execution detail route serves it, so the transcript page can name the
/// session a deep-linked turn belongs to. A column this crate reads has to be
/// here or every route test 500s.
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
               finished_at TEXT, outcome TEXT, session_id TEXT,
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
               state TEXT NOT NULL DEFAULT 'ok'
                 CHECK(state IN ('running', 'ok', 'error', 'abandoned')),
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
               (kind, prompt, provider, model, outcome, session_id, cost_usd)
             VALUES
               ('run', 'add a function', 'zai', 'glm-5.2', 'completed', 'ses-1', 0.03),
               ('goal', 'make tests pass', 'local', 'llama', 'goal_unmet', NULL, 0.0);
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
               (execution_id, seq, name, ok, state, error, bytes_out, duration_ms)
             VALUES
               (1, 1, 'read_file', 1, 'ok', '', 2048, 12),
               (1, 2, 'edit_file', 1, 'ok', '', 64, 3),
               (2, 1, 'bash', 0, 'error', 'exit 1', 0, 40);
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
               recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               -- Store schema v23 (#2175).
               parse_error TEXT);
             INSERT INTO execution_reflection
               (execution_id, delivered, self_rating, what_to_improve)
             VALUES (1, 1, 8, 'read the failing test first');
             -- Execution 2 reflected and its response would not parse: a row
             -- with a `parse_error` and no assessment in it. Seeded in the
             -- SHARED fixture on purpose (#2443) — it used to live only in a
             -- private one because both readers of this table counted it as a
             -- reflection, which is the bug. Every route below now sees a
             -- store holding one graded turn and one ungraded one, so a
             -- regression that conflates them fails here rather than in a test
             -- written to look for it.
             INSERT INTO execution_reflection (execution_id, parse_error)
             VALUES (2, 'Let me analyze this turn. First, I should');
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
    //
    // The single tidy `reasoning` row is NOT the shape the store holds — real
    // reasoning arrives one row per streamed fragment, which is what
    // `journal::coalesce_reasoning` exists for. That case is seeded by the
    // test that owns it (`streamed_reasoning_fragments_fold_into_one_block_per_run`)
    // rather than here, because several tests below address these rows by seq
    // and inserting fragments would renumber them for no gain.
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
             (1, 5, 'text', '{"type":"text","delta":"added the function"}'),
             (1, 6, 'text', '{"type":"text","text":"and named it well"}'),
             (1, 7, 'file_change', '{"type":"file_change","path":"src/lib.rs","kind":"modified","added":2,"removed":1,"diff":"--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -3,3 +3,4 @@\n ctx\n-fn a() {}\n+fn a() -> u8 { 0 }\n+fn b() {}\n"}'),
             (1, 8, 'step_usage', '{"type":"step_usage","step":1,"role":"worker","provider":"zai","model":"glm-5.2","input_tokens":3200,"output_tokens":410,"cached_input_tokens":29100,"cache_write_tokens":1200,"reasoning_tokens":96,"estimated_input_tokens":203,"cost_usd":0.0134,"duration_ms":8400,"retries":0,"tool_calls":1,"complete":true,"finish_reason":"stop"}');"#,
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
        "/api/rules",
        "/api/reflections",
        // #2175: the lifecycle payload now also reads `store.db`'s
        // `execution_reflection.parse_error`, a column that arrives in schema
        // v23 — so an unmigrated store must blank that section, not 500.
        "/api/context-lifecycle",
    ] {
        let response = respond(ws.path(), route);
        assert_eq!(response.status, "200 OK", "route {route}");
    }
}

/// The route table and the embedded page must not drift apart. A route
/// nothing fetches is dead weight that still drags in its dependencies —
/// one sat unconsumed for exactly that long once (#640).
///
/// "The page" is `index.html` plus every script it loads from `/assets/`:
/// the self-driving agents view fetches its two routes from
/// `self_driving.js`, and a guard that read only the HTML would call a
/// consumed route dead.
#[test]
fn every_served_route_is_fetched_by_the_embedded_page() {
    let page = format!("{INDEX_HTML}\n{SELF_DRIVING_JS}");
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
        let fetched = page.match_indices(needle.as_str()).any(|(at, _)| {
            page[at + needle.len()..]
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

/// #2443: "ratings" means the model graded the turn, not that a row exists.
///
/// `execution_reflection` has three producers writing disjoint columns, and
/// only one of them records an assessment. A row left by finalize alone, or by
/// a reflection whose response would not parse, carries NULL verdict, NULL
/// rating and three empty strings — which the feed presented as a reflection
/// the model looked at and declined to grade. The Improve tab already answers
/// "did this turn reflect at all" from `parse_error` (#2175); this feed answers
/// the narrower question its name promises.
#[test]
fn the_ratings_feed_lists_only_turns_the_model_graded() {
    let ws = seeded_workspace();
    let conn = Connection::open(ws.path().join(".stella/private/store.db")).unwrap();
    // Two shapes the shared fixture does not carry, on either side of the line.
    // Execution 3 is prose with no numeric grade: an assessment, and what the
    // "what to improve" feed is built from, so the predicate must not key on
    // `self_rating` alone. Execution 4 is prose that is only whitespace, which
    // `SelfReviewRow::is_empty` trims before judging and the SQL mirror must
    // too — unreachable through the writer, which refuses an empty review, and
    // here because the mirror is the thing under test rather than the writer.
    conn.execute_batch(
        "INSERT INTO execution_reflection (execution_id, critique)
         VALUES (3, 'landed, but the first diagnosis was wrong');
         INSERT INTO execution_reflection (execution_id, what_went_well)
         VALUES (4, '  \t\n ');",
    )
    .unwrap();

    let v: serde_json::Value =
        serde_json::from_slice(&respond(ws.path(), "/api/reflections").body).unwrap();
    let ratings = v["ratings"].as_array().unwrap();
    let listed: Vec<i64> = ratings
        .iter()
        .map(|r| r["execution_id"].as_i64().unwrap())
        .collect();
    assert_eq!(
        listed,
        vec![1, 3],
        "execution 2 reflected unreadably and 4 said nothing — neither was \
         graded, so neither is a rating; the prose-only row is one"
    );

    // The same row, through the other projection that reads this table. A
    // Self-reflection panel of blanks in the execution drawer says the model
    // looked at the turn and had nothing to say; `null` says it never graded it.
    let ungraded: serde_json::Value =
        serde_json::from_slice(&respond(ws.path(), "/api/execution?id=2").body).unwrap();
    assert!(
        ungraded["reflection"].is_null(),
        "a parse-failure row is not a self-reflection: {:?}",
        ungraded["reflection"]
    );
    let graded: serde_json::Value =
        serde_json::from_slice(&respond(ws.path(), "/api/execution?id=1").body).unwrap();
    assert_eq!(
        graded["reflection"]["self_rating"], 8,
        "and a real self-review still reaches the drawer"
    );
}

/// The second-order half of #2443: an ungraded row must not spend the window.
///
/// The bound is applied by SQL `LIMIT`, so filtering after the fact would leave
/// a workspace whose newest `MAX_LISTED_REFLECTIONS` rows are all parse
/// failures — a plausible shape, since that is precisely what a broken
/// reflection loop produces every turn — showing an empty ratings chart while
/// real ratings sat one row past the horizon. The predicate runs before the
/// limit, so the window means "the newest N ratings".
#[test]
fn ungraded_rows_do_not_spend_the_ratings_window() {
    use crate::db::MAX_LISTED_REFLECTIONS;
    let ws = seeded_workspace();
    let conn = Connection::open(ws.path().join(".stella/private/store.db")).unwrap();
    conn.execute_batch("BEGIN").unwrap();
    {
        // Ids from 1000, all newer than the seeded graded turn: enough of them
        // to fill the window on their own.
        let mut stmt = conn
            .prepare(
                "INSERT INTO execution_reflection (execution_id, parse_error)
                 VALUES (?1, 'I will now analyze the turn. First,')",
            )
            .unwrap();
        for i in 0..MAX_LISTED_REFLECTIONS {
            stmt.execute([1000 + i as i64]).unwrap();
        }
    }
    conn.execute_batch("COMMIT").unwrap();

    let v: serde_json::Value =
        serde_json::from_slice(&respond(ws.path(), "/api/reflections").body).unwrap();
    let ratings = v["ratings"].as_array().unwrap();
    assert_eq!(
        ratings.len(),
        1,
        "500 unreadable reflections must not crowd out the one real rating"
    );
    assert_eq!(ratings[0]["execution_id"], 1);
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
               (execution_id, seq, name, ok, state, error, bytes_out, duration_ms, ts)
             VALUES (3, 1, 'bash', 0, 'error', 'boom', 0, 4, 'sometime');",
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
    assert_eq!(
        ratings.len(),
        1,
        "one of the fixture's two reflection rows is a parse failure, which is \
         not a rating (#2443)"
    );
    assert_eq!(ratings[0]["self_rating"], 8);
    assert_eq!(ratings[0]["what_to_improve"], "read the failing test first");
}

/// #2175: the Improve tab must be able to tell a starved learning loop from a
/// quiet one.
///
/// Both render an empty `proposals` list, and until this field existed that
/// was the whole of what the dashboard knew — so a workspace whose reflection
/// responses had been unparseable for nine days got the same "no proposals in
/// the ledger" copy as one opened five minutes ago. Its own fixture rather
/// than the shared one because it exercises the *absent* `context.db` path:
/// this route reads two databases and must still name the starved loop when
/// only `store.db` exists. (The shared fixture carries a parse-failure row
/// too, since #2443 — the routes that must not count it as a rating now say
/// so themselves.)
#[test]
fn the_lifecycle_payload_names_reflections_that_produced_no_readable_lessons() {
    let ws = TempDir::new().unwrap();
    let private = ws.path().join(".stella/private");
    std::fs::create_dir_all(&private).unwrap();
    let conn = Connection::open(private.join("store.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE execution_reflection (
           execution_id INTEGER PRIMARY KEY,
           recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           parse_error TEXT);
         INSERT INTO execution_reflection (execution_id, parse_error) VALUES
           (7, 'Let me analyze this turn step by step. First, I should'),
           (9, NULL);",
    )
    .unwrap();

    let body: serde_json::Value =
        serde_json::from_slice(&respond(ws.path(), "/api/context-lifecycle").body).unwrap();
    let starved = body["unreadable_reflections"].as_array().unwrap();
    assert_eq!(
        starved.len(),
        1,
        "a NULL parse_error is a turn that reflected cleanly, not a failure"
    );
    assert_eq!(starved[0]["execution_id"], 7);
    assert!(
        starved[0]["excerpt"]
            .as_str()
            .unwrap()
            .starts_with("Let me analyze"),
        "the excerpt is what tells an operator what shape the failure takes"
    );
    assert_eq!(
        body["proposals"].as_array().unwrap().len(),
        0,
        "and the ledger is empty — which is exactly the ambiguity this field \
         resolves: empty proposals PLUS a starved reflection is a broken loop, \
         empty proposals alone is a new workspace"
    );
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
fn projects_ships_only_selectable_rows_and_counts_the_rest() {
    let _env = crate::test_env::lock();
    let _restore = crate::test_env::EnvRestore::capture(&["STELLA_DATA_DIR"]);
    // The shape of a hub that has run the bench suites: a handful of live
    // workspaces buried under thousands of temp roots whose directory is gone.
    // Those rows carry real spend, so they stay in the hub — but the switcher
    // can never open one, and shipping them cost 868 KB per refresh (#3953).
    const DEAD: usize = 800;
    let home = TempDir::new().unwrap();
    let other = seeded_workspace();
    let data = TempDir::new().unwrap();
    let usage = Connection::open(data.path().join("usage.db")).unwrap();
    usage
        .execute_batch(
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
               PRIMARY KEY (project_id, source_rowid));",
        )
        .unwrap();
    // The live workspace, the serving workspace (registered but with no store
    // of its own yet — it still labels the switcher's "current" entry), and
    // the temp-root horde.
    usage
        .execute(
            "INSERT INTO projects VALUES (?1, 'other', ?2, '2026-01-01', '2026-01-02')",
            rusqlite::params!["feedbeef00000001", other.path().to_string_lossy()],
        )
        .unwrap();
    usage
        .execute(
            "INSERT INTO projects VALUES (?1, 'serving', ?2, '2026-01-01', '2026-01-03')",
            rusqlite::params![
                crate::global::project_id_for(home.path()),
                home.path().to_string_lossy()
            ],
        )
        .unwrap();
    let gone = home.path().join("never-created");
    for i in 0..DEAD {
        let id = format!("dead{i:012}");
        usage
            .execute(
                "INSERT INTO projects VALUES (?1, ?2, ?3, '2026-02-01', '2026-02-02')",
                rusqlite::params![
                    id,
                    format!("bench-run-{i}"),
                    gone.join(format!("run-{i}")).to_string_lossy()
                ],
            )
            .unwrap();
        // Spend the hub must keep accounting for after the directory is gone.
        usage
            .execute(
                "INSERT INTO telemetry
                   (project_id,source_rowid,org_id,workspace_id,repo_id,execution_id,step,
                    recorded_at,provider,call_role,model,input_tokens,estimated_input_tokens,
                    output_tokens,cache_read_tokens,cache_miss_tokens,cache_write_tokens,
                    cost_usd,duration_ms,retries,tool_calls,usage_complete)
                 VALUES (?1,1,NULL,NULL,'',1,0,'2026-02-02 10:00:00','anthropic','worker',
                         'claude-x',100,100,10,0,100,0,0.02,500,0,0,1)",
                rusqlite::params![id],
            )
            .unwrap();
    }
    drop(usage);
    // SAFETY: the env lock is held for the whole test, and `_restore` undoes
    // this on drop — including if a `respond` below panics, which would
    // otherwise leak a path into a TempDir about to be deleted.
    unsafe { std::env::set_var("STELLA_DATA_DIR", data.path()) };

    let listed = respond(home.path(), "/api/projects");
    let hub = respond(home.path(), "/api/hub-telemetry");
    let v: serde_json::Value = serde_json::from_slice(&listed.body).unwrap();

    let shipped = v["projects"].as_array().unwrap();
    assert_eq!(
        shipped.len(),
        2,
        "only the live store and the serving workspace are selectable"
    );
    assert!(
        shipped
            .iter()
            .all(|p| p["has_store"] == true || p["is_current"] == true),
        "no row the switcher is guaranteed to discard is worth serializing"
    );
    assert_eq!(v["known"], DEAD as u64 + 2, "the hub's history is reported");
    assert_eq!(
        v["omitted"], DEAD as u64,
        "and so is what was left out of it"
    );
    // The cost this endpoint exists to stop paying: at ~200 bytes a row the
    // unfiltered payload was ~160 KB for this fixture, and 868 KB on the
    // machine that filed #3953.
    assert!(
        listed.body.len() < 4_096,
        "the selector payload is bounded by live workspaces, not by hub \
         history — got {} bytes",
        listed.body.len()
    );

    // The other half of the contract: the hub aggregate still accounts for
    // every dead project, so no spend disappears from the Org tab.
    let h: serde_json::Value = serde_json::from_slice(&hub.body).unwrap();
    assert_eq!(h["totals"]["projects"], DEAD as i64);
    assert!((h["totals"]["cost_usd"].as_f64().unwrap() - DEAD as f64 * 0.02).abs() < 1e-6);
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

/// One workspace whose store holds only what the AGENTS panel reads: the
/// executions a session owns, and the invocation log under them.
/// `kind_column` selects the v30 shape or the one that predates it (#3822).
fn agent_uses_workspace(kind_column: bool) -> TempDir {
    let dir = TempDir::new().unwrap();
    let dot = dir.path().join(".stella");
    std::fs::create_dir_all(dot.join("private")).unwrap();
    let conn = Connection::open(dot.join("private/store.db")).unwrap();
    let kind_ddl = if kind_column {
        ", kind TEXT NOT NULL DEFAULT 'definition'"
    } else {
        ""
    };
    conn.execute_batch(&format!(
        "CREATE TABLE executions (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           kind TEXT NOT NULL, prompt TEXT NOT NULL,
           provider TEXT NOT NULL, model TEXT NOT NULL,
           started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
           finished_at TEXT, outcome TEXT, session_id TEXT,
           cost_usd REAL NOT NULL DEFAULT 0);
         CREATE TABLE agent_uses (
           execution_id INTEGER NOT NULL, agent TEXT NOT NULL,
           version INTEGER NOT NULL, reason TEXT NOT NULL DEFAULT '',
           ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP{kind_ddl});
         INSERT INTO executions
           (kind, prompt, provider, model, outcome, session_id, cost_usd)
         VALUES ('deck', 'ship it', 'zai', 'glm-5.2', 'completed', 'ses-1', 0.03);"
    ))
    .unwrap();
    let rows: &[(&str, &str)] = &[
        ("reviewer", "definition"),
        ("reviewer", "definition"),
        ("find-retry-policy", "delegation"),
        ("find-retry-policy-2", "delegation"),
        ("audit-the-budget-guard", "delegation"),
    ];
    for (agent, kind) in rows {
        if kind_column {
            conn.execute(
                "INSERT INTO agent_uses (execution_id, agent, version, kind) VALUES (1, ?, 1, ?)",
                rusqlite::params![agent, kind],
            )
            .unwrap();
        } else {
            conn.execute(
                "INSERT INTO agent_uses (execution_id, agent, version) VALUES (1, ?, 1)",
                rusqlite::params![agent],
            )
            .unwrap();
        }
    }
    dir
}

/// **The #3822 witness.** An installed agent definition is one counted group;
/// every `task` delegation collapses into one more.
///
/// `GROUP BY agent` over both writers rendered this fixture as `reviewer x 2`
/// beside three one-row groups named after whatever the model typed, and got
/// noisier the more the session delegated. A minted child id is the model's
/// prose, so the delegation group names none of them and reports how many
/// there were.
#[test]
fn the_agents_panel_counts_definitions_and_collapses_delegations() {
    let dir = agent_uses_workspace(true);
    let v: serde_json::Value =
        serde_json::from_slice(&respond(dir.path(), "/api/session?id=ses-1").body).unwrap();

    let agents = v["agents"].as_array().expect("agents panel");
    assert_eq!(
        agents.len(),
        2,
        "one definition group and one delegation group, never N singletons: {v}"
    );
    let definition = agents
        .iter()
        .find(|a| a["kind"] == "definition")
        .expect("the installed definition's group");
    assert_eq!(definition["agent"], "reviewer", "{definition}");
    assert_eq!(definition["uses"], 2, "{definition}");

    let delegations = agents
        .iter()
        .find(|a| a["kind"] == "delegation")
        .expect("the delegation group");
    assert!(delegations["agent"].is_null(), "{delegations}");
    assert_eq!(delegations["uses"], 3, "{delegations}");
    assert_eq!(delegations["distinct"], 3, "{delegations}");
}

/// **The #4567 witness.** A store whose `telemetry` predates
/// `cache_write_tokens` still lists its session's turns.
///
/// `session_turns` sums that column for the turn table's cache tooltip, and a
/// prepare over a missing column fails the whole query — the established
/// per-query degrade, but here it blanked the entire turn list to buy one
/// tooltip figure. The cross-project switcher (`?project=`) is exactly where
/// stores that old appear. The sum is probed per store and reads 0 when the
/// column is absent; everything else the query serves must still arrive.
#[test]
fn a_store_without_cache_write_tokens_still_lists_its_turns() {
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
           finished_at TEXT, outcome TEXT, session_id TEXT,
           cost_usd REAL NOT NULL DEFAULT 0);
         -- The pre-cache_write telemetry shape: every column the turn query
         -- reads except the one under test.
         CREATE TABLE telemetry (
           execution_id INTEGER NOT NULL, step INTEGER NOT NULL,
           provider TEXT NOT NULL, model TEXT NOT NULL,
           input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
           cache_read_tokens INTEGER NOT NULL,
           cache_miss_tokens INTEGER NOT NULL,
           cost_usd REAL NOT NULL, duration_ms INTEGER NOT NULL,
           retries INTEGER NOT NULL, tool_calls INTEGER NOT NULL);
         -- Present but empty: the turn query joins both, and a missing table
         -- fails the whole prepare — that degrade is not what this test is
         -- about.
         CREATE TABLE tool_calls (
           execution_id INTEGER NOT NULL, seq INTEGER NOT NULL,
           name TEXT NOT NULL, error TEXT NOT NULL DEFAULT '');
         CREATE TABLE files_touched (
           execution_id INTEGER NOT NULL, path TEXT NOT NULL,
           lines_added INTEGER NOT NULL DEFAULT 0,
           lines_removed INTEGER NOT NULL DEFAULT 0);
         INSERT INTO executions
           (kind, prompt, provider, model, outcome, session_id, cost_usd)
         VALUES ('run', 'add a function', 'zai', 'glm-5.2', 'completed',
                 'ses-1', 0.03);
         INSERT INTO telemetry
           (execution_id, step, provider, model, input_tokens, output_tokens,
            cache_read_tokens, cache_miss_tokens, cost_usd, duration_ms,
            retries, tool_calls)
         VALUES (1, 1, 'zai', 'glm-5.2', 1000, 200, 400, 600, 0.03, 1500, 0, 2);",
    )
    .unwrap();
    drop(conn);

    let v: serde_json::Value =
        serde_json::from_slice(&respond(dir.path(), "/api/session?id=ses-1").body).unwrap();
    let turns = v["turns"].as_array().expect("turns");
    assert_eq!(
        turns.len(),
        1,
        "the turn list must survive the old store: {v}"
    );
    assert_eq!(turns[0]["id"], 1);
    assert_eq!(
        turns[0]["input_tokens"], 1000,
        "the surviving sums stay real"
    );
    assert_eq!(
        turns[0]["cache_write_tokens"], 0,
        "the absent column reads as zero, not as an empty session: {v}"
    );
}

/// A store written before v30 cannot answer which writer minted a name, and
/// says so: the panel still lists what it has, with `kind` null. Degrading to
/// an empty panel would blank a surface that works today.
#[test]
fn a_store_without_the_kind_column_still_lists_its_agents_as_unknown() {
    let dir = agent_uses_workspace(false);
    let v: serde_json::Value =
        serde_json::from_slice(&respond(dir.path(), "/api/session?id=ses-1").body).unwrap();

    let agents = v["agents"].as_array().expect("agents panel");
    assert_eq!(agents.len(), 4, "the pre-v30 grouping, ungrouped: {v}");
    assert!(agents.iter().all(|a| a["kind"].is_null()), "{v}");
    assert_eq!(agents[0]["agent"], "reviewer", "{v}");
    assert_eq!(agents[0]["uses"], 2, "{v}");
}
