//! The self-driving *sessions* view: every `stella self-driving drive` session
//! on this machine, the issues each one claimed, what became of them, the
//! ranked queue it is drawing from, and the learning that happened alongside.
//!
//! [`super::self_driving`] reads the older cycle ledger (`runs.jsonl` +
//! `ledger.jsonl`), which the shell daemon wrote one record per audit cycle.
//! The issue-level loop writes something different: `audit.jsonl`, one record
//! per **action** — `session_started`, `claimed`, `work_started`,
//! `work_changed`, `verify_failed`, `pr_opened`, `pr_observed`, `pr_merged`,
//! `session_stopped`, … — and that journal is the only durable trace a drive
//! session leaves. Until it was folded here, a CIO asking "what did the agents
//! do last night?" had nothing to look at but a terminal scrollback.
//!
//! Everything is read, nothing is written, and nothing is fetched: the queue
//! is the **snapshot the loop itself wrote** (`queue.json`) when it last
//! ranked the backlog, never a live `gh` call from the dashboard. A dashboard
//! that reached the forge on every refresh would be egress this crate forbids
//! (see the crate README's three boundaries).
//!
//! # Sessions from a journal that did not name them
//!
//! Records written before the loop stamped a `session_id` carry
//! `run_id: "unassigned"` and nothing else to group by. They are still
//! folded: a `session_started` record opens a session and every following
//! record without its own `session_id` belongs to it. Those legacy sessions
//! have no pid to probe, so their liveness is inferred from the age of their
//! last record rather than known — the payload says which (`liveness`).
//!
//! # Learning, correlated rather than claimed
//!
//! The workspace's `context.db` records one `episode` per turn and one
//! `memory` row per lesson the reflection pass kept; `context_records` of kind
//! `context_use` mark a lesson being rendered into a later prompt. Joining
//! those to the journal by time window is what lets the page put "issues
//! fixed" and "lessons learned" on one axis. It is a window join, and the
//! payload labels it as such: a lesson recorded during an issue's turn is
//! attributed to that issue, which is the honest strength of the evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::db::{collect_rows, is_missing_schema, open_read_only, truncate};

/// Loop directories scanned under `~/.stella/self-driving`.
const MAX_LOOPS: usize = 64;

/// Journal lines read per loop. The fold needs every record of a session, so
/// this is a generous cap with the overflow reported, not a tail.
const MAX_AUDIT_LINES: usize = 50_000;

/// Sessions carried in the machine-wide payload, newest first.
const MAX_SESSIONS: usize = 200;

/// Seconds without a journal record after which a legacy session (one with no
/// pid to probe) is reported `lost` rather than `running`.
///
/// The loop polls a parked pull request every 45 s by default and writes a
/// `pr_observed` record each time, so a healthy session is never this quiet.
/// Read from `stella-autonomy` so this page and the terminal share one notion
/// of stale.
const STALE_AFTER_SECS: i64 = stella_autonomy::DEFAULT_STALE_AFTER_SECS;

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
        .unwrap_or(0)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`). Pure integer arithmetic; no calendar crate.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse `YYYY-MM-DDTHH:MM:SSZ` or `YYYY-MM-DD HH:MM:SS` (fractional seconds
/// tolerated, UTC assumed) to unix seconds. Every writer this module reads
/// stamps UTC: the journal writes RFC 3339 `Z`, SQLite's `datetime('now')`
/// is UTC by definition.
pub(crate) fn parse_unix(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> { s.get(from..to)?.parse::<i64>().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, se) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || se > 60 {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3_600 + mi * 60 + se)
}

/// The `YYYY-MM-DD` a timestamp falls on, for the daily series.
fn day_of(ts: &str) -> Option<String> {
    (ts.len() >= 10 && parse_unix(ts).is_some()).then(|| ts[..10].to_string())
}

fn str_at(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn read_jsonl(path: &Path) -> (Vec<Value>, bool) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return (Vec::new(), false);
    };
    let total = text.lines().count();
    let rows = text
        .lines()
        .take(MAX_AUDIT_LINES)
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(Value::is_object)
        .collect();
    (rows, total > MAX_AUDIT_LINES)
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// First `#<digits>` in a string — how a `pr_opened` outcome ("opened for
/// #1180") and an episode summary ("Issue #1182: …") name their issue.
fn first_issue_ref(text: &str) -> Option<String> {
    for (i, c) in text.char_indices() {
        if c != '#' {
            continue;
        }
        let digits: String = text[i + 1..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if !digits.is_empty() {
            return Some(digits);
        }
    }
    None
}

/// One journal record, typed as far as the fold needs.
struct Rec {
    at: String,
    at_unix: i64,
    action: String,
    subject: String,
    outcome: String,
    session_id: Option<String>,
    pid: Option<u32>,
}

fn as_rec(v: &Value) -> Option<Rec> {
    let at = str_at(v, "at");
    let subject = match v.get("subject") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    };
    Some(Rec {
        at_unix: parse_unix(&at)?,
        at,
        action: str_at(v, "action"),
        subject,
        outcome: str_at(v, "outcome"),
        session_id: v
            .get("session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        pid: v
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|p| u32::try_from(p).ok()),
    })
}

/// One issue's path through a session.
#[derive(Default)]
struct Issue {
    number: String,
    title: String,
    claimed_unix: i64,
    last_unix: i64,
    pr: Option<String>,
    /// The last state-changing action, in journal order.
    stage: String,
    events: Vec<Value>,
}

