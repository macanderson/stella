//! Tests for proposal induction.

use super::*;
use stella_context::ContextStore;
use stella_core::context_record::{ObservationRecord, ObservationSource};

fn store() -> (tempfile::TempDir, ContextStore) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ContextStore::open(dir.path().join("context.db")).expect("open");
    (dir, store)
}

/// An observation as extraction would have built it: `source_ref` carries the
/// turn timestamp, `task_id` the turn.
fn observation(text: &str, turn: u64) -> ObservationRecord {
    ObservationRecord::new(
        ObservationSource::ReflectionLesson,
        format!("reflection:{turn}"),
        format!("turn:{turn}"),
        text,
        vec!["testing".into()],
        false,
        stella_context::format_rfc3339(turn as i64),
    )
    .expect("observation")
}

const LESSON: &str = "Prefer updating witness-test assertions to match the live renderer output.";

// ---- GATE: three distinct tasks vs. thirty repetitions in one ----

#[test]
fn three_distinct_tasks_induce_one_eligible_proposal() {
    let (_dir, store) = store();
    let observations: Vec<_> = [100, 200, 300]
        .into_iter()
        .map(|t| observation(LESSON, t))
        .collect();

    let induced = induce_proposals(&store, &observations, &[], &SkillMineConfig::default());
    assert_eq!(induced.len(), 1, "one cluster, one proposal");
    let proposal = &induced[0].proposal;
    assert_eq!(proposal.score.distinct_tasks, 3);
    assert_eq!(proposal.score.occurrences, 3);
    assert!(
        proposal.is_eligible(3, 3),
        "three separate tasks must clear the bar: {:?}",
        proposal.score
    );
    assert_eq!(proposal.status, RecordProposalStatus::Eligible);
    assert_eq!(
        proposal.supporting_observations.len(),
        3,
        "the evidence is carried so a reviewer can see it"
    );
}

/// The gate's other half. The miner still clusters them — it is unchanged — but
/// the proposal records `distinct_tasks == 1` and is not eligible, so nothing
/// is promoted.
#[test]
fn thirty_repetitions_inside_one_task_induce_nothing_eligible() {
    let (_dir, store) = store();
    // Thirty DIFFERENT lessons, all from the same turn, all saying the same
    // thing in slightly different words — the shape a poisoning attempt takes,
    // and one the ledger's content-dedup cannot collapse.
    let observations: Vec<_> = (0..30)
        .map(|i| {
            observation(
                &format!("{LESSON} Variation number {i} of the very same point."),
                100,
            )
        })
        .collect();

    let induced = induce_proposals(&store, &observations, &[], &SkillMineConfig::default());
    assert!(!induced.is_empty(), "the miner still clusters them");
    for i in &induced {
        assert_eq!(
            i.proposal.score.distinct_tasks, 1,
            "every observation came from one task"
        );
        assert!(
            !i.proposal.is_eligible(3, 3),
            "thirty repetitions inside one task reached eligibility: {:?}",
            i.proposal.score
        );
        assert_eq!(i.proposal.status, RecordProposalStatus::Collecting);
    }
}

// ---- behavior compatibility with the lexical miner ----

/// The candidate id is the skill filename. Induction must not move it — and
/// this pins it against the same literal `stella-core`'s migration contract
/// uses, so a drift in either place is caught in both.
#[test]
fn induction_preserves_the_lexical_candidate_identity() {
    let (_dir, store) = store();
    let observations: Vec<_> = [100, 200, 300]
        .into_iter()
        .map(|t| observation(LESSON, t))
        .collect();
    let induced = induce_proposals(&store, &observations, &[], &SkillMineConfig::default());
    assert_eq!(
        induced[0].candidate.name,
        "prefer-updating-witness-test-assertions-e2010443"
    );
    assert_eq!(
        induced[0].proposal.candidate_id, induced[0].candidate.name,
        "the proposal names the same candidate the miner minted"
    );
}

