//! Session-memory tests — moved verbatim out of the module's inline
//! `mod tests` to make room for the retrieval-tuning and suppression
//! seams (#712). The assertions are unchanged.

// #1221: the A/B recall control — its durable schedule, what a control turn
// suppresses, and the attribution that separates the arms afterwards.
mod ab_control;
// #2320: the session clock. That the learning loop reads one injected time
// source rather than the wall clock is the property replay rests on, and it is
// what makes the truth sweep's TTL branch reachable from a test at all.
mod determinism;
mod path_token;
mod quarantine;
// Which sessions actually receive the volatile record channel — a separate
// question from what recall renders, and the one that went unasked (epic #897).
mod record_channel;
// Everything between a model's reflection response and a stored lesson: the
// parser, the self-review, and what `reflect_and_record` writes. The largest
// seam here, and the one this file kept growing on — #2465 is what took it
// over the ratchet, whose answer to a first crossing is a split.
mod reflection_mining;
// Spec §8 (#737): the seam where auto-created skill files meet what is
// already on disk — its own module, and the reason tests.rs fits the ratchet.
mod skill_creation;
// #2459: a lesson's `trigger` decides what recall returns. The experiment
// needs two goals, two crossed lessons and a scripted provider to set up, so it
// lives beside the other multi-fixture seams rather than inline here.
mod trigger_recall;

use super::*;

fn msg(role: MessageRole, content: &str) -> CompletionMessage {
    CompletionMessage {
        role,
        content: content.into(),
        tool_calls: vec![],
        tool_results: vec![],
        attachments: Vec::new(),
    }
}

#[test]
fn ab_control_fires_exactly_once_per_rate_not_every_turn() {
    // The witness for the wall-clock bug: on a microsecond-resolution
    // realtime clock the old `ns % rate == 0` predicate was true on EVERY
    // turn, silently disabling recall. The turn-counter schedule must
    // suppress exactly turns rate, 2*rate, 3*rate — and no others.
    let rate = 10;
    let suppressed: Vec<u64> = (1..=30).filter(|&t| ab_control_turn(t, rate)).collect();
    assert_eq!(
        suppressed,
        vec![10, 20, 30],
        "exactly 1-in-{rate} turns is a control turn"
    );
    // The old bug would have suppressed all 30; guard against a regression
    // back to "always on".
    assert_eq!(
        (1..=30).filter(|&t| ab_control_turn(t, rate)).count(),
        3,
        "recall must be live on the other 27 of 30 turns"
    );
}

#[test]
fn ab_control_disabled_for_rate_zero_and_one() {
    for rate in [0, 1] {
        assert!(
            (1..=50).all(|t| !ab_control_turn(t, rate)),
            "rate {rate} must never suppress"
        );
    }
}

#[test]
fn inject_slots_the_block_before_an_already_present_prompt() {
    let mut messages = vec![
        msg(MessageRole::System, "sys"),
        msg(MessageRole::User, "do the thing"),
    ];
    inject_recall_block(&mut messages, Some(format!("{RECALL_MARKER}\nstuff")));
    assert_eq!(messages.len(), 3);
    assert!(messages[1].content.starts_with(RECALL_MARKER));
    assert_eq!(messages[0].content, "sys", "stable prefix untouched (L-E8)");
    assert_eq!(
        messages[2].content, "do the thing",
        "context precedes the question"
    );
}

/// The cache contract: a later turn's refresh may not rewrite, remove,
/// or reorder anything already in history — the old index-1 refresh
/// byte-changed the front of the replayed history every turn and cut
/// the provider cache's reusable prefix to the system message alone.
#[test]
fn inject_appends_fresh_blocks_without_touching_history() {
    let mut messages = vec![msg(MessageRole::System, "sys")];
    inject_recall_block(&mut messages, Some(format!("{RECALL_MARKER}\nfirst")));
    messages.push(msg(MessageRole::User, "turn 1"));
    messages.push(msg(MessageRole::Assistant, "did it"));
    let history: Vec<String> = messages.iter().map(|m| m.content.clone()).collect();
    inject_recall_block(&mut messages, Some(format!("{RECALL_MARKER}\nsecond")));
    // Fresh block at the tail; every prior message byte-identical.
    assert_eq!(messages.len(), history.len() + 1);
    assert!(messages.last().unwrap().content.contains("second"));
    for (i, prior) in history.iter().enumerate() {
        assert_eq!(&messages[i].content, prior, "history rewritten at {i}");
    }
}

