//! Context-use extraction — Phase 4 deliverable 1 (#715).
//!
//! Turns what a finished turn *did* into immutable ledger records: which
//! context records the frame put in front of the model ([`ContextUse`]), and
//! what the turn then said about them ([`ContextUseFeedback`]).
//!
//! # Why this is an extractor and not a write at close-out
//!
//! The obvious design records a use where the use happens. It was rejected for
//! two reasons, both structural:
//!
//! 1. **`record_execution_end` holds `store.db` only**, and has nine call
//!    sites (`run`, REPL, two `/goal` loops, three deck paths, fleet,
//!    subsession). Threading a second database through all of them puts the
//!    audit close-out — the function that decides `audit_complete` — one
//!    borrow away from a `context.db` failure it has no business caring about.
//! 2. **The receipt plane has no turn-end hook at all.** The `ReceiptLedger`
//!    is stack-local to `run_turn_with_sender` and emits its manifest *before*
//!    the model call commits, so there is no moment inside it where an outcome
//!    is known.
//!
//! Extraction sidesteps both. Everything a use record needs is already durable
//! in `store.db` — `context_blocks` says what the frame carried,
//! `memory_citations` says what the turn thought of it, `executions.outcome`
//! says how the turn ended — so the record can be *derived* later, exactly the
//! shape Phase 3 already uses for reflection lessons
//! ([`super::observations`]). Deriving also makes the write replay-idempotent
//! for free: ids are content-addressed, so a re-scan re-derives the same ids
//! and the ledger absorbs them as `AlreadyPresent`.
//!
//! # Why the records land in `context.db` and not `store.db`
//!
//! The Phase 4 gate is "aggregates rebuild exactly from immutable records."
//! `memory_citations` is in [`stella_store::prune::DEPENDENT_TABLES`], so
//! `stella stats prune` deletes citation rows with their execution — a fold
//! over them is *not* rebuildable across a prune. `context_records` is
//! append-only enforced by SQLite triggers and is never pruned, so it is the
//! only surface in the tree where that gate actually holds.
//!
//! The consequence, stated rather than hidden: extraction must run *before* a
//! prune reaches an execution, or that turn's uses are never captured. The
//! capture window is bounded by prune retention (90 days by default), and the
//! extractor runs every turn, so it is not a practical risk — but it is a real
//! ordering dependency and not an invariant this module can enforce alone.

use stella_context::{AppendOutcome, ContextStore, LedgerAppend};
use stella_core::context_record::{
    ContextInfluenceStage, ContextOutcomeRelation, ContextRecordKind, ContextUse,
    ContextUseEvaluation, ContextUseFeedback, ContextUseKind, LIFECYCLE_SCHEMA_VERSION,
    SelectionHealth, SelectionHealthPolicy, fold_selection_health, record_hash,
};
use stella_store::{FinishedExecution, MemoryCitationRow, Store};

/// The cursor key for execution-sourced context uses. Stable — it is a primary
/// key in `context_extraction_cursor`, and changing it re-scans from zero.
const USE_SOURCE: &str = "executions.context_use";

/// How many executions one extraction pass will look at.
///
/// Bounded because this runs on the turn path: a workspace whose ledger has
/// been off for a thousand turns must not pay for all of them at once. The
/// cursor advances by whatever this pass covered, so the backlog drains over
/// subsequent turns rather than stalling one.
const EXTRACTION_BATCH: u32 = 64;

/// The evaluation method every verdict this module writes carries.
///
/// **`agent_self_report`, and the choice matters.** `cite_memory` is a tool the
/// *agent* calls about context the agent was given; no human and no external
/// system is involved. Labelling that `trace_correlation` would be a category
/// error with teeth, because
/// `ContextEvaluationMethod::is_pruning_eligible` reads the method to decide
/// whether a verdict may retire a record — and a self-report that could retire
/// its own subject is self-reinforcing: a model that misreads a memory reports
/// it unhelpful, which retires it, which destroys the evidence that the reading
/// was wrong.
///
/// So citations inform selection health and stay fully visible, and cannot on
/// their own remove anything. They still drive the shipped quarantine loop
/// exactly as before — that path is untouched.
const METHOD_AGENT_SELF_REPORT: &str =
    stella_core::context_record::ContextEvaluationMethod::AGENT_SELF_REPORT;

