use super::*;
use crate::config::{ConfiguredProvider, PROVIDERS, ProviderConfig};
use stella_model::credential::ApiKey;
use stella_protocol::event::BudgetMode; // no longer re-exported via `super::*` (#971)

#[test]
fn one_shot_reflection_defaults_on_for_every_output_format() {
    let _env = crate::test_env::lock();
    let _restore = crate::test_env::EnvRestore::capture(&[DISABLE_REFLECTION_ENV]);

    assert!(one_shot_reflection_enabled(OutputFormat::Text));
    assert!(one_shot_reflection_enabled(OutputFormat::Json));
    assert!(one_shot_reflection_enabled(OutputFormat::StreamJson));
}

#[test]
fn explicit_reflection_opt_out_suppresses_every_one_shot_format() {
    let _env = crate::test_env::lock();
    let _restore = crate::test_env::EnvRestore::capture(&[DISABLE_REFLECTION_ENV]);
    unsafe { std::env::set_var(DISABLE_REFLECTION_ENV, "  YeS  ") };

    assert!(!one_shot_reflection_enabled(OutputFormat::Text));
    assert!(!one_shot_reflection_enabled(OutputFormat::Json));
    assert!(!one_shot_reflection_enabled(OutputFormat::StreamJson));
}

#[test]
fn reflection_opt_out_uses_explicit_truthy_values() {
    for value in ["1", "true", "TRUE", " yes ", "On"] {
        assert!(is_truthy_env_value(value), "{value:?} should be truthy");
    }
    for value in ["", "0", "false", "no", "off", "disabled", "2"] {
        assert!(!is_truthy_env_value(value), "{value:?} should be falsey");
    }
}

/// Bare `/rename` and `/color` are reserved names `expand` never claims, so
/// without a local usage answer they would fall through to a paid model
/// turn. Regression: the REPL must answer them locally, and must NOT claim
/// the argument-carrying forms the real handlers own.
#[test]
fn bare_rename_and_color_get_a_local_usage_line() {
    assert_eq!(
        bare_local_command_usage("/rename"),
        Some("usage: /rename <name>")
    );
    assert_eq!(
        bare_local_command_usage("/color"),
        Some("usage: /color <name>")
    );
    // Whitespace-only arguments are the bare form too (defensive: the REPL
    // trims input before dispatch, so these normally arrive pre-collapsed).
    assert_eq!(
        bare_local_command_usage("/rename \t "),
        Some("usage: /rename <name>")
    );
    // Argument-carrying forms stay with the real handlers…
    assert_eq!(bare_local_command_usage("/rename new-name"), None);
    assert_eq!(bare_local_command_usage("/color amber"), None);
    // …and everything else is not this seam's business.
    assert_eq!(bare_local_command_usage("/goal"), None);
    assert_eq!(bare_local_command_usage("hello"), None);
}

/// The store write path for `StepUsage`: every token field on the event
/// — cache writes included — lands in the telemetry row verbatim.
/// Regression for issue #97, where `cache_write_tokens` was hard-coded
/// to 0 at this exact seam while the schema and `stella stats` already
/// carried the column.
#[test]
fn persist_event_records_cache_write_tokens_from_step_usage() {
    let store = Store::in_memory().expect("in-memory store");
    let execution_id = store
        .begin_execution("run", "prompt", "anthropic", "claude-fable-5")
        .expect("begin execution");
    let event = AgentEvent::StepUsage {
        upstream_provider: None,
        reasoning_tokens: None,
        output_text: None,
        step: 0,
        role: stella_protocol::ModelCallRole::Worker,
        provider: "anthropic".into(),
        model: "claude-fable-5".into(),
        input_tokens: 1_000,
        output_tokens: 50,
        cached_input_tokens: 900,
        cache_write_tokens: 640,
        estimated_input_tokens: 980,
        cost_usd: 0.0042,
        duration_ms: 1_830,
        retries: 0,
        tool_calls: 1,
        complete: true,
        finish_reason: None,
    };

    assert!(persist_event(&store, execution_id, 0, &event, "anthropic"));
    store
        .finish_execution(execution_id, "completed", 0.0042)
        .expect("finish execution");

    let rows = store.usage_stats().expect("usage stats");
    let row = rows
        .iter()
        .find(|r| r.provider == "anthropic")
        .expect("anthropic row");
    assert_eq!(row.input_tokens, 1_000);
    assert_eq!(row.output_tokens, 50);
    assert_eq!(row.cache_read_tokens, 900);
    assert_eq!(
        row.cache_write_tokens, 640,
        "the event's cache-write count must reach the store, never a hard-coded 0"
    );
}

/// Build a real code-graph index in a tempdir: `hub.rs` (three symbols) is
/// busiest, `leaf.rs` (one) is not. Returns the workspace root tempdir.
fn graph_fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        root.path().join("hub.rs"),
        "pub fn a() {}\npub fn b() {}\npub struct C;\n",
    )
    .unwrap();
    std::fs::write(root.path().join("leaf.rs"), "pub fn d() {}\n").unwrap();
    let db = stella_store::workspace_private_sqlite_path(root.path(), "codegraph.db").unwrap();
    let graph = stella_graph::CodeGraph::open(root.path(), &db).expect("open graph");
    graph.index_all().expect("index");
    graph.shutdown();
    root
}

/// The default snapshot roots on the busiest file and carries the full,
/// sorted file list the deck's picker browses — sourced straight from the
/// graph store, a superset of the rooted neighborhood.
#[test]
fn graph_snapshot_defaults_to_the_busiest_file_and_lists_all_files() {
    let root = graph_fixture();
    let snap = graph_snapshot(root.path()).expect("snapshot");
    assert_eq!(snap.focus, "hub.rs", "default focus is the busiest file");
    assert_eq!(
        snap.files,
        vec!["hub.rs".to_string(), "leaf.rs".to_string()],
        "the picker's file list is every indexed file, sorted"
    );
}

/// An explicit focus re-roots the neighborhood on that file — the picker's
/// selection path — while still shipping the same browsable file list.
#[test]
fn graph_snapshot_focus_re_roots_on_the_requested_file() {
    let root = graph_fixture();
    let snap = graph_snapshot_focus(root.path(), Some("leaf.rs")).expect("snapshot");
    assert_eq!(snap.focus, "leaf.rs", "re-rooted on the requested file");
    assert!(
        snap.nodes.iter().any(|n| n.label == "leaf.rs"),
        "the neighborhood is centered on leaf.rs, not the busiest file"
    );
    assert!(snap.files.contains(&"hub.rs".to_string()));
}

