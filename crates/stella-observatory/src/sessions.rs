//! The sessions view: the cross-process session registry joined with the
//! store's per-session telemetry rollups — the list a reader drills from
//! session → turn → step → tool call.
//!
//! Two sources, deliberately kept apart until the final merge:
//!
//! - **The registry** (`~/.stella/sessions/<id>.json`, one JSON file per
//!   session — see `stella_store::sessions` for the write side) carries what
//!   only the owning process knew: title, summary, status, liveness. Read
//!   here the same way the deck's SESSIONS overlay reads it: sweep the
//!   directory, skip what doesn't parse, and downgrade a live status whose
//!   owning pid is gone to `error` ("crashed") without rewriting the dead
//!   process's file.
//! - **The store** (`executions.session_id`, stamped since schema v8, served
//!   by the `executions_by_session` index) carries what actually happened:
//!   turns, tokens, spend, tool traffic, files, skills, MCP calls.
//!
//! A session can exist in either source alone. A just-started session has a
//! registry record and no stamped executions yet; a pruned registry loses the
//! record while the store still remembers every turn. Both shapes are listed
//! — `registry: false` marks the latter, with the session's first prompt
//! standing in for the title the registry no longer holds.
//!
//! The turn-by-turn drill stays per-execution by design: a session's deeper
//! layers (transcript, sent context, prompt diff) are served by the existing
//! `/api/execution-journal` and `/api/execution-context` routes one execution
//! at a time, which is the sanctioned shape for `events` reads — a
//! session-wide transcript query would be exactly the cross-execution `WHERE`
//! the journal rule forbids (see `crate::db::Observatory::execution_journal`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::db::{DbError, is_missing_schema, merge};

/// Sessions returned by [`sessions`]. Same shape of reasoning as
/// `MAX_LISTED_EXECUTIONS`: the page re-fetches the list on every refresh, so
/// an unbounded listing would make each poll pay for the machine's whole
/// session history; 200 sessions is months of work on a busy workspace.
const MAX_LISTED_SESSIONS: usize = 200;

/// Registry files swept per request. A bound, not a limit anyone should
/// reach — the registry is pruned opportunistically by the deck driver, so a
/// directory this full means something else went wrong, and a synchronous
/// request must not stall on it.
const MAX_REGISTRY_RECORDS: usize = 500;

/// Newest turns returned by [`session_detail`]. The drill fetches one
/// session, not the store's history, but a perpetual session (a self-driving
/// loop's, say) can accumulate turns without bound; the payload says when it
/// clipped (`turns_truncated`) instead of presenting the window as the whole.
const MAX_SESSION_TURNS: usize = 500;

/// `~/.stella/sessions` — where every session announces itself.
///
/// Resolved through `stella-home` rather than re-derived, so `STELLA_HOME`
/// moves the observatory's view and the registry's writes together.
fn registry_dir() -> PathBuf {
    stella_home::data_dir().join("sessions")
}

/// Whether `pid` is a live process. Unix: `kill(pid, 0)` (EPERM still means
/// alive). Elsewhere: assume alive (no downgrade — better to show a stale
/// in-progress row than to mislabel a live session as crashed).
///
/// An **acknowledged copy** of `stella_store::sessions::pid_alive`, the same
/// bargain `sent_context`'s reconstruction fold takes: this crate re-reads
/// artifacts instead of linking the crates that write them (see the crate
/// README's acknowledged-copies paragraph). The semantics being copied are
/// load-bearing: a pid that does not fit `pid_t` must read as dead — an `as`
/// cast would wrap it negative, and `kill(-N, 0)` probes process *group* N,
/// which can spuriously report alive.
fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        if pid == 0 {
            return false;
        }
        let rc = unsafe { libc::kill(pid, 0) };
        rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// A registry id that is safe to use as a filename. Session ids are minted
/// `ses-<ms>-<pid>`, so anything outside `[A-Za-z0-9._-]` (or starting with a
/// dot) is not an id — and this is the one place a client-supplied string
/// meets the filesystem, so the check is a gate, not a formality.
fn safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Whether a registry record's `workspace` names the workspace being served.
///
/// Canonicalized when the recorded path still exists (so a symlinked checkout
/// matches), string-compared when it doesn't (a deleted workspace's sessions
/// should still not leak into another project's list).
fn same_workspace(recorded: &str, canon_root: &Path) -> bool {
    if recorded.is_empty() {
        return false;
    }
    match std::fs::canonicalize(recorded) {
        Ok(c) => c == canon_root,
        Err(_) => Path::new(recorded) == canon_root,
    }
}