/// The task identity for one execution's uses.
///
/// **A turn is not a task**, and this is the same honest approximation
/// `super::observations::task_id_for` makes for reflection lessons: it
/// under-counts correlation (three turns on one task read as three tasks),
/// which is the unsafe direction for the anti-poisoning rule in spec §7.
/// Prefer the session when the row carries one — turns in a session are at
/// least *plausibly* one task, which is strictly closer than the per-turn
/// fallback — and say so in the id so a reader can tell which they are looking
/// at.
fn task_id_for(execution: &FinishedExecution) -> String {
    match execution.session_id.as_deref() {
        Some(session) if !session.is_empty() => format!("session:{session}"),
        _ => format!("execution:{}", execution.execution_id),
    }
}

/// The use-trace id joining one execution's uses to their feedback.
///
/// Derived from the execution id rather than random so a replay of the same
/// execution produces the same trace — the property that makes the whole
/// extractor idempotent. Prefixed `ut_` to match the `nod_`/`obs_`/`pev_`
/// convention the rest of the tree uses.
fn use_trace_id(execution_id: i64) -> String {
    format!("ut_{execution_id}")
}

/// When a use was observed. Falls back to the ledger's own clock only when the
/// execution row has no `finished_at` — a nullable column, so a reader that
/// assumed it was present would panic on a crashed turn's row.
fn observed_at(execution: &FinishedExecution) -> String {
    execution
        .finished_at
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| stella_context::format_rfc3339(0))
}

/// Extract every finished execution the ledger has not yet attributed.
///
/// Returns how many new records were appended, for reporting. Errors are
/// swallowed for the same reason [`super::observations`] swallows them: a
/// store that will not answer means no attribution this turn, not a failed
/// turn. Spec §5.6 — mining failure must never fail the user's task.
pub(crate) fn extract_context_uses(store: &Store, context: &ContextStore) -> usize {
    let cursor: i64 = context
        .extraction_cursor(USE_SOURCE)
        .ok()
        .flatten()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);

    let Ok(executions) = store.finished_executions_after(cursor, EXTRACTION_BATCH) else {
        return 0;
    };

    let mut appended = 0usize;
    let mut highest = cursor;
    for execution in &executions {
        appended += extract_one(store, context, execution);
        highest = highest.max(execution.execution_id);
    }

    // Advanced last and best-effort, exactly as the reflection extractor does:
    // if this write fails the next pass re-scans and re-derives the same
    // content-addressed ids, which the ledger absorbs as replays.
    if highest > cursor {
        let _ = context.set_extraction_cursor(USE_SOURCE, &highest.to_string());
    }
    appended
}

