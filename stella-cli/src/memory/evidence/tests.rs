//! Phase 4 deliverable 2: citations and tool outcomes are evidence sources
//! alongside reflection lessons — without changing what citation already means.

use stella_context::{AppendOutcome, ContextStore};
use stella_core::context_record::{ContextRecordKind, ObservationRecord, ObservationSource};
use stella_store::MemoryCitationRow;

use super::*;

const AT: &str = "2026-07-26T00:00:00Z";

fn store() -> (tempfile::TempDir, ContextStore) {
    let dir = tempfile::tempdir().expect("workspace");
    let store = ContextStore::open(dir.path().join("context.db")).expect("context.db");
    (dir, store)
}

fn citation(memory_id: &str, score: i64, truthful: bool, remark: &str) -> MemoryCitationRow {
    MemoryCitationRow {
        memory_id: memory_id.into(),
        useful_score: score,
        truthful,
        remark: remark.into(),
    }
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
fn a_negative_citation_becomes_a_citation_sourced_observation() {
    let (_dir, store) = store();
    let outcome = citation_observation(
        &store,
        &citation("nod_a", 1, false, "the path it names was deleted"),
        "task1",
        AT,
    );

    assert_eq!(outcome, Some(AppendOutcome::Appended));
    let observations = observations(&store);
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].source, ObservationSource::MemoryCitation);
    assert!(
        observations[0]
            .text
            .contains("the path it names was deleted")
    );
    assert_eq!(observations[0].task_id, "task1");
}

#[test]
fn a_positive_citation_is_not_mined_because_it_would_cite_itself() {
    let (_dir, store) = store();
    // Spec §7: a proposal may not cite itself. "This memory was right" is the
    // memory restating its own claim.
    let outcome = citation_observation(&store, &citation("nod_a", 5, true, "spot on"), "task1", AT);

    assert_eq!(outcome, None);
    assert!(observations(&store).is_empty());
}

#[test]
fn a_citation_with_no_remark_records_nothing() {
    let (_dir, store) = store();
    let outcome = citation_observation(&store, &citation("nod_a", 1, false, "   "), "task1", AT);

    assert_eq!(outcome, None);
    assert!(observations(&store).is_empty());
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
    let row = citation("nod_a", 1, false, "wrong");
    assert_eq!(
        citation_observation(&store, &row, "task1", AT),
        Some(AppendOutcome::Appended)
    );
    assert_eq!(
        citation_observation(&store, &row, "task1", AT),
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
    // The deliverable keeps citation semantics. This module must not have
    // introduced a second quarantine path or moved the shipped constants.
    assert_eq!(stella_store::QUARANTINE_NEGATIVES_THRESHOLD, 2);
    assert_eq!(stella_store::PROMOTION_CITATIONS_REQUIRED, 10);
    assert_eq!(stella_store::POSITIVE_SCORE_MIN, 3);
}
