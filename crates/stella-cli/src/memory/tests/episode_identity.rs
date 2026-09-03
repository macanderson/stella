//! Two turns in one second are two episodes.
//!
//! An episode is keyed by its text and its time window. Both are cut to whole
//! seconds. So two turns on the same prompt in one second get the same key.
//! The second write then lands on the row the first turn wrote. What is left
//! holds one turn's result under the other turn's name, and no one is told.
//!
//! The turn's own row id in the store keeps them apart. The chat path sets it
//! in `stamp_and_record_skill_usage`. A path that sets none can still clash,
//! and that gap has its own issue.

use std::sync::Arc;

use stella_context::{Clock, EpisodeOutcome, FixedClock};

use crate::memory::SessionMemory;

const PROMPT: &str = "run the failing test again";

/// A session on a fixed clock, so both turns land in one second. A real clock
/// would hit that case only by luck.
fn session(root: &std::path::Path) -> SessionMemory {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(1_700_000_000));
    SessionMemory::open_with_clock(root, false, false, clock).expect("session memory")
}

/// The text of each episode in this store.
fn episodes(memory: &SessionMemory) -> Vec<String> {
    memory
        .store
        .recallable_nodes()
        .expect("nodes readable")
        .into_iter()
        .filter(|n| n.kind == stella_context::NodeKind::Episode)
        .map(|n| n.content)
        .collect()
}

/// Same prompt, same second, two turns. Both must be kept.
#[tokio::test]
async fn two_executions_in_one_second_keep_their_own_episodes() {
    let dir = tempfile::tempdir().unwrap();
    let mut memory = session(dir.path());

    memory.set_execution_id(41);
    memory
        .record_episode(PROMPT, EpisodeOutcome::Success, &[], 1_700_000_000, None)
        .await;
    memory.set_execution_id(42);
    memory
        .record_episode(PROMPT, EpisodeOutcome::Failure, &[], 1_700_000_000, None)
        .await;

    assert_eq!(
        episodes(&memory).len(),
        2,
        "the second turn wrote over the first turn's row"
    );
}

/// One turn, written twice, is still one episode. A key that does not hold
/// still would double each row a retry writes again.
#[tokio::test]
async fn one_execution_recorded_twice_keeps_one_episode() {
    let dir = tempfile::tempdir().unwrap();
    let mut memory = session(dir.path());

    memory.set_execution_id(41);
    for _ in 0..2 {
        memory
            .record_episode(PROMPT, EpisodeOutcome::Success, &[], 1_700_000_000, None)
            .await;
    }

    assert_eq!(episodes(&memory).len(), 1);
}