/// The status a record is *presented* with: a live claim whose owning process
/// is gone reads as `error` ("crashed"), everything else as stored. Returns
/// `(status, live, crashed)` — mirroring `SessionRegistry::presented_status`.
fn presented_status(record: &Value) -> (String, bool, bool) {
    let stored = record
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("error");
    let claims_live = matches!(stored, "in_progress" | "needs_input");
    let pid = record
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|p| u32::try_from(p).ok())
        .unwrap_or(0);
    if claims_live && !pid_alive(pid) {
        ("error".to_string(), false, true)
    } else {
        (stored.to_string(), claims_live, false)
    }
}

/// The registry records belonging to `workspace_root`, plus how many files
/// were present but unusable (reported, never silently dropped — a damaged
/// record very likely still has a recoverable sidecar behind it).
fn registry_records(workspace_root: &Path) -> (Vec<Value>, usize) {
    let Ok(entries) = std::fs::read_dir(registry_dir()) else {
        return (Vec::new(), 0);
    };
    let canon_root =
        std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
    let mut records = Vec::new();
    let mut damaged = 0;
    for entry in entries.flatten().take(MAX_REGISTRY_RECORDS) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            damaged += 1;
            continue;
        };
        let Ok(record) = serde_json::from_str::<Value>(&text) else {
            damaged += 1;
            continue;
        };
        if !record.is_object() {
            damaged += 1;
            continue;
        }
        let workspace = record
            .get("workspace")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if same_workspace(workspace, &canon_root) {
            records.push(record);
        }
    }
    (records, damaged)
}

/// One registry record read by exact id — the detail view's half of
/// [`registry_records`], without the directory sweep.
fn registry_record(id: &str) -> Option<Value> {
    if !safe_session_id(id) {
        return None;
    }
    let text = std::fs::read_to_string(registry_dir().join(format!("{id}.json"))).ok()?;
    serde_json::from_str::<Value>(&text)
        .ok()
        .filter(Value::is_object)
}

/// The start-time milliseconds a session id itself carries (`ses-<ms>-<pid>`),
/// used as the sort key for sessions whose registry record is gone. Zero for
/// an id in any other shape — those sort last, which is where a session with
/// no other timestamp belongs.
fn ms_from_id(id: &str) -> u64 {
    id.split('-')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// The store's per-session rollup: one aggregate row per stamped session over
/// the three tables every store has carried since long before `session_id`
/// existed (telemetry, tool_calls, files_touched). Skills and MCP counts ride
/// in separately ([`per_session_counts`]) so a store predating those tables
/// blanks those two numbers, not the whole list.
fn session_rollups(conn: &Connection) -> Result<BTreeMap<String, Value>, DbError> {
    let sql = format!(
        "SELECT e.session_id,
                count(*),
                count(*) FILTER (WHERE e.outcome = 'completed'),
                coalesce(sum(e.cost_usd), 0),
                min(e.started_at), max(coalesce(e.finished_at, e.started_at)),
                min(e.id), max(e.id),
                coalesce(sum(t.input_tokens), 0),
                coalesce(sum(t.output_tokens), 0),
                coalesce(sum(t.cache_read_tokens), 0),
                coalesce(sum(t.cache_miss_tokens), 0),
                coalesce(sum(t.retries), 0),
                coalesce(sum(t.duration_ms), 0),
                coalesce(sum(tc.calls), 0),
                coalesce(sum(tc.errors), 0),
                coalesce(sum(ft.files), 0),
                coalesce(sum(ft.added), 0),
                coalesce(sum(ft.removed), 0)
         FROM executions e
         LEFT JOIN (SELECT execution_id,
                           sum(input_tokens)      AS input_tokens,
                           sum(output_tokens)     AS output_tokens,
                           sum(cache_read_tokens) AS cache_read_tokens,
                           sum(cache_miss_tokens) AS cache_miss_tokens,
                           sum(retries)           AS retries,
                           sum(duration_ms)       AS duration_ms
                    FROM telemetry GROUP BY execution_id) t
           ON t.execution_id = e.id
         LEFT JOIN (SELECT execution_id, count(*) AS calls,
                           count(*) FILTER (WHERE error <> '') AS errors
                    FROM tool_calls GROUP BY execution_id) tc
           ON tc.execution_id = e.id
         LEFT JOIN (SELECT execution_id, count(*) AS files,
                           sum(lines_added)  AS added,
                           sum(lines_removed) AS removed
                    FROM files_touched GROUP BY execution_id) ft
           ON ft.execution_id = e.id
         WHERE e.session_id IS NOT NULL AND e.session_id <> ''
         GROUP BY e.session_id
         ORDER BY max(e.id) DESC
         LIMIT {MAX_LISTED_SESSIONS}"
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(BTreeMap::new()),
        Err(e) => return Err(e.into()),
    };
    let mapped = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            json!({
                "turns": r.get::<_, i64>(1)?,
                "resolved": r.get::<_, i64>(2)?,
                "cost_usd": r.get::<_, f64>(3)?,
                "first_started_at": r.get::<_, String>(4)?,
                "last_activity_at": r.get::<_, String>(5)?,
                "first_id": r.get::<_, i64>(6)?,
                "last_id": r.get::<_, i64>(7)?,
                "input_tokens": r.get::<_, i64>(8)?,
                "output_tokens": r.get::<_, i64>(9)?,
                "cache_read_tokens": r.get::<_, i64>(10)?,
                "cache_miss_tokens": r.get::<_, i64>(11)?,
                "retries": r.get::<_, i64>(12)?,
                "model_ms": r.get::<_, i64>(13)?,
                "tool_calls": r.get::<_, i64>(14)?,
                "tool_errors": r.get::<_, i64>(15)?,
                "files_touched": r.get::<_, i64>(16)?,
                "lines_added": r.get::<_, i64>(17)?,
                "lines_removed": r.get::<_, i64>(18)?,
            }),
        ))
    })?;
    let mut out = BTreeMap::new();
    for row in mapped {
        let (id, rollup) = row?;
        out.insert(id, rollup);
    }
    Ok(out)
}

