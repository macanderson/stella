// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `/api/context-records` and `/api/context-record` end to end. The use
//! ledger's bare ids are joined back to the words they name, in both
//! stores a record can live in.
//!
//! This lives in `tests/` for the reason `rules_ledger.rs` gives.
//! `src/tests.rs` sits at the file-size ratchet, and this crate has no
//! baseline entry. New route coverage lands in a sibling.
//!
//! The fixture is hand-written DDL for the subset of both databases these
//! routes read. `seeded_workspace` strikes the same bargain for
//! `store.db`. The real-migration half lives in
//! `tests/schema_conformance/`. It builds `context.db` through
//! `stella-context`'s own writer. That proves the `node` and `memory`
//! queries here still resolve against it.

use std::path::Path;

use rusqlite::Connection;
use stella_observatory::respond;

/// The recall node the ledger's uses name, and the memory row behind it.
const NODE: &str = "nod_e7e2e23a62d124769c55e695";
const MEMORY: &str = "mem_ffdf73a1859e2ec5726363a0";
const STATEMENT: &str = "Ranged reads are counted separately from full reads. \
                         A ranged read followed by a full read is two reads.";
/// A published record the ledger cites by handle.
const HANDLE: &str = "^pre-push-runs-gate";
/// A published record nothing has rendered yet.
const IDLE_LINEAGE: &str = "ctx.demo.idle-rule";

