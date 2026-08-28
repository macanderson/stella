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
//! A **crash** costs a re-scan. A *failure* used to cost the records outright:
//! the loop swallowed a failed append and advanced the cursor to the end of the
//! log regardless, so a line whose write lost to `SQLITE_BUSY` was left below
//! the cursor and never looked at again. The lexical path re-reads the whole log
//! every turn and is unaffected, so the two diverged exactly there — against the
//! behaviour-compatibility contract [`super::learning`] holds them to (#5323).
//!
//! So the cursor stops at the first line whose append failed, and everything
//! from there is re-scanned next turn. Content-derived ids make that replay a
//! no-op for the lines that did land.
//!
//! The **scan** does not stop there. A lock released a moment later would
//! otherwise leave every remaining lesson unwritten for no reason, and they are
//! going to be re-scanned regardless — the cursor is what bounds the work, not
//! the loop.
//!
//! A line the ledger refuses **deterministically** — one whose record cannot be
//! constructed or serialized at all — is not a barrier. Retrying it can never
//! succeed, and a cursor parked on it would re-scan the whole log every turn
//! forever without advancing. Those are skipped the way a malformed JSON line
//! already is.
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
/// **The timestamp is the fallback, not the rule.** Every shipped door stamps a
/// real boundary onto [`ReflectionLesson::task_id`] before anything persists the
/// lesson: `SessionMemory::reflect_and_record` writes the session's own
/// `session:<secs>-<pid>`, and a fleet attempt narrows that to `fleet:<task id>`
/// (`fleet_cmd::attempt_task_boundary` → `SessionMemory::set_task_id`), so three
/// attempts at one task merge rather than reading as three. The log line carries
/// whichever it was, so extraction reads it back.
///
/// What reaches `turn:{occurred_at}` is a lesson with an empty `task_id` — a log
/// line written before the field existed, which `#[serde(default)]` still
/// parses. For those the turn is the best available boundary, and it is wrong in
/// the *unsafe* direction: three turns spent on one task read as three distinct
/// tasks, under-counting how correlated the evidence is.
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
    // Where the cursor may advance to: the first line whose ledger write failed,
    // or the end of the log if none did.
    let mut settled = lines.len();
    for (offset, line) in lines[start..].iter().enumerate() {
        let Ok(lesson) = serde_json::from_str::<ReflectionLesson>(line) else {
            continue;
        };
        if lesson.lesson.trim().is_empty() {
            continue;
        }
        match append_observation(store, &lesson) {
            // Only a genuinely NEW record counts. A replay reports
            // `AlreadyPresent` and must not inflate the number, or "how many
            // observations did this turn produce" becomes "how many lines did
            // it re-read".
            Ok(AppendOutcome::Appended) => appended += 1,
            Ok(_) => {}
            // Deterministic: this line will not append on any future turn
            // either, so a barrier here would park the cursor forever.
            Err(AppendFailure::Unrepresentable) => {}
            // Transient — a concurrent writer holding the lock past the busy
            // timeout is the case. The cursor stops here so the next scan sees
            // this line again; the scan itself does NOT stop, because a lock
            // released a moment later would otherwise leave every remaining
            // lesson unwritten for no reason. Everything after this point is
            // re-scanned next turn either way, which content-derived ids make a
            // no-op replay for whatever did land.
            Err(AppendFailure::LedgerRefused) => {
                settled = settled.min(start + offset);
            }
        }
    }

    // Advanced last and best-effort. If this write fails the next turn re-scans
    // and re-derives the same ids, which the ledger absorbs as replays.
    let _ = store.set_extraction_cursor(REFLECTION_SOURCE, &settled.to_string());
    appended
}

/// Why one lesson did not become a record.
///
/// The distinction is the whole of #5323: one of these is worth retrying and
/// the other never will be, and the old `Option` could not tell a caller which
/// it was holding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppendFailure {
    /// The storage layer would not take the write — a lock held past the busy
    /// timeout, a disk error. The same bytes may well append on the next turn,
    /// so the cursor stops here.
    LedgerRefused,
    /// This line cannot become a record: it did not construct, did not
    /// serialize, or the ledger refused its *content*. Retrying is pointless —
    /// the input is fixed, so the outcome is — and a cursor waiting on it would
    /// never advance again.
    Unrepresentable,
}

/// Redact, type, and append one lesson.
///
/// The error says whether the next turn should try again. Everything before the
/// ledger call is a pure function of the line, so its failures are
/// [`AppendFailure::Unrepresentable`]; the ledger's own refusal is not.
fn append_observation(
    store: &ContextStore,
    lesson: &ReflectionLesson,
) -> Result<AppendOutcome, AppendFailure> {
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
    .map_err(|_| AppendFailure::Unrepresentable)?;

    let body = serde_json::to_string(&record).map_err(|_| AppendFailure::Unrepresentable)?;
    store
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
        .map_err(|error| match error {
            // The storage layer itself: `SQLITE_BUSY` from a writer holding the
            // lock past the busy timeout, a disk error, a full volume. The same
            // bytes may well append on the next turn.
            stella_context::ContextError::Sqlite(_) => AppendFailure::LedgerRefused,
            // Everything else is the ledger judging this record's *content* —
            // `InvalidInput` for two records claiming one id, chief among them.
            // The input is fixed, so the verdict is; retrying it forever is how
            // a cursor stops advancing at all.
            _ => AppendFailure::Unrepresentable,
        })
}

/// Every observation in the ledger, oldest first — what the miner consumes.
///
/// `limit` bounds the read because the ledger grows monotonically and mining
/// runs on the turn path. Errors and unparseable bodies are skipped rather than
/// propagated.
pub(crate) fn all_observations(store: &ContextStore, limit: usize) -> Vec<ObservationRecord> {
    store
        // The NEWEST `limit` observations, not the oldest: mining and health
        // want recent activity, and the oldest-`limit` read made every new
        // observation invisible once the log grew past the bound (#818). Under
        // the bound this returns the same set in the same order.
        .records_of_kind_newest(
            stella_core::context_record::ContextRecordKind::Observation.as_str(),
            limit,
        )
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| serde_json::from_str::<ObservationRecord>(&row.body).ok())
        .collect()
}

#[cfg(test)]
mod tests;
