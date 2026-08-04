//! Session-memory tests — moved verbatim out of the module's inline
//! `mod tests` to make room for the retrieval-tuning and suppression
//! seams (#712). The assertions are unchanged.

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

    // A provider that declares none keeps `None`. Per docs/design/adaptive-context/context-reuse.md §1
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

#[test]
fn untrusted_project_skill_bodies_are_absent_while_recalled_context_still_renders() {
    let _env = crate::test_env::lock();
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
    // SAFETY: serialized behind the binary-wide environment lock.
    let _restore = crate::test_env::EnvRestore::capture(&["HOME"]);
    unsafe { std::env::set_var("HOME", &home) };
    let _test_home = crate::settings::test_user_home(home.clone());

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

#[test]
fn parse_lessons_drops_invented_domains_and_caps_at_three() {
    let allowed = vec!["api".to_string(), "cli".to_string()];
    let text = r#"Sure! [
            {"lesson": "prefer tables", "domains": ["cli", "made-up"]},
            {"lesson": "b", "domains": []},
            {"lesson": "c", "domains": ["API"]},
            {"lesson": "d", "domains": []}
        ]"#;
    let lessons = lessons_with(text, &allowed);
    assert_eq!(lessons.len(), 3, "capped at 3");
    assert_eq!(lessons[0].domains, vec!["cli"], "invented domain dropped");
    assert_eq!(
        lessons[2].domains,
        vec!["API"],
        "case-insensitive match kept"
    );
    assert!(lessons[0].occurred_at > 0);
}

#[test]
fn parse_lessons_tolerates_garbage_and_empty_output() {
    assert!(lessons_of("[]").is_empty());
    assert!(lessons_of("[{\"lesson\": \"   \"}]").is_empty());
}

/// An unreadable response and a turn with nothing to learn are different
/// outcomes and must not report the same way.
///
/// Reporting them identically is what let the context lifecycle starve in
/// silence: a model that narrates instead of answering yields `recorded: 0,
/// error: null` on every turn, which is indistinguishable from an agent that
/// keeps getting everything right.
#[test]
fn unreadable_reflection_is_distinguished_from_having_nothing_to_say() {
    use crate::memory::reflection::{ReflectionParse, parse_lessons_checked};

    assert!(
        matches!(
            parse_lessons_checked("", &[]),
            ReflectionParse::Lessons(lessons) if lessons.is_empty()
        ),
        "an empty response is a legitimate 'nothing to record'"
    );
    assert!(
        matches!(
            parse_lessons_checked("no json here", &[]),
            ReflectionParse::Unreadable(_)
        ),
        "prose with no array is a response we failed to read, not an empty one"
    );
}

/// The real-world failure: a model that thinks out loud before answering.
///
/// The previous first-`[`-to-last-`]` rule broke on exactly this, because the
/// narration contains brackets of its own. Observed live on
/// `z-ai/glm-4.7-flash` and `deepseek/deepseek-v4-flash`, both of which
/// produced zero lessons on every turn.
#[test]
fn parse_lessons_reads_an_array_out_of_narrated_output() {
    let allowed = vec!["cli".to_string()];
    let narrated = "1. **Analyze the request:**\n   *   Output format: JSON array \
         (max 3 items).\n   *   Allowed tags: [\"cli\", \"api\"].\n\n\
         2. Now the answer:\n\n```json\n\
         [{\"lesson\": \"register handlers in registry.py\", \"domains\": [\"cli\"]}]\n\
         ```\nHope that helps!";
    let lessons = lessons_with(narrated, &allowed);
    assert_eq!(lessons.len(), 1, "the real array is found past the prose");
    assert_eq!(lessons[0].lesson, "register handlers in registry.py");
    assert_eq!(lessons[0].domains, vec!["cli"]);
}