impl Issue {
    fn outcome(&self) -> &'static str {
        match self.stage.as_str() {
            "pr_merged" => "merged",
            "pr_opened" | "pr_observed" => "pr open",
            "verified" | "work_changed" | "waived" | "verify_failed" | "verify_started" => {
                "changed"
            }
            "work_no_change" => "no change",
            "work_failed" => "failed",
            "deferred" => "deferred",
            "escalated" => "escalated",
            "transient" => "retrying",
            "claimed" | "work_started" => "working",
            _ => "unknown",
        }
    }

    /// Whether the issue is still moving — counts toward "agents busy now".
    fn is_open(&self) -> bool {
        matches!(
            self.outcome(),
            "working" | "changed" | "pr open" | "retrying"
        )
    }
}

/// The fold of one session's records.
struct Session {
    id: String,
    legacy: bool,
    pid: Option<u32>,
    started_at: String,
    started_unix: i64,
    last_unix: i64,
    stopped: bool,
    stop_reason: String,
    params: Vec<String>,
    issues: BTreeMap<String, Issue>,
    /// PR number → issue number, so `pr_observed`/`pr_merged` (whose subject
    /// is the PR) land on the issue they belong to.
    prs: BTreeMap<String, String>,
    counts: BTreeMap<&'static str, i64>,
    triaged: Vec<Value>,
}

impl Session {
    fn new(id: String, legacy: bool, rec: &Rec) -> Self {
        Self {
            id,
            legacy,
            pid: rec.pid,
            started_at: rec.at.clone(),
            started_unix: rec.at_unix,
            last_unix: rec.at_unix,
            stopped: false,
            stop_reason: String::new(),
            params: Vec::new(),
            issues: BTreeMap::new(),
            prs: BTreeMap::new(),
            counts: BTreeMap::new(),
            triaged: Vec::new(),
        }
    }

    fn bump(&mut self, key: &'static str) {
        *self.counts.entry(key).or_insert(0) += 1;
    }

    fn absorb(&mut self, rec: &Rec) {
        self.last_unix = self.last_unix.max(rec.at_unix);
        if rec.pid.is_some() && self.pid.is_none() {
            self.pid = rec.pid;
        }
        match rec.action.as_str() {
            "session_started" => {
                if !rec.outcome.is_empty() {
                    self.params.push(rec.outcome.clone());
                }
                return;
            }
            "session_stopped" => {
                self.stopped = true;
                self.stop_reason = rec.outcome.clone();
                return;
            }
            "triaged" | "triage_started" => {
                if rec.action == "triaged" {
                    self.bump("triaged");
                    self.triaged.push(json!({
                        "issue": rec.subject,
                        "at": rec.at,
                        "placement": rec.outcome,
                    }));
                }
                return;
            }
            "claimed" => self.bump("claimed"),
            "work_changed" => self.bump("changed"),
            "work_no_change" => self.bump("no_change"),
            "work_failed" => self.bump("failed"),
            "deferred" => self.bump("deferred"),
            "escalated" => self.bump("escalated"),
            "pr_opened" => self.bump("prs_opened"),
            "pr_merged" => self.bump("prs_merged"),
            "verified" => self.bump("verified"),
            "verify_failed" => self.bump("verify_failed"),
            "waived" => self.bump("waived"),
            _ => {}
        }

        // Resolve which issue this record is about. PR-subject records are
        // redirected through the PR map; `pr_opened` is where the map is
        // learned ("opened for #1180").
        let issue_no = match rec.action.as_str() {
            "pr_opened" => {
                let Some(n) = first_issue_ref(&rec.outcome) else {
                    return;
                };
                self.prs.insert(rec.subject.clone(), n.clone());
                n
            }
            "pr_observed" | "pr_merged" | "waited" => match self.prs.get(&rec.subject) {
                Some(n) => n.clone(),
                None => return,
            },
            _ if rec.subject.is_empty() => return,
            _ => rec.subject.clone(),
        };

        let issue = self
            .issues
            .entry(issue_no.clone())
            .or_insert_with(|| Issue {
                number: issue_no,
                claimed_unix: rec.at_unix,
                ..Issue::default()
            });
        issue.last_unix = issue.last_unix.max(rec.at_unix);
        match rec.action.as_str() {
            "work_started" => {
                // "began work — P1: <title>"
                issue.title = rec
                    .outcome
                    .split_once(" — ")
                    .map_or(rec.outcome.as_str(), |(_, t)| t)
                    .to_string();
            }
            "pr_opened" => issue.pr = Some(rec.subject.clone()),
            _ => {}
        }
        // `pr_observed` polls are the loop waiting, not the issue changing
        // stage; they are kept as events (collapsed below) but never advance
        // the stage past `pr_opened`.
        if rec.action != "pr_observed" && rec.action != "waited" {
            issue.stage = rec.action.clone();
        }
        issue.events.push(json!({
            "at": rec.at,
            "at_unix": rec.at_unix,
            "action": rec.action,
            "outcome": rec.outcome,
            "pr": if matches!(rec.action.as_str(), "pr_opened" | "pr_observed" | "pr_merged" | "waited") {
                Value::String(rec.subject.clone())
            } else {
                Value::Null
            },
        }));
    }

    /// `running` / `stopped` / `crashed` / `lost`, and how that was decided.
    fn liveness(&self, now: i64) -> (&'static str, &'static str) {
        if self.stopped {
            return ("stopped", "stop record");
        }
        if let Some(pid) = self.pid {
            return if crate::sessions::pid_alive(pid) {
                ("running", "pid alive")
            } else {
                ("crashed", "pid gone, no stop record")
            };
        }
        if now - self.last_unix < STALE_AFTER_SECS {
            ("running", "recent journal record; no pid recorded")
        } else {
            ("lost", "no stop record and the journal went quiet")
        }
    }

    fn count(&self, key: &str) -> i64 {
        self.counts.get(key).copied().unwrap_or(0)
    }
}