#[test]
fn inject_dedupes_an_unchanged_block() {
    let mut messages = vec![msg(MessageRole::System, "sys")];
    let block = format!("{RECALL_MARKER}\nstuff");
    inject_recall_block(&mut messages, Some(block.clone()));
    messages.push(msg(MessageRole::User, "turn 1"));
    messages.push(msg(MessageRole::Assistant, "did it"));
    inject_recall_block(&mut messages, Some(block));
    let markers = messages
        .iter()
        .filter(|m| m.content.starts_with(RECALL_MARKER))
        .count();
    assert_eq!(markers, 1, "an unchanged block is not re-appended");
}

#[test]
fn inject_none_adds_nothing_and_touches_nothing() {
    let mut messages = vec![msg(MessageRole::System, "sys")];
    inject_recall_block(&mut messages, Some(format!("{RECALL_MARKER}\nstuff")));
    let before: Vec<String> = messages.iter().map(|m| m.content.clone()).collect();
    inject_recall_block(&mut messages, None);
    let after: Vec<String> = messages.iter().map(|m| m.content.clone()).collect();
    assert_eq!(before, after, "suppressed recall leaves history untouched");
}

fn frame(
    id: &str,
    kind: contextgraph_types::FrameKind,
    label: &str,
    content: &str,
) -> RecalledFrame {
    let kind = serde_json::to_value(kind)
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    RecalledFrame {
        citation_label: label.into(),
        provider: "workspace-memory".into(),
        source: "stella-context".into(),
        kind,
        uri: None,
        method: None,
        content: content.into(),
        token_cost: 10,
        id: Some(id.into()),
        content_digest: None,
    }
}

fn contextgraph_frame(
    id: &str,
    kind: contextgraph_types::FrameKind,
    label: &str,
    content: &str,
) -> contextgraph_types::ContextFrame {
    contextgraph_types::ContextFrame {
        id: id.into(),
        kind,
        title: label.into(),
        content: Some(content.into()),
        uri: None,
        score: 0.5,
        token_cost: 10,
        content_digest: None,
        representation: contextgraph_types::Representation::Full,
        content_fidelity: None,
        canonical_content_hash: None,
        content_ref: None,
        transform: None,
        minimum_content_fidelity: None,
        inline_content_requirement: None,
        canonical_token_cost: None,
        tokenizer_ref: None,
        valid_from: None,
        valid_to: None,
        recorded_at: None,
        provenance: vec![],
        citation_label: Some(label.into()),
        embedding: None,
        relations: vec![],
    }
}

#[test]
fn recall_section_tags_memory_frames_with_ids_and_asks_for_citations() {
    let frames = vec![
        frame(
            "nod_0123456789abcdef01234567",
            contextgraph_types::FrameKind::Memory,
            "prefer rg",
            "prefer rg over grep here",
        ),
        frame(
            "nod_bbb",
            contextgraph_types::FrameKind::Snippet,
            "src/lib.rs",
            "fn main",
        ),
    ];
    let section = render_context_section(&frames).unwrap();
    assert!(
        section.contains("- [nod_0123456789abcdef01234567] prefer rg — prefer rg over grep here"),
        "memory frames carry the citable id: {section}"
    );
    assert!(
        section.contains("- src/lib.rs — fn main"),
        "non-memory frames keep the plain label form: {section}"
    );
    assert!(
        section.contains("cite_memory"),
        "instruction present: {section}"
    );
    // #2195: one wording, shared with the pipeline's render of the same
    // frames. Two copies of this sentence is how the pipeline path came to
    // have none at all while this one read as evidence the affordance existed.
    assert!(
        section.contains(stella_pipeline::CITE_MEMORY_REQUEST),
        "the ask is the shared constant, verbatim: {section}"
    );
}

#[test]
fn recall_section_without_memories_never_asks_for_citations() {
    let frames = vec![frame(
        "nod_ccc",
        contextgraph_types::FrameKind::Snippet,
        "src/lib.rs",
        "fn main",
    )];
    let section = render_context_section(&frames).unwrap();
    assert!(!section.contains("cite_memory"));

    // No labeled frames at all → no section (an empty block only burns
    // cache).
    assert!(render_context_section(&[]).is_none());
}