/// No index → no snapshot (the tab shows its "run stella init" hint).
#[test]
fn graph_snapshot_is_none_without_an_index() {
    let root = tempfile::tempdir().expect("tempdir");
    assert!(graph_snapshot(root.path()).is_none());
    assert!(graph_snapshot_focus(root.path(), Some("x.rs")).is_none());
}

/// Whether the workspace's code-graph index exists on disk — the fact the
/// session builder promises, read through the same resolver production uses.
fn graph_index_exists(root: &std::path::Path) -> bool {
    stella_store::existing_workspace_private_sqlite_path(root, "codegraph.db")
        .ok()
        .flatten()
        .is_some_and(|db| db.exists())
}

/// The files the index currently covers, read through the real graph store.
fn graph_indexed_files(root: &std::path::Path) -> Vec<String> {
    let Some(db) = stella_store::existing_workspace_private_sqlite_path(root, "codegraph.db")
        .ok()
        .flatten()
    else {
        return Vec::new();
    };
    let Ok(graph) = stella_graph::CodeGraph::open(root, &db) else {
        return Vec::new();
    };
    let files = graph.all_files().unwrap_or_default();
    graph.shutdown();
    files
}

/// Auto-build on session start (task part A): a workspace with a source
/// file. [`spawn_session_graph`] builds `.stella/private/codegraph.db`
/// EAGERLY in the background, so `stella search` and the deck's Graph tab
/// harvest a ready index instead of paying the build cost inline. Awaiting
/// the returned handle is the deterministic "index ready" signal.
#[tokio::test]
async fn spawn_session_graph_eagerly_builds_the_index_in_the_background() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("lib.rs"), "pub fn find_me() {}\n").unwrap();

    assert!(!graph_index_exists(&root), "no index on disk yet");

    let (session_graph, build) = spawn_session_graph(&root, Box::new(|_| {}), Box::new(|| {}));
    build.await.expect("background build task");

    assert!(
        graph_index_exists(&root),
        "the background build must create .stella/private/codegraph.db"
    );
    assert!(
        graph_indexed_files(&root)
            .iter()
            .any(|file| file.ends_with("lib.rs")),
        "the built index covers the workspace's source"
    );
    session_graph.shutdown();
}

/// Live freshness (task part B): after the session graph is up, a
/// brand-new source file the agent (or an external tool) writes is
/// incrementally re-indexed by the live `notify` watcher, so the very next
/// query reflects it. Polls with a generous budget because the OS watcher
/// + debounce are asynchronous, and re-writes the file each iteration so a
/// create event lost during the watcher's async arming window is retried
/// (the un-indexed file re-parses on the first event that lands).
#[tokio::test]
async fn session_graph_live_refreshes_after_a_file_is_added() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("lib.rs"), "pub fn original() {}\n").unwrap();

    let (session_graph, build) = spawn_session_graph(&root, Box::new(|_| {}), Box::new(|| {}));
    build.await.expect("background build task");

    assert!(
        !graph_indexed_files(&root)
            .iter()
            .any(|file| file.ends_with("added.rs")),
        "the new file must not be indexed yet"
    );

    let added = root.join("added.rs");
    let mut reflected = false;
    for _ in 0..150 {
        std::fs::write(&added, "pub fn added_later() {}\n").unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        if graph_indexed_files(&root)
            .iter()
            .any(|file| file.ends_with("added.rs"))
        {
            reflected = true;
            break;
        }
    }
    assert!(
        reflected,
        "the live watcher must re-index the new file so queries reflect it"
    );
    session_graph.shutdown();
}

/// Tier-1 rule wiring (issue #103): a workspace rule renders into the
/// assembled system prompt, appended after the untouched base prefix.
#[test]
fn system_prompt_carries_the_workspace_rules_section() {
    let root = tempfile::tempdir().expect("tempdir");
    let rules_dir = root.path().join(".stella/rules");
    std::fs::create_dir_all(&rules_dir).unwrap();
    std::fs::write(
        rules_dir.join("no-force-push.md"),
        "---\nguard-tool: Bash\nguard-deny-command: git push --force*\n---\nNever force-push.",
    )
    .unwrap();

    let mut cfg = cfg_for("zai");
    cfg.authority.project_prompts_allowed = true;
    let rules = crate::rules::load_workspace_rules(root.path(), &cfg.authority);
    let prompt = build_system_prompt(&cfg, root.path(), &rules);
    assert!(
        prompt.starts_with(SYSTEM_PROMPT),
        "rules append to the prompt; the base prefix must stay intact"
    );
    assert!(prompt.contains("## Workspace rules"));
    assert!(
        prompt.contains("### Must"),
        "records group by force so a fact and a constraint stop looking identical: {prompt}"
    );
    assert!(
        prompt.contains("- Never force-push. ^no-force-push [enforced]"),
        "a guarded rule must render with its citable handle and the enforced marker: {prompt}"
    );
}

/// An untrusted checkout must not steer through the TOML record surface either.
///
/// The markdown half of this is covered by
/// `untrusted_project_prompt_sources_are_absent_from_the_system_prompt`. This is the
/// record half, and it is a separate test because the two formats reach the prompt
/// through different code: the record path filters the rule directories itself, and
/// an early version of it collapsed "read the user's rules" and "read the
/// repository's rules" into one flag — which loaded an untrusted repository's
/// records into the privileged prompt while every markdown test still passed.
#[test]
fn an_untrusted_project_record_is_absent_from_the_system_prompt() {
    let root = tempfile::tempdir().expect("tempdir");
    let rules_dir = root.path().join(".stella/rules");
    std::fs::create_dir_all(&rules_dir).unwrap();
    std::fs::write(
        rules_dir.join("acme.web.toml"),
        "schema = \"context-record/v0.1\"\nset_id = \"acme.web\"\n\
         \n[[record]]\nlineage_id = \"ctx.acme.web.marker\"\nkind = \"rule\"\n\
         statement = \"PROJECT_RECORD_AUTHORITY_MARKER\"\n\
         \n[record.steering]\nforce = \"must\"\n",
    )
    .unwrap();

    let mut cfg = cfg_for("zai");
    cfg.workspace_root = root.path().to_path_buf();
    cfg.authority.project_prompts_allowed = false;
    let untrusted = crate::rules::load_workspace_rules(root.path(), &cfg.authority);
    let prompt = build_system_prompt(&cfg, root.path(), &untrusted);
    assert!(
        !prompt.contains("PROJECT_RECORD_AUTHORITY_MARKER"),
        "an untrusted project record reached the privileged system prompt: {prompt}"
    );
    assert!(
        untrusted.registry().entries.is_empty(),
        "and it must not even be loaded — a record that loads is a record that can arm a guard"
    );

    cfg.authority.project_prompts_allowed = true;
    let trusted = crate::rules::load_workspace_rules(root.path(), &cfg.authority);
    assert!(
        build_system_prompt(&cfg, root.path(), &trusted)
            .contains("PROJECT_RECORD_AUTHORITY_MARKER"),
        "explicit repository trust must restore it"
    );
}

