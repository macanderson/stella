// Imported here rather than inherited through `use super::*`: the label
// belongs to the deck's ask io, which lives in `mid_turn_ask` now, and this
// module is its only remaining user in `command_deck`'s namespace.
use crate::interactive::FREE_TEXT_LABEL;

use super::pr_observe::scrub_gh_command;
use super::skills::{
    build_skill_creation_prompt, extract_skill_md, extract_skill_md_from_use, parse_installs_count,
    parse_skill_hits, rank_hits,
};
use super::*;

#[tokio::test]
async fn pr_observer_preserves_github_auth_only() {
    let mut command = tokio::process::Command::new("sh");
    command
        .args([
            "-c",
            "printf '%s|%s|%s|%s' \"${OPENROUTER_API_KEY-unset}\" \"${GITHUB_TOKEN-unset}\" \"${AWS_SECRET_ACCESS_KEY-unset}\" \"${STELLA_TEST_BENIGN-unset}\"",
        ])
        .env("OPENROUTER_API_KEY", "provider-secret")
        .env("GITHUB_TOKEN", "repository-secret")
        .env("AWS_SECRET_ACCESS_KEY", "cloud-secret")
        .env("STELLA_TEST_BENIGN", "visible");
    scrub_gh_command(&mut command);

    let output = command.output().await.unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "unset|repository-secret|unset|visible"
    );
}

#[test]
fn deck_arg_commands_parse_models_forms_and_leave_sentences_as_prompts() {
    assert!(matches!(
        parse_models_command("/models refresh"),
        Some(ModelsCommand::Refresh { force: false })
    ));
    assert!(matches!(
        parse_models_command("/models refresh --force"),
        Some(ModelsCommand::Refresh { force: true })
    ));
    assert!(matches!(
        parse_models_command("/models list"),
        Some(ModelsCommand::List)
    ));
    // One unrecognized token is a typo'd subcommand → usage, never a
    // model call; a sentence stays a prompt.
    assert!(matches!(
        parse_models_command("/models refrsh"),
        Some(ModelsCommand::Usage(_))
    ));
    assert!(parse_models_command("/models what can I use").is_none());
    // Bare forms and non-command paths are not arg commands — and the
    // removed `/model-<role>` heads no longer parse (model config lives
    // on the SETTINGS tab).
    assert!(parse_models_command("/models").is_none());
    assert!(parse_models_command("/model-default zai/glm-5.2").is_none());
    assert!(parse_models_command("/src/main.rs explain").is_none());
}

#[test]
fn parse_skill_hits_strips_ansi_and_extracts_id_installs_url() {
    // The real `npx skills find` shape: ANSI SGR codes, an "Install with"
    // instruction line, result rows, and `└ url` continuation lines.
    let out = "\n\u{1b}[38;5;102mInstall with\u{1b}[0m npx skills add <owner/repo@skill>\n\n\
\u{1b}[38;5;145mwshobson/agents@rust-async-patterns\u{1b}[0m \u{1b}[36m15.8K installs\u{1b}[0m\n\
\u{1b}[38;5;102m└ https://skills.sh/wshobson/agents/rust-async-patterns\u{1b}[0m\n\n\
\u{1b}[38;5;145mapollographql/skills@rust-best-practices\u{1b}[0m \u{1b}[36m13.9K installs\u{1b}[0m\n\
\u{1b}[38;5;102m└ https://skills.sh/apollographql/skills/rust-best-practices\u{1b}[0m\n";
    let hits = parse_skill_hits(out);
    assert_eq!(hits.len(), 2, "only the two result rows: {hits:?}");
    assert_eq!(hits[0].id, "wshobson/agents@rust-async-patterns");
    assert_eq!(hits[0].installs, "15.8K installs");
    assert_eq!(hits[0].installs_rank, 15_800);
    assert_eq!(
        hits[0].url,
        "https://skills.sh/wshobson/agents/rust-async-patterns"
    );
    assert_eq!(hits[1].id, "apollographql/skills@rust-best-practices");
    assert_eq!(hits[1].installs_rank, 13_900);
    // Never leak escape codes or the instruction line into a hit.
    for h in &hits {
        assert!(!h.id.contains('\u{1b}') && !h.id.contains('['), "{h:?}");
        assert!(
            !h.id.contains("Install"),
            "instruction line rejected: {h:?}"
        );
    }
}

#[test]
fn parse_skill_hits_rejects_rows_without_owner_repo_at_skill() {
    // A plain description line (no `@`) and the placeholder are not results.
    let out = "acme/auth  not a real hit\nInstall with npx skills add <owner/repo@skill>\n";
    assert!(parse_skill_hits(out).is_empty());
}

#[test]
fn parse_installs_count_handles_k_m_and_plain() {
    assert_eq!(parse_installs_count("15.8K installs"), 15_800);
    assert_eq!(parse_installs_count("9K installs"), 9_000);
    assert_eq!(parse_installs_count("2.5M installs"), 2_500_000);
    assert_eq!(parse_installs_count("342 installs"), 342);
    assert_eq!(parse_installs_count("installs"), 0);
}

