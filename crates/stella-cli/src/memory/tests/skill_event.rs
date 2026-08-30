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
        scope: None,
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

/// A workspace holding one directive-carrying skill the prompt below
/// selects: it grants `task_list` alone and asks for high effort.
fn directive_workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let skill_dir = dir.path().join(".stella/skills/reviewer");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: reviewer\ndescription: database review\nallowed-tools: task_list\n\
         effort: high\n---\nALWAYS_REVIEW_DATABASES",
    )
    .expect("skill file");
    dir
}

/// **The witness (#5465).** A skill recall auto-selects expands as an
/// invocation: its `allowed-tools` grant narrows the turn's tool surface
/// through the same plane an explicit `/slug` mounts, a call outside the
/// grant is DENIED naming the skill, a call inside it still runs, its
/// `effort:` reaches the turn, and the narrowing lifts when the spans drop.
///
/// Fails on base, where `inject_opening_recall` pinned `skill_scope: None`
/// for every auto-selected block: the skill's body reached the prompt and
/// its directives changed nothing.
#[tokio::test]
async fn an_auto_selected_directive_skill_narrows_the_turn_and_denies_a_disallowed_tool() {
    use serde_json::json;
    use stella_core::ports::{Principal, ToolExecutor};
    use stella_protocol::tool::ToolOutput;
    use stella_tools::policy::ToolPolicy;
    use stella_tools::skill_plane::{SkillInvocationPlane, SkillScopedTools};

    let dir = directive_workspace();
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
    // Selection is the trigger — the transcript still says the block chose
    // it, not that a person typed `/reviewer`.
    assert!(
        block.telemetry_events().iter().any(|event| matches!(
            event,
            stella_protocol::AgentEvent::SkillInjected {
                name,
                trigger: stella_protocol::SkillTrigger::Auto,
                ..
            } if name == "reviewer"
        )),
        "an auto-selected skill is announced as auto-selected"
    );

    let mut messages = Vec::new();
    let recall = inject_opening_recall(&mut messages, block);
    let [scope] = recall.skill_scopes.as_slice() else {
        panic!(
            "the selected directive skill must yield exactly one scope: {:?}",
            recall.skill_scopes
        );
    };
    assert_eq!(scope.slug, "reviewer");
    assert_eq!(
        scope.allowed_tools.as_deref(),
        Some(&["task_list".to_string()][..]),
        "the grant rides the scope un-resolved"
    );
    assert_eq!(
        recall.skill_effort(),
        Some(stella_protocol::ReasoningEffort::High),
        "the skill's `effort:` reaches the turn"
    );

    // Mounted over the shipped session stack at the position every driver
    // composes the plane at — above the operator's policy layer.
    let registry = stella_tools::registry::ToolRegistry::new(dir.path().to_path_buf());
    let stack = crate::agent::tool_stack::session_stack_with_gate(
        &registry,
        Vec::new(),
        dir.path().to_path_buf(),
        ToolPolicy::from_switches(Vec::new()),
        crate::agent::tool_stack::session_gate(dir.path()),
        Principal::User,
    );
    let plane = SkillInvocationPlane::new();
    let spans = recall.mount_skill_spans(&plane);
    let view = SkillScopedTools::new(&stack, plane.clone());
    assert_eq!(
        view.active_skill_slugs(),
        vec!["reviewer".to_string()],
        "the auto-selected skill is live through the shipped chain"
    );
    // Inside the grant: runs (anti-vacuity — a plane that denies everything
    // would pass the denial below too).
    assert!(
        matches!(
            view.execute("task_list", &json!({})).await,
            ToolOutput::Ok { .. }
        ),
        "the granted call must run"
    );
    // Outside the grant: denied, naming the skill that narrowed the turn.
    match view.execute("get_state", &json!({"key": "k"})).await {
        ToolOutput::Error { message, .. } => assert!(
            message.contains("reviewer"),
            "the denial names the auto-selected skill: {message}"
        ),
        other => panic!("a call outside the auto-selected grant must be denied, got {other:?}"),
    }
    // The spans end with the turn's guards, and the narrowing with them.
    drop(spans);
    assert!(
        view.active_skill_slugs().is_empty(),
        "dropping the guards lifts the narrowing"
    );
}

/// A directive-less skill the block selects is what it always was —
/// injected context, unscoped — so the plane stays inert for the common
/// turn.
#[tokio::test]
async fn a_directive_less_auto_selected_skill_scopes_nothing() {
    let dir = workspace();
    let memory =
        SessionMemory::open_with_workspace_skills(dir.path(), false, true).expect("session memory");
    let block = memory
        .recall_block_reported("review the database migrations", &[])
        .await;
    assert!(
        block.injected_skills.iter().any(|s| s.name == "reviewer"),
        "the fixture prompt must inject the reviewer skill"
    );
    assert!(
        block.skill_scopes.is_empty(),
        "a skill with no directive yields no scope: {:?}",
        block.skill_scopes
    );
    let recall = inject_opening_recall(&mut Vec::new(), block);
    assert!(recall.skill_scopes.is_empty());
    assert_eq!(recall.skill_effort(), None);
}

/// An explicit invocation's scope sits ahead of the auto-selected ones, so
/// its `effort:` is the one the turn honors when both declare one.
#[test]
fn an_invoked_skill_effort_outranks_an_auto_selected_one() {
    use stella_core::skills::invoke::SkillInvocationMode;
    let auto_scope = crate::extensions::SkillTurnScope {
        slug: "auto".to_string(),
        allowed_tools: None,
        effort: Some(stella_protocol::ReasoningEffort::Low),
        mode: SkillInvocationMode::Inline,
    };
    let mut recall = OpeningRecall {
        skill_scopes: vec![auto_scope],
        ..OpeningRecall::default()
    };
    recall.note_invoked_skill(Some(crate::extensions::InvokedSkill {
        name: "invoked".to_string(),
        summary: "typed by a person".to_string(),
        tokens: 1,
        scope: Some(crate::extensions::SkillTurnScope {
            slug: "invoked".to_string(),
            allowed_tools: None,
            effort: Some(stella_protocol::ReasoningEffort::Max),
            mode: SkillInvocationMode::Inline,
        }),
    }));
    assert_eq!(
        recall
            .skill_scopes
            .iter()
            .map(|s| s.slug.as_str())
            .collect::<Vec<_>>(),
        vec!["invoked", "auto"]
    );
    assert_eq!(
        recall.skill_effort(),
        Some(stella_protocol::ReasoningEffort::Max)
    );
}
