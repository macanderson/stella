// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The context-records drill-down. Two routes serve it.
//! `/api/context-records` lists every record that steers this workspace,
//! with its standing. `/api/context-record` shows one record in full: its
//! uses, the turns it shaped, the records it was shown beside, and its
//! source.
//!
//! The Self-improve tab's health table shows a bare `context_record_id`
//! and nothing else. A reader who sees `✕ failing` beside `nod_e7e2…` must
//! leave the page to learn what the record says. This module joins that
//! id back to the thing it names.
//!
//! An id in the use ledger is whatever `context_blocks.memory_id` held
//! when the turn ran (`crates/stella-cli/src/memory/uses.rs`). Today that
//! is one of two things. A **recall node** (`nod_…`) is a row of
//! `context.db`'s `node` table, most often a mined memory. A **published
//! record** is one `[[record]]` in `.stella/rules/*.toml`, cited by its
//! `^handle`. The list holds both. It also holds every published record
//! that no turn has rendered yet. A rule nobody has seen still steers the
//! workspace, and a list that left it out would look complete when it was
//! not.
//!
//! Two of this crate's rules hold here as everywhere else. Every file
//! opens `SQLITE_OPEN_READ_ONLY`. A missing file, table or column is a
//! state, and it degrades to an empty section, never a 500. And nothing
//! here writes. The source view shows a record's TOML and the
//! `stella context` or `stella memory` command that would change it. The
//! observatory has no mutation verb and must not grow one.
//!
//! What reaches the browser was audited as the README asks. A node's
//! content and a rendered block's text are already served by
//! `/api/execution-context`. A record file is TOML an operator wrote, and
//! its only fields are the schema's. It is read by the path the server's
//! own directory walk found, never by a path from the request.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::Connection;
use serde_json::{Value, json};
use stella_records::context_record::{
    ContextUse, ContextUseFeedback, SelectionHealth, SelectionHealthPolicy, fold_selection_health,
};

use crate::db::{DbError, collect_rows, is_missing_schema, open_read_only, truncate};
use crate::fsview::PluginContributions;

/// The longest record id the detail route accepts. A `nod_` id is 28
/// bytes and a handle is a lineage tail. Anything longer is not ours.
const MAX_ID_LEN: usize = 200;
/// Uses listed on a record's page, newest first.
const MAX_LISTED_USES: usize = 100;
/// Turns listed on a record's page, newest first.
const MAX_LISTED_RENDERINGS: usize = 25;
/// Co-rendered records listed on a record's page.
const MAX_RELATED: usize = 8;
/// A record's full content on its own page. Memory text is a paragraph.
/// This guards against a huge row; it is not a display budget.
const MAX_CONTENT_CHARS: usize = 4_000;
/// A record file's text on the source view.
const MAX_SOURCE_CHARS: usize = 16_000;
/// The title cut from a record's first sentence.
const MAX_TITLE_CHARS: usize = 120;
/// The statement clip on the list.
const MAX_LIST_STATEMENT_CHARS: usize = 600;

/// Whether `id` looks like something the ledger could hold. That is a
/// handle (`^pre-push-runs-gate`), a node id (`nod_…`), a lineage id
/// (`ctx.stella.rust-toolchain-pin`) or a stamped record id. The fragment
/// is text the user typed. This is the only gate between it and a query
/// parameter, so the alphabet is closed.
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '^' | '_' | '.' | ':' | '-' | '@'))
}