/// An untrusted checkout cannot append repository-authored content to the
/// privileged system prompt. Explicit repository trust restores those
/// sources within the already-computed managed ceiling.
#[test]
fn untrusted_project_prompt_sources_are_absent_from_the_system_prompt() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(root.path().join(".stella/memories")).unwrap();
    std::fs::write(
        root.path().join(".stella/memories/project.md"),
        "PROJECT_MEMORY_AUTHORITY_MARKER",
    )
    .unwrap();
    std::fs::create_dir_all(root.path().join(".stella/rules")).unwrap();
    std::fs::write(
        root.path().join(".stella/rules/project.md"),
        "PROJECT_RULE_AUTHORITY_MARKER",
    )
    .unwrap();

    let mut cfg = cfg_for("zai");
    cfg.workspace_root = root.path().to_path_buf();
    cfg.authority.project_prompts_allowed = false;
    let untrusted_rules = crate::rules::load_workspace_rules(root.path(), &cfg.authority);
    let untrusted = build_system_prompt(&cfg, root.path(), &untrusted_rules);
    for marker in [
        "PROJECT_MEMORY_AUTHORITY_MARKER",
        "PROJECT_RULE_AUTHORITY_MARKER",
    ] {
        assert!(
            !untrusted.contains(marker),
            "untrusted project marker reached system prompt: {marker}\n{untrusted}"
        );
    }

    cfg.authority.project_prompts_allowed = true;
    let trusted_rules = crate::rules::load_workspace_rules(root.path(), &cfg.authority);
    let trusted = build_system_prompt(&cfg, root.path(), &trusted_rules);
    for marker in [
        "PROJECT_MEMORY_AUTHORITY_MARKER",
        "PROJECT_RULE_AUTHORITY_MARKER",
    ] {
        assert!(trusted.contains(marker), "trusted marker missing: {marker}");
    }
}

/// The #639 acceptance criterion, guarded across the WHOLE prefix rather than
/// the one section that regressed.
///
/// A one-shot run assembles this prefix and pays for it; the next one-shot run
/// only hits the provider's cache if it assembles the same bytes. The exact
/// bug was a section rendering `age_human` and a live pid — so the guard is
/// that no section emits a wall-clock-relative age or this process's identity,
/// whichever section a later change adds them to.
#[test]
fn the_cached_prefix_carries_no_wall_clock_or_per_process_bytes() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(root.path().join(".stella/memories")).unwrap();
    std::fs::write(
        root.path().join(".stella/memories/lesson.md"),
        "PREFIX_MEMORY_MARKER: prefer rg here\n",
    )
    .unwrap();

    let mut cfg = cfg_for("zai");
    cfg.authority.project_prompts_allowed = true;
    let rules = crate::rules::ResolvedRules::default();
    let prompt = build_system_prompt(&cfg, root.path(), &rules);

    // The fixture must actually reach the prefix, or every assertion below
    // passes vacuously on a record that silently failed to parse.
    assert!(
        prompt.contains("PREFIX_MEMORY_MARKER"),
        "the fixture never reached the prefix — this guard would be vacuous:\n{prompt}"
    );

    for volatile in [" ago", "just now", "IN PROGRESS", "abandoned draft"] {
        assert!(
            !prompt.contains(volatile),
            "the cached prefix must carry no wall-clock-relative bytes \
             ({volatile:?}) — an unstable prefix pays the cache-WRITE premium \
             on every call instead of hitting cache (#639):\n{prompt}"
        );
    }
    assert!(
        !prompt.contains(&format!("pid {}", std::process::id())),
        "the cached prefix must not name the producing process (#639):\n{prompt}"
    );
    assert_eq!(
        prompt,
        build_system_prompt(&cfg, root.path(), &rules),
        "same workspace state ⇒ identical bytes"
    );
}

#[test]
fn the_prefix_never_promises_a_shell_it_cannot_provide() {
    // The built-in surface registers no command-executing tool, so nothing
    // here can promise one: execution arrives only as an MCP or custom tool.
    // The prompt already tells the model to "never assume a capability no
    // schema names", and an unconditional "write commands in its dialect"
    // contradicts that to its face. A real session read the shell line,
    // found no tool that could run one, and had nowhere to go (#3319).
    let root = tempfile::tempdir().expect("tempdir");
    let cfg = cfg_for("zai");
    let rules = crate::rules::ResolvedRules::default();
    let prompt = build_system_prompt(&cfg, root.path(), &rules);

    assert!(
        !prompt.contains("write commands in its dialect"),
        "the environment block must state the shell as a constant, never \
         instruct the model to write commands for a tool that may not \
         exist:\n{prompt}"
    );
    // The git-delta rule is the same promise in another voice: it names four
    // commands to run, so it has to be conditioned on being able to run one.
    if prompt.contains("git status") {
        assert!(
            prompt.contains("if a tool in your schema list can run a command"),
            "the git-delta rule prescribes commands and must say what it \
             depends on:\n{prompt}"
        );
    }
}