/// A workspace with three records. A recall node was rendered into two
/// turns and judged twice, once helpful and once not. A published record
/// was rendered once and never judged. A second published record was
/// never rendered at all.
fn seeded(root: &Path) {
    let private = root.join(".stella/private");
    std::fs::create_dir_all(&private).unwrap();

    // context.db: the recall graph plus the use ledger.
    let ctx = Connection::open(private.join("context.db")).unwrap();
    ctx.execute_batch(
        "CREATE TABLE node (
           id INTEGER PRIMARY KEY, public_id TEXT NOT NULL UNIQUE, kind TEXT NOT NULL,
           display_name TEXT NOT NULL, content TEXT NOT NULL DEFAULT '',
           content_hash TEXT NOT NULL, uri TEXT, properties TEXT NOT NULL DEFAULT '{}',
           valid_from TEXT, valid_to TEXT, recorded_at TEXT NOT NULL, superseded_at TEXT,
           recall_tier INTEGER NOT NULL DEFAULT 0);
         CREATE TABLE memory (
           id INTEGER PRIMARY KEY, public_id TEXT NOT NULL UNIQUE, kind TEXT NOT NULL,
           content TEXT NOT NULL, recorded_at TEXT NOT NULL, lineage_id TEXT, superseded_at TEXT);
         CREATE TABLE domain (
           id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, description TEXT,
           recorded_at TEXT NOT NULL);
         CREATE TABLE node_domains (
           node_id INTEGER NOT NULL, domain_id INTEGER NOT NULL,
           PRIMARY KEY (node_id, domain_id));
         CREATE TABLE context_records (
           record_id TEXT PRIMARY KEY, lineage_id TEXT, record_kind TEXT NOT NULL,
           record_hash TEXT, schema_version TEXT, body TEXT NOT NULL,
           observed_at TEXT, recorded_at TEXT, supersedes TEXT);",
    )
    .unwrap();
    ctx.execute(
        "INSERT INTO node (id, public_id, kind, display_name, content, content_hash, uri, recorded_at)
         VALUES (1, ?1, 'memory', ?2, ?2, 'h', ?3, '2026-08-18T23:12:27Z')",
        rusqlite::params![NODE, STATEMENT, format!("memory://{MEMORY}")],
    )
    .unwrap();
    ctx.execute(
        "INSERT INTO memory (id, public_id, kind, content, recorded_at, lineage_id)
         VALUES (1, ?1, 'reflection', ?2, '2026-08-18T23:12:27Z', ?1)",
        rusqlite::params![MEMORY, STATEMENT],
    )
    .unwrap();
    ctx.execute_batch(
        "INSERT INTO domain (id, name, recorded_at) VALUES (1, 'tools-plugins', '2026-08-01T00:00:00Z');
         INSERT INTO domain (id, name, recorded_at) VALUES (2, 'agent-engine', '2026-08-01T00:00:00Z');
         INSERT INTO node_domains VALUES (1, 1); INSERT INTO node_domains VALUES (1, 2);",
    )
    .unwrap();

    let append = |id: &str, kind: &str, body: serde_json::Value| {
        ctx.execute(
            "INSERT INTO context_records (record_id, record_kind, body) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, kind, body.to_string()],
        )
        .unwrap();
    };
    for (n, task) in [(1, "session:ses-1"), (2, "session:ses-2")] {
        append(
            &format!("cu_node_{n}"),
            "context_use",
            serde_json::json!({
                "use_kind": "rendered", "context_record_id": NODE,
                "use_trace_id": format!("ut_{n}"), "task_id": task,
                "influence_stage": "none", "observed_at": format!("2026-09-0{n}T10:00:00Z"),
            }),
        );
    }
    for (n, verdict) in [(1, "helpful"), (2, "not_helpful")] {
        append(
            &format!("fb_node_{n}"),
            "context_use_feedback",
            serde_json::json!({
                "context_use_id": format!("cu_node_{n}"), "use_trace_id": format!("ut_{n}"),
                "task_id": format!("session:ses-{n}"), "evaluation": verdict,
                "had_opportunity": true, "influence_stage": "execution",
                "outcome_relation": "unknown",
                "observable_effect_refs": ["path-missing:docs/x.md"],
                "evaluation_method": "deterministic_validation",
                "attribution_confidence": 100,
                "influence_statement": "the cited path is missing",
                "observed_at": format!("2026-09-0{n}T10:05:00Z"),
            }),
        );
    }
    append(
        "cu_handle_1",
        "context_use",
        serde_json::json!({
            "use_kind": "rendered", "context_record_id": HANDLE,
            "use_trace_id": "ut_3", "task_id": "session:ses-1",
            "influence_stage": "none", "observed_at": "2026-09-03T10:00:00Z",
        }),
    );

    // store.db: the turns those uses were folded from.
    let store = Connection::open(private.join("store.db")).unwrap();
    store
        .execute_batch(
            "CREATE TABLE executions (
               id INTEGER PRIMARY KEY, kind TEXT NOT NULL, prompt TEXT NOT NULL,
               provider TEXT NOT NULL, model TEXT NOT NULL,
               started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               finished_at TEXT, outcome TEXT, session_id TEXT,
               cost_usd REAL NOT NULL DEFAULT 0);
             CREATE TABLE context_blocks (
               execution_id INTEGER NOT NULL, block_id TEXT NOT NULL, kind TEXT NOT NULL,
               origin_turn INTEGER NOT NULL, origin_step INTEGER NOT NULL, call_id TEXT,
               memory_id TEXT, token_cost INTEGER, content_digest TEXT NOT NULL,
               citation_label TEXT, content TEXT,
               first_seen_ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               PRIMARY KEY (execution_id, block_id));
             INSERT INTO executions (id, kind, prompt, provider, model, outcome, session_id)
               VALUES (7, 'run', 'fix the ranged read cache', 'anthropic', 'claude', 'completed', 'ses-1');
             INSERT INTO executions (id, kind, prompt, provider, model, outcome, session_id)
               VALUES (9, 'run', 'audit the telemetry counters', 'anthropic', 'claude', 'failed', 'ses-2');",
        )
        .unwrap();
    for (execution, memory_id, tokens, content) in [
        (7, NODE, 187, format!("- [{NODE}] {STATEMENT}")),
        (9, NODE, 187, format!("- [{NODE}] {STATEMENT}")),
        (
            7,
            HANDLE,
            40,
            format!("- The pre-push hook runs the gate. {HANDLE}"),
        ),
    ] {
        store
            .execute(
                "INSERT INTO context_blocks (execution_id, block_id, kind, origin_turn, origin_step,
                   memory_id, token_cost, content_digest, citation_label, content, first_seen_ts)
                 VALUES (?1, ?2, 'recalled_frame', 0, 1, ?3, ?4, 'sha256:x', ?5, ?5, ?6)",
                rusqlite::params![
                    execution,
                    format!("blk_{execution}_{}", memory_id.len()),
                    memory_id,
                    tokens,
                    content,
                    format!("2026-09-0{}T10:00:00Z", if execution == 7 { 1 } else { 2 }),
                ],
            )
            .unwrap();
    }

    // .stella/rules: the published records. The ledger cites one, and
    // nothing has rendered the other.
    let rules = root.join(".stella/rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(
        rules.join("ctx.demo.pre-push-runs-gate.toml"),
        r#"
schema = "context-record/v0.1"
set_id = "demo"

[defaults]
origin = "imported"
status = "active"

[[record]]
lineage_id = "ctx.demo.pre-push-runs-gate"
record_id  = "rec_demo_pre_push_runs_gate_0123456789ab"
kind       = "rule"
statement  = "The pre-push hook runs the full gate on every push."
tags       = ["ci", "hooks"]

  [record.steering]
  force      = "must"
  precedence = 80

  [record.steering.applies_to]
  paths    = [".githooks/**"]
  keywords = ["push"]

  [record.enforcement]
  mode = "soft"
"#,
    )
    .unwrap();
    std::fs::write(
        rules.join("ctx.demo.idle-rule.toml"),
        r#"
schema = "context-record/v0.1"
set_id = "demo"

[[record]]
lineage_id = "ctx.demo.idle-rule"
kind       = "constraint"
statement  = "Never widen deny.toml to turn a gate green."
status     = "active"
"#,
    )
    .unwrap();
}

