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

/// **Witness (#3366).** An answered mid-turn re-query reports its recall into
/// the turn's event stream — one `ContextRecall` for the spend it just made.
///
/// Fails on base twice over: `signal_recall_block` returned only the rendered
/// `String`, discarding the [`Recall`] the event is built from, and
/// `SessionRequery` held no event channel to report it into. A re-query runs a
/// full fan-out with provider spend behind it, so a silent one is exactly the
/// unmeterable cost #452 closed for pre-turn recall.
#[tokio::test]
async fn an_answered_requery_emits_one_context_recall() {
    use stella_core::ports::SteeringRequery as _;

    let dir = workspace();
    let lesson = "the deploy script must run migrations before restarting the api";
    let memory = session(dir.path());
    memory
        .store
        .upsert(ContextDelta {
            memories: vec![MemoryInput::reflection(lesson, Vec::<String>::new())],
            ..ContextDelta::default()
        })
        .await
        .expect("store a recallable lesson");

    // Through the seam both drivers take, so the witness covers the wiring
    // and not just the adapter's ability to send.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let requery = crate::memory::requery_for_turn(Some(&memory), &[], tx.into())
        .expect("a session with memory has a re-query adapter");

    let touched = vec!["crates/stella-model/anthropic.rs".to_string()];
    let drifted = stella_core::steering::TurnSignal {
        prompt: lesson,
        touched_paths: &touched,
        since_last_query: 5,
        ..Default::default()
    };
    requery
        .requery(&drifted)
        .await
        .expect("the drifted signal recalls the planted lesson");

    let recalls: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok())
        .filter(|event| matches!(event, stella_protocol::AgentEvent::ContextRecall { .. }))
        .collect();
    assert_eq!(
        recalls.len(),
        1,
        "the re-query's recall reaches the turn's stream exactly once: {recalls:?}"
    );
    let stella_protocol::AgentEvent::ContextRecall { frames, .. } = &recalls[0] else {
        unreachable!("filtered above")
    };
    assert!(
        !frames.is_empty(),
        "the event names the frames the re-query spent on"
    );
}

/// **Witness (#3358, frames).** A frame the recall host's cross-provider merge
/// evicted is named in `SteeringSet::dropped` under `SteeringSource::Memory`,
/// beside the frames the same turn kept.
///
/// Fails on base: `recalled_frames_anchored` consumed `recalled.dropped` for
/// the stderr warning and then discarded it — `Recall` carries frames and
/// usage but no drop list, so by the time the plane gathered its candidates
/// there was nothing left to map, and neither `frame_drop` nor the plane's
/// frame-drop channel existed to map it into.
///
/// The merge's own drop *report* is exercised where it is produced (the CGP
/// composition conformance run in `contextgraph::tests`, which asserts every
/// non-admitted frame is named); a local one-provider recall cannot reach it,
/// since the provider applies the same budget first and leaves the merge
/// nothing to evict. So the kept side here is a real recall and the evicted
/// side is a merge drop of exactly the shape that run admits.
#[tokio::test]
async fn a_frame_the_host_merge_evicted_reaches_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let goal = "the deploy script must run migrations before restarting the api";
    let memory = session(dir.path());
    memory
        .store
        .upsert(ContextDelta {
            memories: vec![MemoryInput::reflection(goal, Vec::<String>::new())],
            ..ContextDelta::default()
        })
        .await
        .expect("store a recallable lesson");

    let recalled = memory.recalled_frames_anchored(goal, vec![], |_| {}).await;
    assert_eq!(
        recalled.recall.frames.len(),
        1,
        "the control: the planted lesson is recalled"
    );

    let evicted = crate::memory::recall::frame_drop(&crate::contextgraph::HostDroppedFrame {
        provider: "local".into(),
        id: "nod_evicted".into(),
        citation_label: "[evicted lesson]".into(),
        token_cost: 120,
        reason: stella_context::DropReason::TokenBudget,
    });
    assert_eq!(
        (evicted.source, evicted.handle.as_str(), evicted.est_tokens),
        (
            stella_core::steering::SteeringSource::Memory,
            "nod_evicted",
            120
        ),
        "a merge eviction keeps its stable id and the cost the budget refused"
    );

    let empty = stella_core::skills::select_skills_reporting(
        &[],
        goal,
        &[],
        &stella_core::skills::SelectionConfig::default(),
    );
    let signal = stella_core::steering::TurnSignal {
        prompt: goal,
        ..Default::default()
    };
    let set = crate::memory::recall::query_gathered_plane(
        &signal,
        &recalled.recall.frames,
        std::slice::from_ref(&evicted),
        &empty,
        None,
    );

    assert_eq!(
        set.by_source().get("memory").copied(),
        Some((1, 1)),
        "the plane reports true (selected, dropped) counts for the memory \
         source: {set:?}"
    );
    assert!(
        set.dropped.contains(&evicted),
        "the eviction is named in the ledger: {:?}",
        set.dropped
    );
}