/// `/api/context-records`: every record that steers this workspace.
pub(crate) fn list(root: &Path, plugins: &dyn PluginContributions) -> Result<Value, DbError> {
    let policy = crate::fsview::selection_health_policy(root);
    let published = published_records(root, plugins);
    let context = open_read_only(&root.join(".stella/private/context.db"));
    let (uses, feedback) = match &context {
        Some(conn) => crate::context_db::use_ledger(conn)?,
        None => (Vec::new(), Vec::new()),
    };
    let health = fold_selection_health(&uses, &feedback, policy);
    let last_used = last_used_by_record(&uses);
    let renderings = renderings_by_record(root)?;
    let nodes = match &context {
        Some(conn) => nodes_with_uses(conn)?,
        None => HashMap::new(),
    };

    let mut rows: Vec<Value> = Vec::new();
    let mut listed: HashSet<String> = HashSet::new();
    for row in &health {
        let id = row.context_record_id.as_str();
        listed.insert(id.to_owned());
        let identity = match nodes.get(id) {
            Some(node) => node_identity(node),
            None => match published_by_id(&published, id) {
                Some(card) => published_identity(card),
                // A use of something neither store can name now. That is
                // a forgotten memory, or a record whose file was deleted.
                // It stays listed, since its uses happened. Its id is its
                // name.
                None => orphan_identity(id),
            },
        };
        rows.push(list_row(
            identity,
            Some(row),
            last_used.get(id).cloned(),
            renderings.get(id),
        ));
    }
    for card in &published {
        let handle = format!("^{}", card["name"].as_str().unwrap_or_default());
        let lineage = card["lineage_id"].as_str().unwrap_or_default();
        if listed.contains(&handle) || listed.contains(lineage) {
            continue;
        }
        listed.insert(handle.clone());
        rows.push(list_row(
            published_identity(card),
            None,
            last_used.get(&handle).cloned(),
            renderings.get(&handle),
        ));
    }
    rows.sort_by(|a, b| {
        standing_rank(a).cmp(&standing_rank(b)).then_with(|| {
            b["health"]["uses"]
                .as_u64()
                .cmp(&a["health"]["uses"].as_u64())
                .then_with(|| b["recorded_at"].as_str().cmp(&a["recorded_at"].as_str()))
                .then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
        })
    });

    let count = |standing: &str| rows.iter().filter(|r| r["standing"] == standing).count();
    Ok(json!({
        "present": context.is_some(),
        "policy": policy_json(policy),
        "totals": {
            "records": rows.len(),
            "failing": count("failing"),
            "earning": count("earning"),
            "unassessed": count("unassessed"),
            "unused": count("unused"),
            "uses": uses.len(),
            "tasks": uses.iter().map(|(_, u)| u.task_id.as_str()).collect::<HashSet<_>>().len(),
        },
        "records": rows,
    }))
}

/// `/api/context-record?id=…`: one record in full. An id nothing names
/// answers `found: false` as a 200. An unknown record is a state the page
/// renders, not a server failure.
pub(crate) fn detail(
    root: &Path,
    plugins: &dyn PluginContributions,
    id: &str,
) -> Result<Value, DbError> {
    let policy = crate::fsview::selection_health_policy(root);
    let published = published_records(root, plugins);
    let context = open_read_only(&root.join(".stella/private/context.db"));
    let (uses, feedback) = match &context {
        Some(conn) => crate::context_db::use_ledger(conn)?,
        None => (Vec::new(), Vec::new()),
    };
    let node = match &context {
        Some(conn) => node_by_id(conn, id)?,
        None => None,
    };
    let card = published_by_id(&published, id);
    let health_row = fold_selection_health(&uses, &feedback, policy)
        .into_iter()
        .find(|row| row.context_record_id == id);
    if node.is_none() && card.is_none() && health_row.is_none() {
        return Ok(json!({ "found": false, "id": id }));
    }

    let identity = match (&node, card) {
        (Some(node), _) => node_identity(node),
        (None, Some(card)) => published_identity(card),
        (None, None) => orphan_identity(id),
    };
    let last_used = last_used_by_record(&uses);
    let renderings_index = renderings_by_record(root)?;
    let mut record = list_row(
        identity,
        health_row.as_ref(),
        last_used.get(id).cloned(),
        renderings_index.get(id),
    );
    // The page shows the whole statement; the list clipped it.
    let full_content = node
        .as_ref()
        .map(|n| n.content.clone())
        .or_else(|| card.and_then(|c| c["statement"].as_str().map(str::to_owned)))
        .unwrap_or_default();
    record["content"] = json!(truncate(&full_content, MAX_CONTENT_CHARS));
    if let Some(node) = &node {
        record["domains"] = json!(match &context {
            Some(conn) => node_domains(conn, &node.public_id)?,
            None => Vec::new(),
        });
    }

    let use_rows = uses_of(&uses, &feedback, id);
    let rendered = renderings_of(root, id)?;
    let related = related_records(root, id, context.as_ref(), &published)?;
    let source = source_of(node.as_ref(), card, context.as_ref(), &rendered)?;

    Ok(json!({
        "found": true,
        "present": context.is_some(),
        "policy": policy_json(policy),
        "record": record,
        "uses": use_rows,
        "renderings": rendered,
        "related": related,
        "source": source,
    }))
}