fn get(root: &Path, route: &str) -> serde_json::Value {
    let response = respond(root, route);
    assert_eq!(response.status, "200 OK", "{route}");
    serde_json::from_slice(&response.body).unwrap()
}

fn record<'a>(list: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    list["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("no record {id} in {list}"))
}

/// **Witness.** The list names what the ledger only numbered. One row
/// holds the recall node's own statement, its domains, and its fold. A
/// published record the ledger cites by handle sits beside it, with the
/// file's fields. Fails on main, where the route does not exist.
#[test]
fn the_list_joins_ledger_ids_to_the_words_they_name() {
    let ws = tempfile::TempDir::new().unwrap();
    seeded(ws.path());
    let v = get(ws.path(), "/api/context-records");
    assert_eq!(v["present"], true, "{v}");

    let node = record(&v, NODE);
    assert_eq!(node["plane"], "recall");
    assert_eq!(node["kind"], "memory");
    assert_eq!(
        node["origin"], "reflection",
        "the memory row's kind: {node}"
    );
    assert_eq!(
        node["title"],
        "Ranged reads are counted separately from full reads"
    );
    assert_eq!(node["statement"], STATEMENT);
    assert_eq!(node["health"]["uses"], 2);
    assert_eq!(node["health"]["distinct_tasks"], 2);
    assert_eq!(node["health"]["helpful"], 1);
    assert_eq!(node["health"]["not_helpful"], 1);
    assert_eq!(node["health"]["eligible_assessed"], 2);
    assert_eq!(
        node["standing"], "unassessed",
        "two verdicts are under the floor of five"
    );
    assert_eq!(node["last_used"], "2026-09-02T10:00:00Z");
    assert_eq!(
        node["rendered_turns"], 2,
        "from the store's receipts: {node}"
    );
    assert_eq!(node["prompt_tokens"], 374);

    let rule = record(&v, HANDLE);
    assert_eq!(rule["plane"], "published");
    assert_eq!(rule["kind"], "rule");
    assert_eq!(rule["origin"], "imported", "read from [defaults]: {rule}");
    assert_eq!(
        rule["published"]["lineage_id"],
        "ctx.demo.pre-push-runs-gate"
    );
    assert_eq!(
        rule["published"]["record_id"],
        "rec_demo_pre_push_runs_gate_0123456789ab"
    );
    assert_eq!(rule["published"]["status"], "active");
    assert_eq!(rule["published"]["precedence"], 80);
    assert_eq!(rule["published"]["applies_to"]["paths"][0], ".githooks/**");
    assert_eq!(rule["published"]["steering_force"], "must");
    assert_eq!(rule["health"]["uses"], 1);
    assert_eq!(rule["standing"], "unassessed");
    assert_eq!(rule["rendered_turns"], 1);
}