/// The rollup shape for a session the store has no stamped turns for — every
/// aggregate zero, so the page renders one row schema everywhere.
fn empty_rollup() -> Value {
    json!({
        "turns": 0, "resolved": 0, "cost_usd": 0.0,
        "first_started_at": "", "last_activity_at": "",
        "first_id": 0, "last_id": 0,
        "input_tokens": 0, "output_tokens": 0,
        "cache_read_tokens": 0, "cache_miss_tokens": 0,
        "retries": 0, "model_ms": 0,
        "tool_calls": 0, "tool_errors": 0,
        "files_touched": 0, "lines_added": 0, "lines_removed": 0,
    })
}

/// Per-session counts from one younger table (`skill_usage`, `mcp_usage`),
/// folded separately so a store predating the table degrades this number to
/// zero instead of blanking the whole listing.
fn per_session_counts(conn: &Connection, table: &str) -> Result<BTreeMap<String, i64>, DbError> {
    let sql = format!(
        "SELECT e.session_id, count(*)
         FROM {table} u JOIN executions e ON e.id = u.execution_id
         WHERE e.session_id IS NOT NULL AND e.session_id <> ''
         GROUP BY e.session_id"
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(BTreeMap::new()),
        Err(e) => return Err(e.into()),
    };
    let mapped = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = BTreeMap::new();
    for row in mapped {
        let (id, n) = row?;
        out.insert(id, n);
    }
    Ok(out)
}

/// Each stamped session's first prompt — the title stand-in for sessions
/// whose registry record has been pruned.
fn first_prompts(conn: &Connection) -> Result<BTreeMap<String, String>, DbError> {
    let sql = "SELECT e.session_id, e.prompt FROM executions e
               WHERE e.session_id IS NOT NULL AND e.session_id <> ''
                 AND e.id = (SELECT min(id) FROM executions f
                             WHERE f.session_id = e.session_id)";
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(BTreeMap::new()),
        Err(e) => return Err(e.into()),
    };
    let mapped = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = BTreeMap::new();
    for row in mapped {
        let (id, prompt) = row?;
        out.insert(id, clip(prompt, 140));
    }
    Ok(out)
}