#[test]
fn benchmark_gate_excludes_hostile_filesystem_steering_and_extensions() {
    let workspace = tempfile::tempdir().expect("workspace");
    let home = tempfile::tempdir().expect("home");
    let root = workspace.path();
    let dot_stella = root.join(".stella");

    for (path, body) in [
        (
            dot_stella.join("memories/hostile.md"),
            "HOSTILE_WORKSPACE_MEMORY",
        ),
        (dot_stella.join("rules/hostile.md"), "HOSTILE_STELLA_RULE"),
        (
            root.join(".claude/rules/hostile-claude.md"),
            "HOSTILE_CLAUDE_RULE",
        ),
        (
            dot_stella.join("skills/hostile/SKILL.md"),
            "---\nname: hostile-workspace-skill\ndescription: hostile workspace skill\n---\nHOSTILE_WORKSPACE_SKILL",
        ),
        (
            home.path().join(".stella/rules/hostile-user.md"),
            "HOSTILE_USER_RULE",
        ),
        (
            home.path().join(".stella/skills/hostile-user/SKILL.md"),
            "---\nname: hostile-user-skill\ndescription: hostile user skill\n---\nHOSTILE_USER_SKILL",
        ),
    ] {
        std::fs::create_dir_all(path.parent().expect("fixture parent")).unwrap();
        std::fs::write(path, body).unwrap();
    }

    for (path, name) in [
        (
            dot_stella.join("tools/hostile.toml"),
            "hostile_workspace_tool",
        ),
        (
            home.path().join(".stella/tools/hostile.toml"),
            "hostile_user_tool",
        ),
    ] {
        std::fs::create_dir_all(path.parent().expect("tool fixture parent")).unwrap();
        std::fs::write(
            path,
            format!(
                "name = \"{name}\"\ndescription = \"must not load\"\ncommand = [\"sh\", \"-c\", \"exit 99\"]\n"
            ),
        )
        .unwrap();
    }

    std::fs::write(
        dot_stella.join("mcp.toml"),
        "[servers.hostile]\ntransport = \"stdio\"\ncmd = \"sh\"\nargs = [\"-c\", \"exit 98\"]\n",
    )
    .unwrap();
    let context_bytes = b"HOSTILE_CONTEXT_DB";
    std::fs::write(dot_stella.join("context.db"), context_bytes).unwrap();
    // A *real* workspace store rather than a garbage file. What this test
    // proves about `store.db` is that the isolation gate never opens it, and a
    // valid database proves that as well as an invalid one. A garbage one no
    // longer works, because the system prompt now reads this workspace's
    // workspace-memory tombstones and fails closed when it cannot (#712
    // deliverable 6) — so the hostile-memory assertion below would pass or fail
    // for a reason that has nothing to do with the isolation gate.
    let store_db = {
        drop(stella_store::Store::open(root).expect("seed workspace store"));
        stella_store::existing_workspace_private_sqlite_path(root, "store.db")
            .expect("resolve workspace store")
            .expect("workspace store exists")
    };
    let store_bytes = std::fs::read(&store_db).expect("read seeded store");

    let _home = crate::paths::test_user_home(home.path().to_path_buf());
    let isolation = crate::paths::test_filesystem_isolation(true);

    let mut cfg = cfg_for("zai");
    cfg.workspace_root = root.to_path_buf();
    cfg.authority.project_prompts_allowed = true;

    let rules = crate::rules::load_workspace_rules(root, &cfg.authority);
    let prompt = build_pipeline_system_prompt(&cfg, root, &rules, None);
    let skills = crate::memory::load_workspace_skills(root);
    let custom_tools = custom_tool_report_for_workspace(root).tools;
    let memory = SessionMemory::open(root, false);
    let store = open_store(root);
    let mcp = load_mcp_plan(&cfg);

    let registry = ToolRegistry::new(root.to_path_buf());
    let schema_names: Vec<String> = registry
        .schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect();

    assert_eq!(prompt, expected_isolated_pipeline_prompt(root));
    assert!(rules.is_empty(), "rules loaded under benchmark isolation");
    assert!(skills.is_empty(), "skills loaded under benchmark isolation");
    assert!(
        custom_tools.is_empty(),
        "custom tools loaded under benchmark isolation"
    );
    assert!(memory.is_none(), "context memory opened under isolation");
    assert!(
        store.is_none(),
        "workspace telemetry store opened under isolation"
    );
    assert!(matches!(mcp, McpPlan::None));
    for forbidden in ["hostile_workspace_tool", "hostile_user_tool"] {
        assert!(
            !schema_names.iter().any(|name| name == forbidden),
            "{forbidden} leaked into the isolated tool schema: {schema_names:?}"
        );
    }
    assert_eq!(
        std::fs::read(dot_stella.join("context.db")).unwrap(),
        context_bytes
    );
    assert_eq!(std::fs::read(&store_db).unwrap(), store_bytes);

    // Dropping only the isolation signal proves normal product behavior is
    // unchanged against the exact same workspace/user fixtures.
    drop(isolation);
    let normal_rules = crate::rules::load_workspace_rules(root, &cfg.authority);
    let normal_prompt = build_pipeline_system_prompt(&cfg, root, &normal_rules, None);
    let normal_skills = crate::memory::load_workspace_skills(root);
    let normal_custom_tools = custom_tool_report_for_workspace(root).tools;
    assert!(normal_prompt.contains("HOSTILE_WORKSPACE_MEMORY"));
    assert!(normal_prompt.contains("HOSTILE_STELLA_RULE"));
    assert!(normal_prompt.contains("HOSTILE_CLAUDE_RULE"));
    assert!(normal_prompt.contains("HOSTILE_USER_RULE"));
    assert!(!normal_rules.is_empty());
    assert_eq!(normal_skills.len(), 2);
    assert_eq!(normal_custom_tools.len(), 2);
    assert!(matches!(load_mcp_plan(&cfg), McpPlan::Invalid(_)));
}

/// A `Config` selecting `provider_id` at its default model
/// ([`Config::for_tests`] carries the shared offline defaults, including the
/// "no --model given" posture — the flag-pinned case sets that explicitly).
fn cfg_for(provider_id: &str) -> Config {
    let provider = PROVIDERS
        .iter()
        .find(|p| p.id == provider_id)
        .unwrap_or_else(|| panic!("provider `{provider_id}` not in PROVIDERS"))
        .clone();
    let model_id = provider.default_model.to_string();
    Config::for_tests(provider, model_id)
}

#[tokio::test]
async fn untrusted_project_custom_tools_are_absent_from_the_runtime_surface() {
    let workspace = tempfile::tempdir().unwrap();
    let workspace_tools = workspace.path().join(".stella/tools");
    std::fs::create_dir_all(&workspace_tools).unwrap();
    std::fs::write(
        workspace_tools.join("workspace.toml"),
        "name = \"workspace_tool\"\ndescription = \"d\"\ncommand = [\"./workspace.sh\"]",
    )
    .unwrap();
    let mut cfg = cfg_for("zai");
    cfg.workspace_root = workspace.path().to_path_buf();
    cfg.authority.project_custom_tools_allowed = false;

    let tools = discover_custom_tools(&cfg, false).await;

    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    assert!(
        !names.contains(&"workspace_tool"),
        "runtime tools: {names:?}"
    );
}

#[test]
fn non_tty_text_output_is_headless_without_losing_text_rendering() {
    let cfg = cfg_for("zai");
    let format = OutputFormat::Text;
    let worker_model = ModelRef::new(cfg.provider.id, cfg.model_id.clone());
    let non_tty = pipeline_config_for_approval_capability(
        &cfg,
        PipelineApprovalCapability::Unavailable,
        None,
        &worker_model,
    );
    assert!(
        non_tty.headless,
        "text redirected through a non-TTY host cannot prompt for approval"
    );
    assert!(
        !non_tty.headless_bypass_scope_review,
        "output serialization must never grant execution authority"
    );
    assert_eq!(format, OutputFormat::Text, "rendering remains text");

    let interactive = pipeline_config_for_approval_capability(
        &cfg,
        PipelineApprovalCapability::Stdio,
        None,
        &worker_model,
    );
    assert!(
        !interactive.headless,
        "an explicit interactive approval host retains scope review"
    );
    assert!(!interactive.headless_bypass_scope_review);
}

