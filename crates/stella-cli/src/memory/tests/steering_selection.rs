//! Witnesses for #3243 D1 and D2 — what skill selection is allowed to claim
//! is "relevant to this turn".
//!
//! Its own file rather than more of `memory/tests.rs`, which sits just under
//! the ratchet `scripts/check-file-size.sh` enforces.

use crate::memory::*;

/// A workspace with one domain covering `crates/stella-model`, one real file
/// under it to anchor against, and one skill tagged with that domain whose
/// wording shares nothing with the prompts below.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    crate::domains::Domains {
        version: 1,
        inferred_by: "heuristic".into(),
        source_fingerprint: None,
        domains: vec![crate::domains::Domain {
            name: "model-adapters".into(),
            description: "provider adapters".into(),
            paths: vec!["crates/stella-model".into()],
        }],
    }
    .save(root)
    .expect("domains.toml writes");

    let anchored = root.join("crates").join("stella-model");
    std::fs::create_dir_all(&anchored).unwrap();
    std::fs::write(anchored.join("anthropic.rs"), "// adapter\n").unwrap();

    let skill_dir = root.join(".stella").join("skills").join("adapter-notes");
    std::fs::create_dir_all(&skill_dir).unwrap();
    // Deliberately zero lexical overlap with either prompt: the ONLY thing
    // that can select this skill is the domain tag, which is what makes the
    // two assertions below about domain scope and nothing else.
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: adapter-notes\ndescription: streaming dialect quirks per vendor\n\
         domains: model-adapters\n---\n\nAlways reuse an existing adapter's shape.\n",
    )
    .unwrap();

    dir
}

/// Workspace skills are behind the project-trust boundary, so a witness about
/// selecting them has to open the session the way a trusted project does.
fn session(root: &std::path::Path) -> SessionMemory {
    SessionMemory::open_with_workspace_skills(root, false, true)
        .expect("session memory opens in a temp workspace")
}

/// **Witness (#3243 Phase 2).** With steering off, the plane injects nothing —
/// not even for a prompt that anchors squarely in a skill's own domain.
///
/// Fails on base, where there is no switch to turn off at all: the selection
/// is unconditional once the skill is on disk. The control arm this buys is
/// the point — an A/B of the whole plane needs an "off" that is one decision,
/// not four unrelated ones.
#[test]
fn steering_turned_off_injects_nothing() {
    let dir = workspace();
    let mut memory = session(dir.path());
    memory.set_steering_enabled(false);

    let selected = memory.selected_skills("fix crates/stella-model/anthropic.rs");

    assert!(
        selected.is_empty(),
        "the switch withholds every injection, including a domain-anchored \
         one the same session would otherwise select: {selected:?}"
    );
}

/// **Witness (#3243 Phase 2, the pipeline leg).** With steering off, the
/// frame query itself answers empty — not just the rendered block.
///
/// Fails on a gate at the block render alone: a pipeline-driven turn never
/// renders the block, it recalls through [`ContextRecallPort`] and feeds the
/// frames to the goal message, the planner, and the witness author — so a
/// switch that stopped at the block would withhold the injection on exactly
/// the surfaces that do not use it.
#[tokio::test]
async fn steering_turned_off_is_frameless_through_the_pipeline_port_too() {
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

    // The control: with steering on, the port recalls the planted lesson —
    // or the empty assertion below passes for the wrong reason.
    assert!(
        !ContextRecallPort::recall(&memory, lesson).await.is_empty(),
        "the port must recall the planted lesson while steering is on"
    );

    memory.set_steering_enabled(false);
    assert!(
        ContextRecallPort::recall(&memory, lesson).await.is_empty(),
        "the switch must reach the pipeline's recall port, not just the \
         rendered block"
    );
}

/// **Witness (D1).** A skill tagged with a domain this turn is not working in
/// is not injected.
///
/// Fails on base: every `select_skills` call site passed
/// `self.domains.names()` — every domain the *repository* declares, constant
/// for the session — so `matched_domains` was non-empty for any domain-tagged
/// skill on any prompt. One match is worth `domain_boost` 0.5 against a
/// `min_score` of 0.08 and satisfies `corroborated` on its own, so the skill
/// below was injected on every non-control turn no matter what was asked.
#[test]
fn a_skill_tagged_with_an_inactive_domain_is_not_selected() {
    let dir = workspace();
    let memory = session(dir.path());

    let selected = memory.selected_skills("rename the changelog heading");

    assert!(
        selected.is_empty(),
        "a prompt that touches no path in the domain must not pull the \
         domain's skills in: {selected:?}"
    );
}

/// **Witness (#3243 Phase 3).** The two-phase task: a turn opens outside a
/// skill's domain, then its work drifts INTO that domain — and the phase-2
/// skill arrives by proactive re-query, selected against the paths the turn
/// touched rather than the prompt it opened on. Absent on base twice over:
/// there is no re-query port at all, and every selector fires once against
/// the opening prompt, so this skill was structurally unreachable mid-turn.
#[tokio::test]
async fn a_drifted_turn_recalls_the_skill_its_prompt_could_not() {
    use stella_core::ports::SteeringRequery as _;

    let dir = workspace();
    let memory = session(dir.path());
    let prompt = "rename the changelog heading";
    assert!(
        memory.selected_skills(prompt).is_empty(),
        "phase 1: the prompt anchors nowhere near the skill's domain"
    );

    let requery = crate::memory::SessionRequery::new(&memory, &[]);
    let touched = vec!["crates/stella-model/anthropic.rs".to_string()];
    let drifted = stella_core::steering::TurnSignal {
        prompt,
        touched_paths: &touched,
        since_last_query: 5,
        ..Default::default()
    };
    let undrifted = stella_core::steering::TurnSignal {
        prompt,
        since_last_query: 5,
        ..Default::default()
    };

    assert!(
        requery.requery(&undrifted).await.is_none(),
        "an undrifted signal buys no re-query — the fingerprint never moved"
    );
    let block = requery
        .requery(&drifted)
        .await
        .expect("the drift into the domain surfaces its skill");
    assert!(
        block.contains("adapter-notes"),
        "phase 2: the touched path selects the domain's skill: {block}"
    );
    assert!(
        requery.requery(&drifted).await.is_none(),
        "the same drift is answered once, not once per step"
    );
}

/// **Witness (D1, the other side).** The same skill IS injected once the
/// prompt names a file inside its domain.
///
/// Passes on base too — for the wrong reason, since base injects it for every
/// prompt. Its job is to prove the fix scoped selection rather than disabling
/// the domain signal, which an empty-selection assertion alone cannot tell
/// apart.
#[test]
fn the_same_skill_is_selected_once_the_prompt_anchors_in_its_domain() {
    let dir = workspace();
    let memory = session(dir.path());

    let selected = memory.selected_skills("fix crates/stella-model/anthropic.rs");

    assert!(
        selected.iter().any(|(name, _)| name == "adapter-notes"),
        "a prompt anchored in the domain still selects its skill: {selected:?}"
    );
}