/// The self-review rides in the same response as the lessons, so the one
/// reflection call that already runs now fills `execution_reflection`'s
/// model-authored half — the columns the Observatory's AVG SELF-RATING,
/// DELIVERED and "what to improve" panels read, and which no producer in the
/// tree had ever written.
#[test]
fn reflection_response_yields_both_lessons_and_a_self_review() {
    let allowed = vec!["cli".to_string()];
    let response = "{\"lessons\": [{\"lesson\": \"register handlers in registry.py\", \
         \"kind\": \"domain\", \"domains\": [\"cli\"]}], \
         \"self_review\": {\"delivered\": true, \"rating\": 7, \
         \"went_well\": \"found the seam fast\", \
         \"to_improve\": \"should have run the gate by exit code\", \
         \"critique\": \"correct, but slow to verify\"}}";
    let lessons = lessons_with(response, &allowed);
    assert_eq!(lessons.len(), 1, "the lessons array is still read");
    assert_eq!(lessons[0].lesson, "register handlers in registry.py");

    let review = crate::memory::reflection::parse_self_review(response)
        .expect("the self_review object is read");
    assert_eq!(review.delivered, Some(true));
    assert_eq!(review.self_rating, Some(7));
    assert_eq!(
        review.what_to_improve,
        "should have run the gate by exit code"
    );
}

/// A model that answers with a bare lesson array — the format this prompt asked
/// for until the self-review was added, and what any model that ignores the new
/// envelope will still produce — must keep mining lessons exactly as before.
/// Lesson mining is the one stage of this loop that already worked; adding the
/// self-review must not put it at risk.
#[test]
fn a_bare_lesson_array_still_mines_lessons_and_simply_has_no_self_review() {
    let allowed = vec!["cli".to_string()];
    let bare = "[{\"lesson\": \"money is integer minor units\", \
         \"kind\": \"domain\", \"domains\": [\"cli\"]}]";
    assert_eq!(lessons_with(bare, &allowed).len(), 1);
    assert!(
        crate::memory::reflection::parse_self_review(bare).is_none(),
        "no self_review offered is None, not an invented row"
    );
}

/// Narration around the JSON breaks a naive slice in both directions, and the
/// self-review scanner has to tolerate it for the same reason the lesson
/// scanner does.
#[test]
fn a_self_review_is_read_out_of_narrated_output() {
    let narrated = "Let me reflect. The rubric said {rating: 0-10}.\n\n```json\n\
         {\"lessons\": [], \"self_review\": {\"rating\": 3, \
         \"to_improve\": \"read the whole file first\"}}\n```\nDone!";
    let review =
        crate::memory::reflection::parse_self_review(narrated).expect("found past the prose");
    assert_eq!(review.self_rating, Some(3));
    assert_eq!(review.what_to_improve, "read the whole file first");
    assert_eq!(review.delivered, None, "a field not offered stays absent");
}

/// A rating on some other scale is dropped rather than clamped. Clamping 95 to
/// 10 would put a fabricated perfect score under a label that promises the
/// model's own number; `None` reads as "declined to grade", which is true.
#[test]
fn an_out_of_range_rating_is_dropped_not_clamped() {
    let of = |rating: &str| {
        crate::memory::reflection::parse_self_review(&format!(
            "{{\"self_review\": {{\"rating\": {rating}}}}}"
        ))
        .expect("object parses")
        .self_rating
    };
    assert_eq!(of("95"), None);
    assert_eq!(of("-1"), None);
    assert_eq!(of("10"), Some(10));
    assert_eq!(of("0"), Some(0));
}