/// Witness for #2901: the model is handed a knowledge cutoff
/// (`crate::agent::prompt::append_session_environment`) but never a "now" to
/// measure it against. `render_today_section` is that second operand, and it
/// must actually vary with the clock it is given — a constant string would
/// satisfy every other assertion in this file while leaving the gap open.
///
/// The clock is injected as a plain `i64`, never read via `SystemTime::now()`
/// inside the renderer, which is what makes this provable without racing
/// real time or waiting for a UTC midnight in CI.
#[test]
fn todays_date_section_tracks_the_injected_clock_not_the_wall_clock() {
    // 2026-08-11T00:00:00Z and the same instant one day later.
    let day_one = 1_786_406_400;
    let day_two = day_one + 86_400;

    let first = render_today_section(day_one);
    let second = render_today_section(day_two);

    assert!(
        first.contains("2026-08-11") && !first.contains("2026-08-12"),
        "the section must name the day the injected clock reads: {first}"
    );
    assert!(
        second.contains("2026-08-12") && !second.contains("2026-08-11"),
        "a clock one day later must render a different day: {second}"
    );
    assert_ne!(
        first, second,
        "two instants a day apart must not render identical text"
    );
}

/// Wiring witness for #2901: on a workspace with nothing else to recall (no
/// memories, skills, drafts, or records), both doors a driver can take to
/// inject the volatile block still carry today's date — the date is
/// unconditional, unlike every other section in this block, because a model
/// needs "now" on every turn to judge staleness, not only the turns that
/// also recalled something.
#[tokio::test]
async fn an_otherwise_empty_recall_block_still_names_the_date() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".stella")).unwrap();
    let memory = SessionMemory::open(dir.path(), false).expect("session memory");

    let full = memory
        .recall_block("hello")
        .await
        .expect("the date section alone makes the block non-empty");
    assert!(
        full.contains("Today's date:"),
        "the worker's block must name the date even with nothing else to \
         recall: {full}"
    );

    let pipeline = memory
        .pipeline_recall_block("hello")
        .await
        .expect("the pipeline door carries the same unconditional section");
    assert!(
        pipeline.contains("Today's date:"),
        "the pipeline-driven turn's frames-free block must also name the \
         date: {pipeline}"
    );
}

#[test]
fn graph_frame_projection_preserves_provider_and_origin_provenance() {
    let mut graph = contextgraph_frame(
        "code-graph:sym:src/lib.rs:7:run",
        contextgraph_types::FrameKind::Symbol,
        "fn run (src/lib.rs:7)",
        "fn run() {}",
    );
    graph.uri = Some("file:///repo/src/lib.rs".into());
    graph.provenance = vec![
        contextgraph_types::Provenance {
            kind: "file".into(),
            uri: graph.uri.clone(),
            range: Some("L7-9".into()),
            digest: None,
            method: None,
            by: Some("git-worktree".into()),
        },
        contextgraph_types::Provenance {
            kind: "derivation".into(),
            uri: None,
            range: None,
            digest: None,
            method: Some("tree-sitter/symbol-extract".into()),
            by: Some("code-graph".into()),
        },
    ];

    let recalled = project_recalled_frame(crate::contextgraph::AttributedContextFrame {
        provider: "code-graph".into(),
        frame: graph,
    })
    .expect("labeled graph frame projects");

    assert_eq!(recalled.provider, "code-graph");
    assert_eq!(
        recalled.source, "git-worktree",
        "source is the earliest origin actor, not the latest derivation actor"
    );
    assert_eq!(recalled.kind, "symbol");
    assert_eq!(recalled.uri.as_deref(), Some("file:///repo/src/lib.rs"));
    assert_eq!(
        recalled.method.as_deref(),
        Some("tree-sitter/symbol-extract")
    );
}