/// One row of `context.db`'s `node` table. The recall graph keeps a
/// memory, an episode, a concept or a file this way.
struct NodeRow {
    public_id: String,
    kind: String,
    content: String,
    uri: Option<String>,
    recorded_at: String,
    superseded_at: Option<String>,
    valid_from: Option<String>,
    valid_to: Option<String>,
    /// The `memory.kind` behind a `memory://` node: `reflection`, `note`
    /// or `insight`. That is the record's real origin. The URI's scheme
    /// only says it has one. `None` when the node mirrors no memory row,
    /// or when the schema predates the `memory` table.
    memory_kind: Option<String>,
}

/// The node columns every node query selects. The memory row is joined
/// by the URI the writer mints (`memory://<memory public id>`).
const NODE_SELECT: &str =
    "SELECT n.public_id, n.kind, n.content, n.uri, n.recorded_at, n.superseded_at,
            n.valid_from, n.valid_to, m.kind
     FROM node n
     LEFT JOIN memory m ON n.uri = 'memory://' || m.public_id";

/// What a record is, before any health is attached. The list and the
/// page both show these fields above the fold.
struct Identity {
    id: String,
    /// `memory` / `episode` / `concept` / `file` for a node. `rule` and
    /// its siblings for a published record. `unknown` for an orphan.
    kind: String,
    /// Where the words came from. `recall` for a node, `published` for a
    /// record file, `missing` for an orphan.
    plane: &'static str,
    /// The origin the record declares. For a node, the memory's own kind
    /// (`reflection`). For a published record, the TOML `origin`.
    origin: String,
    title: String,
    statement: String,
    recorded_at: String,
    superseded: bool,
    /// The published-record fields, `Null` on a node.
    published: Value,
}

fn node_identity(node: &NodeRow) -> Identity {
    Identity {
        id: node.public_id.clone(),
        kind: node.kind.clone(),
        plane: "recall",
        origin: node.memory_kind.clone().unwrap_or_default(),
        title: title_of(&node.content),
        statement: node.content.clone(),
        recorded_at: node.recorded_at.clone(),
        superseded: node.superseded_at.is_some(),
        published: Value::Null,
    }
}

fn published_identity(card: &Value) -> Identity {
    let statement = card["statement"].as_str().unwrap_or_default();
    Identity {
        id: format!("^{}", card["name"].as_str().unwrap_or_default()),
        kind: card["kind"].as_str().unwrap_or("rule").to_owned(),
        plane: "published",
        origin: card["origin"].as_str().unwrap_or_default().to_owned(),
        title: title_of(statement),
        statement: statement.to_owned(),
        recorded_at: unix_to_rfc3339(card["modified_unix"].as_i64().unwrap_or(0)),
        superseded: matches!(card["status"].as_str(), Some("retracted" | "archived")),
        published: json!({
            "handle": card["name"],
            "lineage_id": card["lineage_id"],
            "record_id": card["record_id"],
            "record_hash": card["record_hash"],
            "status": card["status"],
            "tags": card["tags"],
            "steering_force": card["steering_force"],
            "enforcement_mode": card["enforcement_mode"],
            "precedence": card["precedence"],
            "applies_to": card["applies_to"],
            "contributed_by": card["contributed_by"],
            "path": card["path"],
        }),
    }
}