/// Attribute one execution. Returns how many records were newly appended.
fn extract_one(store: &Store, context: &ContextStore, execution: &FinishedExecution) -> usize {
    let Ok(blocks) = store.context_blocks(execution.execution_id) else {
        return 0;
    };

    // One use per distinct context record the frame carried. `context_blocks`
    // is per block, and a record rendered into two blocks is still one use of
    // one record — deduplicating here rather than at read time keeps the
    // ledger's own count honest.
    let mut record_ids: Vec<String> = blocks
        .into_iter()
        .filter_map(|block| block.memory_id)
        .filter(|id| !id.is_empty())
        .collect();
    record_ids.sort();
    record_ids.dedup();
    if record_ids.is_empty() {
        return 0;
    }

    let trace = use_trace_id(execution.execution_id);
    let task = task_id_for(execution);
    let at = observed_at(execution);

    let mut appended = 0usize;
    // Built as the uses are written so the feedback below can name the exact
    // id the ledger holds. Deriving the id twice from the same inputs would
    // work until someone changed one derivation; carrying it forward cannot
    // drift.
    let mut use_ids: Vec<(String, String)> = Vec::with_capacity(record_ids.len());
    for record_id in &record_ids {
        // `Rendered` and not `Selected`: a `context_blocks` row exists because
        // the block was registered into the prompt the model actually saw.
        // Claiming `Cited` here would be a lie — that is what the citation
        // feedback below establishes, separately.
        let use_record = ContextUse {
            use_kind: ContextUseKind::Rendered,
            context_record_id: record_id.clone(),
            use_trace_id: trace.clone(),
            task_id: task.clone(),
            influence_stage: ContextInfluenceStage::None,
            observed_at: at.clone(),
        };
        let Some((id, outcome)) = append_use(context, &use_record) else {
            continue;
        };
        if outcome == AppendOutcome::Appended {
            appended += 1;
        }
        use_ids.push((record_id.clone(), id));
    }

    let Ok(citations) = store.memory_citations_for_execution(execution.execution_id) else {
        return appended;
    };
    for citation in &citations {
        // Feedback only for a record this frame actually carried. A citation
        // naming something the frame never rendered has no use to attach to,
        // and inventing one would make the ledger claim an opportunity that
        // never existed.
        let Some((_, use_id)) = use_ids.iter().find(|(id, _)| id == &citation.memory_id) else {
            continue;
        };
        let feedback = feedback_for(citation, use_id, &trace, &task, &at);
        if append_feedback(context, &feedback) == Some(AppendOutcome::Appended) {
            appended += 1;
        }
    }

    // Deliverable 2: the same citations also become observations, so the miner
    // that learns from reflection prose learns from them too. Every citation is
    // considered — including ones naming a record this frame never carried,
    // which have no use to attach feedback to but are still a real remark about
    // a real memory.
    for citation in &citations {
        if super::evidence::citation_observation(context, citation, &task, &at)
            == Some(AppendOutcome::Appended)
        {
            appended += 1;
        }
    }

    // The third source. Declared in Phase 1, unwritten until now.
    if let Ok(failures) = store.failed_tool_calls_for_execution(execution.execution_id) {
        for (tool, error) in &failures {
            if super::evidence::tool_outcome_observation(context, tool, error, &task, &at)
                == Some(AppendOutcome::Appended)
            {
                appended += 1;
            }
        }
    }
    appended
}

/// Map one citation onto the typed feedback record.
///
/// **This is where deliverable 3 lives.** An explicit citation is the one
/// evidence source that genuinely establishes opportunity — the model named
/// the record, so it had the record in hand — which is why `had_opportunity`
/// is `true` here and why nothing else in this module writes a `NotHelpful`
/// verdict. Absence of a citation is *not* evidence of uselessness, so an
/// uncited use gets no feedback record at all rather than a negative one.
fn feedback_for(
    citation: &MemoryCitationRow,
    use_id: &str,
    trace: &str,
    task: &str,
    at: &str,
) -> ContextUseFeedback {
    let positive = citation.is_positive();
    ContextUseFeedback {
        // The id the ledger actually holds, carried from the write above, so
        // the two records resolve to each other without a join table.
        context_use_id: use_id.to_string(),
        use_trace_id: trace.to_string(),
        task_id: task.to_string(),
        evaluation: if positive {
            ContextUseEvaluation::Helpful
        } else {
            ContextUseEvaluation::Neutral
        },
        had_opportunity: true,
        influence_stage: ContextInfluenceStage::FinalResponse,
        outcome_relation: ContextOutcomeRelation::Unknown,
        observable_effect_refs: Vec::new(),
        evaluation_method: stella_core::context_record::ContextEvaluationMethod::new(
            METHOD_AGENT_SELF_REPORT,
        )
        .ok(),
        attribution_confidence: stella_core::context_record::Confidence::new(
            CITATION_ATTRIBUTION_CONFIDENCE,
        )
        .expect("50 is within 0..=100"),
        outcome_assessment_id: None,
        influence_statement: None,
        observed_at: at.to_string(),
    }
}

/// The confidence a citation-derived attribution carries.
///
/// Deliberately below `EfficacySettings::min_attribution_confidence` (80): a
/// citation proves the model *referenced* the record, not that the record
/// changed the outcome. Recording it at a confidence that would clear the
/// pruning bar on its own would let one enthusiastic turn retire something.
const CITATION_ATTRIBUTION_CONFIDENCE: u16 = 50;