/// Fold a journal into sessions, in file order.
fn fold_sessions(records: &[Value]) -> Vec<Session> {
    let mut sessions: Vec<Session> = Vec::new();
    let mut by_id: BTreeMap<String, usize> = BTreeMap::new();
    let mut current: Option<usize> = None;

    for v in records {
        let Some(rec) = as_rec(v) else { continue };
        let idx = match (&rec.session_id, rec.action.as_str()) {
            (Some(id), _) => match by_id.get(id) {
                Some(i) => *i,
                None => {
                    sessions.push(Session::new(id.clone(), false, &rec));
                    let i = sessions.len() - 1;
                    by_id.insert(id.clone(), i);
                    i
                }
            },
            (None, "session_started") => {
                // The loop writes two `session_started` lines at launch (the
                // bound and the proof command). Same second, same session.
                match current {
                    Some(i) if sessions[i].legacy && sessions[i].started_unix == rec.at_unix => i,
                    _ => {
                        sessions.push(Session::new(format!("legacy-{}", rec.at_unix), true, &rec));
                        sessions.len() - 1
                    }
                }
            }
            (None, _) => match current {
                Some(i) => i,
                None => {
                    sessions.push(Session::new(format!("legacy-{}", rec.at_unix), true, &rec));
                    sessions.len() - 1
                }
            },
        };
        if rec.session_id.is_none() {
            current = Some(idx);
        }
        sessions[idx].absorb(&rec);
    }
    sessions
}

/// One loop directory, read.
struct LoopDir {
    slug: String,
    dir: PathBuf,
    roots: Vec<String>,
    stats: Value,
    queue: Value,
    sessions: Vec<Session>,
    truncated: bool,
}

/// The ranked queue as the loop last saw it.
///
/// `queue.json` is the snapshot the drive verb writes each time it ranks
/// (`{"at", "items": [{number, title, rank, labels, url}], "open_total",
/// "untriaged"}`). Older loops cached the raw `gh` listing in `.queue.json`
/// instead; it is read as a fallback and ranked here from its labels so a
/// machine that has not upgraded still shows a queue rather than nothing.
fn load_queue(dir: &Path) -> Value {
    if let Some(q) = read_json(&dir.join("queue.json")) {
        return q;
    }
    let Some(raw) = read_json(&dir.join(".queue.json")) else {
        return json!({ "at": Value::Null, "items": [], "source": "none" });
    };
    let items: Vec<Value> = raw
        .as_array()
        .map(|a| {
            a.iter()
                .map(|i| {
                    let labels: Vec<String> = i
                        .get("labels")
                        .and_then(Value::as_array)
                        .map(|ls| ls.iter().map(|l| str_at(l, "name")).collect())
                        .unwrap_or_default();
                    let rank = ["P0", "P1", "P2"]
                        .iter()
                        .find(|p| labels.iter().any(|l| l == *p))
                        .map_or("untriaged", |p| p);
                    json!({
                        "number": i.get("number").cloned().unwrap_or(Value::Null),
                        "title": str_at(i, "title"),
                        "url": str_at(i, "url"),
                        "rank": rank,
                        "labels": labels,
                        "created_at": str_at(i, "createdAt"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "at": Value::Null,
        "items": items,
        "source": "legacy-cache",
    })
}

fn load_loop(dir: &Path) -> Option<LoopDir> {
    let slug = dir.file_name()?.to_string_lossy().into_owned();
    let workspace = read_json(&dir.join("workspace.json")).unwrap_or_else(|| json!({}));
    let roots = workspace
        .get("roots")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let (records, truncated) = read_jsonl(&dir.join("audit.jsonl"));
    let queue = load_queue(dir);
    let mut sessions = fold_sessions(&records);
    fill_titles(&mut sessions, &queue);
    Some(LoopDir {
        slug,
        dir: dir.to_path_buf(),
        roots,
        stats: read_json(&dir.join("stats.json")).unwrap_or_else(|| json!({})),
        queue,
        sessions,
        truncated,
    })
}

/// Give every issue row a title, from wherever the loop learned one.
///
/// Only `work_started` carries the title, and an issue that one session
/// claimed and a later one re-claimed (the first run crashed, the lease
/// expired) has the title in the first session's journal and nothing in the
/// second's. The issue is the same issue; the title goes with it. The queue
/// snapshot is the other source, for an issue claimed but never worked.
fn fill_titles(sessions: &mut [Session], queue: &Value) {
    let mut titles: BTreeMap<String, String> = BTreeMap::new();
    for s in sessions.iter() {
        for i in s.issues.values() {
            if !i.title.is_empty() {
                titles
                    .entry(i.number.clone())
                    .or_insert_with(|| i.title.clone());
            }
        }
    }
    if let Some(items) = queue.get("items").and_then(Value::as_array) {
        for item in items {
            let n = match item.get("number") {
                Some(Value::Number(n)) => n.to_string(),
                Some(Value::String(s)) => s.clone(),
                _ => continue,
            };
            let t = str_at(item, "title");
            if !t.is_empty() {
                titles.entry(n).or_insert(t);
            }
        }
    }
    for s in sessions.iter_mut() {
        for i in s.issues.values_mut() {
            if i.title.is_empty()
                && let Some(t) = titles.get(&i.number)
            {
                i.title = t.clone();
            }
        }
    }
}

fn state_roots() -> Vec<PathBuf> {
    let mut roots = Vec::with_capacity(3);
    roots.extend(stella_home::self_driving_root());
    roots.extend(stella_home::legacy_self_driving_roots());
    roots
}

fn load_loops_from(roots: &[PathBuf]) -> Vec<LoopDir> {
    let mut out: Vec<LoopDir> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten().take(MAX_LOOPS) {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            if let Some(l) = load_loop(&entry.path())
                && seen.insert(l.slug.clone())
            {
                out.push(l);
            }
        }
        if out.len() >= MAX_LOOPS {
            break;
        }
    }
    out.sort_by(|a, b| a.slug.cmp(&b.slug));
    out
}

/// Daily counters, merged from every source into one keyed map.
#[derive(Default)]
struct Daily {
    claimed: i64,
    changed: i64,
    prs_opened: i64,
    merged: i64,
    failed: i64,
    lessons: i64,
    applied: i64,
    episodes: i64,
    turns: i64,
    spend_usd: f64,
}

type DailyMap = BTreeMap<String, Daily>;

fn bump_day(days: &mut DailyMap, ts: &str, f: impl FnOnce(&mut Daily)) {
    if let Some(day) = day_of(ts) {
        f(days.entry(day).or_default());
    }
}

/// What the workspace learned: episodes, lessons, and lessons being used.
struct Learning {
    present: bool,
    episodes: Vec<Value>,
    lessons: Vec<Value>,
    uses: Vec<Value>,
}

fn learning_for(root: &Path) -> Learning {
    let db = root.join(".stella").join("private").join("context.db");
    let Some(conn) = open_read_only(&db) else {
        return Learning {
            present: false,
            episodes: Vec::new(),
            lessons: Vec::new(),
            uses: Vec::new(),
        };
    };
    let tolerant = |r: Result<Vec<Value>, crate::db::DbError>| match r {
        Ok(v) => v,
        Err(crate::db::DbError::Query(e)) if is_missing_schema(&e) => Vec::new(),
        Err(_) => Vec::new(),
    };
    let episodes = tolerant(episode_rows(&conn));
    let lessons = tolerant(lesson_rows(&conn));
    let uses = tolerant(use_rows(&conn));
    Learning {
        present: true,
        episodes,
        lessons,
        uses,
    }
}

fn episode_rows(conn: &Connection) -> Result<Vec<Value>, crate::db::DbError> {
    collect_rows(
        conn,
        "SELECT public_id, summary, outcome, started_at, ended_at
         FROM episode ORDER BY started_at DESC LIMIT 500",
        |r| {
            let summary: String = r.get(1)?;
            let started: String = r.get(3)?;
            let ended: String = r.get(4)?;
            let (su, eu) = (
                parse_unix(&started).unwrap_or(0),
                parse_unix(&ended).unwrap_or(0),
            );
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "issue": first_issue_ref(&summary),
                "summary": truncate(summary.lines().next().unwrap_or(""), 160),
                "outcome": r.get::<_, String>(2)?,
                "started_at": started,
                "started_unix": su,
                "ended_at": ended,
                "seconds": (eu - su).max(0),
            }))
        },
    )
}

fn lesson_rows(conn: &Connection) -> Result<Vec<Value>, crate::db::DbError> {
    collect_rows(
        conn,
        "SELECT public_id, kind, content, salience, recorded_at
         FROM memory WHERE superseded_at IS NULL
         ORDER BY recorded_at DESC LIMIT 500",
        |r| {
            let recorded: String = r.get(4)?;
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "kind": r.get::<_, String>(1)?,
                "text": truncate(&r.get::<_, String>(2)?, 240),
                "salience": r.get::<_, f64>(3)?,
                "recorded_at": recorded,
                "recorded_unix": parse_unix(&recorded).unwrap_or(0),
            }))
        },
    )
}