/// End-to-end proof that the self-review reaches the table the Observatory
/// reads. The dashboard's AVG SELF-RATING / DELIVERED / "what to improve"
/// panels select from `execution_reflection`, and before this every row in a
/// real workspace had NULL in all five model-authored columns because no
/// producer existed — the writer hardcoded `self_rating: None` and nothing else
/// ever wrote them. This drives the actual loop against a stub provider and
/// asserts the row.
#[tokio::test]
async fn reflect_and_record_stores_the_models_self_review_against_its_execution() {
    use async_trait::async_trait;
    use stella_protocol::{
        CompletionRequestRef, CompletionResult, CompletionUsage, Provider, ProviderError,
    };

    struct StubProvider;
    #[async_trait]
    impl Provider for StubProvider {
        fn id(&self) -> &str {
            "stub"
        }
        async fn complete_ref(
            &self,
            _req: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            Ok(CompletionResult {
                text: r#"{"lessons": [{"lesson": "prefer withTenantDb over raw db()",
                     "kind": "domain", "domains": []}],
                     "self_review": {"delivered": true, "rating": 6,
                     "went_well": "found the leak", "to_improve": "should have run the suite",
                     "critique": "correct fix, thin verification"}}"#
                    .into(),
                tool_calls: vec![],
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 1,
                    ..CompletionUsage::default()
                },
                model: "stub".into(),
                cost_usd: 0.0,
                finish_reason: None,
            })
        }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".stella")).unwrap();
    let store = stella_store::Store::open(dir.path()).expect("open store in temp workspace");
    let execution_id = store
        .begin_execution("deck-pipeline", "fix the tenancy leak", "stub", "stub")
        .unwrap();
    let mut memory =
        SessionMemory::open(dir.path(), false).expect("open session memory in temp workspace");
    memory.set_execution_id(execution_id);

    let transcript = vec![
        msg(MessageRole::User, "fix the tenancy leak"),
        msg(MessageRole::Assistant, "swapped db() for withTenantDb"),
    ];
    let report = memory
        .reflect_and_record(&StubProvider, "stub", &transcript, true, true, None)
        .await;
    assert_eq!(report.recorded, 1, "lesson mining still works");

    let review = store
        .self_review(execution_id)
        .unwrap()
        .expect("a reflection row exists for this execution");
    assert_eq!(
        review.self_rating,
        Some(6),
        "AVG SELF-RATING now has something to average"
    );
    assert_eq!(
        review.delivered,
        Some(true),
        "DELIVERED now has something to count"
    );
    assert_eq!(review.what_to_improve, "should have run the suite");
    assert_eq!(review.what_went_well, "found the leak");

    // The lesson is traceable to the turn that taught it, too — the same
    // missing execution id that starved the self-review left every mined
    // reflection row unattributed.
    assert_eq!(
        reflection_execution_ids(dir.path()),
        vec![Some(execution_id)],
        "the mined lesson names its execution"
    );
}

/// Every `reflections.execution_id` in a workspace store, in row order. Read
/// with a fresh connection because the attribution is what is under test and
/// `Store` exposes no targeted reader for it.
fn reflection_execution_ids(workspace_root: &std::path::Path) -> Vec<Option<i64>> {
    let conn = rusqlite::Connection::open(workspace_root.join(".stella/private/store.db")).unwrap();
    let mut stmt = conn
        .prepare("SELECT execution_id FROM reflections ORDER BY id")
        .unwrap();
    stmt.query_map([], |r| r.get::<_, Option<i64>>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// A path that never adopted `set_execution_id` must degrade exactly as before
/// — lessons still mine, the self-review is dropped rather than written against
/// a guessed row. Attributing one turn's grade to another execution would be
/// worse than not recording it.
#[tokio::test]
async fn without_an_execution_id_the_self_review_is_dropped_not_misattributed() {
    use async_trait::async_trait;
    use stella_protocol::{
        CompletionRequestRef, CompletionResult, CompletionUsage, Provider, ProviderError,
    };

    struct StubProvider;
    #[async_trait]
    impl Provider for StubProvider {
        fn id(&self) -> &str {
            "stub"
        }
        async fn complete_ref(
            &self,
            _req: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            Ok(CompletionResult {
                text: r#"{"lessons": [{"lesson": "money is integer minor units",
                     "kind": "domain", "domains": []}],
                     "self_review": {"rating": 9}}"#
                    .into(),
                tool_calls: vec![],
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 1,
                    ..CompletionUsage::default()
                },
                model: "stub".into(),
                cost_usd: 0.0,
                finish_reason: None,
            })
        }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".stella")).unwrap();
    let store = stella_store::Store::open(dir.path()).unwrap();
    let orphan = store
        .begin_execution("deck-pipeline", "unrelated turn", "stub", "stub")
        .unwrap();
    let mut memory = SessionMemory::open(dir.path(), false).unwrap();
    // Deliberately no `set_execution_id`.

    let report = memory
        .reflect_and_record(
            &StubProvider,
            "stub",
            &[msg(MessageRole::User, "convert the totals")],
            true,
            true,
            None,
        )
        .await;
    assert_eq!(report.recorded, 1, "lessons are unaffected");

    assert_eq!(
        store.self_review(orphan).unwrap(),
        None,
        "no rating was pinned to an unrelated execution"
    );
    assert_eq!(
        reflection_execution_ids(dir.path()),
        vec![None],
        "and the lesson is filed unattributed, as it was before"
    );
}

