//! Generalizing the citation loop — Phase 4 deliverable 2 (#715).
//!
//! [`ObservationSource`] has always had three variants. Only one of them,
//! `ReflectionLesson`, had a constructor: `MemoryCitation` and `ToolOutcome`
//! were declared in Phase 1 and written by nothing, so the typed loop learned
//! from reflection prose and from no other evidence at all. This module fills
//! the other two, which is what turns explicit citation from *the* evidence
//! source into *one* of them.
//!
//! # What does not change
//!
//! The deliverable is explicit that the citation loop keeps its semantics, and
//! it does — untouched. `fold_citation_stats` still derives quarantine at
//! [`stella_store::QUARANTINE_NEGATIVES_THRESHOLD`] untruthful citations and
//! promotion eligibility at a streak past
//! [`stella_store::PROMOTION_CITATIONS_REQUIRED`], still recomputed on every
//! read, still never stored. Nothing here reads those thresholds, changes
//! them, or adds a second path to quarantine.
//!
//! What changes is that a citation now *also* leaves an observation, so the
//! same miner that learns from reflection prose can learn from what the model
//! said about a memory while using it. Two evidence sources feeding one loop,
//! rather than one source feeding the loop and another feeding a separate
//! quarantine counter that nothing else can see.
//!
//! # Why only negative citations become observations
//!
//! A positive citation says "this memory was right", which the memory already
//! claims — mining it produces an observation that restates its own evidence,
//! and spec §7's second anti-poisoning rule is that a proposal may not cite
//! itself. A *negative* citation says something the corpus does not already
//! contain: that a stored belief is wrong. That is new information and is worth
//! learning from.
//!
//! # Why only failed tool calls
//!
//! Same asymmetry. A tool that worked is the expected case; mining it would
//! bury the signal under thousands of successes. A failure is a fact about this
//! workspace that the next turn could use.

use stella_context::{AppendOutcome, ContextStore, LedgerAppend};
use stella_core::context_record::{
    ContextRecordKind, LIFECYCLE_SCHEMA_VERSION, ObservationRecord, ObservationSource,
};
use stella_core::redact::redact_secrets;
use stella_store::MemoryCitationRow;

/// The longest observation text either source will emit.
///
/// A remark is capped at 300 characters by `cite_memory` itself, but a tool
/// error is unbounded — a stack trace or a wall of compiler output would
/// otherwise become one enormous "observation" that the miner cannot cluster
/// and a person cannot read.
const MAX_OBSERVATION_CHARS: usize = 300;

/// Truncate on a character boundary, marking that it happened.
///
/// Silent truncation is banned by spec §5.5, and this is the honest form: the
/// ellipsis is in the stored text, so a reader of the ledger can see the
/// sentence was cut rather than believing the tool said only that much.
fn bounded(text: &str) -> String {
    if text.chars().count() <= MAX_OBSERVATION_CHARS {
        return text.to_string();
    }
    let kept: String = text.chars().take(MAX_OBSERVATION_CHARS - 1).collect();
    format!("{kept}…")
}

/// Append an observation for one negative citation. `None` if there is nothing
/// worth recording or the write failed.
///
/// The candidate id is the cited memory rather than the citation, so repeated
/// negative citations of the same memory across different tasks cluster —
/// which is exactly the recurrence the proposal miner counts.
pub(super) fn citation_observation(
    store: &ContextStore,
    citation: &MemoryCitationRow,
    task_id: &str,
    observed_at: &str,
) -> Option<AppendOutcome> {
    // Positive citations are self-confirming; see the module docs.
    if citation.is_positive() {
        return None;
    }
    if citation.remark.trim().is_empty() {
        return None;
    }
    let text = format!(
        "memory {} was cited unhelpful: {}",
        citation.memory_id,
        citation.remark.trim()
    );
    append(
        store,
        ObservationSource::MemoryCitation,
        format!("citation:{}", citation.memory_id),
        task_id,
        &text,
        observed_at,
    )
}

/// Append an observation for one failed tool call.
pub(super) fn tool_outcome_observation(
    store: &ContextStore,
    tool: &str,
    error: &str,
    task_id: &str,
    observed_at: &str,
) -> Option<AppendOutcome> {
    if error.trim().is_empty() {
        return None;
    }
    let text = format!("tool {tool} failed: {}", error.trim());
    // Keyed on the tool, so repeated failures of the same tool cluster into one
    // candidate rather than each becoming its own singleton.
    append(
        store,
        ObservationSource::ToolOutcome,
        format!("tool:{tool}"),
        task_id,
        &text,
        observed_at,
    )
}

/// Redact, bound, type, and append. `None` on any failure.
fn append(
    store: &ContextStore,
    source: ObservationSource,
    candidate_id: String,
    task_id: &str,
    text: &str,
    observed_at: &str,
) -> Option<AppendOutcome> {
    // Redaction happens BEFORE the record is constructed, matching
    // [`super::observations::append_observation`]: no unredacted typed record
    // ever exists, not even transiently as something a later refactor could
    // persist by accident. A tool error is the single most likely place in this
    // tree for a credential to appear — a failing curl or psql prints its own
    // arguments — so this ordering is load-bearing here, not ceremonial.
    let redaction = redact_secrets(&bounded(text));
    let record = ObservationRecord::new(
        source,
        candidate_id,
        task_id.to_string(),
        redaction.text,
        Vec::new(),
        redaction.redacted,
        observed_at.to_string(),
    )
    .ok()?;

    let body = serde_json::to_string(&record).ok()?;
    store
        .append_record(LedgerAppend {
            record_id: &record.record_id,
            lineage_id: &record.lineage_id,
            record_kind: ContextRecordKind::Observation.as_str(),
            record_hash: &record.record_hash,
            schema_version: LIFECYCLE_SCHEMA_VERSION,
            body: &body,
            observed_at: &record.observed_at,
            supersedes: None,
        })
        .ok()
}

#[cfg(test)]
mod tests;