/// Issue: a squash-merge (#284 x #297/#276) silently dropped
/// `run_pipeline_one_shot`'s `approval_capability` computation, collapsing
/// its production call site to a bare `is_text` check with no test to catch
/// it — the helper above was already covered in isolation, but nothing
/// exercised the actual condition the call site computes. These three tests
/// pin `approval_capability_for` (the extracted, directly-testable seam)
/// against every input combination that matters.
#[test]
fn approval_capability_for_requires_both_terminal_handles_not_just_text_format() {
    // The exact regression: a redirected/piped text-format run (is_text,
    // stdout still a TTY, but stdin is NOT) must stay Unavailable — a bare
    // `is_text` check would wrongly select Stdio here and try to read an
    // approval decision from a pipe no one is at the other end of.
    assert_eq!(
        approval_capability_for(false, true, false, true),
        PipelineApprovalCapability::Unavailable,
        "text format alone must not select Stdio when stdin isn't a real terminal"
    );
    assert_eq!(
        approval_capability_for(false, true, true, false),
        PipelineApprovalCapability::Unavailable,
        "text format alone must not select Stdio when stdout isn't a real terminal"
    );
    assert_eq!(
        approval_capability_for(false, true, false, false),
        PipelineApprovalCapability::Unavailable
    );
}

#[test]
fn approval_capability_for_json_is_always_unavailable() {
    // Output serialization must never grant execution authority, regardless
    // of the terminal state — JSON output has nowhere to render a prompt.
    assert_eq!(
        approval_capability_for(false, false, true, true),
        PipelineApprovalCapability::Unavailable
    );
    assert_eq!(
        approval_capability_for(false, false, false, false),
        PipelineApprovalCapability::Unavailable
    );
}

#[test]
fn approval_capability_for_full_tty_text_is_stdio() {
    // Only the genuine interactive case — text format, real stdin, real
    // stdout — selects Stdio.
    assert_eq!(
        approval_capability_for(false, true, true, true),
        PipelineApprovalCapability::Stdio
    );
}

/// The composition gap the incident actually exploited: `approval_capability_for`
/// and `pipeline_config_for_approval_capability` were each covered above in
/// isolation, but nothing pinned them wired together the way
/// `run_pipeline_one_shot` actually wires them (agent.rs, around the
/// `pipeline_config` construction) — feeding one straight into the other. A
/// regression that breaks *that* composition (e.g. hardcoding
/// `PipelineApprovalCapability::Stdio` at the call site instead of using the
/// computed value) would pass every test above while still shipping the
/// scope-review bypass this incident (#284 x #297, fixed in #305) shipped.
#[test]
fn non_tty_text_run_wiring_stays_headless_and_json_run_wiring_never_bypasses_scope_review() {
    let cfg = cfg_for("zai");
    let model_ref = ModelRef::new(cfg.provider.id, cfg.model_id.clone());

    // A non-TTY text-format run (e.g. `stella run` piped in a script or CI)
    // must not select the interactive stdio approval gate, and its wired
    // config must stay headless.
    let text_capability = approval_capability_for(false, true, false, false);
    let text_config =
        pipeline_config_for_approval_capability(&cfg, text_capability, None, &model_ref);
    assert_ne!(
        text_capability,
        PipelineApprovalCapability::Stdio,
        "a non-tty text run must not select the interactive stdio approval gate"
    );
    assert!(
        text_config.headless,
        "a non-tty text run's wired config must stay headless"
    );

    // A JSON-format one-shot run is headless by construction — and even with
    // both terminal handles real, its wired config must never bypass scope
    // review; JSON has nowhere to render a prompt regardless of TTY state.
    let json_capability = approval_capability_for(false, false, true, true);
    let json_config =
        pipeline_config_for_approval_capability(&cfg, json_capability, None, &model_ref);
    assert!(json_config.headless);
    assert!(
        !json_config.headless_bypass_scope_review,
        "a JSON-format run's wired config must never bypass scope review"
    );
}

#[test]
fn existing_providers_still_route_to_their_current_adapter() {
    // Regression: switching the catalog check to resolve_for, the
    // (provider, id) dedup, and the inserted vertex/bedrock arms must NOT
    // change selection for any provider that worked before. `build_provider`
    // dispatches on `cfg.provider.id`: OpenAI/Anthropic/Gemini each get
    // their own native adapter, while the OpenAI-compatible gateways (xAI,
    // DeepSeek, OpenRouter) share the ZaiProvider implementation but are
    // re-identified via `with_identity`, so each adapter's `id()` is its own
    // provider name — i.e. every provider reports itself.
    for (provider_id, expected_adapter) in [
        ("openai", "openai"),
        ("anthropic", "anthropic"),
        ("zai", "zai"),
        ("xai", "xai"),
        ("deepseek", "deepseek"),
        ("gemini", "gemini"),
        ("openrouter", "openrouter"),
    ] {
        let provider = build_provider(&cfg_for(provider_id))
            .unwrap_or_else(|e| panic!("build_provider({provider_id}) failed: {e}"));
        assert_eq!(
            provider.id(),
            expected_adapter,
            "provider `{provider_id}` must still route to the `{expected_adapter}` adapter"
        );
    }
}

#[test]
fn vertex_and_bedrock_route_to_their_native_adapters_not_a_fallthrough() {
    // The new providers must construct their own native adapter (not the
    // shared ZaiProvider shim, id "zai", nor the anthropic branch). Both
    // arms read extra addressing/credentials from the environment; set
    // the minimum each requires. build_provider only constructs — no
    // network call. Env mutation is UB against concurrent getenv on
    // POSIX, so hold the binary-wide env lock for the whole
    // mutate-read-cleanup window; the missing-project error case shares
    // this test so the set/remove stays serialized.
    let _env = crate::test_env::lock();
    let _restore = crate::test_env::EnvRestore::capture(&[
        "VERTEX_PROJECT_ID",
        "GOOGLE_CLOUD_PROJECT",
        "AWS_SECRET_ACCESS_KEY",
    ]);
    unsafe {
        std::env::set_var("VERTEX_PROJECT_ID", "test-project");
        std::env::set_var("AWS_SECRET_ACCESS_KEY", "test-secret");
    }

    let vertex = build_provider(&cfg_for("vertex")).expect("vertex builds");
    assert_eq!(vertex.id(), "vertex", "vertex must route to VertexProvider");

    let bedrock = build_provider(&cfg_for("bedrock")).expect("bedrock builds");
    assert_eq!(
        bedrock.id(),
        "bedrock",
        "bedrock must route to BedrockProvider"
    );

    // A vertex selection with no project id must fail loudly with a named
    // error, never silently fall through to another adapter.
    unsafe {
        std::env::remove_var("VERTEX_PROJECT_ID");
        std::env::remove_var("GOOGLE_CLOUD_PROJECT");
    }
    // `.err()` (not `.unwrap_err()`) so the Ok type `Box<dyn Provider>`,
    // which is not `Debug`, is never required to be printed.
    let err = build_provider(&cfg_for("vertex"))
        .err()
        .expect("vertex without a project id must be an error");
    assert!(
        err.contains("VERTEX_PROJECT_ID"),
        "expected a named VERTEX_PROJECT_ID error, got: {err}"
    );
}

