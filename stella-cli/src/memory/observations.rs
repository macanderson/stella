//! Typed observation extraction — turning captured evidence into ledger
//! records, replay-idempotently and with secrets removed first.
//!
//! ## What this replaces
//!
//! The lexical loop re-read `.stella/private/reflections.jsonl` in full on every
//! reflection turn and mined it directly. That works, but it keeps no record of
//! *what was observed* as distinct from *what was proposed*, has no task
//! identity to count distinct tasks with, and persists whatever the model wrote
//! verbatim — including any credential that was in the transcript.
//!
//! Extraction sits between the log and the miner. The log stays exactly as it
//! is (it is the reflection recorder's own append-only journal and other code
//! reads it); what changes is that the miner now consumes typed observations out
//! of the ledger rather than raw JSONL.
//!
//! ## Replay-idempotence has two independent mechanisms, on purpose
//!
//! 1. **A cursor** bounds the work: a log with ten thousand lines is not
//!    re-parsed and re-redacted every turn.
//! 2. **Content-derived record ids** make the correctness claim: the same
//!    evidence always computes the same `record_id`, so the ledger's append
//!    recognizes a repeat as a replay.
//!
//! The cursor alone would be wrong, because it cannot be advanced atomically
//! with the record write — a crash between the two would either duplicate every
//! record in the batch or lose it. With derived ids, the cursor is a pure
//! optimization and a crash costs one re-scan, never a duplicate.
//!
//! ## Failure isolation
//!
//! Every function here swallows its errors and degrades to "no observations
//! this turn". `stella-cli`'s memory module states the contract: a failed
//! reflection, a malformed store, or a broken skills dir must NEVER fail or slow
//! the user's actual turn.

use std::path::Path;

use stella_context::{AppendOutcome, ContextStore, LedgerAppend};
use stella_core::context_record::{LIFECYCLE_SCHEMA_VERSION, ObservationRecord, ObservationSource};
use stella_core::redact::redact_secrets;

use super::ReflectionLesson;

/// The cursor key for the reflection log. Stable — it is a primary key.
const REFLECTION_SOURCE: &str = "reflections.jsonl";

/// The task identity for a reflection lesson.
///
/// Reflection runs **once per turn** and stamps every lesson it produced with
/// one `occurred_at`, so the timestamp is a real turn identity: thirty lessons
/// emitted by one reflection call share it, and lessons from different turns do
/// not. That is exactly the boundary spec §7's anti-poisoning rule needs against
/// the concrete attack — a lesson repeated many times inside a single turn can
/// never satisfy a three-task threshold.
///
/// **Known limitation, stated rather than hidden.** A turn is not a task. Three
/// turns spent on one task read as three distinct tasks, which is the *unsafe*
/// direction: it under-counts how correlated the evidence is. Closing it needs a
/// session- or goal-scoped identity that the reflection log does not currently
/// carry. [`ReflectionLesson::task_id`] exists so a caller that knows better can
/// supply one without a log-format change; nothing populates it yet.
fn task_id_for(lesson: &ReflectionLesson) -> String {
    if !lesson.task_id.is_empty() {
        return lesson.task_id.clone();
    }
    format!("turn:{}", lesson.occurred_at)
}

/// RFC 3339 UTC for a Unix timestamp, without pulling in a date library — the
/// context plane already speaks this format and the ledger stores it as text.
fn rfc3339(unix_secs: u64) -> String {
    stella_context::format_rfc3339(unix_secs as i64)
}

/// Extract every reflection lesson the ledger has not yet seen.
///
/// Returns how many new observations were appended, for reporting. Errors are
/// swallowed: a store that will not answer means no observations this turn, not
/// a failed turn.
pub(crate) fn extract_reflection_observations(store: &ContextStore, log_path: &Path) -> usize {
    let Ok(log) = std::fs::read_to_string(log_path) else {
        return 0;
    };
    // The cursor is a line count. A malformed or absent cursor restarts from
    // zero, which costs one re-scan and — because ids are content-derived —
    // produces no duplicates.
    let consumed: usize = store
        .extraction_cursor(REFLECTION_SOURCE)
        .ok()
        .flatten()
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);

    let lines: Vec<&str> = log.lines().collect();
    // A log that SHRANK cannot be the one the cursor described — it was
    // truncated, rotated, or replaced. Restart rather than silently skipping
    // the new content that now sits under the old offset.
    let start = if consumed > lines.len() { 0 } else { consumed };

    let mut appended = 0usize;
    for line in &lines[start..] {
        let Ok(lesson) = serde_json::from_str::<ReflectionLesson>(line) else {
            continue;
        };
        if lesson.lesson.trim().is_empty() {
            continue;
        }
        // Only a genuinely NEW record counts. A replay reports `AlreadyPresent`
        // and must not inflate the number, or "how many observations did this
        // turn produce" becomes "how many lines did it re-read".
        if append_observation(store, &lesson) == Some(AppendOutcome::Appended) {
            appended += 1;
        }
    }

    // Advanced last and best-effort. If this write fails the next turn re-scans
    // and re-derives the same ids, which the ledger absorbs as replays.
    let _ = store.set_extraction_cursor(REFLECTION_SOURCE, &lines.len().to_string());
    appended
}

/// Redact, type, and append one lesson. `None` on any failure.
fn append_observation(store: &ContextStore, lesson: &ReflectionLesson) -> Option<AppendOutcome> {
    // Redaction happens BEFORE the record is constructed, so no unredacted
    // typed record ever exists — not even transiently in memory as something a
    // later refactor could persist by accident.
    let redaction = redact_secrets(&lesson.lesson);

    let record = ObservationRecord::new(
        ObservationSource::ReflectionLesson,
        format!("reflection:{}", lesson.occurred_at),
        task_id_for(lesson),
        redaction.text,
        lesson.domains.clone(),
        redaction.redacted,
        rfc3339(lesson.occurred_at),
    )
    .ok()?;

    let body = serde_json::to_string(&record).ok()?;
    let outcome = store
        .append_record(LedgerAppend {
            record_id: &record.record_id,
            lineage_id: &record.lineage_id,
            record_kind: stella_core::context_record::ContextRecordKind::Observation.as_str(),
            record_hash: &record.record_hash,
            schema_version: LIFECYCLE_SCHEMA_VERSION,
            body: &body,
            observed_at: &record.observed_at,
            supersedes: None,
        })
        .ok()?;
    Some(outcome)
}

/// Every observation in the ledger, oldest first — what the miner consumes.
///
/// `limit` bounds the read because the ledger grows monotonically and mining
/// runs on the turn path. Errors and unparseable bodies are skipped rather than
/// propagated.
pub(crate) fn all_observations(store: &ContextStore, limit: usize) -> Vec<ObservationRecord> {
    store
        .records_of_kind(
            stella_core::context_record::ContextRecordKind::Observation.as_str(),
            limit,
        )
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| serde_json::from_str::<ObservationRecord>(&row.body).ok())
        .collect()
}

#[cfg(test)]
mod observation_tests;