#[test]
fn projection_carries_the_frames_content_digest_instead_of_dropping_it() {
    // Phase 2 (#713) deliverable 2. The store mints this digest over exactly
    // the bytes that become the frame's content, and this projection used to
    // drop it on the floor — which is why every `ContextFrameRef` on every
    // recall event carried `content_digest: null`. Without it a frame
    // reference names a row whose text may since have been superseded; with it
    // the reference identifies a revision, which is what makes a past turn's
    // context verifiable rather than merely reconstructed.
    let mut memory = contextgraph_frame(
        "nod_abc",
        contextgraph_types::FrameKind::Memory,
        "auth module",
        "validate the token",
    );
    memory.content_digest = Some("sha256:feedface".into());
    let recalled = project_recalled_frame(crate::contextgraph::AttributedContextFrame {
        provider: "workspace-memory".into(),
        frame: memory,
    })
    .expect("labeled memory frame projects");
    assert_eq!(recalled.content_digest.as_deref(), Some("sha256:feedface"));

    // A provider that declares none keeps `None`. Per docs/spec/adaptive-context/context-reuse.md §1
    // such a frame is not verifiable and must be re-queried rather than reused,
    // so the absence is information — recomputing a digest locally would erase
    // it and, since this projection trims the content, would not even agree
    // with the provider's.
    let mut undeclared = contextgraph_frame(
        "nod_def",
        contextgraph_types::FrameKind::Memory,
        "deploy runbook",
        "staging first",
    );
    undeclared.content_digest = None;
    let recalled = project_recalled_frame(crate::contextgraph::AttributedContextFrame {
        provider: "workspace-memory".into(),
        frame: undeclared,
    })
    .expect("projects");
    assert_eq!(recalled.content_digest, None);
}

#[test]
fn quarantine_is_scoped_to_local_memory_provider_and_kind() {
    let quarantined = std::collections::HashSet::from(["shared-id".to_string()]);
    let mut local = frame(
        "shared-id",
        contextgraph_types::FrameKind::Memory,
        "local",
        "local memory",
    );
    assert!(is_suppressed_local_frame(&local, &quarantined));

    local.provider = "external-graph".into();
    assert!(
        !is_suppressed_local_frame(&local, &quarantined),
        "an external provider may reuse a local id"
    );
    local.provider = "workspace-memory".into();
    local.kind = "symbol".into();
    assert!(
        !is_suppressed_local_frame(&local, &quarantined),
        "only actual local memory frames participate in memory quarantine"
    );
}

/// An episode is a verbatim copy of a past user prompt, recalled and
/// injected exactly like a memory. It used to be unsuppressable: the
/// predicate required `kind == "memory"`, so `stella memory forget` on an
/// episode id silently did nothing and a stale instruction kept surfacing
/// in unrelated runs. This is the regression guard for that.
#[test]
fn a_forgotten_episode_is_suppressed_like_a_forgotten_memory() {
    let forgotten = std::collections::HashSet::from(["ep-1".to_string()]);
    let episode = frame(
        "ep-1",
        contextgraph_types::FrameKind::Episode,
        "local",
        "can you remove the witness tests please",
    );
    assert_eq!(
        episode.kind, "episode",
        "the fixture must actually be an episode-kind frame, or this \
             proves nothing"
    );
    assert!(
        is_suppressed_local_frame(&episode, &forgotten),
        "forgetting an episode must stop it being recalled"
    );

    // Still scoped: a different id is untouched, so the predicate cannot
    // be passing merely because it now accepts every episode.
    let other = frame(
        "ep-2",
        contextgraph_types::FrameKind::Episode,
        "local",
        "unrelated prompt",
    );
    assert!(!is_suppressed_local_frame(&other, &forgotten));
}

#[tokio::test]
async fn ab_control_suppresses_skills_before_any_recall_section_is_built() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join(".stella/skills/reviewer");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: reviewer\ndescription: database review\n---\nALWAYS_REVIEW_DATABASES",
    )
    .unwrap();
    let mut memory =
        SessionMemory::open_with_workspace_skills(dir.path(), false, true).expect("session memory");
    memory.ab_suppressed = true;

    assert!(
        memory.recall_block("review the database").await.is_none(),
        "a control turn must suppress skills as well as context frames"
    );
}

/// The usage report must mirror the injection channels it describes: a
/// control turn injects no skills (the test above), so `selected_skills` —
/// the source of every `skill_usage` telemetry row — must report none either,
/// or the appraisal signal counts skills the model never saw.
#[test]
fn a_control_turn_reports_no_selected_skills() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join(".stella/skills/reviewer");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: reviewer\ndescription: database review\n---\nALWAYS_REVIEW_DATABASES",
    )
    .unwrap();
    let mut memory =
        SessionMemory::open_with_workspace_skills(dir.path(), false, true).expect("session memory");
    assert!(
        !memory.selected_skills("review the database").is_empty(),
        "the fixture skill is selected on an armed turn"
    );

    memory.ab_suppressed = true;
    assert!(
        memory.selected_skills("review the database").is_empty(),
        "a control turn reports exactly what it injected: nothing"
    );
}

