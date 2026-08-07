// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Read-only view over `.stella/private/context.db`'s self-improvement plane
//! (#1871): the promotion audit trail, episodic memory, and selection health.
//!
//! The Self-improve tab shows the skills a session *used*; this module shows
//! the lifecycle that *created* them — the append-only `context_records`
//! ledger (observation → `record_proposal` → `promotion_event`, context.db v8,
//! `crates/stella-context/src/store/schema.rs::migrate_v8`), the `episode`
//! rows recall ranks, and the use/feedback records selection health is folded
//! from.
//!
//! Two boundaries hold, same as everywhere else in this crate:
//!
//! - **The file is opened `SQLITE_OPEN_READ_ONLY` and never migrated.** A
//!   missing file, table, or column is a state — each section degrades to an
//!   empty payload and fills in the moment a session migrates the store.
//! - **Folds are linked, never copied.** Record bodies deserialize through
//!   `stella_core::context_record`'s own types, and selection health goes
//!   through the same `fold_selection_health` the CLI runs — pure decision
//!   logic over owned data (invariant 2), the same bargain that lets this
//!   crate link the self-driving fold. What *is* re-implemented here is the
//!   SQL itself, because the write side (`stella-context`) runs migrations on
//!   open and an observer must never mutate what it observes; the
//!   schema-conformance suite builds a real `context.db` through that crate's
//!   migration path to prove these queries stay resolvable.
//!
//! Episode ↔ execution linkage is deliberately absent: the only tie is a
//! summary-tag convention (see `crates/stella-cli/src/memory.rs`, the #1042
//! trace-pointer note), so episodes render as they are rather than through an
//! invented join.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use rusqlite::Connection;
use serde_json::{Value, json};
use stella_core::context_record::{
    ContextUse, ContextUseFeedback, PromotionAction, PromotionEventRecord, ProposalRecord,
    SelectionHealthPolicy, fold_selection_health,
};

use crate::db::{DbError, collect_rows, is_missing_schema, open_read_only, truncate};

/// Ledger revisions read per record kind, newest first in append order —
/// mirrors `stella proposals`' own `READ_LIMIT`
/// (`crates/stella-cli/src/proposals_cmd.rs`), so the dashboard folds the same
/// window the CLI folds.
const RECORD_READ_LIMIT: usize = 2_000;

/// Use/feedback records read for the selection-health fold — mirrors the
/// CLI's `HEALTH_READ_LIMIT` (`crates/stella-cli/src/memory/uses.rs`). This is
/// the largest read here, and it runs on the lazily-fetched tab, never the 5 s
/// poll.
const USE_READ_LIMIT: usize = 20_000;

/// Newest episodes listed. Episodic memory grows without bound; the tab wants
/// the recent window, not the corpus.
const MAX_LISTED_EPISODES: usize = 100;

/// Newest promotion events on the timeline.
const MAX_LISTED_EVENTS: usize = 100;

/// Selection-health rows returned, worst standing first.
const MAX_HEALTH_ROWS: usize = 50;

/// The promotion gate `stella proposals` displays eligibility against
/// (`proposals_cmd::list` calls `is_eligible(3, 3)`), mirrored so the two
/// surfaces can never disagree about what "eligible" means.
const MIN_DISTINCT_TASKS: u32 = 3;
/// See [`MIN_DISTINCT_TASKS`].
const MIN_OBSERVATIONS: u32 = 3;

/// The self-improvement lifecycle payload for `/api/context-lifecycle`.
pub(crate) fn self_improvement(root: &Path) -> Result<Value, DbError> {
    let db = root.join(".stella").join("private").join("context.db");
    let Some(conn) = open_read_only(&db) else {
        return Ok(empty_payload(false));
    };

    let counts = collect_rows(
        &conn,
        "SELECT record_kind, count(*) FROM context_records
         GROUP BY record_kind ORDER BY record_kind ASC",
        |r| {
            Ok(json!({
                "kind": r.get::<_, String>(0)?,
                "records": r.get::<_, i64>(1)?,
            }))
        },
    )?;

    let proposals = proposal_rows(&conn)?;
    let events = event_rows(&conn)?;
    let episodes = episode_rows(&conn)?;
    let health = health_rows(&conn)?;
    let policy = SelectionHealthPolicy::default();

    Ok(json!({
        "present": true,
        "counts": counts,
        "proposals": proposals,
        "events": events,
        "episodes": episodes,
        "selection_health": health,
        // The thresholds the fold above ran with. The CLI reads tuned values
        // out of the settings scope chain; the dashboard runs the shipped
        // defaults and says so, rather than silently presenting a verdict
        // computed under thresholds the reader cannot see.
        "health_policy": {
            "min_attributable_uses": policy.min_attributable_uses,
            "not_helpful_ratio_threshold": policy.not_helpful_ratio_threshold,
            "min_attribution_confidence": policy.min_attribution_confidence,
        },
    }))
}

