//! What a turn taught, read back off durable state.
//!
//! The helper is exercised directly rather than through `work::run_turn`: that
//! spawns a real `stella run` child process, which is minutes and a provider
//! key, and the thing under test is the delta, not the spawn.

use std::path::Path;

use stella_context::{ContextDelta, ContextStore, MemoryInput};
use stella_core::context_record::{
    ProposalRecord, ProposalScore, RecordProposalKind, RecordProposalStatus, confidence_from_score,
};

use super::{LearningTally, tally};

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(f)
}

/// Open the context store where [`tally`] will look for it.
fn context_store(root: &Path) -> ContextStore {
    let path = stella_store::workspace_private_sqlite_path(root, "context.db")
        .expect("resolve context.db");
    ContextStore::open(&path).expect("open context.db")
}

fn log_lesson(root: &Path, lesson: &str) {
    stella_store::append_workspace_private_line(
        root,
        "reflections.jsonl",
        &format!(r#"{{"lesson":"{lesson}"}}"#),
    )
    .expect("append a lesson");
}

fn remember(store: &ContextStore, content: &str) {
    let delta = ContextDelta {
        memories: vec![MemoryInput::reflection(content, ["testing".to_owned()])],
        ..Default::default()
    };
    block_on(store.upsert(delta)).expect("upsert a memory");
}

fn propose(store: &ContextStore, candidate_id: &str) {
    let score = ProposalScore {
        occurrences: 3,
        distinct_tasks: 3,
        salient: false,
        rank: 30.0,
    };
    let proposal = ProposalRecord::new(
        RecordProposalKind::Knowledge,
        RecordProposalStatus::Eligible,
        candidate_id,
        "Prefer rg over grep",
        "Use ripgrep instead of grep in this repository.",
        vec!["tooling".into()],
        vec!["obs_a".into(), "obs_b".into(), "obs_c".into()],
        score,
        confidence_from_score(&score).expect("confidence"),
        "2026-07-26T12:00:00Z",
    )
    .expect("proposal");
    crate::memory::proposals::record_proposal(store, &proposal).expect("record a proposal");
}

/// **The witness.** A turn that reflects, remembers, and proposes moves all
/// three learning counters, and each moves by its own number.
///
/// Before this existed the three fields had a `println!` and no write site
/// anywhere in the workspace, so `stella self-driving stats` reported a session
/// that learned nothing however much it learned (#4118).
#[test]
fn a_turn_that_learns_moves_all_three_counters() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let store = context_store(root);

    let before = tally(root);

    log_lesson(root, "a restated lesson still reaches the log");
    log_lesson(root, "read the trace, never a surface signal");
    log_lesson(root, "read the trace, never a surface signal");
    remember(&store, "read the trace, never a surface signal");
    propose(&store, "read-the-trace-aaaa1111");

    let learned = tally(root).since(before);

    assert_eq!(
        learned.reflections, 3,
        "every lesson reaches the log, restatements included"
    );
    assert_eq!(learned.memories, 1, "only the novel lesson became a memory");
    assert_eq!(learned.proposals, 1);

    let mut stats = stella_autonomy::SessionStats::default();
    learned.add_to(&mut stats);
    assert_eq!(stats.reflections_logged, 3);
    assert_eq!(stats.memories_created, 1);
    assert_eq!(stats.proposals_made, 1);
}

/// Reflections and memories are different questions, and a turn whose lessons
/// were all restatements proves it: the log grows, no memory is created.
#[test]
fn a_restatement_only_turn_logs_a_reflection_and_claims_no_memory() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let store = context_store(root);
    remember(&store, "already known");

    let before = tally(root);
    log_lesson(root, "already known");
    let learned = tally(root).since(before);

    assert_eq!(learned.reflections, 1);
    assert_eq!(learned.memories, 0);
}

/// Counting must never create what it counts. A workspace that has learned
/// nothing reads as zero and gains no database for having been asked — this
/// runs twice per turn on somebody's live repository.
#[test]
fn counting_an_untouched_workspace_creates_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();

    assert_eq!(tally(root), LearningTally::default());
    assert!(
        !root
            .join(".stella")
            .join("private")
            .join("context.db")
            .exists(),
        "the tally must not create a context store"
    );
}

/// Retention runs. A prune between the two readings lowers a count, and "this
/// turn taught minus three lessons" is not a thing to report.
#[test]
fn a_shrinking_count_reports_nothing_learned_rather_than_a_negative() {
    let later = LearningTally {
        reflections: 1,
        memories: 0,
        proposals: 2,
    };
    let earlier = LearningTally {
        reflections: 9,
        memories: 4,
        proposals: 2,
    };
    assert_eq!(later.since(earlier), LearningTally::default());
}