/// A `ConfiguredProvider` for `provider_id` at its default model with a
/// dummy key — the offline analogue of `cfg_for` for verifier routing. The
/// key is never sent anywhere: routing only constructs adapters and
/// reads `.id()`.
fn configured_provider(provider_id: &str) -> ConfiguredProvider {
    let config = PROVIDERS
        .iter()
        .find(|p| p.id == provider_id)
        .unwrap_or_else(|| panic!("provider `{provider_id}` not in PROVIDERS"))
        .clone();
    ConfiguredProvider {
        config,
        api_key: ApiKey::new("dummy-key-unused-offline"),
        aux: Default::default(),
    }
}

#[test]
fn single_configured_provider_reuses_the_worker_as_verifier() {
    // (a) Only the worker's own provider is configured: no distinct
    // family exists, so the router degrades to the worker and we build no
    // second provider — the verifier IS the worker (identical to the
    // pre-routing behavior, no extra cost).
    let configured = vec![configured_provider("zai")];
    assert!(
        resolve_cross_family_verifier("zai", "glm-5.2", &configured).is_none(),
        "a single configured family must leave the verifier as the worker provider"
    );
}

#[test]
fn same_family_providers_reuse_the_worker_as_verifier() {
    // Two providers but ONE family (Gemini and Gemini-via-Vertex both
    // group under `google`): still no bias-resistant verifier available, so
    // it stays the worker — proves `provider_family` grouping gates the
    // cross-family verifier, not the raw provider count.
    let configured = vec![configured_provider("gemini"), configured_provider("vertex")];
    assert!(
        resolve_cross_family_verifier("gemini", "gemini-3-pro", &configured).is_none(),
        "same-vendor providers share a family and must not route a cross-family verifier"
    );
}

#[test]
fn distinct_families_route_a_cross_family_verifier() {
    // (b) Worker on Z.ai with Anthropic also configured: the router picks
    // the distinct family and we build that concrete adapter. No network
    // — only construction and `.id()`.
    let configured = vec![configured_provider("zai"), configured_provider("anthropic")];
    let (verifier, verifier_id) = resolve_cross_family_verifier("zai", "glm-5.2", &configured)
        .expect("a distinct family must route a cross-family verifier");
    assert_eq!(
        verifier_id, "anthropic",
        "verifier must be the distinct family"
    );
    assert_eq!(
        verifier.id(),
        "anthropic",
        "verifier adapter must be Anthropic's"
    );
    assert_ne!(
        verifier.id(),
        "zai",
        "verifier must differ from the worker's family"
    );
}

#[test]
fn verifier_build_failure_falls_back_to_the_worker() {
    // (c) The router selects a distinct family, but building that verifier
    // adapter fails (an unknown model slug the catalog rejects). Verifier
    // routing must never break the loop: it falls back to the worker
    // provider (`None`). Fully offline and race-free — no shared env, no
    // network — unlike an env-gated Vertex/Bedrock build failure.
    let faux = ConfiguredProvider {
        config: ProviderConfig {
            upstream_pin: &[],
            id: "faux",
            env_var: "STELLA_TEST_FAUX_KEY",
            env_var_aliases: &[],
            display_name: "Faux (unbuildable)",
            default_model: "faux-model-not-in-catalog",
            base_url: "http://localhost:0",
            dialect: crate::config::Dialect::OpenaiCompatible,
            // Seeded on purpose: the catalog check must reject the
            // phantom slug, which is exactly the build failure this
            // test needs.
            seeded: true,
        },
        api_key: ApiKey::new("dummy-key-unused-offline"),
        aux: Default::default(),
    };
    let configured = vec![configured_provider("zai"), faux];
    assert!(
        resolve_cross_family_verifier("zai", "glm-5.2", &configured).is_none(),
        "a verifier adapter that fails to build must fall back to the worker provider"
    );
}

#[test]
fn reflection_json_preserves_full_paid_call_envelope_and_cost() {
    let report = ReflectionReport {
        recorded: 1,
        model_error: None,
        cost_usd: 0.0042,
        events: vec![AgentEvent::StepUsage {
            upstream_provider: None,
            reasoning_tokens: None,
            output_text: None,
            step: 0,
            role: stella_protocol::ModelCallRole::Reflection,
            provider: "anthropic".into(),
            model: "claude-reflect".into(),
            input_tokens: 100,
            output_tokens: 20,
            cached_input_tokens: 5,
            cache_write_tokens: 3,
            estimated_input_tokens: 90,
            cost_usd: 0.0042,
            duration_ms: 12,
            retries: 1,
            tool_calls: 0,
            complete: true,
            finish_reason: None,
        }],
    };

    let value = reflection_json(&report);
    assert_eq!(value["cost_usd"], 0.0042);
    assert_eq!(value["events"][0]["type"], "step_usage");
    assert_eq!(value["events"][0]["role"], "reflection");
    assert_eq!(value["events"][0]["provider"], "anthropic");
    assert_eq!(value["events"][0]["model"], "claude-reflect");
    assert_eq!(value["events"][0]["complete"], true);
}