/// Lessons being *used*: `context_use` records, whose body names the record
/// rendered and how far it reached (`influence_stage`).
fn use_rows(conn: &Connection) -> Result<Vec<Value>, crate::db::DbError> {
    collect_rows(
        conn,
        "SELECT body, observed_at FROM context_records
         WHERE record_kind = 'context_use'
         ORDER BY observed_at DESC LIMIT 2000",
        |r| {
            let body: String = r.get(0)?;
            let observed: String = r.get(1)?;
            let b: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            Ok(json!({
                "record": str_at(&b, "context_record_id"),
                "use_kind": str_at(&b, "use_kind"),
                "influence": str_at(&b, "influence_stage"),
                "task": str_at(&b, "task_id"),
                "observed_at": observed,
                "observed_unix": parse_unix(&observed).unwrap_or(0),
            }))
        },
    )
}

/// Spend per day from the workspace store, so the page can put dollars on the
/// same axis as fixes. `executions.started_at` is `datetime('now')` — UTC,
/// space-separated — and `cost_usd` is what the provider billed.
fn spend_rows(root: &Path) -> Vec<Value> {
    let db = root.join(".stella").join("private").join("store.db");
    let Some(conn) = open_read_only(&db) else {
        return Vec::new();
    };
    collect_rows(
        &conn,
        "SELECT substr(started_at, 1, 10) AS day,
                round(coalesce(sum(cost_usd), 0), 4), count(*),
                min(started_at), max(started_at)
         FROM executions
         GROUP BY day ORDER BY day DESC LIMIT 400",
        |r| {
            Ok(json!({
                "day": r.get::<_, String>(0)?,
                "usd": r.get::<_, f64>(1)?,
                "turns": r.get::<_, i64>(2)?,
            }))
        },
    )
    .unwrap_or_default()
}

/// Per-execution spend with timestamps, for the per-session window sum.
fn execution_costs(root: &Path) -> Vec<(i64, f64)> {
    let db = root.join(".stella").join("private").join("store.db");
    let Some(conn) = open_read_only(&db) else {
        return Vec::new();
    };
    collect_rows(
        &conn,
        "SELECT started_at, coalesce(cost_usd, 0) FROM executions
         ORDER BY started_at DESC LIMIT 5000",
        |r| {
            let at: String = r.get(0)?;
            Ok(json!({ "u": parse_unix(&at).unwrap_or(0), "c": r.get::<_, f64>(1)? }))
        },
    )
    .unwrap_or_default()
    .iter()
    .map(|v| {
        (
            v.get("u").and_then(Value::as_i64).unwrap_or(0),
            v.get("c").and_then(Value::as_f64).unwrap_or(0.0),
        )
    })
    .collect()
}

