//! Phase 4 deliverable 2: a failed tool call is an evidence source, alongside
//! reflection lessons. `evidence.rs`'s module doc covers the other source,
//! `MemoryCitation`. It is retired, and this module does not test it.

use stella_context::{AppendOutcome, ContextStore};
use stella_records::context_record::{ContextRecordKind, ObservationRecord, ObservationSource};

use super::*;

const AT: &str = "2026-07-26T00:00:00Z";

fn store() -> (tempfile::TempDir, ContextStore) {
    let dir = tempfile::tempdir().expect("workspace");
    let store = ContextStore::open(dir.path().join("context.db")).expect("context.db");
    (dir, store)
}

fn observations(store: &ContextStore) -> Vec<ObservationRecord> {
    store
        .records_of_kind(ContextRecordKind::Observation.as_str(), 100)
        .expect("read")
        .into_iter()
        .filter_map(|row| serde_json::from_str(&row.body).ok())
        .collect()
}

#[test]
fn a_failed_tool_call_becomes_a_tool_outcome_observation() {
    let (_dir, store) = store();
    let outcome = tool_outcome_observation(&store, "run_tests", "linker not found", "task1", AT);

    assert_eq!(outcome, Some(AppendOutcome::Appended));
    let observations = observations(&store);
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].source, ObservationSource::ToolOutcome);
    assert!(observations[0].text.contains("linker not found"));
}

#[test]
fn repeated_failures_of_one_tool_cluster_under_one_candidate() {
    let (_dir, store) = store();
    // Different tasks, same tool: the candidate id must be the tool so the
    // miner sees recurrence rather than two unrelated singletons.
    tool_outcome_observation(&store, "run_tests", "linker not found", "task1", AT);
    tool_outcome_observation(
        &store,
        "run_tests",
        "linker not found",
        "task2",
        "2026-07-27T00:00:00Z",
    );

    let observations = observations(&store);
    assert_eq!(observations.len(), 2);
    assert_eq!(
        observations[0].source_ref, observations[1].source_ref,
        "same tool must share a source ref so failures cluster"
    );
    assert_ne!(
        observations[0].task_id, observations[1].task_id,
        "two distinct tasks is what makes the recurrence count"
    );
}

#[test]
fn re_appending_the_same_evidence_is_a_replay() {
    let (_dir, store) = store();
    assert_eq!(
        tool_outcome_observation(&store, "run_tests", "linker not found", "task1", AT),
        Some(AppendOutcome::Appended)
    );
    assert_eq!(
        tool_outcome_observation(&store, "run_tests", "linker not found", "task1", AT),
        Some(AppendOutcome::AlreadyPresent),
        "content-derived ids must absorb a re-scan as a replay"
    );
    assert_eq!(observations(&store).len(), 1);
}

#[test]
fn a_secret_in_a_tool_error_is_redacted_before_it_is_stored() {
    let (_dir, store) = store();
    // A failing command that printed its own arguments is the single most
    // likely place a credential reaches this path.
    tool_outcome_observation(
        &store,
        "shell",
        "curl failed: Authorization: Bearer sk-ant-api03-SECRETVALUE1234567890",
        "task1",
        AT,
    );

    let observations = observations(&store);
    assert_eq!(observations.len(), 1);
    assert!(
        !observations[0].text.contains("SECRETVALUE1234567890"),
        "the stored observation still carries the secret: {}",
        observations[0].text
    );
    assert!(observations[0].redacted, "redaction must be declared");
}

#[test]
fn an_unbounded_tool_error_is_truncated_visibly() {
    let (_dir, store) = store();
    let wall_of_text = "e".repeat(5_000);
    tool_outcome_observation(&store, "build", &wall_of_text, "task1", AT);

    let observations = observations(&store);
    let text = &observations[0].text;
    assert!(
        text.chars().count() <= MAX_OBSERVATION_CHARS,
        "a stack trace must not become one enormous observation"
    );
    // Spec §5.5: no silent truncation. The cut is visible in the stored text.
    assert!(text.ends_with('…'), "truncation must be visible: {text}");
}

#[test]
fn bounded_leaves_a_short_string_untouched() {
    assert_eq!(bounded("short"), "short");
}

#[test]
fn bounded_never_splits_a_multibyte_character() {
    // Truncating by bytes would panic here. The boundary is characters.
    let text = "é".repeat(MAX_OBSERVATION_CHARS + 50);
    let out = bounded(&text);
    assert!(out.chars().count() <= MAX_OBSERVATION_CHARS);
}

#[test]
fn the_citation_loops_own_thresholds_are_untouched() {
    // Removing the citation observation source must not move these
    // shipped constants.
    assert_eq!(stella_store::QUARANTINE_NEGATIVES_THRESHOLD, 2);
    assert_eq!(stella_store::PROMOTION_CITATIONS_REQUIRED, 10);
    assert_eq!(stella_store::POSITIVE_SCORE_MIN, 3);
}