#[test]
fn parse_skill_hits_caps_at_fifty() {
    let out = (0..100)
        .map(|i| format!("pkg/repo@skill-{i}  {i} installs"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(parse_skill_hits(&out).len(), 50);
}

#[test]
fn extract_skill_md_from_use_unwraps_the_wrapped_body() {
    let out = "You are being given a Skill.\n\nUse the following SKILL.md as your instructions:\n\n<SKILL.md>\n---\nname: rust-async\n---\n\n# Rust Async\n\nbody\n</SKILL.md>\n";
    let md = extract_skill_md_from_use(out);
    assert!(md.starts_with("---"), "starts at frontmatter: {md}");
    assert!(md.contains("# Rust Async"));
    assert!(
        !md.contains("You are being given"),
        "preamble dropped: {md}"
    );
    assert!(!md.contains("</SKILL.md>"), "close marker dropped: {md}");
}

#[test]
fn rank_hits_orders_by_relevance_then_popularity() {
    let hits = vec![
        SkillSearchHit {
            id: "a/pkg@pdf-extract".into(),
            installs: "120 installs".into(),
            installs_rank: 120,
            url: String::new(),
        },
        SkillSearchHit {
            id: "b/pkg@img-resize".into(),
            installs: "9K installs".into(),
            installs_rank: 9_000,
            url: String::new(),
        },
        SkillSearchHit {
            id: "c/pkg@pdf-reader".into(),
            installs: "5 installs".into(),
            installs_rank: 5,
            url: String::new(),
        },
    ];
    let ranked = rank_hits(&hits, "extract tables from pdf");
    assert!(ranked[0].contains("a/pkg@pdf-extract"), "{ranked:?}");
    assert!(
        ranked.iter().position(|l| l.contains("a/pkg"))
            < ranked.iter().position(|l| l.contains("b/pkg")),
        "relevance beats popularity: {ranked:?}"
    );
}

#[test]
fn build_skill_creation_prompt_includes_request_and_ranked_candidates() {
    let p = build_skill_creation_prompt(
        "format sql nicely",
        &["a/sql-fmt  sql formatter".to_string()],
    );
    assert!(p.contains("format sql nicely"));
    assert!(p.contains("a/sql-fmt"));
    assert!(p.contains("SINGLE skill"));
    let empty = build_skill_creation_prompt("do a thing", &[]);
    assert!(empty.contains("from scratch"));
}

#[test]
fn extract_skill_md_unwraps_a_fenced_block_or_frontmatter() {
    let fenced = "Here you go:\n```markdown\n---\nname: x\ndescription: d\n---\nbody\n```\ndone";
    let got = extract_skill_md(fenced);
    assert!(got.starts_with("---"), "{got}");
    assert!(got.ends_with("body"), "{got}");
    let bare = "prose\n---\nname: y\ndescription: d\n---\nbody";
    assert!(extract_skill_md(bare).starts_with("---\nname: y"));
}

#[test]
fn mcp_outcome_report_lists_connected_servers_by_name() {
    let report = crate::mcp_cmd::mcp_outcome_report(&["files", "search"], &[], &[], &[], &[]);
    assert_eq!(report, "2 MCP server(s) connected: files, search");
}

#[test]
fn mcp_outcome_report_names_each_failure_with_its_reason() {
    let failed = vec![(
        "slow".to_string(),
        "connect timed out after 10000ms".to_string(),
    )];
    let report = crate::mcp_cmd::mcp_outcome_report(&["files"], &failed, &[], &[], &[]);
    let lines: Vec<&str> = report.lines().collect();
    assert_eq!(lines[0], "1 MCP server(s) connected: files");
    assert_eq!(
        lines[1],
        "MCP server `slow` unavailable: connect timed out after 10000ms"
    );
}

#[test]
fn mcp_outcome_report_states_total_failure_outright() {
    let failed = vec![("a".to_string(), "spawn failed".to_string())];
    let report = crate::mcp_cmd::mcp_outcome_report(&[], &failed, &[], &[], &[]);
    assert!(
        report.starts_with("no MCP servers connected"),
        "the degraded mode is stated, not implied: {report}"
    );
    assert!(report.contains("MCP server `a` unavailable: spawn failed"));
}

/// The witness for #689: before this, an over-advertising server produced no
/// operator-visible output at all — the signal existed in stella-mcp and had
/// no consumer, so the model silently had fewer tools than the server offered.
#[test]
fn mcp_outcome_report_reports_a_truncated_server_as_connected_not_unavailable() {
    let report = crate::mcp_cmd::mcp_outcome_report(&["greedy"], &[], &[("greedy", 12)], &[], &[]);
    let lines: Vec<&str> = report.lines().collect();
    assert_eq!(lines[0], "1 MCP server(s) connected: greedy");
    assert!(
        lines[1].contains("12 tool(s) dropped"),
        "the operator is told how much surface was lost: {report}"
    );
    assert!(
        lines[1].contains(&stella_mcp::MAX_TOOLS_PER_SERVER.to_string()),
        "the notice names the cap it hit rather than hard-coding a number: {report}"
    );
    // The whole point of the separate channel: this server is up and routing.
    assert!(
        !report.contains("unavailable"),
        "a truncated server is connected and healthy — calling it unavailable \
         would be false: {report}"
    );
}

/// Truncation and connection failure are independent axes, and a server can be
/// on the wrong side of both lists at once without either notice swallowing
/// the other.
#[test]
fn mcp_outcome_report_carries_truncation_and_failure_together() {
    let failed = vec![("dead".to_string(), "spawn failed".to_string())];
    let report =
        crate::mcp_cmd::mcp_outcome_report(&["greedy"], &failed, &[("greedy", 3)], &[], &[]);
    assert!(report.contains("MCP server `dead` unavailable: spawn failed"));
    assert!(report.contains("`greedy` advertised more than"));
}

/// The witness for #3722: the per-server schema BYTE budget could trim tools
/// with no operator-visible output at all — `over_budget_servers()` had no
/// caller outside its own unit test, so a server with a dozen enormous schemas
/// lost tools in silence while the count cap next to it announced itself.
#[test]
fn mcp_outcome_report_reports_a_schema_budget_trim_in_its_own_words() {
    let budgeted = vec![("verbose".to_string(), 4)];
    let report = crate::mcp_cmd::mcp_outcome_report(&["verbose"], &[], &[], &budgeted, &[]);
    let lines: Vec<&str> = report.lines().collect();
    assert_eq!(lines[0], "1 MCP server(s) connected: verbose");
    assert!(
        lines[1].contains("4 tool(s) trimmed"),
        "the operator is told how much surface was lost: {report}"
    );
    assert!(
        lines[1].contains(&stella_mcp::MAX_SERVER_SCHEMA_BYTES.to_string()),
        "the notice names the budget it hit rather than hard-coding a number: {report}"
    );
    assert!(
        !report.contains("unavailable"),
        "a trimmed server is connected and routing: {report}"
    );
}

/// The two caps are different walls and must not be read as one. A reader told
/// only "tools dropped" cannot tell whether to split the server or shrink its
/// schemas, which is the whole reason `over_budget_servers` is separate from
/// `over_advertising_servers` in the first place.
#[test]
fn mcp_outcome_report_keeps_the_count_cap_and_the_byte_budget_distinct() {
    let budgeted = vec![("verbose".to_string(), 4)];
    let report = crate::mcp_cmd::mcp_outcome_report(
        &["greedy", "verbose"],
        &[],
        &[("greedy", 3)],
        &budgeted,
        &[],
    );
    assert!(report.contains("`greedy` advertised more than"), "{report}");
    assert!(
        report.contains("`verbose` advertised more tool schema than"),
        "{report}"
    );
    assert!(
        report.contains("trimmed to fit"),
        "the byte budget trims rather than drops past a count: {report}"
    );
}

/// A well-behaved session says nothing about the cap. A notice that fires at
/// zero is noise that trains operators to ignore the real one.
#[test]
fn mcp_outcome_report_is_silent_when_nothing_was_truncated() {
    let report = crate::mcp_cmd::mcp_outcome_report(&["files"], &[], &[], &[], &[]);
    assert_eq!(report, "1 MCP server(s) connected: files");
}

/// #2675: a contested wire name is reported with every claimant, and the
/// claimant servers stay "connected" — their uncontested tools route
/// normally, so `unavailable` would be a lie (the same reasoning as
/// truncation above).
#[test]
fn mcp_outcome_report_names_every_claimant_of_a_contested_wire_name() {
    let collisions = vec![stella_mcp::WireNameCollision {
        wire_name: "mcp__acme___status".to_string(),
        claimants: vec![
            ("acme_".to_string(), "status".to_string()),
            ("acme".to_string(), "_status".to_string()),
        ],
    }];
    let report = crate::mcp_cmd::mcp_outcome_report(&["acme_", "acme"], &[], &[], &[], &collisions);
    assert!(report.contains("`mcp__acme___status`"), "{report}");
    assert!(report.contains("`acme_` tool `status`"), "{report}");
    assert!(report.contains("`acme` tool `_status`"), "{report}");
    assert!(
        report.contains("every claimant dropped"),
        "the resolution is stated — no connect-order winner: {report}"
    );
    assert!(
        !report.contains("unavailable"),
        "claimant servers are connected, not down: {report}"
    );
}

/// Drive [`DeckAskUserIo::prompt`] with a scripted answer and inspect the
/// Inbound stream it produces. The answer is sent only AFTER the AskUser
/// card appears: `prompt` drains stale answers before presenting (the
/// cancelled-turn contract), so a pre-sent answer would be swallowed and
/// the await would hang.
async fn run_prompt(options: &[&str], answer: &str) -> (Result<String, String>, Vec<Inbound>) {
    let (in_tx, mut in_rx) = mpsc::unbounded_channel();
    let (ans_tx, ans_rx) = mpsc::unbounded_channel();
    let io = super::mid_turn_ask::DeckAskUserIo {
        agent: "lead".into(),
        inbound: in_tx,
        answers: Arc::new(tokio::sync::Mutex::new(ans_rx)),
    };
    let opts: Vec<String> = options.iter().map(|s| s.to_string()).collect();
    let asking = tokio::spawn(async move { io.prompt("which one?", &opts).await });
    let mut seen = Vec::new();
    seen.push(in_rx.recv().await.expect("the AskUser card is presented"));
    ans_tx.send(answer.to_string()).unwrap();
    let result = asking.await.expect("the prompt task settles");
    while let Ok(inbound) = in_rx.try_recv() {
        seen.push(inbound);
    }
    (result, seen)
}

#[tokio::test]
async fn deck_ask_io_strips_the_free_text_option_and_maps_answers_to_indices() {
    let free = format!("{FREE_TEXT_LABEL}…");
    let (result, seen) = run_prompt(&["postgres", "sqlite", free.as_str()], "sqlite").await;
    // The picked option maps to its 1-based index, the shape
    // the numeric quick-pick expects.
    assert_eq!(result.unwrap(), "2");
    match &seen[0] {
        Inbound::Event {
            event: AgentEvent::AskUser { options, .. },
            ..
        } => {
            assert_eq!(options, &vec!["postgres".to_string(), "sqlite".to_string()]);
        }
        other => panic!("expected the AskUser card first, got {other:?}"),
    }
}

#[tokio::test]
async fn deck_ask_io_echoes_the_clearing_tool_result_with_the_card_id() {
    let (_, seen) = run_prompt(&["a", "b"], "b").await;
    let card_id = match &seen[0] {
        Inbound::Event {
            event: AgentEvent::AskUser { id, .. },
            ..
        } => id.clone(),
        other => panic!("expected AskUser, got {other:?}"),
    };
    match &seen[1] {
        Inbound::Event {
            event: AgentEvent::ToolResult {
                call_id, output, ..
            },
            ..
        } => {
            assert_eq!(*call_id, card_id, "the echo clears the exact card");
            assert!(!output.is_error());
        }
        other => panic!("expected the echoed ToolResult, got {other:?}"),
    }
}

#[tokio::test]
async fn deck_ask_io_passes_free_text_through_verbatim() {
    let (result, _) = run_prompt(&["a", "b"], "actually do it my way").await;
    assert_eq!(result.unwrap(), "actually do it my way");
}

// Double-Esc hold

/// Single Esc: the plain cancel retains the prompt but never parks
/// dispatch — "interrupt current, run next" is unchanged.
#[test]
fn plain_cancel_retains_without_holding() {
    let mut dispatch = HoldState::new();
    dispatch.cancelled("prompt a");
    assert!(!dispatch.held(), "single Esc must not park dispatch");
}

/// The pair with an empty backlog: the escalation lands at the idle recv
/// (its `Stop` was consumed first — the channel is FIFO), and must still
/// requeue the prompt that cancel dropped and park dispatch. This is the
/// sequence that used to fall into the stray-input arm and vanish.
#[test]
fn stop_and_hold_requeues_the_prompt_the_first_esc_cancelled() {
    let mut dispatch = HoldState::new();
    dispatch.cancelled("prompt a");
    assert_eq!(dispatch.stop_and_hold(None), vec!["prompt a".to_string()]);
    assert!(dispatch.held(), "double Esc parks dispatch");
    // The retention was consumed: a re-sent escalation has nothing more
    // to requeue.
    assert!(dispatch.stop_and_hold(None).is_empty());
}

/// The pair with a backlog: the gap between its two messages is where
/// the driver auto-dispatches the next queued prompt, so the escalation
/// cancels THAT turn. Both prompts return — the retained one in front of
/// the auto-dispatched one (push order is front-most last), the order
/// the user last saw.
#[test]
fn stop_and_hold_restores_the_backlog_order_the_user_saw() {
    let mut dispatch = HoldState::new();
    dispatch.cancelled("prompt a"); // first Esc: A dropped, B dispatched
    assert_eq!(
        dispatch.stop_and_hold(Some("prompt b")), // second Esc during B
        vec!["prompt b".to_string(), "prompt a".to_string()],
    );
    assert!(dispatch.held());
}

/// A submission releases the hold, and each plain cancel replaces the
/// retention — the escalation only ever requeues its own pair's prompt.
#[test]
fn release_and_overwrite_scope_retention_to_the_latest_pair() {
    let mut dispatch = HoldState::new();
    dispatch.cancelled("stale");
    dispatch.cancelled("fresh");
    assert_eq!(dispatch.stop_and_hold(None), vec!["fresh".to_string()]);
    dispatch.release();
    assert!(!dispatch.held(), "the next submission releases the hold");
}

/// A stray escalation with nothing retained and nothing in flight stays
/// the documented no-op — nothing to requeue, nothing to hold.
#[test]
fn stray_stop_and_hold_is_a_no_op() {
    let mut dispatch = HoldState::new();
    assert!(dispatch.stop_and_hold(None).is_empty());
    assert!(!dispatch.held());
}

// ISSUES tab: entity-hit assembly

#[test]
fn agent_entity_hits_filter_by_name_or_description_case_insensitively() {
    let entries = vec![
        stella_tui::InstalledAgentEntry {
            name: "reviewer".into(),
            description: "Reviews diffs".into(),
            tools: None,
            scope: AgentScope::Project,
            source_path: String::new(),
            version: 1,
            versions: vec![],
            content: String::new(),
        },
        stella_tui::InstalledAgentEntry {
            name: "planner".into(),
            description: "Plans work".into(),
            tools: None,
            scope: AgentScope::User,
            source_path: String::new(),
            version: 1,
            versions: vec![],
            content: String::new(),
        },
    ];
    let hits = agent_entity_hits(&entries, "REVIEW");
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].kind, "Agent");
    assert_eq!(hits[0].insert, "reviewer");
    // Description text matches too; the empty query matches all.
    assert_eq!(agent_entity_hits(&entries, "plans")[0].label, "planner");
    assert_eq!(agent_entity_hits(&entries, "").len(), 2);
}

