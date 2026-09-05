//! The witness. A turn writes a trial row for the memory it recalled, and
//! one for the rule it rendered.
//!
//! Every case drives the doors a real turn takes: `arm_controls_at`,
//! `recall_block_reported`, `record_episode`. Each then reads the file the
//! sweep reads. Nothing here builds a trial by hand.
//!
//! Take the seam away and the ledger holds `skill` rows alone. Every
//! assertion here about a `memory` row or a `rule` row then fails.

use stella_context::{ContextDelta, EpisodeOutcome, MemoryInput};
use stella_protocol::ContextRecallPort;

use crate::memory::{SessionMemory, appraisals};

/// The lesson stored as a memory, and the prompt that recalls it.
const LESSON: &str = "always use the obsolete frobnicator for database migrations";

/// A workspace with a session memory over an empty context store.
fn session(dir: &std::path::Path) -> SessionMemory {
    std::fs::create_dir_all(dir.join(".stella")).expect("stella dir");
    SessionMemory::open(dir, false).expect("session memory")
}

/// Hand the session one volatile record, and give back the `^handle` the
/// ledger files it under.
///
/// The handle is read off the loaded registry rather than spelled here. It
/// is derived from the lineage and widens on a collision, so a literal in
/// this file would be a second copy of a rule that lives in
/// `stella_records::records::handle`.
///
/// The record is unscoped, so it applies to every turn. The rule population
/// is then one handle.
fn with_one_record(memory: &mut SessionMemory) -> String {
    let file = stella_learn::rules::RuleFile {
        path: ".stella/rules/ctx.acme.staging.toml".to_string(),
        contents: r#"
schema = "context-record/v0.1"
set_id = "acme"

[[record]]
lineage_id = "ctx.acme.staging-url"
kind = "preference"
statement = "The staging URL is https://stage.example."
status = "active"
origin = "user"

[record.steering]
force = "may"
"#
        .to_string(),
        contributed_by: None,
    };
    let registry = stella_records::records::registry::load(
        &[],
        &[file],
        &stella_records::records::Facts::default(),
    );
    let handle = registry
        .entries
        .first()
        .map(|entry| entry.record.handle.clone())
        .expect("the fixture loads one record");
    memory.set_record_registry(registry);
    handle
}

/// Store the lesson as a recallable memory.
async fn remember_lesson(memory: &SessionMemory) {
    memory
        .store
        .upsert(ContextDelta {
            memories: vec![MemoryInput::reflection(LESSON, Vec::<String>::new())],
            ..ContextDelta::default()
        })
        .await
        .expect("the lesson is stored");
}

/// The `selected` flag of every ledger row of `kind` for `id`, oldest first.
///
/// Read as raw JSON, so the test reads the file the sweep reads. The kind is
/// matched too: a row filed under the wrong kind would still fold under the
/// right id.
fn rows(root: &std::path::Path, kind: &str, id: &str) -> Vec<bool> {
    std::fs::read_to_string(root.join(".stella/private").join(appraisals::TRIALS_FILE))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|row| row["kind"].as_str() == Some(kind) && row["id"].as_str() == Some(id))
        .map(|row| row["selected"].as_bool().unwrap_or(false))
        .collect()
}

/// **The memory producer.** A turn that recalls a memory writes an
/// `ArtifactKind::Memory` row for it, marked selected. The row names the
/// frame the block put in front of the model.
#[tokio::test]
async fn a_recalled_memory_writes_a_treatment_arm_trial() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut memory = session(dir.path());
    remember_lesson(&memory).await;

    let handle = ContextRecallPort::recall(&memory, LESSON)
        .await
        .frames
        .iter()
        .find(|frame| frame.content.contains("frobnicator"))
        .map(crate::memory::steering::frame_handle)
        .expect("the lesson is recallable");

    assert!(!memory.arm_controls_at(0, 0), "no control fires here");
    let block = memory.recall_block_reported(LESSON, &[]).await;
    assert!(
        block.produced.has_frame(&handle),
        "the block puts the memory in front of the model"
    );
    memory
        .record_episode(LESSON, EpisodeOutcome::Success, &[], 1_000, None)
        .await;

    assert_eq!(
        rows(dir.path(), "memory", &handle),
        vec![true],
        "the recalled memory is one row, in the with-memory arm"
    );
}