fn lessons_of(text: &str) -> Vec<crate::memory::ReflectionLesson> {
    lessons_with(text, &[])
}

fn lessons_with(text: &str, allowed: &[String]) -> Vec<crate::memory::ReflectionLesson> {
    match crate::memory::reflection::parse_lessons_checked(text, allowed) {
        crate::memory::reflection::ReflectionParse::Lessons(lessons) => lessons,
        crate::memory::reflection::ReflectionParse::Unreadable(excerpt) => {
            panic!("expected a readable lesson array, got unreadable: {excerpt}")
        }
    }
}

#[test]
fn reflection_gate_fires_on_tool_use_and_skips_tool_free_turns() {
    use stella_protocol::ToolCall;

    // A pure conversational turn — no tool calls — is not worth a
    // reflection model call (the common, cheap-to-skip case).
    let chat_only = vec![
        msg(MessageRole::User, "what does this crate do?"),
        msg(MessageRole::Assistant, "it is a terminal coding agent"),
    ];
    assert!(!turn_warrants_reflection(&chat_only));

    // A turn where the assistant called a tool DID work worth mining.
    let mut worked = msg(MessageRole::Assistant, "reading the file first");
    worked.tool_calls = vec![ToolCall {
        call_id: "c1".into(),
        name: "read_file".into(),
        input: serde_json::json!({ "path": "src/main.rs" }),
    }];
    assert!(turn_warrants_reflection(&[worked]));

    // An empty turn slice (nothing happened) is trivially skippable.
    assert!(!turn_warrants_reflection(&[]));
}

/// End-to-end proof that the self-improvement write path works: a
/// reflection model call returning lessons must land them in BOTH the
/// mining log (`.stella/private/reflections.jsonl`) and the recallable context
/// store. Uses a stub provider so the assertion is deterministic (the
/// live model legitimately returns `[]` for trivial turns).
#[tokio::test]
async fn reflect_and_record_writes_lessons_to_log_and_store() {
    use async_trait::async_trait;
    use stella_protocol::{
        AgentEvent, CompletionRequestRef, CompletionResult, CompletionUsage, Provider,
        ProviderError,
    };

    struct StubProvider;
    #[async_trait]
    impl Provider for StubProvider {
        fn id(&self) -> &str {
            "stub"
        }
        async fn complete_ref(
            &self,
            _req: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            Ok(CompletionResult {
                text: r#"[{"lesson": "prefer withTenantDb over raw db()", "domains": []}]"#.into(),
                tool_calls: vec![],
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 1,
                    ..CompletionUsage::default()
                },
                model: "stub".into(),
                cost_usd: 0.0,
                finish_reason: None,
            })
        }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".stella")).unwrap();
    let mut memory =
        SessionMemory::open(dir.path(), false).expect("open session memory in temp workspace");

    let transcript = vec![
        msg(MessageRole::User, "fix the tenancy leak"),
        msg(MessageRole::Assistant, "swapped db() for withTenantDb"),
    ];
    let report = memory
        .reflect_and_record(&StubProvider, "stub", &transcript, true, true, None)
        .await;

    assert_eq!(report.recorded, 1, "the lesson was stored");
    assert!(report.model_error.is_none());
    assert!(report.events.iter().any(|event| matches!(
        event,
        AgentEvent::StepUsage {
            role: stella_protocol::ModelCallRole::Reflection,
            ..
        }
    )));

    // The mining log now carries the lesson, one JSON object per line.
    let log = std::fs::read_to_string(dir.path().join(".stella/private/reflections.jsonl"))
        .expect("reflections.jsonl was written");
    assert!(
        log.contains("withTenantDb"),
        "the lesson reached the mining log: {log}"
    );

    // And the durable `reflections` table mirrors it — the surface the
    // observatory panel, the JSON export, and the prune carve-out actually
    // read (the jsonl is only the mining log).
    let store = stella_store::Store::open(dir.path()).expect("open store.db");
    let export = store.export_all_json().expect("export store tables");
    let (_, reflections) = export
        .iter()
        .find(|(name, _)| *name == "reflections")
        .expect("reflections table exported");
    assert!(
        reflections.contains("withTenantDb"),
        "the lesson reached the store's reflections table: {reflections}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir.path().join(".stella/private")), 0o700);
        assert_eq!(
            mode(&dir.path().join(".stella/private/reflections.jsonl")),
            0o600
        );
    }
}