#[test]
fn memory_hits_carry_the_preview_provenance_and_citation_suffixes() {
    let hit = memory_hit(
        "naming-convention",
        "Prefer kebab-case for  skill names\nand slugs.",
        "2026-07-01T00:00:00Z",
        Some((12, 0.9)),
    );
    assert_eq!(hit.kind, "Memory");
    assert_eq!(hit.insert, "naming-convention");
    assert_eq!(
        hit.description,
        "Prefer kebab-case for skill names and slugs. · observed \
         2026-07-01T00:00:00Z · cited 12× avg 0.9"
    );
    // Observation time is the only time a node carries: no `valid from`
    // clause restating it (#3136).
    assert!(!hit.description.contains("valid from"));

    // No citations → no suffix; a long content truncates char-safe with an
    // ellipsis.
    let long = "x".repeat(200);
    let hit = memory_hit("m", &long, "2026-07-01", None);
    assert!(
        hit.description
            .starts_with(&"x".repeat(MEMORY_PREVIEW_CHARS - 1))
    );
    assert!(hit.description.ends_with("… · observed 2026-07-01"));
    assert!(!hit.description.contains("cited"));
}

#[test]
fn symbol_hits_take_the_bare_name_and_the_file_location() {
    let frame = contextgraph_types::ContextFrame {
        id: "code-graph:sym:src/lib.rs:12:issue_row".into(),
        kind: contextgraph_types::FrameKind::Symbol,
        title: "fn issue_row".into(),
        content: Some("fn issue_row(...) { ... }".into()),
        uri: Some("file:///repo/src/lib.rs".into()),
        score: 0.9,
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
        citation_label: Some("fn issue_row (src/lib.rs:12)".into()),
        embedding: None,
        relations: vec![],
    };
    let hit = symbol_hit(&frame);
    assert_eq!(hit.kind, "Symbol");
    assert_eq!(hit.label, "fn issue_row");
    assert_eq!(hit.insert, "issue_row", "the bare name is what inserts");
    assert_eq!(hit.description, "src/lib.rs:12");

    // Without a citation label the frame's uri stands in.
    let mut bare = frame;
    bare.citation_label = None;
    assert_eq!(symbol_hit(&bare).description, "file:///repo/src/lib.rs");
}