/// The frames-free block for pipeline-driven turns: skills and the record
/// channel ride, recalled frames do not — those are the pipeline recall
/// port's job, and rendering them here too is the double-recall/double-bill
/// this method exists to end.
#[tokio::test]
async fn the_pipeline_recall_block_carries_skills_and_records_but_never_frames() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".stella")).unwrap();
    let skill_dir = dir.path().join(".stella/skills/reviewer");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: reviewer\ndescription: database review\n---\nALWAYS_REVIEW_DATABASES",
    )
    .unwrap();
    let mut memory =
        SessionMemory::open_with_workspace_skills(dir.path(), false, true).expect("session memory");
    let lesson = "always use the frobnicator for database migrations";
    memory
        .store
        .upsert(ContextDelta {
            memories: vec![MemoryInput::reflection(lesson, Vec::<String>::new())],
            ..ContextDelta::default()
        })
        .await
        .unwrap();
    let record_file = stella_core::rules::RuleFile {
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
    };
    memory.set_record_registry(stella_core::records::registry::load(
        &[],
        &[record_file],
        &stella_core::records::Facts::default(),
    ));

    let full = memory
        .recall_block("review the database migrations")
        .await
        .expect("the full block renders frames, skills, and records");
    assert!(full.contains("frobnicator"), "full block carries frames");

    let pipeline = memory
        .pipeline_recall_block("review the database migrations")
        .await
        .expect("skills + records still render");
    assert!(pipeline.contains("ALWAYS_REVIEW_DATABASES"));
    assert!(pipeline.contains("staging URL"));
    assert!(
        !pipeline.contains("frobnicator"),
        "frames must stay on the pipeline's recall port, not be billed twice"
    );
}

#[tokio::test]
async fn a_fresh_quarantine_filters_rendered_and_pipeline_recall_next_time() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".stella")).unwrap();
    let memory = SessionMemory::open(dir.path(), false).expect("session memory");
    let lesson = "always use the obsolete frobnicator for database migrations";
    memory
        .store
        .upsert(ContextDelta {
            memories: vec![MemoryInput::reflection(lesson, Vec::<String>::new())],
            ..ContextDelta::default()
        })
        .await
        .unwrap();

    let before = ContextRecallPort::recall(&memory, lesson).await.frames;
    let memory_id = before
        .iter()
        .find(|frame| frame.content.contains("frobnicator"))
        .and_then(|frame| frame.id.clone())
        .expect("new memory is recallable before feedback");
    let rendered_before = memory.recall_block(lesson).await.expect("recall block");
    assert!(rendered_before.contains("frobnicator"));

    let feedback = stella_store::Store::open(dir.path()).unwrap();
    for turn in 0..2 {
        let execution = feedback
            .begin_execution("test", &format!("turn {turn}"), "local", "test")
            .unwrap();
        feedback
            .record_memory_citations(
                execution,
                &[stella_store::MemoryCitationRow {
                    memory_id: memory_id.clone(),
                    useful_score: 1,
                    truthful: false,
                    remark: "verified stale".into(),
                }],
            )
            .unwrap();
    }

    let pipeline_after = ContextRecallPort::recall(&memory, lesson).await.frames;
    assert!(
        pipeline_after
            .iter()
            .all(|frame| frame.id.as_deref() != Some(&memory_id)),
        "pipeline recall must apply quarantine written after session open"
    );
    let rendered_after = memory.recall_block(lesson).await;
    assert!(
        rendered_after
            .as_deref()
            .is_none_or(|block| !block.contains("frobnicator")),
        "rendered recall must use the same freshly quarantined frame set"
    );
}

#[cfg(unix)]
#[test]
fn context_database_is_private_inside_permissive_dot_stella() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let dot = dir.path().join(".stella");
    std::fs::create_dir_all(&dot).unwrap();
    std::fs::set_permissions(&dot, std::fs::Permissions::from_mode(0o777)).unwrap();
    drop(SessionMemory::open(dir.path(), false).expect("memory opens"));

    let mode = |path: &Path| {
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(&dot), 0o777, "mixed project directory is untouched");
    assert_eq!(mode(&dot.join("private")), 0o700);
    assert_eq!(mode(&dot.join("private/context.db")), 0o600);
}

