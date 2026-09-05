//! The memory lifecycle twin. A memory that stops helping is retired from
//! what the turns measured. A memory a person wrote is not.
//!
//! The trials here are real ledger rows. `appraisals::record_turn` writes
//! them, and `SessionMemory::auto_create_skills` reads them back. Both are
//! production doors. Nothing hands the sweep a verdict.
//!
//! Recall notes what a turn offered and what it showed. That half has its own
//! witness, in `memory::trials`.

use std::path::Path;

use stella_learn::ledger::ArtifactKind;

use crate::memory::{SessionMemory, appraisals, retirement, trials};

/// Turns per arm. The floor is five per arm. Eight is well clear of it, and
/// it is the count `stella-learn` uses in its own `Harms` case.
const TURNS_PER_ARM: usize = 8;

/// A lesson the reflection loop would have mined.
const MINED_LESSON: &str = "Regenerate gen/ rather than editing it by hand.";

/// A note a person keeps. The words differ from the lesson above, so the
/// store gives it a lineage of its own.
const HAND_WRITTEN_NOTE: &str = "The billing webhook retries three times.";

fn session(root: &Path) -> SessionMemory {
    SessionMemory::open_with_workspace_skills(root, false, true).expect("session memory")
}

fn log_path(root: &Path) -> std::path::PathBuf {
    root.join(".stella/private/reflections.jsonl")
}

/// Write one mined memory and one hand-written one. Returns their `nod_…`
/// ids, in that order.
async fn seed_memories(root: &Path) -> (String, String) {
    let db = stella_store::workspace_private_sqlite_path(root, "context.db").expect("db path");
    let context = stella_context::ContextStore::open(db).expect("context.db");
    context
        .upsert(stella_context::ContextDelta {
            memories: vec![
                stella_context::MemoryInput::reflection(MINED_LESSON, Vec::<String>::new()),
                stella_context::MemoryInput::new(
                    stella_context::MemoryKind::Note,
                    HAND_WRITTEN_NOTE,
                ),
            ],
            ..Default::default()
        })
        .await
        .expect("seed the memories");

    let nodes = context.memory_nodes().expect("memory nodes");
    let id_of = |text: &str| {
        nodes
            .iter()
            .find(|n| n.content.contains(text))
            .unwrap_or_else(|| panic!("no node holds {text:?}"))
            .public_id
            .clone()
    };
    let ids = (id_of(MINED_LESSON), id_of(HAND_WRITTEN_NOTE));
    drop(context);
    ids
}

/// A window that says these records hurt. Every turn that showed them failed.
/// Every turn that withheld them passed.
///
/// Both records ride every turn. Their rows are the same, so only the origin
/// can tell them apart.
fn record_a_harming_window(root: &Path, ids: &[String]) {
    for _ in 0..TURNS_PER_ARM {
        appraisals::record_turn(
            root,
            ArtifactKind::Memory,
            ids,
            ids,
            &trials::live_trial(false),
        );
        appraisals::record_turn(
            root,
            ArtifactKind::Memory,
            ids,
            &[],
            &trials::live_trial(true),
        );
    }
}

/// **The witness.** The mined memory is retired, and the reason names the
/// measurement. The hand-written one carries the same rows and is kept.
///
/// Nothing here writes a `ContextUse` record. A retirement fed by
/// `uses::selection_health` therefore reads an empty list and returns before
/// it retires anything. So this test passes only where the trial rows are the
/// evidence.
#[tokio::test]
async fn a_memory_that_stops_helping_is_retired_and_a_hand_written_one_is_kept() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let (mined, hand_written) = seed_memories(root).await;
    record_a_harming_window(root, &[mined.clone(), hand_written.clone()]);

    let mut memory = session(root);
    memory.auto_create_skills(&log_path(root), true);

    let retired = retirement::retired_ids(&memory.store);
    assert!(
        retired.contains(&mined),
        "its own turns say withholding it won: {retired:?}"
    );
    assert!(
        !retired.contains(&hand_written),
        "a memory a person wrote is kept, whatever the numbers say"
    );

    // The reason names the measurement and the way back.
    let standing = retirement::standings(&memory.store);
    let reason = &standing
        .get(&mined)
        .expect("a standing for the retirement")
        .reason;
    assert!(
        reason.contains("lift") && reason.contains("reaffirm"),
        "the reason must say what measured it: {reason}"
    );

    // Retirement is not deletion. The record is still there to reaffirm.
    assert!(retirement::reaffirm(
        &memory.store,
        &mined,
        "still needed",
        "2026-09-05T00:00:00Z"
    ));
    assert!(!retirement::retired_ids(&memory.store).contains(&mined));
}

/// A window with no separation retires nothing. Without it, the case above
/// would pass on a sweep that retired every memory handed to it.
#[tokio::test]
async fn a_memory_the_turns_cannot_separate_is_left_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let (mined, _hand_written) = seed_memories(root).await;

    // Every turn passes, shown or withheld. Neither side wins. `Inert` needs
    // a full window in both arms, and this is far short of one.
    let ids = [mined.clone()];
    for _ in 0..TURNS_PER_ARM {
        appraisals::record_turn(
            root,
            ArtifactKind::Memory,
            &ids,
            &ids,
            &trials::live_trial(true),
        );
        appraisals::record_turn(
            root,
            ArtifactKind::Memory,
            &ids,
            &[],
            &trials::live_trial(true),
        );
    }

    let mut memory = session(root);
    memory.auto_create_skills(&log_path(root), true);

    assert!(
        retirement::retired_ids(&memory.store).is_empty(),
        "no measurement, no retirement"
    );
}