/// The rendered evidence lines quote `reference` and `occurred_at` verbatim, so
/// both must survive the round trip through the ledger unchanged — otherwise a
/// re-render would not be byte-identical to a file already on disk.
#[test]
fn evidence_references_and_timestamps_survive_the_ledger_round_trip() {
    let (_dir, store) = store();
    let observations: Vec<_> = [100, 200, 300]
        .into_iter()
        .map(|t| observation(LESSON, t))
        .collect();
    let induced = induce_proposals(&store, &observations, &[], &SkillMineConfig::default());

    let evidence = &induced[0].candidate.evidence;
    let mut seen: Vec<(String, u64)> = evidence
        .iter()
        .map(|e| (e.reference.clone(), e.occurred_at))
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        vec![
            ("reflection:100".to_string(), 100),
            ("reflection:200".into(), 200),
            ("reflection:300".into(), 300),
        ]
    );
}

/// A candidate already captured by a skill on disk is dropped before it becomes
/// a proposal — the miner's own guard, still in force.
#[test]
fn an_already_captured_lesson_induces_no_proposal() {
    let (_dir, store) = store();
    let existing = vec![Skill {
        name: "witness-assertions".into(),
        description: "Prefer updating witness-test assertions".into(),
        body: LESSON.into(),
        domains: vec!["testing".into()],
        source_path: ".stella/skills/witness-assertions.md".into(),
        origin: stella_core::skills::SkillOrigin::AutoCreated,
    }];
    let observations: Vec<_> = [100, 200, 300]
        .into_iter()
        .map(|t| observation(LESSON, t))
        .collect();
    assert!(
        induce_proposals(
            &store,
            &observations,
            &existing,
            &SkillMineConfig::default()
        )
        .is_empty()
    );
}

// ---- the durable record ----

#[test]
fn proposals_are_persisted_to_the_ledger() {
    let (_dir, store) = store();
    let observations: Vec<_> = [100, 200, 300]
        .into_iter()
        .map(|t| observation(LESSON, t))
        .collect();
    induce_proposals(&store, &observations, &[], &SkillMineConfig::default());

    let stored = all_proposals(&store, 100);
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].score.distinct_tasks, 3);
    assert!(stored[0].confidence.get() > 0);
}

/// Re-inducing the same proposal from the same evidence is a replay, not a
/// duplicate — the same property extraction has, for the same reason.
#[test]
fn re_inducing_the_same_proposal_is_idempotent() {
    let (_dir, store) = store();
    let observations: Vec<_> = [100, 200, 300]
        .into_iter()
        .map(|t| observation(LESSON, t))
        .collect();
    induce_proposals(&store, &observations, &[], &SkillMineConfig::default());
    induce_proposals(&store, &observations, &[], &SkillMineConfig::default());
    assert_eq!(all_proposals(&store, 100).len(), 1);
}

/// More evidence for the same candidate is a new REVISION in the same lineage,
/// not an overwrite — so the history of how a proposal grew is readable.
#[test]
fn new_evidence_appends_a_revision_rather_than_replacing() {
    let (_dir, store) = store();
    let three: Vec<_> = [100, 200, 300]
        .into_iter()
        .map(|t| observation(LESSON, t))
        .collect();
    induce_proposals(&store, &three, &[], &SkillMineConfig::default());

    let mut four = three;
    four.push(observation(LESSON, 400));
    induce_proposals(&store, &four, &[], &SkillMineConfig::default());

    let stored = all_proposals(&store, 100);
    assert_eq!(stored.len(), 2, "both revisions survive");
    assert_eq!(
        stored[0].lineage_id, stored[1].lineage_id,
        "and they share one lineage, so a decline can be remembered against it"
    );
    let tasks: Vec<u32> = stored.iter().map(|p| p.score.distinct_tasks).collect();
    assert!(tasks.contains(&3) && tasks.contains(&4));
}
