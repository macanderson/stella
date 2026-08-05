//! The A/B recall control, end to end (#1221): the durable schedule, what a
//! control turn actually suppresses, and the attribution that makes the two
//! arms separable afterwards.
//!
//! Its own file rather than more of `memory/tests.rs`, which is one line under
//! the 1500-line ratchet `scripts/check-file-size.sh` enforces.

use crate::memory::*;

/// Open a session memory over `root` the way a driver does.
fn session(root: &Path) -> SessionMemory {
    SessionMemory::open(root, false).expect("session memory opens in a temp workspace")
}

/// The defect this issue names: `stella run`, a fleet task, and `/goal` are one
/// turn per process, so a per-session counter is 1 on every one of them and the
/// schedule never fires. Each iteration below is a fresh `SessionMemory` over
/// one workspace — the closest thing to consecutive processes a unit test can
/// be — and the arm must still land on exactly turns 3, 6, 9.
#[test]
fn the_schedule_survives_the_session_that_counted_it() {
    let dir = tempfile::tempdir().unwrap();
    let control_turns: Vec<u64> = (1..=9)
        .filter(|_| session(dir.path()).arm_recall_control_at(3))
        .collect();
    assert_eq!(
        control_turns.len(),
        3,
        "three control turns in nine, each from its own session"
    );

    // And the durable counter is the thing that made it work, not a coincidence
    // of ordering: it counted all nine.
    let store = session(dir.path());
    assert_eq!(
        store
            .store
            .ab_control_turns_so_far(crate::memory::recall::AB_RECALL_EXPERIMENT)
            .expect("counter readable"),
        9
    );
}

/// Within one session the arm is still exactly every `rate`-th turn — the
/// durable counter changed where the number comes from, not the schedule.
#[test]
fn one_session_arms_on_exactly_every_rate_th_turn() {
    let dir = tempfile::tempdir().unwrap();
    let mut memory = session(dir.path());
    let arms: Vec<bool> = (1..=6).map(|_| memory.arm_recall_control_at(3)).collect();
    assert_eq!(arms, vec![false, false, true, false, false, true]);
    assert!(
        memory.recall_was_suppressed(),
        "the flag rides the turn it was armed for"
    );
    // Arming again clears it: the flag is turn-scoped, and a driver that arms
    // must not inherit the previous turn's arm.
    memory.arm_recall_control_at(3);
    assert!(!memory.recall_was_suppressed());
}

/// A disabled control must not consume the schedule either — a workspace that
/// turns the experiment off and later turns it back on should resume counting
/// from where it stopped, not from wherever the disabled turns left it.
#[test]
fn a_disabled_control_neither_suppresses_nor_counts() {
    let dir = tempfile::tempdir().unwrap();
    let mut memory = session(dir.path());
    for rate in [0, 1] {
        for _ in 0..5 {
            assert!(!memory.arm_recall_control_at(rate));
        }
    }
    assert_eq!(
        memory
            .store
            .ab_control_turns_so_far(crate::memory::recall::AB_RECALL_EXPERIMENT)
            .expect("counter readable"),
        0,
        "a disabled control claims no turn numbers"
    );
}

/// The rate comes from `context.retrieval.ab_recall_rate`, read at open —
/// [`SessionMemory::arm_recall_control`] is the one production door and it
/// takes no rate, so this is what proves the setting reaches the schedule.
#[test]
fn the_configured_rate_is_the_one_that_arms() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join(".stella")).unwrap();
    std::fs::write(
        root.join(".stella/settings.json"),
        r#"{"context":{"retrieval":{"ab_recall_rate":2}}}"#,
    )
    .unwrap();

    let mut memory = session(root);
    assert!(!memory.arm_recall_control(), "turn 1 of a rate-2 schedule");
    assert!(memory.arm_recall_control(), "turn 2 is the control turn");
}

/// What a control turn suppresses is the whole point: not the rendered block
/// alone, but the frames the pipeline's own port would otherwise hand to the
/// goal message, the planner, and the witness author.
#[tokio::test]
async fn a_control_turn_is_frameless_through_the_pipeline_port_too() {
    let dir = tempfile::tempdir().unwrap();
    let lesson = "the deploy script must run migrations before restarting the api";
    let mut memory = session(dir.path());
    memory
        .store
        .upsert(ContextDelta {
            memories: vec![MemoryInput::reflection(lesson, Vec::<String>::new())],
            ..ContextDelta::default()
        })
        .await
        .expect("store a recallable lesson");

    // The control: an ordinary turn recalls it through both surfaces.
    assert!(
        !ContextRecallPort::recall(&memory, lesson).await.is_empty(),
        "the port must recall the planted lesson on a normal turn, or the \
         suppression assertion below passes for the wrong reason"
    );
    assert!(memory.recall_block(lesson).await.is_some());

    // Rate 1 never suppresses, so reaching a control turn takes a rate of 2 and
    // two turns: turn 1 is an ordinary arm, turn 2 is the control.
    assert!(!memory.arm_recall_control_at(2), "turn 1 recalls normally");
    assert!(
        memory.arm_recall_control_at(2),
        "turn 2 is the control turn"
    );
    assert!(
        ContextRecallPort::recall(&memory, lesson).await.is_empty(),
        "a control turn must reach the pipeline's recall port, not just the \
         rendered block"
    );
    assert!(
        memory.recall_block(lesson).await.is_none(),
        "and the worker's block is frameless on the same turn"
    );
    assert!(
        memory.pipeline_recall_block(lesson).await.is_none(),
        "including the frames-free block a pipeline-driven turn injects"
    );
}