fn orphan_identity(id: &str) -> Identity {
    Identity {
        id: id.to_owned(),
        kind: "unknown".to_owned(),
        plane: "missing",
        origin: String::new(),
        title: id.to_owned(),
        statement: String::new(),
        recorded_at: String::new(),
        superseded: false,
        published: Value::Null,
    }
}

/// One list row. It holds the identity, the health fold, the derived
/// standing, and the store's rendering count. A record nothing has used
/// gets the zero fold.
fn list_row(
    identity: Identity,
    health: Option<&SelectionHealth>,
    last_used: Option<String>,
    renderings: Option<&(i64, i64)>,
) -> Value {
    let (uses, health_json) = match health {
        Some(h) => (h.uses, health_json(h)),
        None => (0, health_json(&zero_health(&identity.id))),
    };
    let standing = match health {
        Some(h) if h.failing => "failing",
        Some(h) if h.attributable => "earning",
        _ if uses > 0 => "unassessed",
        _ => "unused",
    };
    let (rendered_turns, prompt_tokens) = renderings.copied().unwrap_or((0, 0));
    json!({
        "id": identity.id,
        "kind": identity.kind,
        "plane": identity.plane,
        "origin": identity.origin,
        "title": identity.title,
        "statement": truncate(&identity.statement, MAX_LIST_STATEMENT_CHARS),
        "recorded_at": identity.recorded_at,
        "superseded": identity.superseded,
        "published": identity.published,
        "health": health_json,
        "standing": standing,
        "last_used": last_used,
        "rendered_turns": rendered_turns,
        "prompt_tokens": prompt_tokens,
    })
}

/// The sort key for the list: what needs a decision first.
fn standing_rank(row: &Value) -> u8 {
    match row["standing"].as_str() {
        Some("failing") => 0,
        Some("earning") => 1,
        Some("unassessed") => 2,
        _ => 3,
    }
}

fn zero_health(id: &str) -> SelectionHealth {
    SelectionHealth {
        context_record_id: id.to_owned(),
        uses: 0,
        distinct_tasks: 0,
        assessed_uses: 0,
        helpful: 0,
        not_helpful: 0,
        neutral: 0,
        not_helpful_ratio: 0.0,
        eligible_assessed: 0,
        eligible_not_helpful: 0,
        attributable: false,
        failing: false,
    }
}

/// Every field of the fold, with the two the Self-improve table leaves
/// out. `failing` is decided on pruning-eligible evidence alone. A page
/// that shows the verdict owes the reader those numbers.
fn health_json(h: &SelectionHealth) -> Value {
    json!({
        "uses": h.uses,
        "distinct_tasks": h.distinct_tasks,
        "assessed_uses": h.assessed_uses,
        "helpful": h.helpful,
        "not_helpful": h.not_helpful,
        "neutral": h.neutral,
        "not_helpful_ratio": h.not_helpful_ratio,
        "eligible_assessed": h.eligible_assessed,
        "eligible_not_helpful": h.eligible_not_helpful,
        "attributable": h.attributable,
        "failing": h.failing,
    })
}

fn policy_json(policy: SelectionHealthPolicy) -> Value {
    json!({
        "min_attributable_uses": policy.min_attributable_uses,
        "not_helpful_ratio_threshold": policy.not_helpful_ratio_threshold,
        "min_attribution_confidence": policy.min_attribution_confidence,
    })
}

/// The newest `observed_at` per record in the use ledger.
fn last_used_by_record(uses: &[(String, ContextUse)]) -> HashMap<String, String> {
    let mut out: HashMap<String, String> = HashMap::new();
    for (_, u) in uses {
        let entry = out.entry(u.context_record_id.clone()).or_default();
        if u.observed_at.as_str() > entry.as_str() {
            *entry = u.observed_at.clone();
        }
    }
    out
}