/// A published record nothing has rendered still steers the workspace,
/// so it is listed. It reads `unused`, carries the zero fold, and sorts
/// after every record that has evidence.
#[test]
fn a_published_record_nothing_rendered_is_listed_as_unused_and_last() {
    let ws = tempfile::TempDir::new().unwrap();
    seeded(ws.path());
    let v = get(ws.path(), "/api/context-records");
    let idle = record(&v, "^idle-rule");
    assert_eq!(idle["standing"], "unused");
    assert_eq!(idle["health"]["uses"], 0);
    assert_eq!(idle["published"]["lineage_id"], IDLE_LINEAGE);
    assert!(idle["last_used"].is_null());
    let order: Vec<&str> = v["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["standing"].as_str().unwrap())
        .collect();
    assert_eq!(order.last(), Some(&"unused"), "{order:?}");
    assert_eq!(v["totals"]["records"], 3);
    assert_eq!(v["totals"]["unused"], 1);
    assert_eq!(v["totals"]["unassessed"], 2);
    assert_eq!(v["totals"]["uses"], 3);
    assert_eq!(v["totals"]["tasks"], 2);
}

/// **Witness.** One record's page. It carries every use with the verdict
/// the ledger holds for it. It lists the turns the record was rendered
/// into, with their prompts and outcomes. It names the record it was
/// rendered beside. For a recalled memory it shows the memory row and
/// the rendered block.
#[test]
fn a_recalled_memory_page_carries_uses_verdicts_turns_company_and_source() {
    let ws = tempfile::TempDir::new().unwrap();
    seeded(ws.path());
    let v = get(ws.path(), &format!("/api/context-record?id={NODE}"));
    assert_eq!(v["found"], true, "{v}");
    assert_eq!(v["record"]["id"], NODE);
    assert_eq!(v["record"]["content"], STATEMENT);
    assert_eq!(
        v["record"]["domains"],
        serde_json::json!(["agent-engine", "tools-plugins"])
    );

    let uses = v["uses"].as_array().unwrap();
    assert_eq!(uses.len(), 2);
    assert_eq!(
        uses[0]["observed_at"], "2026-09-02T10:00:00Z",
        "newest first: {v}"
    );
    assert_eq!(uses[0]["task_id"], "session:ses-2");
    assert_eq!(uses[0]["verdict"]["evaluation"], "not_helpful");
    assert_eq!(uses[0]["verdict"]["method"], "deterministic_validation");
    assert_eq!(uses[0]["verdict"]["confidence"], 100);
    assert_eq!(uses[0]["verdict"]["effects"][0], "path-missing:docs/x.md");
    assert_eq!(uses[0]["verdict"]["statement"], "the cited path is missing");
    assert_eq!(uses[1]["verdict"]["evaluation"], "helpful");

    let turns = v["renderings"].as_array().unwrap();
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0]["execution_id"], 9, "newest first: {v}");
    assert_eq!(turns[0]["prompt"], "audit the telemetry counters");
    assert_eq!(turns[0]["outcome"], "failed");
    assert_eq!(turns[0]["prompt_tokens"], 187);
    assert_eq!(turns[1]["execution_id"], 7);
    assert_eq!(turns[1]["session_id"], "ses-1");

    let related = v["related"].as_array().unwrap();
    assert_eq!(related.len(), 1);
    assert_eq!(related[0]["id"], HANDLE);
    assert_eq!(
        related[0]["title"],
        "The pre-push hook runs the full gate on every push"
    );
    assert_eq!(related[0]["shared_turns"], 1);

    assert_eq!(v["source"]["kind"], "recall");
    assert_eq!(v["source"]["uri"], format!("memory://{MEMORY}"));
    assert_eq!(v["source"]["memory"]["id"], MEMORY);
    assert_eq!(v["source"]["memory"]["kind"], "reflection");
    assert_eq!(v["source"]["text"], STATEMENT);
    assert_eq!(
        v["source"]["as_rendered"],
        format!("- [{NODE}] {STATEMENT}")
    );
    let commands: Vec<&str> = v["source"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["run"].as_str().unwrap())
        .collect();
    assert!(
        commands
            .iter()
            .any(|c| c.starts_with(&format!("stella memory retire {NODE}"))),
        "{commands:?}"
    );
    assert!(
        commands
            .iter()
            .any(|c| c.starts_with(&format!("stella memory edit {NODE}"))),
        "{commands:?}"
    );
}