/// Append one use record, returning the id the ledger holds and whether it was
/// new. `None` on any failure.
///
/// The id is the record's own canonical hash (ADR 0004), truncated the same
/// way [`stella_core::context_record::ObservationRecord`] truncates its own —
/// so two runs of identical work derive the same id and the second is a
/// replay, not a duplicate.
fn append_use(context: &ContextStore, record: &ContextUse) -> Option<(String, AppendOutcome)> {
    let body = serde_json::to_string(record).ok()?;
    let hash = record_hash(record).ok()?;
    let digest = hash.strip_prefix("sha256:").unwrap_or(&hash);
    let record_id = format!("cu_{}", &digest[..12]);
    let outcome = context
        .append_record(LedgerAppend {
            record_id: &record_id,
            // A use is its own lineage: it is an event, not a revisable
            // statement, so nothing ever supersedes it.
            lineage_id: &record_id,
            record_kind: ContextRecordKind::ContextUse.as_str(),
            record_hash: &hash,
            schema_version: LIFECYCLE_SCHEMA_VERSION,
            body: &body,
            observed_at: &record.observed_at,
            supersedes: None,
        })
        .ok()?;
    Some((record_id, outcome))
}

/// Append one feedback record, refusing any that violates the intra-record
/// invariants. `None` on any failure. Shared with the deterministic-validation
/// source ([`super::validation`], #753) so every verdict — whatever its method
/// — derives its id and passes the validator identically.
pub(crate) fn append_feedback(
    context: &ContextStore,
    record: &ContextUseFeedback,
) -> Option<AppendOutcome> {
    // The validator is the enforcement point for deliverable 3, so it runs on
    // the write path rather than being trusted to have run at the caller. A
    // record that cannot justify its own verdict never reaches the ledger.
    record.validate().ok()?;
    let body = serde_json::to_string(record).ok()?;
    let hash = record_hash(record).ok()?;
    let digest = hash.strip_prefix("sha256:").unwrap_or(&hash);
    let record_id = format!("cuf_{}", &digest[..12]);
    context
        .append_record(LedgerAppend {
            record_id: &record_id,
            lineage_id: &record_id,
            record_kind: ContextRecordKind::ContextUseFeedback.as_str(),
            record_hash: &hash,
            schema_version: LIFECYCLE_SCHEMA_VERSION,
            body: &body,
            observed_at: &record.observed_at,
            supersedes: None,
        })
        .ok()
}

/// How many use/feedback records one health computation reads.
///
/// The ledger grows monotonically and this runs on the turn path. Bounded the
/// same way [`super::observations::all_observations`] is bounded, and for the
/// same reason.
const HEALTH_READ_LIMIT: usize = 20_000;

/// Read the ledger and derive every record's selection health (#715
/// deliverable 4).
///
/// The read is `records_of_kind`, which is `observed_at`-ordered, and the fold
/// is order-independent by construction — so this is a projection of the
/// ledger's contents and nothing else. Recomputing it is the only way to read
/// it; there is no stored copy that could disagree.
pub(crate) fn selection_health(
    context: &ContextStore,
    policy: SelectionHealthPolicy,
) -> Vec<SelectionHealth> {
    let uses: Vec<(String, ContextUse)> = context
        .records_of_kind(ContextRecordKind::ContextUse.as_str(), HEALTH_READ_LIMIT)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| {
            serde_json::from_str::<ContextUse>(&row.body)
                .ok()
                .map(|body| (row.record_id, body))
        })
        .collect();
    let feedback: Vec<ContextUseFeedback> = context
        .records_of_kind(
            ContextRecordKind::ContextUseFeedback.as_str(),
            HEALTH_READ_LIMIT,
        )
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| serde_json::from_str::<ContextUseFeedback>(&row.body).ok())
        .collect();
    fold_selection_health(&uses, &feedback, policy)
}

#[cfg(test)]
mod tests;