#[test]
fn merge_assignee_hits_orders_agents_then_local_and_caps() {
    let person = |l: &str| EntityHit {
        kind: "Person".into(),
        label: l.into(),
        description: String::new(),
        insert: l.into(),
    };
    let agents: Vec<EntityHit> = (0..2).map(|i| person(&format!("a{i}"))).collect();
    let local: Vec<EntityHit> = (0..3).map(|i| person(&format!("m{i}"))).collect();
    let merged = merge_assignee_hits(agents, local, 4);
    let labels: Vec<&str> = merged.iter().map(|h| h.label.as_str()).collect();
    assert_eq!(
        labels,
        vec!["a0", "a1", "m0", "m1"],
        "agents first, then local — capped"
    );
}

#[test]
fn local_assignee_hits_read_as_empty_on_a_bare_workspace() {
    // Read-only politeness: no `.stella/` databases → no hits and, above
    // all, no directories/files created as a side effect.
    let dir = tempfile::tempdir().unwrap();
    assert!(local_assignee_hits(dir.path(), "anything").is_empty());
    assert!(
        !dir.path().join(".stella").exists(),
        "a lookup must never create the workspace store"
    );
}

/// `requeue_front` front-inserts in push order and mirrors every insert
/// to the deck as `PromptRequeued`, so the driver's backlog and the
/// deck's queue view (which front-inserts each mirror in turn) agree.
#[test]
fn requeue_front_mirrors_each_front_insert_to_the_deck() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let dir = std::env::temp_dir().join(format!("stella-requeue-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut queue = crate::session_persist::DurableQueue::fresh(dir.clone());
    queue.push_back("c".to_string());
    requeue_front(&mut queue, &tx, vec!["b".to_string(), "a".to_string()]);
    // The backlog is durable + write-through: the authoritative order is
    // ON DISK the moment the inserts return.
    assert_eq!(queue.len(), 3);
    assert_eq!(
        stella_store::journal::read_queue(&dir),
        vec!["a".to_string(), "b".to_string(), "c".to_string()]
    );
    let _ = std::fs::remove_dir_all(&dir);
    for expected in ["b", "a"] {
        match rx.try_recv() {
            Ok(Inbound::PromptRequeued { agent, text }) => {
                assert_eq!(agent, LEAD);
                assert_eq!(text, expected);
            }
            other => panic!("expected PromptRequeued({expected}), got {other:?}"),
        }
    }
    assert!(rx.try_recv().is_err(), "exactly one mirror per insert");
}

/// `AgentEvent` has no system-notice variant, so a driver message goes out as
/// `Text` — which renders on the AGENT rail. Unmarked, the transcript would be
/// asserting that the model said "conversation cleared". The rail glyph is a
/// visual distinction, and accessible mode (#1258) is exactly the surface where
/// visual distinctions are the ones least likely to land.
#[test]
fn a_chrome_note_is_marked_as_the_program_speaking() {
    let Inbound::Event {
        agent,
        event: AgentEvent::Text { text: delta },
    } = chrome_note("conversation cleared".into())
    else {
        panic!("chrome rides the transcript as Text on the lead lane");
    };
    assert_eq!(agent, LEAD);
    assert!(
        delta.starts_with(stella_tui::NOTICE_MARKER),
        "an unmarked note reads as model speech: {delta:?}"
    );
    assert!(delta.contains("conversation cleared"));
}

/// The marker is idempotent: a caller that already spoke in the program's
/// voice must not end up double-marked.
#[test]
fn an_already_marked_note_is_not_marked_twice() {
    let Inbound::Event {
        event: AgentEvent::Text { text: delta },
        ..
    } = chrome_note(format!("{}already mine", stella_tui::NOTICE_MARKER))
    else {
        panic!("chrome rides the transcript as Text");
    };
    assert_eq!(delta, format!("{}already mine", stella_tui::NOTICE_MARKER));
}

/// The deck's Esc-steer must reach the running turn. `WorkspaceInput::Steer`
/// already carries the intent, and the deck strips the `>` marker off every
/// text before sending — so re-deriving the route from the text is how a live
/// steer used to fall through to a sidecar sub-session (#4025).
#[test]
fn steer_lead_pushes_a_running_turn_into_the_tap_in_order() {
    use stella_core::ports::TurnSteering;

    let dir = tempfile::tempdir().unwrap();
    let mut queue = crate::session_persist::DurableQueue::fresh(dir.path().to_path_buf());
    let tap = subsession::SteeringTap::default();

    steer::steer_lead(
        &tap,
        &mut queue,
        vec![
            "narrow it to the parser".to_string(),
            "and add a test".to_string(),
        ],
    );

    assert_eq!(
        tap.drain_steering(),
        vec![
            "narrow it to the parser".to_string(),
            "and add a test".to_string()
        ],
        "a live steer belongs to the running turn, in the order it was written"
    );
    assert!(
        queue.is_empty(),
        "nothing may reach the backlog while the turn still has a boundary to steer at"
    );
}

/// **The witness for #2899.** A steer aimed at a worker lane lands on that
/// lane's tap — the one its engine drains — claimed out of the backlog and
/// never re-parked. Before this the driver held no handle to a worker's tap
/// and the words went back to the lead's backlog, so steering a drilled-into
/// lane was a queued prompt for a different agent.
#[test]
fn steer_worker_lands_on_the_lanes_own_tap() {
    use stella_core::ports::TurnSteering;

    let dir = tempfile::tempdir().unwrap();
    let mut queue = crate::session_persist::DurableQueue::fresh(dir.path().to_path_buf());
    queue.push_back("> narrow it to the parser".to_string());
    let mut subs = subsession::SubSessions::new();
    subs.started_for_test("req:1");
    let tap = subs.tap_for_test("req:1").expect("a live lane has a tap");
    let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel();

    steer::steer_worker(
        &subs,
        &mut queue,
        "req:1",
        vec![
            "narrow it to the parser".to_string(),
            "and add a test".to_string(),
        ],
        &in_tx,
    );

    assert_eq!(
        tap.drain_steering(),
        vec![
            "narrow it to the parser".to_string(),
            "and add a test".to_string()
        ]
    );
    assert!(queue.is_empty(), "delivered means claimed, not re-parked");
    assert!(
        in_rx.try_recv().is_err(),
        "nothing to report: all delivered"
    );

    // A lane with no worker behind it cannot take the words: they go to the
    // backlog and the deck is told.
    steer::steer_worker(
        &subs,
        &mut queue,
        "req:9",
        vec!["for a lane that ended".to_string()],
        &in_tx,
    );
    assert_eq!(queue.len(), 1);
    assert!(matches!(
        in_rx.try_recv(),
        Ok(Inbound::Event { event: AgentEvent::Text { text }, .. }) if text.contains("req:9")
    ));
}

/// At the idle arm a steer at a worker is delivered and the lead stays idle;
/// a steer at the lead becomes its next turn.
#[test]
fn steer_idle_routes_a_worker_steer_to_the_lane_and_a_lead_steer_to_the_next_turn() {
    let dir = tempfile::tempdir().unwrap();
    let mut queue = crate::session_persist::DurableQueue::fresh(dir.path().to_path_buf());
    let mut subs = subsession::SubSessions::new();
    subs.started_for_test("sub:2");
    let (in_tx, _in_rx) = tokio::sync::mpsc::unbounded_channel();

    assert_eq!(
        steer::steer_idle("sub:2", &subs, &mut queue, vec!["go on".into()], &in_tx),
        None,
        "the lead has nothing to run"
    );
    assert_eq!(
        steer::steer_idle(LEAD, &subs, &mut queue, vec!["next".into()], &in_tx),
        Some("next".to_string())
    );
}

/// **The witness for Esc-with-a-backlog.** A plain stop at the lead with
/// prompts parked in the driver's queue delivers them into the turn — in
/// order, claimed out of the backlog, and the turn is *not* soft-stopped. The
/// deck cannot see this backlog (restored or held prompts), so it sends a
/// stop; before this the stop truncated the turn and auto-dispatched the
/// first parked prompt into a model that had lost its context.
#[test]
fn a_stop_with_prompts_parked_steers_them_instead_of_stopping() {
    use stella_core::ports::TurnSteering;

    let dir = tempfile::tempdir().unwrap();
    let mut queue = crate::session_persist::DurableQueue::fresh(dir.path().to_path_buf());
    queue.push_back("narrow it to the parser".to_string());
    queue.push_back("> and add a test".to_string());
    let tap = subsession::SteeringTap::default();
    let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel();

    assert!(steer::stop_steers_backlog(&tap, &mut queue, &in_tx));
    assert_eq!(
        tap.drain_steering(),
        vec![
            "narrow it to the parser".to_string(),
            "and add a test".to_string()
        ],
        "the backlog reaches the running turn, in order, markers stripped"
    );
    assert!(
        !tap.soft_stop_requested(),
        "delivering the backlog must not also stop the turn"
    );
    assert!(
        queue.is_empty(),
        "delivered prompts leave the backlog (#4026)"
    );
    let Some(Inbound::Event {
        event: AgentEvent::Text { text },
        ..
    }) = in_rx.try_recv().ok()
    else {
        panic!("the deck is told what the key did");
    };
    assert!(text.contains("steering 2 queued prompts"), "{text}");
}

/// And with nothing parked it is a stop — the caller runs its soft stop.
#[test]
fn a_stop_with_an_empty_backlog_is_a_stop() {
    let dir = tempfile::tempdir().unwrap();
    let mut queue = crate::session_persist::DurableQueue::fresh(dir.path().to_path_buf());
    let tap = subsession::SteeringTap::default();
    let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel();
    assert!(!steer::stop_steers_backlog(&tap, &mut queue, &in_tx));
    assert!(in_rx.try_recv().is_err(), "nothing to say, nothing said");
}

/// A settling turn is past its last model step, so it has no boundary left to
/// inject at. The texts continue the thread as the next turn instead — the one
/// case `steer_lead` still has to fall back for.
#[test]
fn steer_lead_falls_back_to_the_queue_once_the_turn_is_settling() {
    use stella_core::ports::TurnSteering;

    let dir = tempfile::tempdir().unwrap();
    let mut queue = crate::session_persist::DurableQueue::fresh(dir.path().to_path_buf());
    let tap = subsession::SteeringTap::default();
    tap.mark_settling();

    steer::steer_lead(
        &tap,
        &mut queue,
        vec!["first".to_string(), "second".to_string()],
    );

    assert!(
        tap.drain_steering().is_empty(),
        "a settling turn has no boundary left to steer at"
    );
    assert_eq!(
        stella_store::journal::read_queue(dir.path()),
        vec!["first".to_string(), "second".to_string()],
        "the backlog keeps the order the batch was written in"
    );
}

/// The backlog lives in two places — the deck's mirror and the driver's
/// `DurableQueue` — and the deck clears its copy as it sends the steer. So a
/// held backlog that is handed to the turn has to leave the driver's copy too,
/// or it dispatches a second time and `queue.json` records the doubled order
/// for the next `stella resume` to replay (#4026).
#[test]
fn steer_lead_claims_each_delivered_prompt_out_of_the_durable_queue() {
    use stella_core::ports::TurnSteering;

    let dir = tempfile::tempdir().unwrap();
    let mut queue = crate::session_persist::DurableQueue::fresh(dir.path().to_path_buf());
    queue.push_back("a".to_string());
    // The deck strips the `>` marker on its way out, so a queued
    // `> fix the tests` arrives here markerless and must still match.
    queue.push_back("> fix the tests".to_string());
    let tap = subsession::SteeringTap::default();

    steer::steer_lead(
        &tap,
        &mut queue,
        vec![
            "a".to_string(),
            "fix the tests".to_string(),
            "and the draft".to_string(),
        ],
    );

    assert_eq!(
        tap.drain_steering(),
        vec![
            "a".to_string(),
            "fix the tests".to_string(),
            "and the draft".to_string()
        ],
        "every delivered text belongs to the running turn"
    );
    assert!(
        queue.is_empty(),
        "a prompt handed to the turn must not stay parked for a second dispatch"
    );
    assert!(
        stella_store::journal::read_queue(dir.path()).is_empty(),
        "the claim is write-through, or a resume replays the backlog"
    );
}

/// The sharpest repro: a double-Esc hold parks the backlog, then an Esc-steer
/// hands all of it back. The first text runs now, the tail is front-inserted —
/// on top of the very entries it was just handed, unless each one is claimed
/// out of the driver's copy first (#4026).
#[test]
fn esc_steer_at_rest_claims_each_delivered_prompt_out_of_the_durable_queue() {
    let dir = tempfile::tempdir().unwrap();
    let mut queue = crate::session_persist::DurableQueue::fresh(dir.path().to_path_buf());
    queue.push_back("a".to_string());
    queue.push_back("b".to_string());

    let first = steer::steer_at_rest(
        &mut queue,
        vec!["a".to_string(), "b".to_string(), "draft".to_string()],
    );

    assert_eq!(first, Some("a".to_string()), "the head runs now");
    assert_eq!(
        queue.len(),
        2,
        "the tail plus the draft — never a second copy of the held backlog"
    );
    assert_eq!(
        stella_store::journal::read_queue(dir.path()),
        vec!["b".to_string(), "draft".to_string()],
        "the composer draft was never queued, so it claims nothing and simply rides along"
    );
}

/// The driver's backlog is not a superset of the deck's mirror: a prompt
/// restored from a previous session sits in `DurableQueue` without the deck
/// ever having drawn it. Claiming by text is what keeps it — `queue.clear()`
/// would silently destroy work the user queued in an earlier session (#4026).
#[test]
fn a_restored_backlog_entry_the_deck_never_mirrored_survives_an_esc_steer() {
    let dir = tempfile::tempdir().unwrap();
    let mut queue = crate::session_persist::DurableQueue::fresh(dir.path().to_path_buf());
    queue.adopt(
        dir.path().to_path_buf(),
        vec!["restored-only".to_string(), "a".to_string()],
    );

    let first = steer::steer_at_rest(&mut queue, vec!["a".to_string()]);

    assert_eq!(first, Some("a".to_string()));
    assert_eq!(
        stella_store::journal::read_queue(dir.path()),
        vec!["restored-only".to_string()],
        "the delivered prompt is claimed; the entry the deck never showed stays parked"
    );
}

/// **The guard (#4338).** Every command the palette's relevance rule can
/// promote is a command this deck actually offers.
///
/// The rule lives in `stella-tui` and names commands by string, because
/// "`/plan` matters while a turn runs" is a fact about the vocabulary, not
/// about a data structure. That is a second copy of those names, so it is
/// held to the same discipline as the keymap's witness table: a name in the
/// rule that no command answers to is a row the palette would silently never
/// promote, and it fails here rather than going unnoticed on screen.
#[test]
fn every_command_a_relevance_rule_can_name_is_a_real_one() {
    let vocabulary: Vec<&str> = super::skills::DECK_BUILTINS
        .iter()
        .map(|(name, ..)| *name)
        .collect();
    for name in stella_tui::composer::palette::rule_command_names() {
        assert!(
            vocabulary.contains(&name),
            "the palette's relevance rule names `{name}`, which the deck does not offer: \
             {vocabulary:?}"
        );
    }
}