#[test]
fn reflection_budget_tick_is_rebased_to_the_caller_session() {
    let mut guard = BudgetGuard::new(BudgetMode::Enforced, Some(1.0), None);
    let _ = guard.record_spend(0.8);
    let mut report = ReflectionReport {
        recorded: 0,
        model_error: None,
        cost_usd: 0.02,
        events: vec![AgentEvent::BudgetTick {
            spent_usd: 0.02,
            limit_usd: Some(0.2),
            mode: BudgetMode::Enforced,
            session_spent_usd: None,
            session_limit_usd: None,
            deadline_remaining_ms: None,
        }],
    };

    settle_reflection_budget(&mut report, &mut guard);

    assert!((guard.spent_usd() - 0.82).abs() < f64::EPSILON);
    let ticks = report
        .events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::BudgetTick {
                spent_usd,
                limit_usd,
                ..
            } => Some((*spent_usd, *limit_usd)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(ticks.len(), 1);
    assert!((ticks[0].0 - 0.82).abs() < f64::EPSILON);
    assert_eq!(ticks[0].1, Some(1.0));
}

#[test]
fn spend_limit_flag_configures_the_session_axis_not_the_turn_axis() {
    // `--spend-limit` must cap the whole run, so its limit lives on the session
    // axis (which `begin_turn` never resets) and the turn axis stays unset.
    let guard = build_budget_guard(Some(5.0));
    assert_eq!(guard.mode(), BudgetMode::Enforced);
    assert_eq!(guard.session_limit_usd(), Some(5.0));
    assert_eq!(
        guard.turn_limit_usd(),
        None,
        "the CLI limit must not land on the per-turn axis"
    );

    // No flag still meters (observed) but never gates.
    let unbounded = build_budget_guard(None);
    assert_eq!(unbounded.mode(), BudgetMode::Observed);
    assert_eq!(unbounded.session_limit_usd(), None);
    assert_eq!(unbounded.turn_limit_usd(), None);
}

#[test]
fn spend_limit_cap_holds_across_turns_rather_than_resetting_each_one() {
    use stella_core::BudgetOutcome;
    use stella_core::budget::BudgetAxis;

    // A multi-turn session (REPL, deck, or goal round) calls `begin_turn` at
    // the top of every turn. Each turn here is individually under the $1.00
    // limit, but their sum is not — the session axis must trip on the second
    // turn instead of the per-turn reset handing back the full limit again.
    let mut budget = build_budget_guard(Some(1.0));

    budget.begin_turn();
    assert_eq!(budget.record_spend(0.6), BudgetOutcome::Continue);

    budget.begin_turn();
    match budget.record_spend(0.6) {
        BudgetOutcome::AbortTurn {
            axis: BudgetAxis::Session,
            spent_usd,
            limit_usd,
        } => {
            assert!((spent_usd - 1.2).abs() < 1e-9);
            assert_eq!(limit_usd, 1.0);
        }
        other => panic!("expected a session-axis abort across turns, got {other:?}"),
    }
}

#[test]
fn remaining_budget_tracks_session_headroom() {
    let mut guard = build_budget_guard(Some(2.0));
    assert_eq!(remaining_budget(&guard), Some(2.0));

    guard.begin_turn();
    guard.record_spend(0.5);
    assert!((remaining_budget(&guard).unwrap() - 1.5).abs() < 1e-9);

    // Headroom survives a turn reset — it is session-scoped, not turn-scoped.
    guard.begin_turn();
    assert!((remaining_budget(&guard).unwrap() - 1.5).abs() < 1e-9);
    guard.record_spend(3.0);
    assert_eq!(remaining_budget(&guard), Some(0.0));

    // No configured limit means no headroom to report.
    assert_eq!(remaining_budget(&build_budget_guard(None)), None);
}

mod usage_completeness;

mod engine_wiring;

mod durability_isolation;

mod code_graph;

/// Issue #644: the machine-readable envelope declares its contract version, and
/// declares the *same* one on both arms. A version present on the success shape
/// but missing from the error shape is worse than no version at all — a script
/// could not rely on reading it, and the error arm is the one a headless
/// consumer hits most.
#[test]
fn the_json_summary_envelope_declares_its_schema_version_on_both_arms() {
    let sample = |status, text: Option<&str>, reason: Option<&str>| PipelineRunSummary {
        schema_version: crate::SUMMARY_SCHEMA_VERSION,
        status,
        text: text.map(str::to_string),
        cost_usd: 0.25,
        reason: reason.map(str::to_string),
        task_class: text.map(|_| "Edit".to_string()),
        verdict: text.map(|_| serde_json::json!({ "passed": true })),
        revisions: text.map(|_| 1),
        candidates_run: text.map(|_| 2),
        model: "anthropic/claude-opus".to_string(),
        events: Vec::new(),
        reflection: serde_json::Value::Null,
    };

    let ok = serde_json::to_value(sample("completed", Some("done"), None)).expect("serializes");
    let err = serde_json::to_value(sample("error", None, Some("boom"))).expect("serializes");

    assert_eq!(
        ok["schema_version"],
        serde_json::json!(crate::SUMMARY_SCHEMA_VERSION)
    );
    assert_eq!(
        err["schema_version"], ok["schema_version"],
        "one run, one envelope contract: the arms cannot declare different versions"
    );

    // The key set is the contract (#373), and the version stamp is now part of
    // it — on both arms, as an explicit key rather than an inferred default.
    let keys = |v: &serde_json::Value| {
        v.as_object()
            .expect("the summary serializes as an object")
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(
        keys(&ok),
        keys(&err),
        "the success and error arms share one key set"
    );
    assert!(keys(&ok).contains("schema_version"));
}

/// Both machine-readable summaries lead with `schema_version`. Key order is
/// deliberately *not* part of the consumer contract — this pins a build
/// convention, not a promise: every envelope is assembled from a struct with the
/// version declared first, so a derived `Serialize` emits it at the head of the
/// object where a human eyeballing output sees it immediately. Rebuilding any of
/// them with `serde_json::json!` would silently undo that (a `json!` object is a
/// sorted map, which sorts `schema_version` into the middle), and nothing else
/// in the suite would notice.
#[test]
fn every_summary_envelope_leads_with_its_version() {
    let pipeline = PipelineRunSummary {
        schema_version: crate::SUMMARY_SCHEMA_VERSION,
        status: "completed",
        text: None,
        cost_usd: 0.0,
        reason: None,
        task_class: None,
        verdict: None,
        revisions: None,
        candidates_run: None,
        model: "anthropic/claude-opus".to_string(),
        events: Vec::new(),
        reflection: serde_json::Value::Null,
    };
    let raw = RawRunSummary {
        schema_version: crate::SUMMARY_SCHEMA_VERSION,
        status: "completed",
        text: None,
        cost_usd: None,
        reason: None,
        model: "anthropic/claude-opus".to_string(),
        events: Vec::new(),
        files_touched: serde_json::Value::Null,
    };

    for (label, encoded) in [
        ("pipeline", serde_json::to_string(&pipeline).unwrap()),
        ("raw step-loop", serde_json::to_string(&raw).unwrap()),
    ] {
        assert!(
            encoded.starts_with(r#"{"schema_version":"#),
            "the {label} summary must lead with its version, got: {encoded}"
        );
    }
}

// ---------------------------------------------------------------------------
// Per-tool policy: the session stack, end to end
// ---------------------------------------------------------------------------

/// Assemble a session tool stack the way every driver does — real registry,
/// customs, the policy filter on top — and return the advertised names plus
/// a closure-free handle for calling into it.
///
/// Deliberately not a mock: the point of these witnesses is *where* the
/// decorator sits in the real chain, which a fake inner executor cannot show.
async fn stack_names_and_execute(
    root: &std::path::Path,
    policy: stella_tools::policy::ToolPolicy,
    custom_tools: Vec<stella_tools::custom::CustomTool>,
    call: &str,
    input: serde_json::Value,
) -> (Vec<String>, ToolOutput) {
    let registry = ToolRegistry::new(root.to_path_buf());
    let customs = CustomToolSet::new(&registry, custom_tools, root.to_path_buf());
    let tools = PolicyToolSet::new(&customs, policy);
    let names = tools
        .schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect();
    let output = tools.execute(call, &input).await;
    (names, output)
}

/// **Witness for the shipped default, at the session boundary.** With no
/// settings at all, the assembled stack advertises the whole registry
/// surface and runs it.
#[tokio::test]
async fn the_session_stack_ships_with_the_registry_surface_available() {
    let root = tempfile::tempdir().unwrap();
    let (names, output) = stack_names_and_execute(
        root.path(),
        stella_tools::policy::ToolPolicy::allow_all(),
        vec![],
        "save_state",
        serde_json::json!({"key": "note", "content": "hi"}),
    )
    .await;

    for expected in stella_tools::catalog::ALL_NAMES {
        assert!(
            names.iter().any(|name| name == expected),
            "`{expected}` must be advertised with no settings at all: {names:?}"
        );
    }
    assert!(
        !output.is_error(),
        "a default built-in must run: {output:?}"
    );
}

/// **Witness: `{"save_state": "off"}` hides AND refuses.** Hiding alone is a
/// prompt-budget measure; a capability gate has to hold when the model calls
/// the name anyway, from a stale prompt or a replayed trajectory.
#[tokio::test]
async fn a_settings_entry_hides_and_refuses_a_tool_in_the_real_stack() {
    let root = tempfile::tempdir().unwrap();
    let policy =
        serde_json::from_str::<crate::settings::Settings>(r#"{"tools": {"save_state": "off"}}"#)
            .unwrap()
            .tool_policy();
    let (names, output) = stack_names_and_execute(
        root.path(),
        policy,
        vec![],
        "save_state",
        serde_json::json!({"key": "note", "content": "hi"}),
    )
    .await;

    assert!(
        !names.iter().any(|name| name == "save_state"),
        "a switched-off tool must not be advertised: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "task_list"),
        "and nothing else is withheld"
    );
    match output {
        ToolOutput::Error { message, .. } => assert!(
            message.contains("unknown tool"),
            "a disabled tool must not announce itself: {message}"
        ),
        other => panic!("a disabled tool must be refused, got {other:?}"),
    }
}

/// **Witness: a group key disables the whole family in the real stack.**
#[tokio::test]
async fn a_group_entry_disables_every_scratch_tool_in_the_real_stack() {
    let root = tempfile::tempdir().unwrap();
    let policy =
        serde_json::from_str::<crate::settings::Settings>(r#"{"tools": {"scratch": "off"}}"#)
            .unwrap()
            .tool_policy();
    let (names, output) = stack_names_and_execute(
        root.path(),
        policy,
        vec![],
        "save_state",
        serde_json::json!({"key": "note", "content": "hi"}),
    )
    .await;

    for withheld in stella_tools::catalog::names_in_group("scratch") {
        assert!(
            !names.iter().any(|name| name == withheld),
            "`{withheld}` must be withheld by the group switch: {names:?}"
        );
    }
    assert!(
        names.iter().any(|name| name == "task_list"),
        "the task family is its own group"
    );
    assert!(matches!(output, ToolOutput::Error { .. }));
}

/// **Witness: the decorator sits ABOVE the custom-tool layer.** A customer's
/// registered tool is not in any compile-time table, so only a filter above
/// the whole assembled stack can reach it by name.
#[tokio::test]
async fn a_customer_registered_tool_is_covered_by_the_policy() {
    let root = tempfile::tempdir().unwrap();
    let manifest = root.path().join(".stella").join("tools");
    std::fs::create_dir_all(&manifest).unwrap();
    std::fs::write(
        manifest.join("deploy_to_staging.toml"),
        "name = \"deploy_to_staging\"\ndescription = \"ship it\"\ncommand = [\"./deploy.sh\"]",
    )
    .unwrap();
    let found = stella_tools::custom::discover_in_scopes(root.path(), None, true);
    let custom_tools = crate::tool_foundry::adopt::gate_discovery(found, root.path()).tools;
    assert_eq!(custom_tools.len(), 1, "fixture must register one tool");

    // On: the tool is there, so the fixture proves the *policy* withheld it
    // below, not a broken manifest.
    let (names, _) = stack_names_and_execute(
        root.path(),
        stella_tools::policy::ToolPolicy::allow_all(),
        custom_tools.clone(),
        "task_list",
        serde_json::json!({}),
    )
    .await;
    assert!(names.iter().any(|name| name == "deploy_to_staging"));

    let policy = serde_json::from_str::<crate::settings::Settings>(
        r#"{"tools": {"deploy_to_staging": "off"}}"#,
    )
    .unwrap()
    .tool_policy();
    let (names, output) = stack_names_and_execute(
        root.path(),
        policy,
        custom_tools,
        "deploy_to_staging",
        serde_json::json!({}),
    )
    .await;
    assert!(
        !names.iter().any(|name| name == "deploy_to_staging"),
        "a custom tool named in settings must be withheld: {names:?}"
    );
    match output {
        ToolOutput::Error { message, .. } => assert!(message.contains("unknown tool"), "{message}"),
        other => panic!("a disabled custom tool must be refused, got {other:?}"),
    }
}

/// The SessionStart hook budget: within it, bytes pass through untouched
/// (the prefix stays exactly what the hook wrote); past it, the cut is
/// marked in-band — the same never-silent posture as every other budgeted
/// prompt section.
#[test]
fn session_hook_context_is_clamped_with_a_visible_marker() {
    let small = "export STAGING_URL=https://stage.example";
    assert_eq!(crate::agent::clamp_hook_context(small), small);

    let big = "x".repeat(crate::agent::SESSION_HOOK_CONTEXT_BUDGET_CHARS + 500);
    let clamped = crate::agent::clamp_hook_context(&big);
    assert!(clamped.contains("hook output truncated"));
    assert!(
        clamped.chars().count() < crate::agent::SESSION_HOOK_CONTEXT_BUDGET_CHARS + 200,
        "the clamp bounds the section, not the hook's appetite"
    );
}