#[cfg(unix)]
#[test]
fn context_database_symlink_is_rejected_without_touching_target() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let dot = dir.path().join(".stella");
    std::fs::create_dir_all(&dot).unwrap();
    let target = dir.path().join("outside.db");
    let external = ContextStore::open(&target).unwrap();
    drop(external);
    let before = std::fs::read(&target).unwrap();
    std::fs::create_dir_all(dot.join("private")).unwrap();
    symlink(&target, dot.join("private/context.db")).unwrap();

    assert!(SessionMemory::open(dir.path(), false).is_none());
    assert_eq!(std::fs::read(&target).unwrap(), before);
}

/// Also the witness for #1139's concrete symptom. This test used to arm BOTH
/// isolation mechanisms at once — mutate the real process `HOME` *and* install
/// the `TEST_USER_HOME` thread-local — because the skill loaders it exercises
/// straddled the seam: some read the thread-local, the rest read ambient env.
/// One seam means one redirect, so the process environment (and the lock and
/// restore guard that had to protect it) is untouched here now.
#[test]
fn untrusted_project_skill_bodies_are_absent_while_recalled_context_still_renders() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(home.join(".stella/skills/user")).unwrap();
    std::fs::write(
        home.join(".stella/skills/user/SKILL.md"),
        "---\nname: user\ndescription: user skill\n---\nUSER_SKILL_BODY",
    )
    .unwrap();
    std::fs::create_dir_all(workspace.join(".stella/skills/project")).unwrap();
    std::fs::write(
        workspace.join(".stella/skills/project/SKILL.md"),
        "---\nname: project\ndescription: project skill\n---\nPROJECT_SKILL_BODY",
    )
    .unwrap();
    let _test_home = crate::paths::test_user_home(home.clone());

    let skills = load_workspace_skills_with_authority(&workspace, false).skills;
    let trusted = load_workspace_skills_with_authority(&workspace, true).skills;

    let names: Vec<&str> = skills.iter().map(|skill| skill.name.as_str()).collect();
    assert_eq!(names, vec!["user"], "loaded skills: {names:?}");
    let trusted_names: Vec<&str> = trusted.iter().map(|skill| skill.name.as_str()).collect();
    assert_eq!(trusted_names, vec!["user", "project"]);

    let ordinary = frame(
        "nod_context",
        contextgraph_types::FrameKind::Snippet,
        "src/lib.rs",
        "ordinary recalled evidence",
    );
    let section = render_context_section(&[ordinary]).expect("ordinary recall renders");
    assert!(section.contains("ordinary recalled evidence"), "{section}");
}

/// An id-less memory frame keeps its content and does not claim citability.
///
/// The render arm for `("memory", None)` was missing: such a frame set the
/// citable flag and then pushed no line, so the recall budget was spent
/// fetching content that never reached the model, while the block still
/// instructed it to cite `[nod_…]` ids that might appear nowhere in it.
/// `RecalledFrame` documents `id: None` as a legitimate state, so this was
/// reachable by contract.
#[test]
fn an_id_less_memory_frame_still_renders_and_does_not_promise_citability() {
    let mut anonymous = frame(
        "ignored",
        contextgraph_types::FrameKind::Memory,
        "house convention",
        "amounts are integer minor units",
    );
    anonymous.id = None;

    let section = render_context_section(&[anonymous]).expect("a frame with content renders");
    assert!(
        section.contains("amounts are integer minor units"),
        "content the recall budget already paid for must reach the model: {section}"
    );
    assert!(
        !section.contains("cite_memory"),
        "nothing here is citable, so the block must not ask for a citation: {section}"
    );

    // The control: the same frame WITH an id is citable and says so.
    let identified = frame(
        "nod_6428c2bb9b9b7aa1adc457fa",
        contextgraph_types::FrameKind::Memory,
        "house convention",
        "amounts are integer minor units",
    );
    let section = render_context_section(&[identified]).expect("renders");
    assert!(
        section.contains("[nod_6428c2bb9b9b7aa1adc457fa]"),
        "{section}"
    );
    assert!(section.contains("cite_memory"), "{section}");
}