/// The payload shape with nothing in it. Same key set as the populated branch
/// so a client indexing a section never finds the key missing — the lesson
/// `Observatory::cursor` already carries.
fn empty_payload(present: bool) -> Value {
    let policy = SelectionHealthPolicy::default();
    json!({
        "present": present,
        "counts": [],
        "proposals": [],
        "events": [],
        "episodes": [],
        "selection_health": [],
        "health_policy": {
            "min_attributable_uses": policy.min_attributable_uses,
            "not_helpful_ratio_threshold": policy.not_helpful_ratio_threshold,
            "min_attribution_confidence": policy.min_attribution_confidence,
        },
    })
}

/// The newest `limit` `(record_id, body)` pairs of one ledger kind, returned
/// in ascending append order.
///
/// Append order (`rowid`), not `observed_at`: the ledger is append-only and
/// never updated or deleted from, so `rowid` is its insertion sequence, and a
/// last-write-wins fold over `observed_at` would order two same-second
/// decisions by content hash — the deterministic wrong answer
/// `stella-context`'s `records_of_kind_newest_in_append_order` documents.
fn kind_rows(
    conn: &Connection,
    kind: &str,
    limit: usize,
) -> Result<Vec<(String, String)>, DbError> {
    let mut stmt = match conn.prepare(
        "SELECT record_id, body FROM context_records
         WHERE record_kind = ?1 ORDER BY rowid DESC LIMIT ?2",
    ) {
        Ok(stmt) => stmt,
        Err(e) if is_missing_schema(&e) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mapped = stmt.query_map(rusqlite::params![kind, limit as i64], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?;
    let mut rows = mapped.collect::<rusqlite::Result<Vec<_>>>()?;
    rows.reverse();
    Ok(rows)
}

/// Every promotion event in the window, parsed. Ascending append order — the
/// replay order the decision fold requires.
fn parsed_events(conn: &Connection) -> Result<Vec<PromotionEventRecord>, DbError> {
    Ok(kind_rows(conn, "promotion_event", RECORD_READ_LIMIT)?
        .into_iter()
        .filter_map(|(_, body)| serde_json::from_str::<PromotionEventRecord>(&body).ok())
        .collect())
}

/// Current proposals with their decision standing and their own slice of the
/// promotion-event log — the `context_records` lineage fold, one entry per
/// proposal lineage.
fn proposal_rows(conn: &Connection) -> Result<Vec<Value>, DbError> {
    // Newest revision per lineage: ascending append order means a later
    // revision overwrites its predecessor in the map, exactly as
    // `proposals_cmd::current_proposals` folds.
    let mut latest: BTreeMap<String, ProposalRecord> = BTreeMap::new();
    for (_, body) in kind_rows(conn, "record_proposal", RECORD_READ_LIMIT)? {
        if let Ok(proposal) = serde_json::from_str::<ProposalRecord>(&body) {
            latest.insert(proposal.lineage_id.clone(), proposal);
        }
    }
    let events = parsed_events(conn)?;
    // Last write wins — the whole governance state machine: an ignored
    // proposal later kept has been kept, and both acts remain readable.
    let mut decided: HashMap<String, PromotionAction> = HashMap::new();
    let mut per_lineage: HashMap<String, Vec<&PromotionEventRecord>> = HashMap::new();
    for event in &events {
        decided.insert(event.proposal_lineage_id.clone(), event.action);
        per_lineage
            .entry(event.proposal_lineage_id.clone())
            .or_default()
            .push(event);
    }

    let mut proposals: Vec<&ProposalRecord> = latest.values().collect();
    proposals.sort_by(|a, b| {
        b.observed_at
            .cmp(&a.observed_at)
            .then_with(|| a.candidate_id.cmp(&b.candidate_id))
    });
    Ok(proposals
        .into_iter()
        .map(|proposal| {
            // The displayed status is derived, never stored — the same
            // derivation `stella proposals` prints, minus the colors.
            let status = match decided.get(&proposal.lineage_id) {
                Some(PromotionAction::Rejected) => "ignored".to_string(),
                Some(action) => action.as_str().to_string(),
                None if proposal.is_eligible(MIN_DISTINCT_TASKS, MIN_OBSERVATIONS) => {
                    "eligible".to_string()
                }
                None => "collecting".to_string(),
            };
            let lineage_events: Vec<Value> = per_lineage
                .get(&proposal.lineage_id)
                .map(|events| events.iter().map(|e| event_json(e, None)).collect())
                .unwrap_or_default();
            json!({
                "lineage_id": proposal.lineage_id,
                "candidate_id": proposal.candidate_id,
                "kind": proposal.proposal_kind.as_str(),
                "title": proposal.title,
                "body": truncate(&proposal.body, 240),
                "status": status,
                "distinct_tasks": proposal.score.distinct_tasks,
                "occurrences": proposal.score.occurrences,
                "salient": proposal.score.salient,
                "confidence": proposal.confidence.get(),
                "supporting_observations": proposal.supporting_observations,
                "observed_at": proposal.observed_at,
                "events": lineage_events,
            })
        })
        .collect())
}

/// The promotions timeline: the newest events, newest first, each carrying the
/// candidate its proposal lineage currently names (when the proposal is still
/// inside the read window).
fn event_rows(conn: &Connection) -> Result<Vec<Value>, DbError> {
    let mut titles: HashMap<String, (String, String)> = HashMap::new();
    for (_, body) in kind_rows(conn, "record_proposal", RECORD_READ_LIMIT)? {
        if let Ok(proposal) = serde_json::from_str::<ProposalRecord>(&body) {
            titles.insert(
                proposal.lineage_id.clone(),
                (proposal.candidate_id, proposal.title),
            );
        }
    }
    let mut events = parsed_events(conn)?;
    events.reverse();
    Ok(events
        .iter()
        .take(MAX_LISTED_EVENTS)
        .map(|event| event_json(event, titles.get(&event.proposal_lineage_id)))
        .collect())
}

/// One promotion event, shaped for the page.
fn event_json(event: &PromotionEventRecord, named: Option<&(String, String)>) -> Value {
    json!({
        "occurred_at": event.occurred_at,
        "action": event.action.as_str(),
        "actor": event.actor.as_str(),
        "reason": truncate(&event.reason, 200),
        "proposal_lineage_id": event.proposal_lineage_id,
        "enforcement": event.enforcement.map(|e| e.as_str()),
        "candidate_id": named.map(|(candidate, _)| candidate.as_str()),
        "title": named.map(|(_, title)| title.as_str()),
    })
}

/// The newest episodes, newest first. Superseded revisions are excluded — the
/// tab shows current episodic memory, and history stays queryable where it
/// lives.
///
/// `superseded_at` arrived with context.db v8; on an older file the whole
/// query degrades to an empty list and fills in after the next migration,
/// like every other missing-schema state here.
fn episode_rows(conn: &Connection) -> Result<Vec<Value>, DbError> {
    collect_rows(
        conn,
        &format!(
            "SELECT summary, files_touched, outcome, salience, started_at, ended_at
             FROM episode WHERE superseded_at IS NULL
             ORDER BY ended_at DESC, id DESC LIMIT {MAX_LISTED_EPISODES}"
        ),
        |r| {
            let files: Value =
                serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or_else(|_| json!([]));
            Ok(json!({
                "summary": truncate(&r.get::<_, String>(0)?, 240),
                "files_touched": files,
                "outcome": r.get::<_, String>(2)?,
                "salience": r.get::<_, f64>(3)?,
                "started_at": r.get::<_, String>(4)?,
                "ended_at": r.get::<_, String>(5)?,
            }))
        },
    )
}

/// Selection health, folded from the ledger's use/feedback records through the
/// same pure fold the CLI runs, worst standing first.
fn health_rows(conn: &Connection) -> Result<Vec<Value>, DbError> {
    let uses: Vec<(String, ContextUse)> = kind_rows(conn, "context_use", USE_READ_LIMIT)?
        .into_iter()
        .filter_map(|(record_id, body)| {
            serde_json::from_str::<ContextUse>(&body)
                .ok()
                .map(|body| (record_id, body))
        })
        .collect();
    let feedback: Vec<ContextUseFeedback> =
        kind_rows(conn, "context_use_feedback", USE_READ_LIMIT)?
            .into_iter()
            .filter_map(|(_, body)| serde_json::from_str::<ContextUseFeedback>(&body).ok())
            .collect();
    let mut health = fold_selection_health(&uses, &feedback, SelectionHealthPolicy::default());
    health.sort_by(|a, b| {
        b.failing
            .cmp(&a.failing)
            .then_with(|| {
                b.not_helpful_ratio
                    .partial_cmp(&a.not_helpful_ratio)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| b.uses.cmp(&a.uses))
            .then_with(|| a.context_record_id.cmp(&b.context_record_id))
    });
    Ok(health
        .iter()
        .take(MAX_HEALTH_ROWS)
        .map(|row| {
            json!({
                "context_record_id": row.context_record_id,
                "uses": row.uses,
                "distinct_tasks": row.distinct_tasks,
                "assessed_uses": row.assessed_uses,
                "helpful": row.helpful,
                "not_helpful": row.not_helpful,
                "neutral": row.neutral,
                "not_helpful_ratio": row.not_helpful_ratio,
                "attributable": row.attributable,
                "failing": row.failing,
            })
        })
        .collect())
}