/// `(turns rendered into, prompt tokens spent)` per record id, from the
/// store's context receipts. Empty when there is no store, or the store
/// predates the receipts table.
fn renderings_by_record(root: &Path) -> Result<HashMap<String, (i64, i64)>, DbError> {
    let Some(conn) = open_read_only(&root.join(".stella/private/store.db")) else {
        return Ok(HashMap::new());
    };
    let rows = collect_rows(
        &conn,
        "SELECT memory_id, count(DISTINCT execution_id), coalesce(sum(token_cost), 0)
         FROM context_blocks
         WHERE memory_id IS NOT NULL AND memory_id != ''
         GROUP BY memory_id",
        |r| {
            Ok(json!([
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?
            ]))
        },
    )?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            Some((
                row[0].as_str()?.to_owned(),
                (row[1].as_i64()?, row[2].as_i64()?),
            ))
        })
        .collect())
}

/// Every published record the workspace loads, as `fsview::rules_files`
/// cards. That is the workspace's own `.stella/rules` plus what installed
/// plugins contribute. The same later-wins-by-lineage rule applies.
/// Markdown rules carry no lineage and are not context records. They stay
/// on the Memory & rules tab.
fn published_records(root: &Path, plugins: &dyn PluginContributions) -> Vec<Value> {
    crate::fsview::rules_files(root, plugins)
        .as_array()
        .map(|cards| {
            cards
                .iter()
                .filter(|card| card["lineage_id"].is_string())
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// A published record by any name a reader might use for it: its
/// `^handle` (caret optional), its lineage id, or its stamped record id.
/// `stella context explain` accepts the same three.
fn published_by_id<'a>(published: &'a [Value], id: &str) -> Option<&'a Value> {
    let bare = id.strip_prefix('^').unwrap_or(id);
    published.iter().find(|card| {
        card["name"].as_str() == Some(bare)
            || card["lineage_id"].as_str() == Some(id)
            || (!bare.is_empty() && card["record_id"].as_str() == Some(id))
    })
}

/// Every node the use ledger names, keyed by public id. One query, so
/// three hundred used records cost the same as three.
fn nodes_with_uses(conn: &Connection) -> Result<HashMap<String, NodeRow>, DbError> {
    let mut stmt = match conn.prepare(&format!(
        "{NODE_SELECT}
         WHERE n.public_id IN (
           SELECT json_extract(body, '$.context_record_id')
           FROM context_records WHERE record_kind = 'context_use')"
    )) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(HashMap::new()),
        Err(e) => return Err(e.into()),
    };
    let rows = stmt.query_map([], node_row)?;
    let mut out = HashMap::new();
    for row in rows {
        let row = row?;
        out.insert(row.public_id.clone(), row);
    }
    Ok(out)
}

fn node_by_id(conn: &Connection, id: &str) -> Result<Option<NodeRow>, DbError> {
    let mut stmt = match conn.prepare(&format!("{NODE_SELECT} WHERE n.public_id = ?1")) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut rows = stmt.query_map([id], node_row)?;
    rows.next().transpose().map_err(Into::into)
}

fn node_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<NodeRow> {
    Ok(NodeRow {
        public_id: r.get(0)?,
        kind: r.get(1)?,
        content: r.get(2)?,
        uri: r.get(3)?,
        recorded_at: r.get(4)?,
        superseded_at: r.get(5)?,
        valid_from: r.get(6)?,
        valid_to: r.get(7)?,
        memory_kind: r.get(8)?,
    })
}