/// A published record's source is the whole file it was read from. The
/// commands are the `stella context` ones. Any of its names finds it.
/// That is the handle, the lineage, or the record id.
#[test]
fn a_published_record_page_serves_its_toml_by_any_of_its_names() {
    let ws = tempfile::TempDir::new().unwrap();
    seeded(ws.path());
    for id in [
        HANDLE,
        "pre-push-runs-gate",
        "ctx.demo.pre-push-runs-gate",
        "rec_demo_pre_push_runs_gate_0123456789ab",
    ] {
        let v = get(ws.path(), &format!("/api/context-record?id={id}"));
        assert_eq!(v["found"], true, "{id}: {v}");
        assert_eq!(v["record"]["id"], HANDLE, "{id}");
        assert_eq!(v["source"]["kind"], "published");
        assert_eq!(v["source"]["language"], "toml");
        assert!(
            v["source"]["path"]
                .as_str()
                .unwrap()
                .ends_with("ctx.demo.pre-push-runs-gate.toml"),
            "{v}"
        );
        assert!(
            v["source"]["text"]
                .as_str()
                .unwrap()
                .contains("lineage_id = \"ctx.demo.pre-push-runs-gate\""),
            "{v}"
        );
        assert!(
            v["source"]["commands"][0]["run"]
                .as_str()
                .unwrap()
                .contains("stella context explain ^pre-push-runs-gate"),
            "{v}"
        );
    }
    let v = get(ws.path(), &format!("/api/context-record?id={HANDLE}"));
    assert_eq!(v["uses"].as_array().unwrap().len(), 1);
    assert!(
        v["uses"][0]["verdict"].is_null(),
        "nothing has judged it: {v}"
    );
    assert_eq!(v["renderings"][0]["execution_id"], 7);
    assert_eq!(v["related"][0]["id"], NODE);
    assert_eq!(
        v["source"]["as_rendered"],
        format!("- The pre-push hook runs the gate. {HANDLE}")
    );
}

/// Missing is a state. An id nothing names answers `found: false` as a
/// 200. A malformed id is refused before it reaches a query.
#[test]
fn an_unknown_record_is_found_false_and_a_malformed_id_is_a_400() {
    let ws = tempfile::TempDir::new().unwrap();
    seeded(ws.path());
    let v = get(ws.path(), "/api/context-record?id=nod_nobody");
    assert_eq!(v["found"], false, "{v}");
    assert_eq!(v["id"], "nod_nobody");
    for bad in [
        "/api/context-record",
        "/api/context-record?id=",
        "/api/context-record?id=a%20b",
        "/api/context-record?id=x%22%3B%20DROP",
    ] {
        let response = respond(ws.path(), bad);
        assert_eq!(response.status, "400 Bad Request", "{bad}");
    }
}

/// A workspace with published records and no databases still lists them.
/// Every one reads `unused`. A record page still serves the file. The
/// store and the ledger are each a state, not a precondition.
#[test]
fn a_workspace_with_rules_and_no_databases_still_lists_and_serves_them() {
    let ws = tempfile::TempDir::new().unwrap();
    let rules = ws.path().join(".stella/rules");
    std::fs::create_dir_all(&rules).unwrap();
    std::fs::write(
        rules.join("ctx.demo.only.toml"),
        "schema = \"context-record/v0.1\"\nset_id = \"demo\"\n\n[[record]]\nlineage_id = \"ctx.demo.only\"\nkind = \"rule\"\nstatement = \"One rule.\"\n",
    )
    .unwrap();
    let v = get(ws.path(), "/api/context-records");
    assert_eq!(v["present"], false);
    assert_eq!(v["totals"]["records"], 1);
    assert_eq!(v["records"][0]["id"], "^only");
    assert_eq!(v["records"][0]["standing"], "unused");
    let d = get(ws.path(), "/api/context-record?id=^only");
    assert_eq!(d["found"], true, "{d}");
    assert_eq!(d["record"]["content"], "One rule.");
    assert!(d["uses"].as_array().unwrap().is_empty());
    assert!(d["renderings"].as_array().unwrap().is_empty());
    assert_eq!(d["source"]["kind"], "published");
}