/// **The rule producer.** A turn that renders a record writes an
/// `ArtifactKind::Rule` row for it, marked selected.
#[tokio::test]
async fn a_rendered_record_writes_a_treatment_arm_trial() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut memory = session(dir.path());
    let handle = with_one_record(&mut memory);

    assert!(!memory.arm_controls_at(0, 0), "no control fires here");
    let block = memory
        .recall_block_reported("review the migrations", &[])
        .await;
    assert!(
        block
            .text
            .as_deref()
            .is_some_and(|text| text.contains("staging URL")),
        "the record reaches the block: {:?}",
        block.text
    );
    memory
        .record_episode(
            "review the migrations",
            EpisodeOutcome::Success,
            &[],
            1_000,
            None,
        )
        .await;

    assert_eq!(
        rows(dir.path(), "rule", &handle),
        vec![true],
        "the rendered record is one row, in the with-rule arm"
    );
}

/// **The rule holdout, end to end.** Six turns at a rate of 2 give three
/// holdouts. The third is the rule arm. That turn holds the one record back
/// and writes its control-arm row. The five turns before it rendered it.
///
/// Nothing else here could write that row. The plane rate is `0` and
/// steering is on. The record is unscoped, so it applies to every turn, and
/// the budget has nothing to cut it against.
#[tokio::test]
async fn the_rule_arm_withholds_a_record_and_writes_its_control_arm() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut memory = session(dir.path());
    let handle = with_one_record(&mut memory);

    let mut rendered = Vec::new();
    for turn in 1..=6 {
        assert!(
            !memory.arm_controls_at(0, 2),
            "the plane control is off for turn {turn}"
        );
        let block = memory
            .recall_block_reported("review the migrations", &[])
            .await;
        rendered.push(
            block
                .text
                .as_deref()
                .is_some_and(|text| text.contains("staging URL")),
        );
        memory
            .record_episode(
                "review the migrations",
                EpisodeOutcome::Success,
                &[],
                1_000,
                None,
            )
            .await;
    }

    assert_eq!(
        rendered,
        vec![true, true, true, true, true, false],
        "the third holdout is the rule arm, and it withholds the record"
    );
    assert_eq!(
        rows(dir.path(), "rule", &handle),
        vec![true, true, true, true, true, false],
        "the withheld turn is recorded in the without-rule arm"
    );
}

/// **The memory holdout.** The second holdout is the memory arm. It holds
/// back the one memory the query offers.
///
/// Only the last turn writes an episode. An episode is a recallable memory
/// of its own. Write one each turn and the population grows, and the pick
/// could land on an episode rather than the lesson.
#[tokio::test]
async fn the_memory_arm_withholds_a_memory_and_writes_its_control_arm() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut memory = session(dir.path());
    remember_lesson(&memory).await;

    let handle = ContextRecallPort::recall(&memory, LESSON)
        .await
        .frames
        .iter()
        .find(|frame| frame.content.contains("frobnicator"))
        .map(crate::memory::steering::frame_handle)
        .expect("the lesson is recallable");

    let mut produced = Vec::new();
    for turn in 1..=4 {
        assert!(
            !memory.arm_controls_at(0, 2),
            "the plane control is off for turn {turn}"
        );
        let block = memory.recall_block_reported(LESSON, &[]).await;
        produced.push(block.produced.has_frame(&handle));
    }
    memory
        .record_episode(LESSON, EpisodeOutcome::Success, &[], 1_000, None)
        .await;

    assert_eq!(
        produced,
        vec![true, true, true, false],
        "the second holdout is the memory arm, and it withholds the memory"
    );
    assert_eq!(
        rows(dir.path(), "memory", &handle),
        vec![false],
        "the withheld turn is recorded in the without-memory arm"
    );
}

/// A turn that offered nothing of a kind writes nothing for that kind. An
/// empty row is evidence about no memory and no rule.
#[tokio::test]
async fn a_turn_with_nothing_to_offer_writes_no_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut memory = session(dir.path());

    assert!(!memory.arm_controls_at(0, 0), "no control fires here");
    let _ = memory.recall_block_reported("hello", &[]).await;
    memory
        .record_episode("hello", EpisodeOutcome::Success, &[], 1_000, None)
        .await;

    let ledger = dir
        .path()
        .join(".stella/private")
        .join(appraisals::TRIALS_FILE);
    assert!(
        !ledger.exists(),
        "an empty turn leaves the ledger alone: {ledger:?}"
    );
}