/// The domains a node is filed under, alphabetical.
fn node_domains(conn: &Connection, public_id: &str) -> Result<Vec<String>, DbError> {
    let mut stmt = match conn.prepare(
        "SELECT d.name FROM node n
         JOIN node_domains nd ON nd.node_id = n.id
         JOIN domain d ON d.id = nd.domain_id
         WHERE n.public_id = ?1 ORDER BY d.name ASC",
    ) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let rows = stmt.query_map([public_id], |r| r.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// One record's uses, newest first. Each carries the verdict the ledger
/// holds for it, or `null` while nothing has judged it.
fn uses_of(uses: &[(String, ContextUse)], feedback: &[ContextUseFeedback], id: &str) -> Vec<Value> {
    let verdicts: HashMap<&str, &ContextUseFeedback> = feedback
        .iter()
        .map(|f| (f.context_use_id.as_str(), f))
        .collect();
    let mut rows: Vec<&(String, ContextUse)> = uses
        .iter()
        .filter(|(_, u)| u.context_record_id == id)
        .collect();
    rows.sort_by(|a, b| {
        b.1.observed_at
            .cmp(&a.1.observed_at)
            .then_with(|| b.0.cmp(&a.0))
    });
    rows.into_iter()
        .take(MAX_LISTED_USES)
        .map(|(use_id, u)| {
            let verdict = verdicts.get(use_id.as_str()).map(|f| {
                json!({
                    "evaluation": f.evaluation.as_str(),
                    "method": f.evaluation_method.as_ref().map(|m| m.as_str()),
                    "confidence": f.attribution_confidence.get(),
                    "had_opportunity": f.had_opportunity,
                    "outcome_relation": f.outcome_relation.as_str(),
                    "effects": f.observable_effect_refs,
                    "statement": f.influence_statement.as_deref().map(|s| truncate(s, 500)),
                    "observed_at": f.observed_at,
                })
            });
            json!({
                "use_id": use_id,
                "task_id": u.task_id,
                "use_kind": u.use_kind.as_str(),
                "influence_stage": u.influence_stage.as_str(),
                "observed_at": u.observed_at,
                "verdict": verdict,
            })
        })
        .collect()
}

/// The turns a record was rendered into, newest first. They come from the
/// store's context receipts joined to their execution. Each row links to
/// that turn's transcript page.
fn renderings_of(root: &Path, id: &str) -> Result<Vec<Value>, DbError> {
    let Some(conn) = open_read_only(&root.join(".stella/private/store.db")) else {
        return Ok(Vec::new());
    };
    let mut stmt = match conn.prepare(
        "SELECT cb.execution_id, min(cb.first_seen_ts), coalesce(sum(cb.token_cost), 0),
                max(cb.content), e.prompt, e.outcome, e.session_id, e.kind
         FROM context_blocks cb
         LEFT JOIN executions e ON e.id = cb.execution_id
         WHERE cb.memory_id = ?1
         GROUP BY cb.execution_id
         ORDER BY cb.execution_id DESC
         LIMIT ?2",
    ) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let rows = stmt.query_map(rusqlite::params![id, MAX_LISTED_RENDERINGS as i64], |r| {
        Ok(json!({
            "execution_id": r.get::<_, i64>(0)?,
            "ts": r.get::<_, Option<String>>(1)?.unwrap_or_default(),
            "prompt_tokens": r.get::<_, i64>(2)?,
            "rendered": r.get::<_, Option<String>>(3)?.map(|s| truncate(&s, 1_200)),
            "prompt": r.get::<_, Option<String>>(4)?.map(|s| truncate(&s, 160)),
            "outcome": r.get::<_, Option<String>>(5)?,
            "session_id": r.get::<_, Option<String>>(6)?,
            "kind": r.get::<_, Option<String>>(7)?,
        }))
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

/// The records most often rendered into the same turns as this one,
/// ranked by shared turns. Each is named from whichever store knows it.
fn related_records(
    root: &Path,
    id: &str,
    context: Option<&Connection>,
    published: &[Value],
) -> Result<Vec<Value>, DbError> {
    let Some(conn) = open_read_only(&root.join(".stella/private/store.db")) else {
        return Ok(Vec::new());
    };
    let mut stmt = match conn.prepare(
        "SELECT other.memory_id, count(DISTINCT other.execution_id)
         FROM context_blocks this
         JOIN context_blocks other
           ON other.execution_id = this.execution_id
          AND other.memory_id != this.memory_id
         WHERE this.memory_id = ?1
           AND other.memory_id IS NOT NULL AND other.memory_id != ''
         GROUP BY other.memory_id
         ORDER BY 2 DESC, other.memory_id ASC
         LIMIT ?2",
    ) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let pairs = stmt
        .query_map(rusqlite::params![id, MAX_RELATED as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::with_capacity(pairs.len());
    for (other, shared) in pairs {
        let (title, kind) = match context
            .map(|c| node_by_id(c, &other))
            .transpose()?
            .flatten()
        {
            Some(node) => (title_of(&node.content), node.kind),
            None => match published_by_id(published, &other) {
                Some(card) => (
                    title_of(card["statement"].as_str().unwrap_or_default()),
                    card["kind"].as_str().unwrap_or("rule").to_owned(),
                ),
                None => (other.clone(), "unknown".to_owned()),
            },
        };
        out.push(json!({
            "id": other,
            "title": title,
            "kind": kind,
            "shared_turns": shared,
        }));
    }
    Ok(out)
}

/// Where the record's words live, for the source view.
///
/// A published record's source is its TOML file, read back whole by the
/// path the directory walk found. A recall node's source is the `memory`
/// row its `memory://` URI names, which is the revision the graph serves
/// now. Beside it sits the block as the model last saw it. Both come with
/// the command that changes them, because this page cannot.
fn source_of(
    node: Option<&NodeRow>,
    card: Option<&Value>,
    context: Option<&Connection>,
    renderings: &[Value],
) -> Result<Value, DbError> {
    let as_rendered = renderings
        .first()
        .and_then(|r| r["rendered"].as_str())
        .map(str::to_owned);
    if let Some(card) = card {
        let path = card["path"].as_str().unwrap_or_default();
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let handle = card["name"].as_str().unwrap_or_default();
        return Ok(json!({
            "kind": "published",
            "path": path,
            "text": truncate(&text, MAX_SOURCE_CHARS),
            "language": "toml",
            "as_rendered": as_rendered,
            "commands": [
                { "why": "Why did it apply, and what evidence carries it",
                  "run": format!("stella context explain ^{handle}") },
                { "why": "Change its scope or precedence and re-stamp its identity",
                  "run": format!("stella context amend ^{handle} --precedence <n>") },
                { "why": "Move it between advisory and blocking, on the ledger",
                  "run": format!("stella context promote ^{handle} --to <advisory|blocking> --reason \"…\"") },
            ],
        }));
    }
    let Some(node) = node else {
        return Ok(json!({ "kind": "missing", "commands": [] }));
    };
    let memory = match (
        context,
        node.uri
            .as_deref()
            .and_then(|u| u.strip_prefix("memory://")),
    ) {
        (Some(conn), Some(memory_id)) => memory_row(conn, memory_id)?,
        _ => None,
    };
    let id = &node.public_id;
    Ok(json!({
        "kind": "recall",
        "uri": node.uri,
        "valid_from": node.valid_from,
        "valid_to": node.valid_to,
        "memory": memory,
        "text": node.content,
        "language": "text",
        "as_rendered": as_rendered,
        "commands": [
            { "why": "Rewrite it in place as a new revision of the same memory",
              "run": format!("stella memory edit {id} \"…\"") },
            { "why": "Stop selecting it automatically; reversible with reaffirm",
              "run": format!("stella memory retire {id} --reason \"…\"") },
            { "why": "Publish it as a project rule under .stella/rules",
              "run": format!("stella memory promote {id}") },
            { "why": "Forget it, and stop the reflection loop re-learning it",
              "run": format!("stella memory forget {id}") },
        ],
    }))
}

/// The `memory` row behind a recall node. `None` on an older schema, or
/// when no row matches the URI.
fn memory_row(conn: &Connection, memory_id: &str) -> Result<Option<Value>, DbError> {
    let mut stmt = match conn.prepare(
        "SELECT public_id, kind, content, recorded_at, lineage_id, superseded_at
         FROM memory WHERE public_id = ?1",
    ) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut rows = stmt.query_map([memory_id], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "kind": r.get::<_, String>(1)?,
            "content": truncate(&r.get::<_, String>(2)?, MAX_CONTENT_CHARS),
            "recorded_at": r.get::<_, String>(3)?,
            "lineage_id": r.get::<_, Option<String>>(4)?,
            "superseded_at": r.get::<_, Option<String>>(5)?,
        }))
    })?;
    rows.next().transpose().map_err(Into::into)
}

/// The first sentence of a statement, clipped. A mined memory has no
/// title of its own, so this is the name it goes by on the list.
fn title_of(text: &str) -> String {
    let first_line = text.lines().next().unwrap_or_default().trim();
    let mut end = first_line.len();
    for (i, c) in first_line.char_indices() {
        if matches!(c, '.' | '!' | '?') {
            let next = first_line[i + c.len_utf8()..].chars().next();
            // A period inside a path or a version (`v1.2`, `foo.rs`) does
            // not end a sentence. One followed by a space or nothing does.
            if next.is_none_or(char::is_whitespace) {
                end = i;
                break;
            }
        }
    }
    truncate(
        first_line[..end].trim_end_matches(['.', '!', '?']),
        MAX_TITLE_CHARS,
    )
}

/// A file's mtime in the RFC 3339 form every other timestamp here uses.
/// The page then sorts and renders one column with one parser.
fn unix_to_rfc3339(unix: i64) -> String {
    if unix <= 0 {
        return String::new();
    }
    // Civil-from-days (Howard Hinnant's algorithm). This leaf needs no
    // calendar crate for one column.
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3_600,
        (secs % 3_600) / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_the_first_sentence_and_a_path_is_not_a_sentence_end() {
        assert_eq!(
            title_of("Read foo.rs first. Then edit."),
            "Read foo.rs first"
        );
        assert_eq!(title_of("One line, no period"), "One line, no period");
        assert_eq!(title_of("Two lines?\nSecond"), "Two lines");
        assert_eq!(title_of(""), "");
    }

    #[test]
    fn the_id_alphabet_is_closed() {
        assert!(valid_id("nod_e7e2e23a62d124769c55e695"));
        assert!(valid_id("^pre-push-runs-gate"));
        assert!(valid_id("ctx.macanderson.stella.cite-doc-by-id"));
        assert!(!valid_id(""));
        assert!(!valid_id("a b"));
        assert!(!valid_id("x\"; DROP TABLE node; --"));
        assert!(!valid_id(&"a".repeat(MAX_ID_LEN + 1)));
    }

    #[test]
    fn unix_seconds_render_as_rfc3339() {
        assert_eq!(unix_to_rfc3339(0), "");
        // Checked against `date -u -r 1756000000`.
        assert_eq!(unix_to_rfc3339(1_756_000_000), "2025-08-24T01:46:40Z");
        assert_eq!(unix_to_rfc3339(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn a_zero_fold_reads_as_unused_and_a_failing_fold_first() {
        let identity = orphan_identity("nod_x");
        let row = list_row(identity, None, None, None);
        assert_eq!(row["standing"], "unused");
        assert_eq!(standing_rank(&row), 3);
        let mut failing = zero_health("nod_y");
        failing.uses = 6;
        failing.attributable = true;
        failing.failing = true;
        let row = list_row(orphan_identity("nod_y"), Some(&failing), None, None);
        assert_eq!(row["standing"], "failing");
        assert_eq!(standing_rank(&row), 0);
    }
}