#[tokio::test]
async fn reflection_preserves_settled_cost_when_budget_rejects_model_output() {
    use async_trait::async_trait;
    use stella_protocol::{
        AgentEvent, CompletionRequestRef, CompletionResult, CompletionUsage, Provider,
        ProviderError,
    };

    struct PaidReflection;
    #[async_trait]
    impl Provider for PaidReflection {
        fn id(&self) -> &str {
            "paid-reflection"
        }

        async fn complete_ref(
            &self,
            _request: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            Ok(CompletionResult {
                text: r#"[{"lesson":"must not apply","domains":[]}]"#.into(),
                tool_calls: Vec::new(),
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 8,
                    output_tokens: 2,
                    ..CompletionUsage::default()
                },
                model: "paid-reflection-model".into(),
                cost_usd: 0.02,
                finish_reason: None,
            })
        }
    }

    let dir = tempfile::tempdir().expect("root");
    let mut memory = SessionMemory::open(dir.path(), false).expect("memory");
    let report = memory
        .reflect_and_record(
            &PaidReflection,
            "paid-reflection-model",
            &[msg(MessageRole::User, "worked")],
            true,
            true,
            Some(0.001),
        )
        .await;
    assert_eq!(report.recorded, 0);
    assert_eq!(report.cost_usd, 0.02);
    assert!(report.model_error.is_some());
    assert!(report.events.iter().any(|event| matches!(
        event,
        AgentEvent::StepUsage {
            role: stella_protocol::ModelCallRole::Reflection,
            cost_usd,
            ..
        } if (*cost_usd - 0.02).abs() < f64::EPSILON
    )));
}

// ── Spec §8: auto-creation must never clobber a hand-edited file (#737) ──────
//
// The guarantee lives in `docs/design/adaptive-context/adaptive-context.md` §8. Its two pure
// halves — the per-session cap and the no-clobber comparison itself — are
// tested in `stella-core/src/skills.rs`, and tombstone suppression is tested
// in `stella-store/src/forget_tests.rs`. What could not be tested there is the
// part that was actually broken: `stella-core` only ever sees the path list
// this crate hands it, and this crate used to build that list from the skills
// that LOADED rather than the files that EXIST. These tests own that seam.

/// A mining log holding three occurrences of each lesson —
/// `SkillMineConfig::default()` requires `min_occurrences: 3` before a cluster
/// is worth a skill. Returns the log path `auto_create_skills` reads.
fn mining_log(root: &Path, lessons: &[&str]) -> PathBuf {
    let dir = root.join(".stella").join("private");
    std::fs::create_dir_all(&dir).expect("private dir");
    let mut out = String::new();
    let mut occurred_at = 1_000u64;
    for lesson in lessons {
        for _ in 0..3 {
            occurred_at += 1;
            let line = serde_json::json!({
                "lesson": lesson,
                "domains": [],
                "occurred_at": occurred_at,
            });
            out.push_str(&line.to_string());
            out.push('\n');
        }
    }
    let path = dir.join("reflections.jsonl");
    std::fs::write(&path, out).expect("mining log");
    path
}