/// **Witness (#1846).** An A → B → A recall sequence must not re-append A.
///
/// The dedup compared only the MOST RECENT marker, so by the time A came
/// round again B was the newest and A read as fresh. Recall content genuinely
/// oscillates — it is a function of the prompt, so returning to an earlier
/// subject returns to an earlier block — and each one is up to ~3k tokens
/// that nothing can reclaim: these are User messages, and compaction passes
/// 1–4 only rewrite tool results. A 30-turn session accumulated ~90k tokens
/// of superseded blocks in the paid prefix.
///
/// Two markers is the correct answer, not one: A and B were each genuinely
/// new when first shown, and removing either would rewrite history — the
/// full-rate re-bill L-E8 exists to prevent.
#[test]
fn inject_does_not_re_append_a_block_the_history_already_holds() {
    let a = format!("{RECALL_MARKER}\nsubject A");
    let b = format!("{RECALL_MARKER}\nsubject B");
    let mut messages = vec![msg(MessageRole::System, "sys")];

    inject_recall_block(&mut messages, Some(a.clone()));
    messages.push(msg(MessageRole::User, "turn 1"));
    inject_recall_block(&mut messages, Some(b.clone()));
    messages.push(msg(MessageRole::User, "turn 2"));
    let before: Vec<String> = messages.iter().map(|m| m.content.clone()).collect();

    inject_recall_block(&mut messages, Some(a));

    let markers = messages
        .iter()
        .filter(|m| m.content.starts_with(RECALL_MARKER))
        .count();
    assert_eq!(
        markers, 2,
        "A returning must not add a third marker — it is already in history"
    );
    // And nothing was rewritten to achieve that: the prefix is untouched.
    let after: Vec<String> = messages.iter().map(|m| m.content.clone()).collect();
    assert_eq!(
        before, after,
        "the dedup must skip an append, never rewrite history (L-E8)"
    );
    assert!(
        messages.iter().any(|m| m.content.contains("subject B")),
        "B is still there — it was genuinely new when it was shown"
    );
}

/// The dedup must still let genuinely new content through, or it would be a
/// fix that simply stops recall working.
#[test]
fn inject_still_appends_content_the_history_has_never_held() {
    let mut messages = vec![msg(MessageRole::System, "sys")];
    for subject in ["A", "B", "C"] {
        inject_recall_block(
            &mut messages,
            Some(format!("{RECALL_MARKER}\nsubject {subject}")),
        );
        messages.push(msg(MessageRole::User, "a turn"));
    }
    let markers = messages
        .iter()
        .filter(|m| m.content.starts_with(RECALL_MARKER))
        .count();
    assert_eq!(markers, 3, "three distinct blocks are three appends");
}

/// Witness for #2709's coverage signal: a record that MATCHED this turn's
/// facts but lost its seat to the record budget is reported, not silently
/// absent — a scoped rule that systematically loses the budget contest looks
/// exactly like a rule that never applied, unless someone says otherwise.
#[test]
fn a_matched_record_evicted_by_the_budget_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let mut memory =
        SessionMemory::open_with_workspace_skills(dir.path(), false, true).expect("session memory");

    // Five records at ~550 rendered chars each against the 2000-char budget:
    // the lowest-precedence one must lose its seat.
    let mut contents = String::from("schema = \"context-record/v0.1\"\nset_id = \"acme\"\n");
    for i in 0..5 {
        let filler = "x".repeat(500);
        contents.push_str(&format!(
            "\n[[record]]\nlineage_id = \"ctx.acme.bulk-{i}\"\nkind = \"fact\"\n\
             statement = \"Rule {i} {filler}\"\nstatus = \"active\"\norigin = \"user\"\n\
             \n[record.steering]\nforce = \"may\"\nprecedence = {}\n",
            50 - i
        ));
    }
    let record_file = stella_core::rules::RuleFile {
        path: ".stella/rules/ctx.acme.toml".to_string(),
        contents,
    };
    memory.set_record_registry(stella_core::records::registry::load(
        &[],
        &[record_file],
        &stella_core::records::Facts::default(),
    ));

    let mut reports = Vec::new();
    let section =
        memory.turn_record_section_reporting("anything at all", |message| reports.push(message));
    assert!(section.is_some(), "the surviving records still render");
    assert!(
        reports.iter().any(|m| m.contains("did not fit")),
        "the evicted record must be reported, never silently absent: {reports:?}"
    );
}