/// Cap a string at `max` chars on a char boundary, appending an ellipsis.
fn clip(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        return s;
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// The `/api/sessions` payload: every session of this workspace, registry
/// and store merged, newest activity first.
pub(crate) fn sessions(conn: Option<&Connection>, workspace_root: &Path) -> Result<Value, DbError> {
    let (mut rollups, skill_counts, mcp_counts, prompts) = match conn {
        Some(conn) => (
            session_rollups(conn)?,
            per_session_counts(conn, "skill_usage")?,
            per_session_counts(conn, "mcp_usage")?,
            first_prompts(conn)?,
        ),
        None => Default::default(),
    };
    let (records, damaged) = registry_records(workspace_root);
    let mut rows = Vec::new();
    for record in records {
        let id = record
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if id.is_empty() {
            continue;
        }
        let (status, live, crashed) = presented_status(&record);
        let mut row = json!({
            "id": id,
            "registry": true,
            "title": record.get("title").and_then(Value::as_str).unwrap_or(""),
            "summary": record.get("summary").and_then(Value::as_str).unwrap_or(""),
            "status": status,
            "live": live,
            "crashed": crashed,
            "supervised": record.get("supervisor").is_some_and(|s| !s.is_null()),
            "started_at_ms": record.get("started_at_ms").and_then(Value::as_u64).unwrap_or(0),
            "updated_at_ms": record.get("updated_at_ms").and_then(Value::as_u64).unwrap_or(0),
        });
        let rollup = rollups.remove(&id).unwrap_or_else(empty_rollup);
        merge(&mut row, rollup);
        row["skill_uses"] = json!(skill_counts.get(&id).copied().unwrap_or(0));
        row["mcp_calls"] = json!(mcp_counts.get(&id).copied().unwrap_or(0));
        rows.push(row);
    }
    // Sessions the registry has already pruned but the store still remembers:
    // no status to present, first prompt standing in for the lost title.
    for (id, rollup) in rollups {
        let ms = ms_from_id(&id);
        let mut row = json!({
            "id": id,
            "registry": false,
            "title": prompts.get(&id).cloned().unwrap_or_default(),
            "summary": "",
            "status": Value::Null,
            "live": false,
            "crashed": false,
            "supervised": false,
            "started_at_ms": ms,
            "updated_at_ms": ms,
        });
        let skills = skill_counts.get(&id).copied().unwrap_or(0);
        let mcp = mcp_counts.get(&id).copied().unwrap_or(0);
        merge(&mut row, rollup);
        row["skill_uses"] = json!(skills);
        row["mcp_calls"] = json!(mcp);
        rows.push(row);
    }
    rows.sort_by_key(|r| {
        std::cmp::Reverse(r.get("updated_at_ms").and_then(Value::as_u64).unwrap_or(0))
    });
    rows.truncate(MAX_LISTED_SESSIONS);
    Ok(json!({ "sessions": rows, "damaged": damaged }))
}

/// The `/api/session?id=` payload: one session drilled to its turns — each
/// with its own tool/skill/MCP/receipt counts, ready to hand off to the
/// per-execution drawer — plus the session-scoped surfaces the turn view
/// can't show: skills and MCP traffic aggregated, memory citations, the task
/// board, and pull requests.
pub(crate) fn session_detail(
    conn: Option<&Connection>,
    _workspace_root: &Path,
    id: &str,
) -> Result<Value, DbError> {
    let record = registry_record(id);
    let registry = record.as_ref().map(|record| {
        let (status, live, crashed) = presented_status(record);
        json!({
            "title": record.get("title").and_then(Value::as_str).unwrap_or(""),
            "summary": record.get("summary").and_then(Value::as_str).unwrap_or(""),
            "workspace": record.get("workspace").and_then(Value::as_str).unwrap_or(""),
            "status": status,
            "live": live,
            "crashed": crashed,
            "supervised": record.get("supervisor").is_some_and(|s| !s.is_null()),
            "started_at_ms": record.get("started_at_ms").and_then(Value::as_u64).unwrap_or(0),
            "updated_at_ms": record.get("updated_at_ms").and_then(Value::as_u64).unwrap_or(0),
            "exploring": record.get("exploring").cloned().unwrap_or_else(|| json!([])),
        })
    });
    let mut out = json!({
        "id": id,
        "registry": registry,
        "turns": [],
        "turns_truncated": false,
        "skills": [],
        "mcp": [],
        "agents": [],
        "memory": [],
        "tasks": [],
        "pull_requests": [],
    });
    let Some(conn) = conn else {
        out["found"] = json!(out["registry"] != Value::Null);
        return Ok(out);
    };
    if !safe_session_id(id) {
        out["found"] = json!(false);
        return Ok(out);
    }
    let mut turns = session_turns(conn, id)?;
    let truncated = turns.len() >= MAX_SESSION_TURNS;
    // Fetched newest-first so the cap keeps recent work; replayed oldest-first
    // because that is the order the session happened in.
    turns.reverse();
    fold_turn_extras(conn, id, &mut turns)?;
    out["found"] = json!(out["registry"] != Value::Null || !turns.is_empty());
    out["turns"] = Value::Array(turns);
    out["turns_truncated"] = json!(truncated);
    out["skills"] = Value::Array(session_group(
        conn,
        id,
        "SELECT u.skill, count(*), max(u.version), max(u.ts)
         FROM skill_usage u
         WHERE u.execution_id IN (SELECT id FROM executions WHERE session_id = ?1)
         GROUP BY u.skill ORDER BY count(*) DESC",
        |r| {
            Ok(json!({
                "skill": r.get::<_, String>(0)?,
                "uses": r.get::<_, i64>(1)?,
                "version": r.get::<_, i64>(2)?,
                "last_used": r.get::<_, String>(3)?,
            }))
        },
    )?);
    out["mcp"] = Value::Array(session_group(
        conn,
        id,
        "SELECT u.server, u.tool, count(*), max(u.called_at_ms)
         FROM mcp_usage u
         WHERE u.execution_id IN (SELECT id FROM executions WHERE session_id = ?1)
         GROUP BY u.server, u.tool ORDER BY count(*) DESC",
        |r| {
            Ok(json!({
                "server": r.get::<_, String>(0)?,
                "tool": r.get::<_, String>(1)?,
                "calls": r.get::<_, i64>(2)?,
                "last_called_at_ms": r.get::<_, i64>(3)?,
            }))
        },
    )?);
    out["agents"] = Value::Array(session_group(
        conn,
        id,
        "SELECT u.agent, count(*), max(u.ts)
         FROM agent_uses u
         WHERE u.execution_id IN (SELECT id FROM executions WHERE session_id = ?1)
         GROUP BY u.agent ORDER BY count(*) DESC",
        |r| {
            Ok(json!({
                "agent": r.get::<_, String>(0)?,
                "uses": r.get::<_, i64>(1)?,
                "last_used": r.get::<_, String>(2)?,
            }))
        },
    )?);
    out["memory"] = Value::Array(session_group(
        conn,
        id,
        "SELECT c.memory_id, count(*), avg(c.useful_score),
                count(*) FILTER (WHERE c.truthful = 0)
         FROM memory_citations c
         WHERE c.execution_id IN (SELECT id FROM executions WHERE session_id = ?1)
         GROUP BY c.memory_id ORDER BY count(*) DESC",
        |r| {
            Ok(json!({
                "memory_id": r.get::<_, String>(0)?,
                "citations": r.get::<_, i64>(1)?,
                "avg_useful_score": r.get::<_, Option<f64>>(2)?,
                "untruthful": r.get::<_, i64>(3)?,
            }))
        },
    )?);
    out["tasks"] = Value::Array(session_group(
        conn,
        id,
        "SELECT task_id, subject, status, owner, updated_at
         FROM tasks WHERE session_id = ?1
         ORDER BY CAST(task_id AS INTEGER) ASC, task_id ASC",
        |r| {
            Ok(json!({
                "task_id": r.get::<_, String>(0)?,
                "subject": r.get::<_, String>(1)?,
                "status": r.get::<_, String>(2)?,
                "owner": r.get::<_, Option<String>>(3)?,
                "updated_at_ms": r.get::<_, i64>(4)?,
            }))
        },
    )?);
    out["pull_requests"] = Value::Array(session_group(
        conn,
        id,
        "SELECT url, number, status, ci_status, updated_at
         FROM pull_requests WHERE session_id = ?1
         ORDER BY updated_at DESC, url ASC",
        |r| {
            Ok(json!({
                "url": r.get::<_, String>(0)?,
                "number": r.get::<_, Option<i64>>(1)?,
                "status": r.get::<_, String>(2)?,
                "ci_status": r.get::<_, Option<String>>(3)?,
                "updated_at_ms": r.get::<_, i64>(4)?,
            }))
        },
    )?);
    // The self-improvement residue this session left behind: mined lessons
    // (reflections whose execution the session stamped — a NULL execution_id
    // is a cross-turn lesson and deliberately stays out of any one session),
    // and durable memory writes, recovered from the `save_memory` tool-call
    // log rather than a table that does not exist.
    out["lessons"] = Value::Array(session_group(
        conn,
        id,
        "SELECT r.kind, r.content, r.domains, r.occurred_at, r.execution_id
         FROM reflections r JOIN executions e ON e.id = r.execution_id
         WHERE e.session_id = ?1 ORDER BY r.occurred_at DESC LIMIT 100",
        |r| {
            Ok(json!({
                "kind": r.get::<_, String>(0)?,
                "content": r.get::<_, String>(1)?,
                "domains": r.get::<_, String>(2)?,
                "occurred_at": r.get::<_, i64>(3)?,
                "execution_id": r.get::<_, i64>(4)?,
            }))
        },
    )?);
    let mut writes = session_group(
        conn,
        id,
        "SELECT tc.execution_id, tc.args_json, tc.ok, tc.ts FROM tool_calls tc
         WHERE tc.name = 'save_memory'
           AND tc.execution_id IN (SELECT id FROM executions WHERE session_id = ?1)
         ORDER BY tc.ts ASC",
        |r| {
            Ok(json!({
                "execution_id": r.get::<_, i64>(0)?,
                "args_json": r.get::<_, String>(1)?,
                "ok": r.get::<_, i64>(2)? != 0,
                "ts": r.get::<_, String>(3)?,
            }))
        },
    )?;
    for write in &mut writes {
        let slug = write
            .get("args_json")
            .and_then(Value::as_str)
            .and_then(|args| serde_json::from_str::<Value>(args).ok())
            .and_then(|args| {
                args.get("slug")
                    .or_else(|| args.get("title"))
                    .or_else(|| args.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        write["slug"] = json!(slug);
        // The full memory text rode in on args_json; the page needs the slug
        // and the fact of the write, not a second copy of the memory.
        write.as_object_mut().map(|o| o.remove("args_json"));
    }
    out["memory_writes"] = Value::Array(writes);
    Ok(out)
}

/// The wire tags of every behavioural event [`execution_tendencies`] folds.
/// Spelled once: the filter and the fold must agree, and the tags are the
/// serde snake_case of `stella_protocol::AgentEvent` variant names.
const TENDENCY_EVENT_TYPES: &str = "'retry','retries_exhausted','loop_detected',\
     'budget_denied','compaction','policy_decision','speculation_discarded',\
     'steered','provider_fallback','usage_incomplete'";

/// One execution's behavioural tendencies — retries, loop detections,
/// compactions, policy verdicts — folded from its slice of the `events`
/// journal (the store projects none of these into a queryable table yet).
///
/// This is a sanctioned `events` read in the same sense as
/// `Observatory::execution_journal`: the filter is `execution_id`-first,
/// served directly by the store's `UNIQUE (execution_id, seq)` index. A
/// cross-execution tendencies view would need a store-side projection, not a
/// wider `WHERE` here.
pub(crate) fn execution_tendencies(conn: &Connection, id: i64) -> Result<Value, DbError> {
    let sql = format!(
        "SELECT event_type, payload FROM events
         WHERE execution_id = ?1 AND event_type IN ({TENDENCY_EVENT_TYPES})
         ORDER BY seq ASC"
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(empty_tendencies(id)),
        Err(e) => return Err(e.into()),
    };
    let mapped = stmt.query_map([id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut out = empty_tendencies(id);
    let mut loops: BTreeMap<String, i64> = BTreeMap::new();
    let mut policy: BTreeMap<String, i64> = BTreeMap::new();
    for row in mapped {
        let (event_type, payload) = row?;
        let payload: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        match event_type.as_str() {
            "retry" => bump(&mut out, "retries"),
            "retries_exhausted" => bump(&mut out, "retries_exhausted"),
            "loop_detected" => {
                let kind = payload
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                *loops.entry(kind).or_insert(0) += 1;
                if payload.get("aborted").and_then(Value::as_bool) == Some(true) {
                    bump(&mut out, "loop_aborts");
                }
            }
            "budget_denied" => bump(&mut out, "budget_denied"),
            "compaction" => {
                bump(&mut out, "compactions");
                let before = payload.get("before_tokens").and_then(Value::as_i64);
                let after = payload.get("after_tokens").and_then(Value::as_i64);
                if let (Some(before), Some(after)) = (before, after) {
                    let reclaimed = out["tokens_reclaimed"].as_i64().unwrap_or(0);
                    out["tokens_reclaimed"] = json!(reclaimed + (before - after).max(0));
                }
            }
            "policy_decision" => {
                let kind = payload
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                *policy.entry(kind).or_insert(0) += 1;
            }
            "speculation_discarded" => bump(&mut out, "speculation_discarded"),
            "steered" => bump(&mut out, "steered"),
            "provider_fallback" => bump(&mut out, "provider_fallback"),
            "usage_incomplete" => bump(&mut out, "usage_incomplete"),
            _ => {}
        }
    }
    out["loops"] = json!(loops);
    out["policy"] = json!(policy);
    Ok(out)
}

/// The all-zero tendencies payload — what a store with no journal (or no
/// `events` table at all) reports, and the base the fold counts into.
fn empty_tendencies(id: i64) -> Value {
    json!({
        "execution_id": id,
        "retries": 0,
        "retries_exhausted": 0,
        "loops": {},
        "loop_aborts": 0,
        "budget_denied": 0,
        "compactions": 0,
        "tokens_reclaimed": 0,
        "policy": {},
        "speculation_discarded": 0,
        "steered": 0,
        "provider_fallback": 0,
        "usage_incomplete": 0,
    })
}

/// Increment one integer counter field on the tendencies payload.
fn bump(out: &mut Value, key: &str) {
    let n = out[key].as_i64().unwrap_or(0);
    out[key] = json!(n + 1);
}

/// The session's turns from the three long-lived tables, newest first (the
/// caller reverses after capping). Younger per-turn attachments — skills,
/// MCP servers, receipt counts, the reflection verdict — ride in afterwards
/// through [`fold_turn_extras`].
fn session_turns(conn: &Connection, id: &str) -> Result<Vec<Value>, DbError> {
    let sql = format!(
        "SELECT e.id, e.kind, e.prompt, e.provider, e.model, e.outcome,
                e.cost_usd, e.started_at, e.finished_at,
                coalesce(t.steps, 0), coalesce(t.input_tokens, 0),
                coalesce(t.output_tokens, 0), coalesce(t.cache_read_tokens, 0),
                coalesce(t.cache_miss_tokens, 0), coalesce(t.retries, 0),
                coalesce(t.duration_ms, 0),
                coalesce(tc.calls, 0), coalesce(tc.errors, 0),
                coalesce(ft.files, 0), coalesce(ft.added, 0), coalesce(ft.removed, 0)
         FROM executions e
         LEFT JOIN (SELECT execution_id, count(*) AS steps,
                           sum(input_tokens)      AS input_tokens,
                           sum(output_tokens)     AS output_tokens,
                           sum(cache_read_tokens) AS cache_read_tokens,
                           sum(cache_miss_tokens) AS cache_miss_tokens,
                           sum(retries)           AS retries,
                           sum(duration_ms)       AS duration_ms
                    FROM telemetry GROUP BY execution_id) t
           ON t.execution_id = e.id
         LEFT JOIN (SELECT execution_id, count(*) AS calls,
                           count(*) FILTER (WHERE error <> '') AS errors
                    FROM tool_calls GROUP BY execution_id) tc
           ON tc.execution_id = e.id
         LEFT JOIN (SELECT execution_id, count(*) AS files,
                           sum(lines_added)  AS added,
                           sum(lines_removed) AS removed
                    FROM files_touched GROUP BY execution_id) ft
           ON ft.execution_id = e.id
         WHERE e.session_id = ?1
         ORDER BY e.id DESC
         LIMIT {MAX_SESSION_TURNS}"
    );
    let mut stmt = match conn.prepare(&sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mapped = stmt.query_map([id], |r| {
        Ok(json!({
            "id": r.get::<_, i64>(0)?,
            "kind": r.get::<_, String>(1)?,
            "prompt": r.get::<_, String>(2)?,
            "provider": r.get::<_, String>(3)?,
            "model": r.get::<_, String>(4)?,
            "outcome": r.get::<_, Option<String>>(5)?,
            "cost_usd": r.get::<_, f64>(6)?,
            "started_at": r.get::<_, String>(7)?,
            "finished_at": r.get::<_, Option<String>>(8)?,
            "steps": r.get::<_, i64>(9)?,
            "input_tokens": r.get::<_, i64>(10)?,
            "output_tokens": r.get::<_, i64>(11)?,
            "cache_read_tokens": r.get::<_, i64>(12)?,
            "cache_miss_tokens": r.get::<_, i64>(13)?,
            "retries": r.get::<_, i64>(14)?,
            "model_ms": r.get::<_, i64>(15)?,
            "tool_calls": r.get::<_, i64>(16)?,
            "tool_errors": r.get::<_, i64>(17)?,
            "files_touched": r.get::<_, i64>(18)?,
            "lines_added": r.get::<_, i64>(19)?,
            "lines_removed": r.get::<_, i64>(20)?,
        }))
    })?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row?);
    }
    // Prompts are carried whole from the store but a listing does not need a
    // multi-kilobyte goal; the drawer shows the full text.
    for row in &mut out {
        if let Some(prompt) = row.get("prompt").and_then(Value::as_str) {
            let clipped = clip(prompt.to_string(), 240);
            row["prompt"] = json!(clipped);
        }
    }
    Ok(out)
}

/// Attach the younger tables' per-turn facts to already-fetched turn rows:
/// skills applied, MCP servers called, receipt counts (whether the sent-
/// context drill has anything to show), and the reflection verdict. Each
/// query degrades alone, so an older store loses one column, not the view.
fn fold_turn_extras(conn: &Connection, id: &str, turns: &mut [Value]) -> Result<(), DbError> {
    let mut skills: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for (execution, skill) in session_pairs(
        conn,
        id,
        "SELECT u.execution_id, u.skill FROM skill_usage u
         WHERE u.execution_id IN (SELECT id FROM executions WHERE session_id = ?1)
         ORDER BY u.execution_id, u.ts",
    )? {
        skills.entry(execution).or_default().push(skill);
    }
    let mut mcp: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for (execution, server) in session_pairs(
        conn,
        id,
        "SELECT DISTINCT u.execution_id, u.server FROM mcp_usage u
         WHERE u.execution_id IN (SELECT id FROM executions WHERE session_id = ?1)
         ORDER BY u.execution_id, u.server",
    )? {
        mcp.entry(execution).or_default().push(server);
    }
    let mut receipts: BTreeMap<i64, i64> = BTreeMap::new();
    for row in session_group(
        conn,
        id,
        "SELECT r.execution_id, count(*) FROM step_receipt r
         WHERE r.execution_id IN (SELECT id FROM executions WHERE session_id = ?1)
         GROUP BY r.execution_id",
        |r| Ok(json!([r.get::<_, i64>(0)?, r.get::<_, i64>(1)?])),
    )? {
        if let (Some(execution), Some(n)) = (
            row.get(0).and_then(Value::as_i64),
            row.get(1).and_then(Value::as_i64),
        ) {
            receipts.insert(execution, n);
        }
    }
    let mut reflections: BTreeMap<i64, Value> = BTreeMap::new();
    for row in session_group(
        conn,
        id,
        "SELECT execution_id, delivered, self_rating FROM execution_reflection
         WHERE execution_id IN (SELECT id FROM executions WHERE session_id = ?1)",
        |r| {
            Ok(json!([
                r.get::<_, i64>(0)?,
                json!({
                    "delivered": r.get::<_, Option<i64>>(1)?,
                    "self_rating": r.get::<_, Option<i64>>(2)?,
                })
            ]))
        },
    )? {
        if let (Some(execution), Some(verdict)) =
            (row.get(0).and_then(Value::as_i64), row.get(1).cloned())
        {
            reflections.insert(execution, verdict);
        }
    }
    for turn in turns.iter_mut() {
        let Some(execution) = turn.get("id").and_then(Value::as_i64) else {
            continue;
        };
        turn["skills"] = json!(skills.get(&execution).cloned().unwrap_or_default());
        turn["mcp_servers"] = json!(mcp.get(&execution).cloned().unwrap_or_default());
        turn["receipts"] = json!(receipts.get(&execution).copied().unwrap_or(0));
        turn["reflection"] = reflections.get(&execution).cloned().unwrap_or(Value::Null);
    }
    Ok(())
}

/// Run one session-bound query collecting every row; a missing table or
/// column degrades to `[]` — the same bargain `crate::db::collect_rows`
/// strikes, with the session id bound as the parameter.
fn session_group<F>(conn: &Connection, id: &str, sql: &str, map: F) -> Result<Vec<Value>, DbError>
where
    F: Fn(&rusqlite::Row<'_>) -> rusqlite::Result<Value>,
{
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mapped = stmt.query_map([id], map)?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row?);
    }
    Ok(out)
}

/// [`session_group`] for `(execution_id, text)` pairs — the shape both
/// per-turn attachment queries share.
fn session_pairs(conn: &Connection, id: &str, sql: &str) -> Result<Vec<(i64, String)>, DbError> {
    let mut stmt = match conn.prepare(sql) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mapped = stmt.query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_that_could_escape_the_registry_dir_are_rejected() {
        assert!(safe_session_id("ses-1722900000000-4242"));
        assert!(!safe_session_id(""));
        assert!(!safe_session_id("../../etc/passwd"));
        assert!(!safe_session_id("a/b"));
        assert!(!safe_session_id("a\\b"));
        assert!(!safe_session_id(".hidden"));
    }

    #[test]
    fn the_id_mint_time_is_recovered_and_junk_sorts_last() {
        assert_eq!(ms_from_id("ses-1722900000000-4242"), 1_722_900_000_000);
        assert_eq!(ms_from_id("not-an-id"), 0);
        assert_eq!(ms_from_id(""), 0);
    }

    #[test]
    fn a_live_claim_with_a_dead_pid_presents_as_crashed() {
        // A pid that cannot fit pid_t reads as dead on unix, so the record
        // downgrades; the stored terminal status is passed through untouched.
        let record = json!({ "status": "in_progress", "pid": u32::MAX });
        let (status, live, crashed) = presented_status(&record);
        if cfg!(unix) {
            assert_eq!(status, "error");
            assert!(!live);
            assert!(crashed);
        }
        let done = json!({ "status": "complete", "pid": 0 });
        assert_eq!(presented_status(&done), ("complete".into(), false, false));
    }
}