/// Every `*.md` actually sitting in the workspace skills dir, sorted.
fn skill_files_on_disk(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root.join(".stella").join("skills")) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort();
    files
}

/// Mine `log` in a fresh session — a new `SessionMemory` each time, so the
/// per-session creation counter starts at zero exactly as it would tomorrow.
fn mine_in_a_fresh_session(root: &Path, log: &Path, workspace_skills: bool) {
    let mut memory = SessionMemory::open_with_workspace_skills(root, false, workspace_skills)
        .expect("session memory");
    memory.auto_create_skills(log, true);
}

/// THE regression (#737). A skill disabled from the SKILLS tab keeps its file
/// on disk by design and drops out of `load_skills()`. Feeding that loaded list
/// to the no-clobber guard meant the guard could not see the file, and because
/// mined identity is a stable `{slug}-{hash8}`, the next recurrence of the same
/// lesson re-targeted the exact path the user had edited and `std::fs::write`
/// destroyed it — silently, with no prompt, no backup, and no version history.
///
/// This is the reachable trigger: disabling a skill is an ordinary supported
/// action needing no unusual configuration.
#[test]
fn a_disabled_hand_edited_skill_is_never_overwritten_by_auto_creation() {
    const LESSON: &str =
        "always run database migrations inside a transaction with an explicit lock timeout";
    let dir = tempfile::tempdir().expect("workspace");
    let root = dir.path();
    let log = mining_log(root, &[LESSON]);

    // Session one mines the lesson and auto-creates the skill.
    mine_in_a_fresh_session(root, &log, true);
    let created = skill_files_on_disk(root);
    assert_eq!(created.len(), 1, "the recurring lesson produced one skill");
    let skill_path = created[0].clone();
    let name = skill_path
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("skill file stem")
        .to_string();

    // The user rewrites the body and then disables the skill. The frontmatter
    // name is kept because that is what the SKILLS tab disables by.
    let hand_edited = format!(
        "---\nname: {name}\ndescription: my own notes\n---\n\n\
         Hand-written by the user. Losing this is the bug.\n"
    );
    std::fs::write(&skill_path, &hand_edited).expect("hand edit");
    crate::skill_manager::set_enabled(stella_tui::SkillScope::Project, &name, false, root)
        .expect("disable from the SKILLS tab");

    // Precondition — without this the test would prove nothing: the file is on
    // disk, and it is absent from the loaded list the guard used to be given.
    assert!(skill_path.exists(), "disabling keeps the file on disk");
    assert!(
        !load_workspace_skills_with_authority(root, true)
            .skills
            .iter()
            .any(|s| s.source_path == skill_path.display().to_string()),
        "a disabled skill is excluded from the loaded list"
    );

    // A later session re-mines the identical cluster onto the identical path.
    mine_in_a_fresh_session(root, &log, true);

    assert_eq!(
        std::fs::read_to_string(&skill_path).expect("the file still exists"),
        hand_edited,
        "auto-creation overwrote a hand-edited skill that was merely disabled"
    );
}

/// Reading and writing the workspace skills directory are one authority.
/// Auto-creation used to compute its target from `workspace_skills_dir()`
/// unconditionally, so a session forbidden from READING project prompts still
/// WROTE mined prose into that directory — where the same flag guaranteed it
/// would never be loaded back.
#[test]
fn auto_creation_never_writes_into_a_skills_dir_the_session_may_not_read() {
    const LESSON: &str = "pin the toolchain version in continuous integration so a floating minor cannot change builds";
    let dir = tempfile::tempdir().expect("workspace");
    let root = dir.path();
    let log = mining_log(root, &[LESSON]);

    mine_in_a_fresh_session(root, &log, false);
    assert!(
        skill_files_on_disk(root).is_empty(),
        "a session that may not read workspace skills must not write one"
    );

    // The control: the very same log DOES create a skill once the session is
    // allowed the workspace scope, so the assertion above is about authority
    // and not about the log failing to mine.
    mine_in_a_fresh_session(root, &log, true);
    assert_eq!(skill_files_on_disk(root).len(), 1);
}