/// Claims live in the workspace's `fleet.db` right now.
///
/// `stella self-driving drive` leases `issue:<n>` in `dispatch_claims` for
/// as long as a turn is in flight, owner `self-driving:<pid>`, and the lease
/// expires on its own if the holder dies (#4300). So this table — not any one
/// journal — is the machine-wide answer to "what is being worked this
/// second, and by whom", including a second drive against the same clone.
fn live_claims(root: &Path, now: i64) -> Vec<Value> {
    let db = root.join(".stella").join("private").join("fleet.db");
    let Some(conn) = open_read_only(&db) else {
        return Vec::new();
    };
    let now_ms = now * 1000;
    let rows = collect_rows(
        &conn,
        &format!(
            "SELECT claim_key, owner, acquired_at_ms, expires_at_ms
             FROM dispatch_claims WHERE expires_at_ms > {now_ms}
             ORDER BY acquired_at_ms ASC"
        ),
        |r| {
            let owner: String = r.get(1)?;
            let pid = owner.rsplit(':').next().and_then(|p| p.parse::<u32>().ok());
            Ok(json!({
                "key": r.get::<_, String>(0)?,
                "owner": owner,
                "pid": pid,
                "held_secs": (now_ms - r.get::<_, i64>(2)?) / 1000,
                "expires_in_secs": (r.get::<_, i64>(3)? - now_ms) / 1000,
            }))
        },
    );
    match rows {
        Ok(v) => v,
        Err(crate::db::DbError::Query(e)) if is_missing_schema(&e) => Vec::new(),
        Err(_) => Vec::new(),
    }
}

fn issue_json(i: &Issue, learning: &Learning) -> Value {
    let (lo, hi) = (i.claimed_unix, i.last_unix.max(i.claimed_unix));
    let in_window = |u: i64| u >= lo && u <= hi + 60;
    let episodes: Vec<&Value> = learning
        .episodes
        .iter()
        .filter(|e| {
            e.get("issue").and_then(Value::as_str) == Some(i.number.as_str())
                || in_window(e.get("started_unix").and_then(Value::as_i64).unwrap_or(-1))
        })
        .collect();
    let lessons: Vec<&Value> = learning
        .lessons
        .iter()
        .filter(|l| in_window(l.get("recorded_unix").and_then(Value::as_i64).unwrap_or(-1)))
        .collect();
    let applied = learning
        .uses
        .iter()
        .filter(|u| in_window(u.get("observed_unix").and_then(Value::as_i64).unwrap_or(-1)))
        .count();

    // Collapse the poll storm: a parked PR writes one `pr_observed` per poll.
    let mut events: Vec<Value> = Vec::new();
    let mut polls = 0;
    for e in &i.events {
        if str_at(e, "action") == "pr_observed" || str_at(e, "action") == "waited" {
            polls += 1;
            continue;
        }
        events.push(e.clone());
    }

    json!({
        "number": i.number,
        "title": i.title,
        "outcome": i.outcome(),
        "open": i.is_open(),
        "pr": i.pr,
        "claimed_unix": i.claimed_unix,
        "last_unix": i.last_unix,
        "seconds": (i.last_unix - i.claimed_unix).max(0),
        "events": events,
        "polls": polls,
        "episodes": episodes.len(),
        "lessons": lessons.iter().map(|l| l.get("text").cloned().unwrap_or(Value::Null)).collect::<Vec<_>>(),
        "applied": applied,
    })
}

fn session_json(
    s: &Session,
    l: &LoopDir,
    now: i64,
    learning: &Learning,
    costs: &[(i64, f64)],
) -> Value {
    let (status, liveness) = s.liveness(now);
    let end = if s.stopped || status != "running" {
        s.last_unix
    } else {
        now
    };
    let spend: f64 = costs
        .iter()
        .filter(|(u, _)| *u >= s.started_unix && *u <= end)
        .map(|(_, c)| c)
        .sum();
    let mut issues: Vec<Value> = s.issues.values().map(|i| issue_json(i, learning)).collect();
    issues.sort_by_key(|i| -i.get("claimed_unix").and_then(Value::as_i64).unwrap_or(0));
    let busy = s.issues.values().filter(|i| i.is_open()).count();
    let lessons: i64 = issues
        .iter()
        .map(|i| {
            i.get("lessons")
                .and_then(Value::as_array)
                .map_or(0, |a| a.len() as i64)
        })
        .sum();

    json!({
        "id": s.id,
        "slug": l.slug,
        "legacy": s.legacy,
        "pid": s.pid,
        "status": status,
        "liveness": liveness,
        "started_at": s.started_at,
        "started_unix": s.started_unix,
        "last_unix": s.last_unix,
        "seconds": (end - s.started_unix).max(0),
        "stop_reason": s.stop_reason,
        "params": s.params,
        "busy": if status == "running" { busy as i64 } else { 0 },
        "claimed": s.count("claimed"),
        "changed": s.count("changed"),
        "no_change": s.count("no_change"),
        "failed": s.count("failed"),
        "deferred": s.count("deferred"),
        "escalated": s.count("escalated"),
        "prs_opened": s.count("prs_opened"),
        "prs_merged": s.count("prs_merged"),
        "verified": s.count("verified"),
        "verify_failed": s.count("verify_failed"),
        "waived": s.count("waived"),
        "triaged": s.count("triaged"),
        "triage": s.triaged,
        "lessons": lessons,
        "spend_usd": (spend * 10_000.0).round() / 10_000.0,
        "issues": issues,
    })
}

