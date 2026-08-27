//! #5031: what the volatile block reports about the skills it carried.
//!
//! The block's bytes are already pinned by `golden_block`; this asks the other
//! half of the same question — whether the turn *says* which skills entered
//! it. Until this landed the answer was nowhere: a skill was rendered into the
//! prompt and the only trace was a `skill_usage` row a live session cannot
//! read.

use crate::memory::*;

/// A workspace holding two skills, one of which the prompt below selects.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for (slug, description, body) in [
        ("reviewer", "database review", "ALWAYS_REVIEW_DATABASES"),
        ("painter", "svg illustration", "ALWAYS_USE_VECTORS"),
    ] {
        let skill_dir = dir.path().join(".stella/skills").join(slug);
        std::fs::create_dir_all(&skill_dir).expect("skill dir");
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {slug}\ndescription: {description}\n---\n{body}"),
        )
        .expect("skill file");
    }
    dir
}

/// **Witness (#5031).** A turn that injects a skill reports it — the name it
/// was authored under, the description the transcript hangs under the head,
/// and what the injected block cost.
///
/// Fails on base, where `RecalledBlock` carried no record of its skills at
/// all: `telemetry_events` returned the recall receipt alone, so every door
/// opened its stream having said nothing about the steering it had just paid
/// for.
#[tokio::test]
async fn an_injected_skill_reaches_the_turn_stream_as_its_own_event() {
    let dir = workspace();
    let memory =
        SessionMemory::open_with_workspace_skills(dir.path(), false, true).expect("session memory");

    let block = memory
        .recall_block_reported("review the database migrations", &[])
        .await;
    assert!(
        block
            .text
            .as_deref()
            .is_some_and(|text| text.contains("ALWAYS_REVIEW_DATABASES")),
        "the fixture prompt must actually inject the reviewer skill: {:?}",
        block.text
    );

    let injected: Vec<_> = block
        .telemetry_events()
        .into_iter()
        .filter_map(|event| match event {
            stella_protocol::AgentEvent::SkillInjected {
                name,
                summary,
                tokens,
                trigger,
            } => {
                assert_eq!(
                    trigger,
                    stella_protocol::SkillTrigger::Auto,
                    "a skill the steering block selected is not a `/slug` invocation"
                );
                Some((name, summary, tokens))
            }
            _ => None,
        })
        .collect();

    let (name, summary, tokens) = injected
        .iter()
        .find(|(name, ..)| name == "reviewer")
        .expect("the injected skill announces itself");
    assert_eq!(summary, "database review");
    assert!(*tokens > 0, "an injected block costs something");
    assert!(
        !injected.iter().any(|(name, ..)| name == "painter"),
        "a skill the prompt did not select must not be announced: {injected:?}"
    );
    let _ = name;
}

/// A session steering is switched off for injects nothing, so it announces
/// nothing.
///
/// The switch's whole point is that the model sees no steering; a stream that
/// announced skills anyway would report an injection that did not happen, and
/// a reader auditing an unsteered run would find the opposite of the truth.
#[tokio::test]
async fn a_turn_with_steering_off_announces_no_skill() {
    let dir = workspace();
    let mut memory =
        SessionMemory::open_with_workspace_skills(dir.path(), false, true).expect("session memory");
    memory.set_steering_enabled(false);

    let block = memory
        .recall_block_reported("review the database migrations", &[])
        .await;
    assert!(block.telemetry_events().is_empty(), "{:?}", block.text);
}

/// **The witness (#5232).** An invoked skill reaches the turn's event channel,
/// carrying the trigger that says a person asked for it.
///
/// Two failures this pins, both of which the tests above miss because they only
/// ever exercise auto-selection. That the invoked skill is reported at all —
/// `extensions::expand` produced prompt text and emitted nothing, so a `/slug`
/// left no `SkillInjected` on any channel and `skill_usage` counted no use,
/// which is what appraisal reads before retiring a skill. And that it is
/// reported as an *invocation*: an event carrying `Auto` would say the
/// steering block chose a skill nobody selected.
#[test]
fn an_invoked_skill_reaches_the_channel_as_a_command() {
    let mut recall = OpeningRecall::default();
    recall.note_invoked_skill(Some(crate::extensions::InvokedSkill {
        name: "reviewer".to_string(),
        summary: "database review".to_string(),
        tokens: 40,
    }));

    match recall.events.as_slice() {
        [
            stella_protocol::AgentEvent::SkillInjected {
                name,
                summary,
                tokens,
                trigger,
            },
        ] => {
            assert_eq!(name, "reviewer");
            assert_eq!(summary, "database review");
            assert_eq!(*tokens, 40);
            assert_eq!(*trigger, stella_protocol::SkillTrigger::Command);
        }
        other => panic!("a `/slug` invocation announced nothing: {other:?}"),
    }
}

/// A turn that invoked nothing adds nothing — the seam cannot manufacture a
/// use out of an ordinary prompt.
#[test]
fn a_turn_that_invoked_no_skill_adds_no_event() {
    let mut recall = OpeningRecall::default();
    recall.note_invoked_skill(None);
    assert!(recall.events.is_empty());
}