/// A prompt naming no path still anchors, on what the turn has touched.
///
/// The witness for #4249. A live turn resolving a merge conflict in
/// `views/issues.rs` asked:
///
/// > Upstream renamed `_model` to `model` and added a PR strip.
///
/// It names no path, so the anchor set came out empty and retrieval ran
/// unscoped across the whole index — which returned 1133 tokens of an
/// unrelated Python benchmark harness, because `model` is a common word and
/// unscoped lexical similarity has nothing to lose against.
///
/// `anchors_for` is the fix and this is what it buys: the same prompt, the same
/// workspace, and the anchor set is the file the turn is actually editing.
#[test]
fn a_pathless_prompt_anchors_on_what_the_turn_touched() {
    let dir = tempfile::tempdir().expect("tmp");
    let root = dir.path();
    let edited = root.join("crates/stella-tui/src/views/issues.rs");
    std::fs::create_dir_all(edited.parent().expect("parent")).expect("mkdir");
    std::fs::write(&edited, "fn render() {}").expect("write");

    let memory = session(root);
    let prompt = "Upstream renamed `_model` to `model` and added a PR strip.";

    assert!(
        memory.anchors_for(prompt, &[]).is_empty(),
        "the prompt names no path — unanchored is exactly the state that let \
         an unrelated subtree answer"
    );

    let touched = vec!["crates/stella-tui/src/views/issues.rs".to_string()];
    let anchored = memory.anchors_for(prompt, &touched);
    assert_eq!(
        anchored,
        vec!["crates/stella-tui/src/views/issues.rs".to_string()],
        "the turn's own file is the anchor the prompt could not contribute"
    );
}

/// A workspace with TWO domains, one skill each, and a real file under each —
/// the shape a turn that drifts twice needs: the second drift must be able to
/// add a frame or a skill the first one did not have.
fn workspace_two_domains() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    crate::domains::Domains {
        version: 1,
        inferred_by: "heuristic".into(),
        source_fingerprint: None,
        domains: vec![
            crate::domains::Domain {
                name: "model-adapters".into(),
                description: "provider adapters".into(),
                paths: vec!["crates/stella-model".into()],
            },
            crate::domains::Domain {
                name: "cli-surface".into(),
                description: "command line surface".into(),
                paths: vec!["crates/stella-cli".into()],
            },
        ],
    }
    .save(root)
    .expect("domains.toml writes");

    for (dir_path, file) in [
        ("crates/stella-model", "anthropic.rs"),
        ("crates/stella-cli", "main.rs"),
    ] {
        let at = root.join(dir_path);
        std::fs::create_dir_all(&at).unwrap();
        std::fs::write(at.join(file), "// source\n").unwrap();
    }

    // Both skills share zero wording with the prompt below, so the only thing
    // that can select either is its domain tag.
    for (slug, domain, body) in [
        (
            "adapter-notes",
            "model-adapters",
            "Always reuse an existing adapter's shape.",
        ),
        (
            "cli-notes",
            "cli-surface",
            "Flags are declared once and parsed once.",
        ),
    ] {
        let skill_dir = root.join(".stella").join("skills").join(slug);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!(
                "---\nname: {slug}\ndescription: quirks worth knowing\n\
                 domains: {domain}\n---\n\n{body}\n"
            ),
        )
        .unwrap();
    }

    dir
}

/// **Witness (#4236).** A second re-query whose steering set is a strict
/// superset of the first renders ONLY what the first one did not — the frame
/// and the skill already in front of the model are left out.
///
/// Fails on base, where the dedup is byte-exact over the whole rendered block
/// (`state.produced.insert(block.clone())`): drift is incremental, so the
/// block that follows `{A, B}` is `{A, B, C}` — different bytes, same first
/// two items, injected whole. These are `User` messages and compaction passes
/// 1–4 only rewrite tool results, so every repeat is permanent in the paid
/// prefix for the rest of the session.
#[tokio::test]
async fn a_second_requery_renders_only_the_frames_the_first_did_not() {
    use stella_core::ports::SteeringRequery as _;

    let dir = workspace_two_domains();
    let lesson = "the deploy script must run migrations before restarting the api";
    let memory = session(dir.path());
    memory
        .store
        .upsert(ContextDelta {
            memories: vec![MemoryInput::reflection(lesson, Vec::<String>::new())],
            ..ContextDelta::default()
        })
        .await
        .expect("store a recallable lesson");

    let requery = crate::memory::SessionRequery::new(&memory, &[]);
    let first_touched = vec!["crates/stella-model/anthropic.rs".to_string()];
    let then_touched = vec![
        "crates/stella-model/anthropic.rs".to_string(),
        "crates/stella-cli/main.rs".to_string(),
    ];
    fn signal<'a>(prompt: &'a str, touched: &'a [String]) -> stella_core::steering::TurnSignal<'a> {
        stella_core::steering::TurnSignal {
            prompt,
            touched_paths: touched,
            since_last_query: 5,
            ..Default::default()
        }
    }

    let first = requery
        .requery(&signal(lesson, &first_touched))
        .await
        .expect("the first drift surfaces the lesson and the domain's skill");
    assert!(
        first.contains(lesson) && first.contains("adapter-notes"),
        "the control: the first block carries both, or the assertions below \
         pass for the wrong reason: {first}"
    );

    let second = requery
        .requery(&signal(lesson, &then_touched))
        .await
        .expect("the second drift surfaces the second domain's skill");
    assert!(
        second.contains("cli-notes"),
        "the second block carries what the drift added: {second}"
    );
    assert!(
        !second.contains(lesson),
        "a frame already in front of the model is not re-injected: {second}"
    );
    assert!(
        !second.contains("adapter-notes"),
        "a skill already in front of the model is not re-injected: {second}"
    );
}

/// An anchor has to name a file that exists — one that names nothing scopes
/// nothing, and would widen the query while looking like it narrowed it.
#[test]
fn a_touched_path_that_is_not_a_file_is_not_an_anchor() {
    let dir = tempfile::tempdir().expect("tmp");
    let memory = session(dir.path());
    let touched = vec!["crates/gone/vanished.rs".to_string()];
    assert!(
        memory
            .anchors_for("resolve the conflict", &touched)
            .is_empty()
    );
}
