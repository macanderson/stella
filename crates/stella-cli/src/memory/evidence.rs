//! Observation sources beyond reflection lessons — Phase 4 deliverable 2
//! (#715).
//!
//! [`ObservationSource`] started with three variants. Only one,
//! `ReflectionLesson`, had a constructor — `MemoryCitation` and `ToolOutcome`
//! were declared in Phase 1 and written by nothing, so the typed loop learned
//! from reflection prose and from no other evidence at all. This module fills
//! `ToolOutcome`, the one source left standing beside `ReflectionLesson`.
//!
//! # Why `MemoryCitation` is gone
//!
//! It shipped here too, pairing an observed retrieval with a model's
//! usefulness judgement — and the judgement half had no producer. The
//! `cite_memory` tool that collected it is retired, and every replacement
//! source checked only knows a memory was *rendered* into the prompt, never
//! that it was judged useful or truthful. Writing `truthful: true` for "the
//! handle appeared somewhere" would fabricate a judgement no evidence
//! earned. See [`ObservationSource`]'s own doc comment for the full account;
//! the citation-derived quarantine loop it fed
//! (`fold_citation_stats`/thresholds in `stella-store`) stays as declared,
//! inert plumbing rather than being deleted with it.
//!
//! # Why only failed tool calls
//!
//! A tool that worked is the expected case; mining it would bury the signal
//! under thousands of successes. A failure is a fact about this workspace
//! that the next turn could use.

use stella_context::{AppendOutcome, ContextStore, LedgerAppend};
use stella_learn::redact::redact_secrets;
use stella_records::context_record::{
    ContextRecordKind, LIFECYCLE_SCHEMA_VERSION, ObservationRecord, ObservationSource,
};

/// The longest observation text this source will emit.
///
/// A tool error is unbounded — a stack trace or a wall of compiler output
/// would otherwise become one enormous "observation" that the miner cannot
/// cluster and a person cannot read.
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
    // arguments — so this ordering is required here, not ceremonial.
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