/// An untrusted workspace withholds the skill FILE, not the whole lifecycle.
///
/// The authority gate used to sit on the mining dispatcher, so a workspace
/// without project trust skipped attribution, retirement, observation
/// extraction and proposal induction along with the file write. Those write
/// only to the session's own private store — which the very same turn has
/// already written reflection memories to — so skipping them protected nothing
/// and left `context_records` permanently empty in every fresh checkout, every
/// sandbox, and every CI job. Reflection kept mining lessons that went nowhere,
/// and nothing reported a problem.
#[test]
fn an_untrusted_workspace_still_extracts_observations_even_though_no_skill_lands() {
    const LESSON: &str = "pin the toolchain version in continuous integration so a floating minor cannot change builds";
    let dir = tempfile::tempdir().expect("workspace");
    let root = dir.path();
    let log = mining_log(root, &[LESSON]);

    mine_in_a_fresh_session(root, &log, false);

    assert!(
        skill_files_on_disk(root).is_empty(),
        "the file write stays gated: no skill in a workspace we may not read"
    );

    let db = stella_store::workspace_private_sqlite_path(root, "context.db").expect("db path");
    let store = stella_context::ContextStore::open(db).expect("context store opens");
    let observations = crate::memory::observations::all_observations(&store, 100);
    assert!(
        !observations.is_empty(),
        "the ledger half of the lifecycle must run: an untrusted workspace \
         still turns reflection lessons into observations, or nothing \
         downstream of reflection can ever happen"
    );
}

/// The guarantees that already held must still hold, now that the guard is fed
/// a different list: the per-session cap, and the ordinary case of not
/// clobbering a skill that IS loaded.
#[test]
fn auto_creation_still_caps_each_session_and_spares_loaded_skills() {
    const LESSONS: [&str; 3] = [
        "always run database migrations inside a transaction with an explicit lock timeout",
        "prefer structured logging over printf debugging when tracing request flow",
        "check the response status before parsing the body in every http client wrapper",
    ];
    let dir = tempfile::tempdir().expect("workspace");
    let root = dir.path();
    let log = mining_log(root, &LESSONS);

    // `AutoCreateConfig::default().max_per_session` is 2: three qualifying
    // clusters, two files. Auto-creation must feel magical, not spammy.
    let mut memory =
        SessionMemory::open_with_workspace_skills(root, false, true).expect("session memory");
    memory.auto_create_skills(&log, true);
    assert_eq!(skill_files_on_disk(root).len(), 2, "per-session cap");
    // Mining again inside the SAME session stays capped.
    memory.auto_create_skills(&log, true);
    assert_eq!(skill_files_on_disk(root).len(), 2, "the cap is per session");
    drop(memory);

    // Hand-edit one of the two, leaving it enabled and therefore loaded.
    let first = skill_files_on_disk(root)[0].clone();
    let name = first
        .file_stem()
        .and_then(|s| s.to_str())
        .expect("skill file stem")
        .to_string();
    let hand_edited =
        format!("---\nname: {name}\ndescription: my own notes\n---\n\nEdited, still enabled.\n");
    std::fs::write(&first, &hand_edited).expect("hand edit");

    // Tomorrow's session: the cap resets, so the third lesson lands — and the
    // edited file is untouched.
    mine_in_a_fresh_session(root, &log, true);
    assert_eq!(
        skill_files_on_disk(root).len(),
        3,
        "a fresh session creates the third skill"
    );
    assert_eq!(
        std::fs::read_to_string(&first).expect("the file still exists"),
        hand_edited,
        "a loaded skill's file must never be rewritten either"
    );
}

/// Tombstone filtering still gates auto-creation: a forgotten lesson must not
/// come back as a skill, even though the append-only mining log still contains
/// every line written before the tombstone existed.
#[test]
fn a_forgotten_lesson_still_cannot_return_as_an_auto_created_skill() {
    const LESSON: &str =
        "cache the compiled regular expression instead of rebuilding it on every call";
    let dir = tempfile::tempdir().expect("workspace");
    let root = dir.path();
    let log = mining_log(root, &[LESSON]);

    stella_store::Store::open(root)
        .expect("store")
        .forget(
            stella_store::ContextSurface::Memory,
            "nod_forgotten",
            LESSON,
            "the user forgot it",
        )
        .expect("forget");

    mine_in_a_fresh_session(root, &log, true);
    assert!(
        skill_files_on_disk(root).is_empty(),
        "a tombstoned lesson must not be resurrected as a skill"
    );
}

