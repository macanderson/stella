//! Session-memory tests — moved verbatim out of the module's inline
//! `mod tests` to make room for the retrieval-tuning and suppression
//! seams (#712). The assertions are unchanged.

use super::*;

#[test]
fn goal_path_anchors_extracts_only_real_workspace_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/driver.rs"), "fn main() {}").unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "").unwrap();
    let root = dir.path();

    // Named files anchor — including file:line and trailing-punctuation
    // spellings; prose, ghosts, and escapes never do.
    assert_eq!(
        goal_path_anchors("fix the panic in src/driver.rs.", root),
        vec!["src/driver.rs"]
    );
    assert_eq!(
        goal_path_anchors("see src/driver.rs:42 and (src/lib.rs)", root),
        vec!["src/driver.rs", "src/lib.rs"]
    );
    assert!(goal_path_anchors("no paths here at all", root).is_empty());
    assert!(goal_path_anchors("src/ghost.rs does not exist", root).is_empty());
    assert!(
        goal_path_anchors("read ../../etc/passwd and src/../src/driver.rs", root).is_empty(),
        "escape spellings must never anchor"
    );
    // Duplicates collapse.
    assert_eq!(
        goal_path_anchors("src/driver.rs then src/driver.rs again", root),
        vec!["src/driver.rs"]
    );
}

#[test]
fn goal_path_anchors_are_capped() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("m")).unwrap();
    let mut goal = String::new();
    for i in 0..8 {
        let rel = format!("m/f{i}.rs");
        std::fs::write(dir.path().join(&rel), "").unwrap();
        goal.push_str(&rel);
        goal.push(' ');
    }
    let anchors = goal_path_anchors(&goal, dir.path());
    assert_eq!(
        anchors.len(),
        4,
        "anchors fan out into neighborhoods — cap them: {anchors:?}"
    );
}

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
    let suppressed: Vec<u32> = (1..=30).filter(|&t| ab_control_turn(t, rate)).collect();
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

    // A provider that declares none keeps `None`. Per docs/context-reuse.md §1
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
    let previous_home = std::env::var_os("HOME");
    unsafe { std::env::set_var("HOME", &home) };
    let _test_home = crate::settings::test_user_home(home.clone());

    let skills = load_workspace_skills_with_authority(&workspace, false).skills;
    let trusted = load_workspace_skills_with_authority(&workspace, true).skills;

    match previous_home {
        Some(previous) => unsafe { std::env::set_var("HOME", previous) },
        None => unsafe { std::env::remove_var("HOME") },
    }
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
    let lessons = parse_lessons(text, &allowed);
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
    assert!(parse_lessons("no json here", &[]).is_empty());
    assert!(parse_lessons("[]", &[]).is_empty());
    assert!(parse_lessons("[{\"lesson\": \"   \"}]", &[]).is_empty());
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
        AgentEvent, CompletionRequest, CompletionResult, CompletionUsage, Provider, ProviderError,
    };

    struct StubProvider;
    #[async_trait]
    impl Provider for StubProvider {
        fn id(&self) -> &str {
            "stub"
        }
        async fn complete(
            &self,
            _req: CompletionRequest,
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
        AgentEvent, CompletionRequest, CompletionResult, CompletionUsage, Provider, ProviderError,
    };

    struct PaidReflection;
    #[async_trait]
    impl Provider for PaidReflection {
        fn id(&self) -> &str {
            "paid-reflection"
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
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
// The guarantee lives in `docs/design/adaptive-context.md` §8. Its two pure
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