/// Attribution is the entire readout of the experiment, and it used to be
/// composed into the *prompt* — where a 240-character prompt truncated it away,
/// silently filing a control turn as an ordinary one.
#[tokio::test]
async fn a_long_prompt_still_carries_the_control_tag() {
    let dir = tempfile::tempdir().unwrap();
    let mut memory = session(dir.path());
    let prompt = "refactor the ingest pipeline ".repeat(20);
    assert!(prompt.chars().count() > 240, "the truncating case");

    memory.arm_recall_control_at(2);
    assert!(
        memory.arm_recall_control_at(2),
        "turn 2 of a rate-2 schedule is the control turn"
    );
    memory
        .record_episode(&prompt, EpisodeOutcome::Success, &[], unix_now_secs(), None)
        .await;

    let summary = episode_summaries(&memory);
    assert_eq!(summary.len(), 1, "one episode was recorded");
    assert!(
        summary[0].ends_with(AB_CONTROL_TAG),
        "the control tag lands after truncation, like any other join key: {}",
        summary[0]
    );
}

/// …and an ordinary turn is not tagged, or every episode reads as a control.
#[tokio::test]
async fn an_ordinary_turn_carries_no_control_tag() {
    let dir = tempfile::tempdir().unwrap();
    let mut memory = session(dir.path());
    assert!(
        !memory.arm_recall_control_at(10),
        "turn 1 of a rate-10 schedule"
    );
    memory
        .record_episode(
            "add a witness test for the ingest path",
            EpisodeOutcome::Success,
            &[],
            unix_now_secs(),
            None,
        )
        .await;

    let summary = episode_summaries(&memory);
    assert_eq!(summary.len(), 1);
    assert!(!summary[0].contains(AB_CONTROL_TAG), "{}", summary[0]);
}

/// Every episode this workspace recorded, newest first.
fn episode_summaries(memory: &SessionMemory) -> Vec<String> {
    memory
        .store
        .recallable_nodes()
        .expect("nodes readable")
        .into_iter()
        .filter(|n| n.kind == stella_context::NodeKind::Episode)
        .map(|n| n.content)
        .collect()
}

/// The failure this issue is about is not a wrong number — it is a driver that
/// never arms at all, which no unit test of the schedule can see. So the guard
/// is over the drivers themselves: every source file that recalls must arm
/// first, and the arm must come before the recall it governs.
///
/// A source-text check because the alternative is instantiating four full
/// session drivers (each of which builds providers, tool registries, and an
/// event bus) to observe one flag. It is exact about what it can be exact
/// about: the names are `pub`/`pub(crate)` items, so a rename that breaks this
/// test breaks the compile too, and only the ordering is textual.
#[test]
fn every_recalling_driver_arms_the_control_first() {
    const DRIVERS: [(&str, &str); 3] = [
        ("agent.rs", include_str!("../../agent.rs")),
        ("agent/goal.rs", include_str!("../../agent/goal.rs")),
        ("command_deck.rs", include_str!("../../command_deck.rs")),
    ];
    // The two recall doors a driver can take: the full block, and the
    // frames-free block a pipeline-driven turn injects beside its port.
    const RECALL_CALLS: [&str; 2] = ["recall_block_reported(", "pipeline_recall_block("];

    for (name, source) in DRIVERS {
        let first_recall = RECALL_CALLS
            .iter()
            .filter_map(|call| source.find(call))
            .min()
            .unwrap_or_else(|| panic!("{name} is listed as a driver but recalls nothing"));
        let first_arm = source.find("arm_recall_control(").unwrap_or_else(|| {
            panic!(
                "{name} recalls without arming the A/B control — its turns can \
                 never be control turns, which is the defect #1221 closed"
            )
        });
        assert!(
            first_arm < first_recall,
            "{name} recalls before it arms: the flag is only reset by arming, so \
             that turn runs on the previous turn's arm"
        );
    }
}