/// A lesson carries the session's task boundary, not a per-turn fallback.
///
/// Governance promotes only after evidence spans three *distinct tasks*. With
/// no boundary the count fell back to `turn:<timestamp>`, which miscounts in
/// both directions: three lessons from one reflection call share a timestamp
/// and read as one task, while three turns on one task read as three.
#[test]
fn lessons_are_stamped_with_the_sessions_task_boundary() {
    let dir = tempfile::tempdir().expect("workspace");
    let mut memory = SessionMemory::open(dir.path(), false).expect("memory opens");

    let default_id = memory.task_id_for_test().to_string();
    assert!(
        default_id.starts_with("session:"),
        "the default boundary is session-scoped, got {default_id:?}"
    );

    memory.set_task_id("proving-ground:eval-03-interest");
    assert_eq!(memory.task_id_for_test(), "proving-ground:eval-03-interest");
}

/// A process note is written into the deferred band; a domain fact is not.
///
/// This is the wiring assertion, and it is deliberately about the *store*, not
/// about `LessonKind`. An earlier version of this test compared
/// `LessonKind::Domain.recall_rank()` against `LessonKind::Process.recall_rank()`
/// — which restates the function body and passes no matter what the recall path
/// does. The distinction only means something once it survives the write, so
/// that is what is asserted here: the tier that comes back off the node row.
///
/// The ordering this tier buys is proven end-to-end against a real budget in
/// `stella-context`'s `a_deferred_memory_loses_the_last_slot_to_a_normal_one`.
#[tokio::test]
async fn a_process_lesson_is_stored_in_the_deferred_recall_band() {
    use crate::memory::LessonKind;
    use stella_context::RecallTier;

    assert_eq!(LessonKind::Domain.recall_tier(), RecallTier::Normal);
    assert_eq!(LessonKind::Process.recall_tier(), RecallTier::Deferred);
    assert_eq!(
        LessonKind::default(),
        LessonKind::Process,
        "unlabelled lessons are not promoted to facts by accident"
    );

    let dir = tempfile::tempdir().expect("workspace");
    let memory = SessionMemory::open(dir.path(), false).expect("memory opens");
    memory
        .store
        .upsert(ContextDelta {
            memories: vec![
                MemoryInput::reflection("money is integer minor units", Vec::<String>::new())
                    .with_recall_tier(LessonKind::Domain.recall_tier()),
                MemoryInput::reflection("the agent should not retry blindly", Vec::<String>::new())
                    .with_recall_tier(LessonKind::Process.recall_tier()),
            ],
            ..ContextDelta::default()
        })
        .await
        .expect("memories land");

    let tiers: std::collections::HashMap<String, RecallTier> = memory
        .store
        .memory_nodes()
        .expect("memory nodes")
        .into_iter()
        .map(|node| (node.content.clone(), node.recall_tier))
        .collect();
    assert_eq!(
        tiers.get("money is integer minor units"),
        Some(&RecallTier::Normal),
        "a durable fact competes on rank like any other memory"
    );
    assert_eq!(
        tiers.get("the agent should not retry blindly"),
        Some(&RecallTier::Deferred),
        "a note about the turn yields first when the budget binds"
    );
}

/// The wire format tolerates a lesson written before `kind` existed.
#[test]
fn a_lesson_logged_before_kind_existed_still_parses() {
    let line = r#"{"lesson":"registry.py is the command registry","domains":[],"occurred_at":7}"#;
    let parsed: ReflectionLesson = serde_json::from_str(line).expect("legacy line parses");
    assert_eq!(parsed.kind, crate::memory::LessonKind::Process);
    assert!(parsed.task_id.is_empty());
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