fn daily_json(days: &DailyMap) -> Vec<Value> {
    days.iter()
        .map(|(day, d)| {
            json!({
                "day": day,
                "claimed": d.claimed,
                "changed": d.changed,
                "prs_opened": d.prs_opened,
                "merged": d.merged,
                "failed": d.failed,
                "lessons": d.lessons,
                "applied": d.applied,
                "episodes": d.episodes,
                "turns": d.turns,
                "spend_usd": (d.spend_usd * 100.0).round() / 100.0,
            })
        })
        .collect()
}

/// `/api/self-driving-sessions` — every drive session on this machine, the
/// queues, the learning, and the daily series the charts draw.
pub fn sessions(workspace_root: &Path) -> Value {
    sessions_from(&state_roots(), workspace_root, now_unix())
}

fn sessions_from(roots: &[PathBuf], workspace_root: &Path, now: i64) -> Value {
    let loops = load_loops_from(roots);
    let here = workspace_root.to_string_lossy().into_owned();

    let mut loop_rows = Vec::new();
    let mut all_sessions = Vec::new();
    let mut days: DailyMap = DailyMap::new();
    let mut lessons_all = Vec::new();
    let mut running = 0i64;
    let mut busy = 0i64;
    let mut queue_by_rank: BTreeMap<String, i64> = BTreeMap::new();
    let mut holders: BTreeSet<String> = BTreeSet::new();

    for l in &loops {
        let mine = l.roots.iter().any(|r| r == &here);
        let root = l.roots.first().map(PathBuf::from);
        let learning = root.as_deref().map(learning_for).unwrap_or(Learning {
            present: false,
            episodes: Vec::new(),
            lessons: Vec::new(),
            uses: Vec::new(),
        });
        let costs = root.as_deref().map(execution_costs).unwrap_or_default();
        let spend_days = root.as_deref().map(spend_rows).unwrap_or_default();
        let claims = root
            .as_deref()
            .map(|r| live_claims(r, now))
            .unwrap_or_default();
        for c in &claims {
            if let Some(o) = c.get("owner").and_then(Value::as_str) {
                holders.insert(o.to_string());
            }
        }

        // Journal → daily.
        for s in &l.sessions {
            for i in s.issues.values() {
                for e in &i.events {
                    let at = str_at(e, "at");
                    match str_at(e, "action").as_str() {
                        "claimed" => bump_day(&mut days, &at, |d| d.claimed += 1),
                        "work_changed" => bump_day(&mut days, &at, |d| d.changed += 1),
                        "pr_opened" => bump_day(&mut days, &at, |d| d.prs_opened += 1),
                        "pr_merged" => bump_day(&mut days, &at, |d| d.merged += 1),
                        "work_failed" => bump_day(&mut days, &at, |d| d.failed += 1),
                        _ => {}
                    }
                }
            }
        }
        for e in &learning.episodes {
            bump_day(&mut days, &str_at(e, "started_at"), |d| d.episodes += 1);
        }
        for m in &learning.lessons {
            bump_day(&mut days, &str_at(m, "recorded_at"), |d| d.lessons += 1);
            lessons_all.push(json!({
                "slug": l.slug,
                "id": m["id"], "kind": m["kind"], "text": m["text"],
                "recorded_at": m["recorded_at"], "recorded_unix": m["recorded_unix"],
            }));
        }
        for u in &learning.uses {
            bump_day(&mut days, &str_at(u, "observed_at"), |d| d.applied += 1);
        }
        for s in &spend_days {
            let day = str_at(s, "day");
            if day.len() == 10 {
                let d = days.entry(day).or_default();
                d.spend_usd += s.get("usd").and_then(Value::as_f64).unwrap_or(0.0);
                d.turns += s.get("turns").and_then(Value::as_i64).unwrap_or(0);
            }
        }

        let mut session_rows: Vec<Value> = l
            .sessions
            .iter()
            .map(|s| session_json(s, l, now, &learning, &costs))
            .collect();
        session_rows.sort_by_key(|s| -s.get("started_unix").and_then(Value::as_i64).unwrap_or(0));
        for s in &session_rows {
            if str_at(s, "status") == "running" {
                running += 1;
                busy += s.get("busy").and_then(Value::as_i64).unwrap_or(0);
            }
        }

        // Overlay the live claims on the queue so "in the queue" and "being
        // worked right now" are one picture.
        let mut queue = l.queue.clone();
        let open_now: BTreeSet<String> = session_rows
            .iter()
            .filter(|s| str_at(s, "status") == "running")
            .flat_map(|s| {
                s.get("issues")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter(|i| i.get("open").and_then(Value::as_bool).unwrap_or(false))
                            .map(|i| str_at(i, "number"))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            })
            .collect();
        if let Some(items) = queue.get_mut("items").and_then(Value::as_array_mut) {
            for item in items.iter_mut() {
                let n = match item.get("number") {
                    Some(Value::Number(n)) => n.to_string(),
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                };
                let rank = str_at(item, "rank");
                *queue_by_rank
                    .entry(if rank.is_empty() {
                        "untriaged".into()
                    } else {
                        rank
                    })
                    .or_insert(0) += 1;
                if let Some(o) = item.as_object_mut() {
                    o.insert("in_progress".into(), json!(open_now.contains(&n)));
                }
            }
        }

        loop_rows.push(json!({
            "slug": l.slug,
            "state_dir": l.dir.display().to_string(),
            "roots": l.roots,
            "is_current_workspace": mine,
            "stats": l.stats,
            "queue": queue,
            "journal_truncated": l.truncated,
            "claims": claims,
            "learning": {
                "present": learning.present,
                "episodes": learning.episodes.len(),
                "lessons": learning.lessons.len(),
                "applied": learning.uses.len(),
            },
            "sessions": session_rows.len(),
        }));
        all_sessions.extend(session_rows);
    }

    all_sessions.sort_by_key(|s| -s.get("started_unix").and_then(Value::as_i64).unwrap_or(0));
    all_sessions.truncate(MAX_SESSIONS);
    lessons_all.sort_by_key(|m| -m.get("recorded_unix").and_then(Value::as_i64).unwrap_or(0));
    lessons_all.truncate(100);

    let merged: i64 = all_sessions
        .iter()
        .map(|s| s.get("prs_merged").and_then(Value::as_i64).unwrap_or(0))
        .sum();
    let claimed: i64 = all_sessions
        .iter()
        .map(|s| s.get("claimed").and_then(Value::as_i64).unwrap_or(0))
        .sum();
    let spend: f64 = all_sessions
        .iter()
        .map(|s| s.get("spend_usd").and_then(Value::as_f64).unwrap_or(0.0))
        .sum();
    let lessons: i64 = days.values().map(|d| d.lessons).sum();
    let applied: i64 = days.values().map(|d| d.applied).sum();

    json!({
        "generated_unix": now,
        "loops": loop_rows,
        "sessions": all_sessions,
        "lessons": lessons_all,
        "daily": daily_json(&days),
        "queue_by_rank": queue_by_rank,
        "totals": {
            "loops": loops.len(),
            "sessions": all_sessions.len(),
            "running": running,
            "busy": busy,
            // Distinct lease holders across every loop: the number of agent
            // processes holding an issue this second, whatever wrote their
            // journals.
            "holders": holders.len(),
            "claimed": claimed,
            "merged": merged,
            "lessons": lessons,
            "applied": applied,
            "spend_usd": (spend * 100.0).round() / 100.0,
            "usd_per_merge": if merged > 0 { Value::from((spend / merged as f64 * 100.0).round() / 100.0) } else { Value::Null },
        },
        "stale_after_secs": STALE_AFTER_SECS,
    })
}

/// `/api/self-driving-session?id=<session id>` — one session, every issue
/// with its full (poll-collapsed) timeline. An unknown id is `{}`.
pub fn session_detail(id: &str) -> Value {
    let now = now_unix();
    for l in load_loops_from(&state_roots()) {
        let Some(s) = l.sessions.iter().find(|s| s.id == id) else {
            continue;
        };
        let root = l.roots.first().map(PathBuf::from);
        let learning = root.as_deref().map(learning_for).unwrap_or(Learning {
            present: false,
            episodes: Vec::new(),
            lessons: Vec::new(),
            uses: Vec::new(),
        });
        let costs = root.as_deref().map(execution_costs).unwrap_or_default();
        let mut v = session_json(s, &l, now, &learning, &costs);
        let end = if s.stopped { s.last_unix } else { now };
        let lessons: Vec<&Value> = learning
            .lessons
            .iter()
            .filter(|m| {
                let u = m.get("recorded_unix").and_then(Value::as_i64).unwrap_or(-1);
                u >= s.started_unix && u <= end
            })
            .collect();
        if let Some(o) = v.as_object_mut() {
            o.insert("lesson_rows".into(), json!(lessons));
            o.insert(
                "workspace_root".into(),
                json!(root.map(|p| p.display().to_string())),
            );
        }
        return v;
    }
    json!({})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(at: &str, action: &str, subject: Option<&str>, outcome: &str) -> String {
        let subj = subject.map_or("null".to_string(), |s| format!("\"{s}\""));
        format!(
            r#"{{"at":"{at}","run_id":"unassigned","action":"{action}","subject":{subj},"outcome":"{outcome}"}}"#
        )
    }

    fn seed(root: &Path, slug: &str, lines: &[String], queue: Option<&str>) -> PathBuf {
        let dir = root.join("self-driving").join(slug);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("audit.jsonl"), lines.join("\n") + "\n").unwrap();
        std::fs::write(
            dir.join("workspace.json"),
            format!(r#"{{"roots":["/nowhere/{slug}"],"slug":"{slug}"}}"#),
        )
        .unwrap();
        if let Some(q) = queue {
            std::fs::write(dir.join("queue.json"), q).unwrap();
        }
        dir
    }

    #[test]
    fn timestamps_in_both_dialects_parse_to_the_same_instant() {
        assert_eq!(
            parse_unix("2026-08-23T04:32:50Z"),
            parse_unix("2026-08-23 04:32:50")
        );
        assert_eq!(parse_unix("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_unix("1970-01-02T00:00:01Z"), Some(86_401));
        assert_eq!(parse_unix("not a date"), None);
    }

    /// The journal that exists today: `run_id: "unassigned"` on every line and
    /// no session id. Two `session_started` lines in the same second are one
    /// launch; the next `session_started` is a new session.
    #[test]
    fn legacy_records_fold_into_sessions_by_session_started() {
        let lines = [
            line(
                "2026-08-23T04:20:02Z",
                "session_started",
                None,
                "session began — up to 10 issue(s)",
            ),
            line(
                "2026-08-23T04:20:02Z",
                "session_started",
                None,
                "changes will be proved here",
            ),
            line(
                "2026-08-23T04:22:38Z",
                "claimed",
                Some("1180"),
                "taken off the ranked queue",
            ),
            line(
                "2026-08-23T04:22:40Z",
                "work_started",
                Some("1180"),
                "began work — P1: drizzle mock",
            ),
            line(
                "2026-08-23T04:26:28Z",
                "session_started",
                None,
                "session began — up to 10 issue(s)",
            ),
            line(
                "2026-08-23T04:32:43Z",
                "claimed",
                Some("1180"),
                "taken off the ranked queue",
            ),
            line(
                "2026-08-23T04:46:41Z",
                "work_changed",
                Some("1180"),
                "the turn left changes",
            ),
            line(
                "2026-08-23T04:48:04Z",
                "pr_opened",
                Some("1186"),
                "opened for #1180 — ci in progress",
            ),
            line(
                "2026-08-23T04:48:55Z",
                "pr_observed",
                Some("1186"),
                "ci=Green -> Wait",
            ),
            line(
                "2026-08-23T04:49:40Z",
                "pr_observed",
                Some("1186"),
                "ci=Green -> Wait",
            ),
            line("2026-08-23T05:00:00Z", "pr_merged", Some("1186"), "merged"),
            line(
                "2026-08-23T05:00:01Z",
                "session_stopped",
                None,
                "reached the bound",
            ),
        ];
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "demo", &lines, None);

        let out = sessions_from(
            &[tmp.path().join("self-driving")],
            Path::new("/nowhere/demo"),
            parse_unix("2026-08-23T05:01:00Z").unwrap(),
        );
        let s = out["sessions"].as_array().unwrap();
        assert_eq!(s.len(), 2, "two launches, two sessions: {s:?}");
        // Newest first.
        assert_eq!(s[0]["status"], "stopped");
        assert_eq!(s[0]["prs_merged"], 1);
        assert_eq!(s[0]["claimed"], 1);
        let issue = &s[0]["issues"][0];
        assert_eq!(issue["number"], "1180");
        assert_eq!(
            issue["pr"], "1186",
            "pr_opened's outcome links the PR to its issue"
        );
        assert_eq!(issue["outcome"], "merged");
        assert_eq!(issue["polls"], 2, "pr_observed polls collapse to a count");
        assert_eq!(issue["title"], "P1: drizzle mock");
        // The first launch never stopped and its journal is stale.
        assert_eq!(s[1]["status"], "lost");
        assert_eq!(s[1]["issues"][0]["outcome"], "working");
        assert_eq!(out["totals"]["merged"], 1);
        assert_eq!(out["loops"][0]["is_current_workspace"], true);
    }

    /// A session-stamped journal (the writer after this change) groups by id,
    /// and a recorded pid decides liveness rather than the journal's age.
    #[test]
    fn stamped_records_group_by_session_id_and_a_dead_pid_reads_crashed() {
        let stamped = |at: &str, action: &str, sid: &str, pid: u32| {
            format!(
                r#"{{"at":"{at}","run_id":"{sid}","session_id":"{sid}","pid":{pid},"action":"{action}","subject":null,"outcome":""}}"#
            )
        };
        // Interleaved: two drives against one repo at once.
        let lines = [
            stamped(
                "2026-08-23T04:20:02Z",
                "session_started",
                "sd-a",
                4_000_000_000,
            ),
            stamped(
                "2026-08-23T04:20:03Z",
                "session_started",
                "sd-b",
                4_000_000_001,
            ),
            stamped("2026-08-23T04:21:00Z", "claimed", "sd-b", 4_000_000_001),
            stamped("2026-08-23T04:22:00Z", "claimed", "sd-a", 4_000_000_000),
        ];
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "demo", &lines, None);
        let out = sessions_from(
            &[tmp.path().join("self-driving")],
            Path::new("/x"),
            parse_unix("2026-08-23T04:23:00Z").unwrap(),
        );
        let s = out["sessions"].as_array().unwrap();
        assert_eq!(s.len(), 2);
        for row in s {
            // A pid past pid_t cannot be alive, so both read crashed even though
            // their journals are seconds old — the pid is the witness now.
            assert_eq!(row["status"], "crashed", "{row}");
            assert_eq!(row["liveness"], "pid gone, no stop record");
        }
        assert_eq!(out["totals"]["running"], 0);
    }

    /// The queue is the loop's own snapshot, ranked, with the issues a running
    /// session holds marked in progress.
    #[test]
    fn queue_snapshot_is_ranked_and_overlaid_with_live_claims() {
        let lines = [
            line(
                "2026-08-23T04:20:02Z",
                "session_started",
                None,
                "session began",
            ),
            line(
                "2026-08-23T04:22:38Z",
                "claimed",
                Some("1180"),
                "taken off the ranked queue",
            ),
            line(
                "2026-08-23T04:22:40Z",
                "work_started",
                Some("1180"),
                "began work — P1: x",
            ),
        ];
        let queue = r#"{"at":"2026-08-23T04:22:00Z","open_total":7,"untriaged":2,
            "items":[{"number":1180,"title":"x","rank":"P1"},{"number":1182,"title":"y","rank":"P1"},
                     {"number":1190,"title":"z","rank":"untriaged"}]}"#;
        let tmp = tempfile::tempdir().unwrap();
        seed(tmp.path(), "demo", &lines, Some(queue));
        let out = sessions_from(
            &[tmp.path().join("self-driving")],
            Path::new("/x"),
            parse_unix("2026-08-23T04:23:00Z").unwrap(),
        );
        assert_eq!(
            out["totals"]["running"], 1,
            "a recent legacy journal reads running"
        );
        assert_eq!(out["totals"]["busy"], 1);
        let items = out["loops"][0]["queue"]["items"].as_array().unwrap();
        assert_eq!(items[0]["in_progress"], true);
        assert_eq!(items[1]["in_progress"], false);
        assert_eq!(out["queue_by_rank"]["P1"], 2);
        assert_eq!(out["queue_by_rank"]["untriaged"], 1);
    }

    #[test]
    fn an_unknown_session_is_an_empty_object() {
        assert_eq!(session_detail("no-such-session"), json!({}));
    }
}
