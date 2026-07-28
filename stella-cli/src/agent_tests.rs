use super::*;
use crate::config::{ConfiguredProvider, PROVIDERS, ProviderConfig};
use stella_model::credential::ApiKey;
use stella_pipeline::CandidateWorkspacePort;

#[test]
fn one_shot_reflection_defaults_on_for_every_output_format() {
    let _env = crate::test_env::lock();
    // SAFETY: the shared test env lock serializes every Stella test that
    // mutates or reads process environment state.
    unsafe { std::env::remove_var(DISABLE_REFLECTION_ENV) };

    assert!(one_shot_reflection_enabled(OutputFormat::Text));
    assert!(one_shot_reflection_enabled(OutputFormat::Json));
    assert!(one_shot_reflection_enabled(OutputFormat::StreamJson));
}

#[test]
fn explicit_reflection_opt_out_suppresses_every_one_shot_format() {
    let _env = crate::test_env::lock();
    // SAFETY: the shared test env lock serializes every Stella test that
    // mutates or reads process environment state.
    unsafe { std::env::set_var(DISABLE_REFLECTION_ENV, "  YeS  ") };

    assert!(!one_shot_reflection_enabled(OutputFormat::Text));
    assert!(!one_shot_reflection_enabled(OutputFormat::Json));
    assert!(!one_shot_reflection_enabled(OutputFormat::StreamJson));

    // SAFETY: still inside the shared test env critical section.
    unsafe { std::env::remove_var(DISABLE_REFLECTION_ENV) };
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

/// The scripts section rides the byte-stable prompt prefix: two
/// assemblies over the same workspace must be byte-identical, the verb
/// bindings must be present, and a scriptless workspace must add
/// nothing (docs/design/scripts-index.md).
#[test]
fn assemble_system_prompt_carries_a_byte_stable_scripts_section() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        root.path().join("package.json"),
        r#"{"scripts": {"build": "next build", "test": "vitest"}}"#,
    )
    .unwrap();
    std::fs::write(root.path().join("pnpm-lock.yaml"), "").unwrap();

    let authority = crate::settings::AuthorityPolicy {
        project_prompts_allowed: true,
        ..crate::settings::AuthorityPolicy::default()
    };
    let rules = crate::rules::ResolvedRules::default();
    let first = assemble_system_prompt(SYSTEM_PROMPT, root.path(), &authority, &rules);
    let second = assemble_system_prompt(SYSTEM_PROMPT, root.path(), &authority, &rules);
    assert_eq!(first, second, "same workspace state ⇒ identical bytes");
    assert!(first.contains("## Project scripts"), "section present");
    assert!(first.contains("build → pnpm run build"), "{first}");
    assert!(first.contains("install → pnpm install"), "{first}");

    let empty = tempfile::tempdir().expect("tempdir");
    let bare = assemble_system_prompt(SYSTEM_PROMPT, empty.path(), &authority, &rules);
    assert!(
        !bare.contains("## Project scripts"),
        "no scripts → no section, no noise"
    );
}

/// Zero-call orientation (issue #328): over a pre-indexed workspace, the
/// interactive system prompt carries the project map — languages, layout,
/// entry points — baked into the byte-stable prefix, so orientation costs no
/// model round-trip and is unconditional rather than left to the model's
/// discretion. Two assemblies over the same index state must be
/// byte-identical: that is the invariant that lets the whole prefix ride the
/// provider's prompt cache (AGENTS.md invariant #7).
#[test]
fn assemble_system_prompt_bakes_a_byte_stable_orientation_map() {
    let root = graph_fixture();

    let authority = crate::settings::AuthorityPolicy {
        project_prompts_allowed: true,
        ..crate::settings::AuthorityPolicy::default()
    };
    let rules = crate::rules::ResolvedRules::default();
    let first = assemble_system_prompt(SYSTEM_PROMPT, root.path(), &authority, &rules);
    let second = assemble_system_prompt(SYSTEM_PROMPT, root.path(), &authority, &rules);
    assert_eq!(
        first, second,
        "same index state ⇒ identical bytes (the prompt-cache invariant)"
    );
    assert!(first.contains("## Project map"), "{first}");
    assert!(first.contains("Languages: rust"), "{first}");
    assert!(
        first.contains("Layout (2 indexed files): 2 at the root"),
        "the slow-churning skeleton includes the top-level layout: {first}"
    );
    assert!(first.contains("Entry points:"), "{first}");
}

/// The #336 wave-1 steering-parity witness: `read_symbol` (#383) must be
/// advertised in BOTH static base personas the way its siblings `repo_diff`
/// (#381) and `diagnostics` (#384) are — a tool the prompt never mentions
/// loses to guessed read_file offsets no matter how good it is.
///
/// The catalogue line this used to match is gone (#639): what `read_symbol`
/// *does* now comes from its schema, and only the offset-guessing steering —
/// which no single schema can express — stayed behind.
#[test]
fn both_static_prompts_carry_a_read_symbol_steering_line() {
    for (name, prompt) in [
        ("SYSTEM_PROMPT", SYSTEM_PROMPT),
        ("PIPELINE_SYSTEM_PROMPT", PIPELINE_SYSTEM_PROMPT),
    ] {
        assert!(
            prompt.contains("read_symbol"),
            "{name} must carry a read_symbol steering line"
        );
        assert!(
            prompt.contains("guessing read_file offsets after a graph_query"),
            "{name}'s read_symbol line must steer AWAY from offset-guessing — \
             that round-trip is the tool's reason to exist (issue #330)"
        );
    }
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

#[cfg(unix)]
#[test]
fn schema_index_population_visibly_rejects_unsafe_legacy_codegraph() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("tempdir");
    let dot = root.path().join(".stella");
    std::fs::create_dir_all(&dot).unwrap();
    std::fs::set_permissions(&dot, std::fs::Permissions::from_mode(0o777)).unwrap();
    std::fs::write(dot.join("codegraph.db"), b"unsafe legacy graph").unwrap();
    let registry = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);

    let error = populate_schema_index(&registry, root.path()).unwrap_err();
    assert!(
        error.contains("legacy") && error.contains("private"),
        "{error}"
    );
    assert!(dot.join("codegraph.db").exists());
}

/// Auto-build on session start (task part A): a workspace with a source
/// file. `graph_query` is now advertised from turn 1 regardless (it builds
/// its own index on first use), so this pins what [`spawn_session_graph`]
/// still adds: it builds `.stella/private/codegraph.db` EAGERLY in the
/// background, so the first real query harvests a ready index instead of
/// paying the build cost inline. Awaiting the returned handle is the
/// deterministic "index ready" signal.
#[tokio::test]
async fn spawn_session_graph_eagerly_builds_the_index_in_the_background() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("lib.rs"), "pub fn find_me() {}\n").unwrap();

    let registry = Arc::new(ToolRegistry::with_issue_backend(root.clone(), None));
    let advertises = |r: &ToolRegistry| r.schemas().iter().any(|s| s.name == "graph_query");

    // Turn 1: advertised already, and no index on disk yet — the tool does
    // not wait for one, it builds on first use.
    assert!(!stella_tools::graph::graph_available(&root).unwrap());
    assert!(
        advertises(&registry),
        "graph_query is advertised from the start, index or not"
    );

    let (session_graph, build) =
        spawn_session_graph(&root, registry.clone(), Box::new(|_| {}), Box::new(|| {}));
    build.await.expect("background build task");

    // After the build: the db exists, the tool is advertised, and it
    // dispatches against the freshly built index.
    assert!(
        stella_tools::graph::graph_available(&root).unwrap(),
        "the background build must create .stella/private/codegraph.db"
    );
    assert!(
        advertises(&registry),
        "graph_query stays advertised after the build"
    );
    let out = registry
        .execute(
            "graph_query",
            &serde_json::json!({"op": "definitions", "target": "find_me"}),
        )
        .await;
    assert!(!out.is_error(), "graph_query must dispatch: {out:?}");
    session_graph.shutdown();
}

/// Live freshness (task part B): after the session graph is up, a
/// brand-new source file the agent (or an external tool) writes is
/// incrementally re-indexed by the live `notify` watcher, so the very next
/// `graph_query` reflects it — the staleness that makes the model distrust
/// the graph is gone. Polls with a generous budget because the OS watcher
/// + debounce are asynchronous, and re-writes the file each iteration so a
/// create event lost during the watcher's async arming window is retried
/// (the un-indexed file re-parses on the first event that lands).
#[tokio::test]
async fn session_graph_live_refreshes_after_a_file_is_added() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    std::fs::write(root.join("lib.rs"), "pub fn original() {}\n").unwrap();

    let registry = Arc::new(ToolRegistry::with_issue_backend(root.clone(), None));
    let (session_graph, build) =
        spawn_session_graph(&root, registry.clone(), Box::new(|_| {}), Box::new(|| {}));
    build.await.expect("background build task");

    // The new symbol is absent from the just-built index.
    let before = stella_tools::graph::run_query(&root, "definitions", "added_later");
    assert!(
        matches!(&before, ToolOutput::Ok { content } if content.contains("no definitions")),
        "the new symbol must not be indexed yet: {before:?}"
    );

    let added = root.join("added.rs");
    let mut reflected = false;
    for _ in 0..150 {
        std::fs::write(&added, "pub fn added_later() {}\n").unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let ToolOutput::Ok { content } =
            stella_tools::graph::run_query(&root, "definitions", "added_later")
            && content.contains("added_later")
        {
            reflected = true;
            break;
        }
    }
    assert!(
        reflected,
        "the live watcher must re-index the new file so graph_query reflects it"
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
        prompt.contains("Never force-push.  [enforced]"),
        "a guarded rule must render with the enforced marker: {prompt}"
    );
}

/// An untrusted checkout cannot append repository-authored content to the
/// privileged system prompt. Explicit repository trust restores those
/// sources within the already-computed managed ceiling.
#[test]
fn untrusted_project_prompt_sources_are_absent_from_the_system_prompt() {
    let root = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        root.path().join("package.json"),
        r#"{"scripts": {"authority-marker": "echo project-script"}}"#,
    )
    .unwrap();
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
    std::fs::create_dir_all(root.path().join(".stella/explorations")).unwrap();
    std::fs::write(
        root.path().join(".stella/explorations/project.json"),
        serde_json::json!({
            "slice": "authority-map",
            "title": "PROJECT_MAP_AUTHORITY_MARKER",
            "summary": "project map",
            "content": "body",
            "files": [],
            "created_at_ms": 1u64
        })
        .to_string(),
    )
    .unwrap();

    let mut cfg = cfg_for("zai");
    cfg.workspace_root = root.path().to_path_buf();
    cfg.authority.project_prompts_allowed = false;
    let untrusted_rules = crate::rules::load_workspace_rules(root.path(), &cfg.authority);
    let untrusted = build_system_prompt(&cfg, root.path(), &untrusted_rules);
    for marker in [
        "authority-marker",
        "PROJECT_MEMORY_AUTHORITY_MARKER",
        "PROJECT_RULE_AUTHORITY_MARKER",
        "PROJECT_MAP_AUTHORITY_MARKER",
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
        "authority-marker",
        "PROJECT_MEMORY_AUTHORITY_MARKER",
        "PROJECT_RULE_AUTHORITY_MARKER",
        "PROJECT_MAP_AUTHORITY_MARKER",
    ] {
        assert!(trusted.contains(marker), "trusted marker missing: {marker}");
    }
}

#[test]
fn system_prompt_carries_the_workspace_maps_index() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join(".stella/explorations");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("cli.json"),
        serde_json::json!({
            "slice": "cli", "title": "CLI surface", "summary": "maps the CLI",
            "content": "big body that must NOT be in the prompt",
            "files": [], "created_at_ms": 1u64
        })
        .to_string(),
    )
    .unwrap();

    let mut cfg = cfg_for("zai");
    cfg.authority.project_prompts_allowed = true;
    let rules = crate::rules::ResolvedRules::default();
    let prompt = build_system_prompt(&cfg, root.path(), &rules);
    assert!(
        prompt.contains("## Workspace maps"),
        "index section missing"
    );
    assert!(prompt.contains("`cli`") && prompt.contains("CLI surface"));
    assert!(
        !prompt.contains("big body"),
        "map bodies must stay pull-only, never in the prompt"
    );

    // No maps → no section, no tokens.
    let bare = tempfile::tempdir().expect("tempdir");
    let empty = build_system_prompt(
        &cfg_for("zai"),
        bare.path(),
        &crate::rules::ResolvedRules::default(),
    );
    assert!(!empty.contains("## Workspace maps"));
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
    let dir = root.path().join(".stella/explorations");
    std::fs::create_dir_all(&dir).unwrap();
    // A completed map and a draft claimed live by THIS process: the pair that
    // used to put a relative age and a pid straight into the cached prefix.
    for (slice, status, pid) in [
        ("cli", "complete", None),
        ("wip", "draft", Some(std::process::id())),
    ] {
        std::fs::write(
            dir.join(format!("{slice}.json")),
            serde_json::json!({
                "slice": slice, "title": format!("Map {slice}"), "summary": "covers it",
                "content": "body", "files": [], "created_at_ms": 1_700_000_000_000u64,
                "status": status, "pid": pid,
            })
            .to_string(),
        )
        .unwrap();
    }
    std::fs::write(
        root.path().join("package.json"),
        r#"{"scripts": {"build": "next build"}}"#,
    )
    .unwrap();
    std::fs::write(root.path().join(".stella/memories/.keep"), "").ok();

    let mut cfg = cfg_for("zai");
    cfg.authority.project_prompts_allowed = true;
    let rules = crate::rules::ResolvedRules::default();
    let prompt = build_system_prompt(&cfg, root.path(), &rules);

    // The fixture must actually reach the prefix, or every assertion below
    // passes vacuously on a record that silently failed to parse.
    assert!(
        prompt.contains("`cli`") && prompt.contains("## Project scripts"),
        "the fixture never reached the prefix — this guard would be vacuous:\n{prompt}"
    );
    assert!(
        prompt.contains("saved 2023-11-14"),
        "freshness must render as an absolute stamp, not a relative age:\n{prompt}"
    );
    assert!(
        !prompt.contains("`wip`"),
        "an in-progress draft belongs in the volatile recall block, never the \
         cached prefix (#639):\n{prompt}"
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

    let _home = crate::settings::test_user_home(home.path().to_path_buf());
    let isolation = crate::settings::test_filesystem_isolation(true);

    let mut cfg = cfg_for("zai");
    cfg.workspace_root = root.to_path_buf();
    cfg.authority.project_prompts_allowed = true;

    let rules = crate::rules::load_workspace_rules(root, &cfg.authority);
    let prompt = build_pipeline_system_prompt(&cfg, root, &rules);
    let skills = crate::memory::load_workspace_skills(root);
    let custom_tools = custom_tool_report_for_workspace(root).tools;
    let memory = SessionMemory::open(root, false);
    let store = open_store(root);
    let mcp = load_mcp_plan(&cfg);

    let registry = ToolRegistry::with_backends_and_options(
        root.to_path_buf(),
        None,
        None,
        registry_options(&cfg),
    );
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let interactive = InteractiveToolSet::new(&registry, event_tx, default_ask_io(false));
    let interactive = match skill_registry_for_run(root.to_path_buf()) {
        Some(registry) => interactive.with_skill_registry(registry),
        None => interactive,
    };
    let discovery = crate::discovery::DiscoveryToolSet::new(&interactive, root.to_path_buf());
    let schema_names: Vec<String> = discovery
        .schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect();

    assert_eq!(prompt, PIPELINE_SYSTEM_PROMPT);
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
    assert!(schema_names.iter().any(|name| name == "tool_search"));
    for forbidden in [
        "skill_search",
        "mcp_search",
        "search_skills",
        "install_skill",
        "hostile_workspace_tool",
        "hostile_user_tool",
    ] {
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
    let normal_prompt = build_pipeline_system_prompt(&cfg, root, &normal_rules);
    let normal_skills = crate::memory::load_workspace_skills(root);
    let normal_custom_tools = custom_tool_report_for_workspace(root).tools;
    assert!(normal_prompt.contains("HOSTILE_WORKSPACE_MEMORY"));
    assert!(normal_prompt.contains("HOSTILE_STELLA_RULE"));
    assert!(normal_prompt.contains("HOSTILE_CLAUDE_RULE"));
    assert!(normal_prompt.contains("HOSTILE_USER_RULE"));
    assert!(!normal_rules.is_empty());
    assert_eq!(normal_skills.len(), 2);
    assert_eq!(normal_custom_tools.len(), 2);
    assert!(skill_registry_for_run(root.to_path_buf()).is_some());
    assert!(matches!(load_mcp_plan(&cfg), McpPlan::Invalid(_)));
}

/// A `Config` selecting `provider_id` at its default model, with a dummy
/// key. `build_provider` only constructs the adapter (no network call),
/// so the key is never used.
fn cfg_for(provider_id: &str) -> Config {
    let provider = PROVIDERS
        .iter()
        .find(|p| p.id == provider_id)
        .unwrap_or_else(|| panic!("provider `{provider_id}` not in PROVIDERS"))
        .clone();
    let model_id = provider.default_model.to_string();
    Config {
        provider,
        model_id,
        // The default posture for these tests is "no --model given", so the
        // settings-driven wiring under test is the thing exercised. The
        // flag-pinned case sets this explicitly.
        model_pinned_by_flag: false,
        api_key: ApiKey::new("dummy-key-unused-offline"),
        credential_source: None,
        workspace_root: std::path::PathBuf::from("/tmp"),
        base_url_override: None,
        hooks: None,
        engine_settings: None,
        tool_policy: Default::default(),
        enable_recap: false,
        authority: crate::settings::AuthorityPolicy::default(),
        credential_advisories: Vec::new(),
    }
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
        approval_capability_for(true, false, true),
        PipelineApprovalCapability::Unavailable,
        "text format alone must not select Stdio when stdin isn't a real terminal"
    );
    assert_eq!(
        approval_capability_for(true, true, false),
        PipelineApprovalCapability::Unavailable,
        "text format alone must not select Stdio when stdout isn't a real terminal"
    );
    assert_eq!(
        approval_capability_for(true, false, false),
        PipelineApprovalCapability::Unavailable
    );
}

#[test]
fn approval_capability_for_json_is_always_unavailable() {
    // Output serialization must never grant execution authority, regardless
    // of the terminal state — JSON output has nowhere to render a prompt.
    assert_eq!(
        approval_capability_for(false, true, true),
        PipelineApprovalCapability::Unavailable
    );
    assert_eq!(
        approval_capability_for(false, false, false),
        PipelineApprovalCapability::Unavailable
    );
}

#[test]
fn approval_capability_for_full_tty_text_is_stdio() {
    // Only the genuine interactive case — text format, real stdin, real
    // stdout — selects Stdio.
    assert_eq!(
        approval_capability_for(true, true, true),
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
    let text_capability = approval_capability_for(true, false, false);
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
    let json_capability = approval_capability_for(false, true, true);
    let json_config =
        pipeline_config_for_approval_capability(&cfg, json_capability, None, &model_ref);
    assert!(json_config.headless);
    assert!(
        !json_config.headless_bypass_scope_review,
        "a JSON-format run's wired config must never bypass scope review"
    );
}

#[tokio::test]
async fn candidate_rules_reuse_the_parent_snapshot_after_source_removal() {
    let root = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t.t"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(root.path().join("base.txt"), "base\n").unwrap();
    git(&["add", "base.txt"]);
    git(&["commit", "-q", "-m", "base"]);

    let rule_path = root.path().join(".stella/rules/protect-session.md");
    std::fs::create_dir_all(rule_path.parent().unwrap()).unwrap();
    std::fs::write(
        &rule_path,
        "---\nguard-tool: Write\nguard-deny-path: protected/**\n---\nOriginal session guard.",
    )
    .unwrap();
    let mut cfg = cfg_for("zai");
    cfg.workspace_root = root.path().to_path_buf();
    cfg.authority.project_prompts_allowed = true;

    let parent_rules = crate::rules::load_workspace_rules(root.path(), &cfg.authority);
    let parent = ToolRegistry::with_issue_backend(root.path().to_path_buf(), None);
    crate::rules::attach_rule_guards(&parent, &parent_rules);
    let parent_denied = parent
        .execute(
            "write_file",
            &serde_json::json!({"path": "protected/parent.txt", "content": "no\n"}),
        )
        .await;
    assert!(parent_denied.is_error(), "parent guard was not attached");

    // Mutate the source after the parent session has resolved and attached
    // it. Candidate creation must retain that original session snapshot.
    std::fs::remove_file(&rule_path).unwrap();
    let prompt = build_system_prompt(&cfg, root.path(), &parent_rules);
    assert!(
        prompt.contains("Original session guard.  [enforced]"),
        "prompt rendering diverged from the parent rule snapshot: {prompt}"
    );
    let ws_ports = workspace_ports(
        root.path().to_path_buf(),
        &cfg,
        stella_tools::RegistryOptions::default(),
        parent_rules.clone(),
        None,
        None,
    )
    .unwrap();
    let candidate = ws_ports.candidate_workspaces.create().await.unwrap();
    let output = candidate
        .tools()
        .execute(
            "write_file",
            &serde_json::json!({"path": "protected/candidate.txt", "content": "no\n"}),
        )
        .await;
    candidate.seal().await.unwrap();
    let adopted = candidate.adopt(&[]).await.unwrap();
    let landed = root.path().join("protected/candidate.txt").exists();
    candidate.remove().await;

    assert!(
        output.is_error(),
        "candidate reloaded weakened sources instead of retaining the parent snapshot: {output:?}"
    );
    assert!(
        adopted.is_empty(),
        "prohibited candidate edit was adoptable: {adopted:?}"
    );
    assert!(!landed, "prohibited candidate edit reached the parent tree");
}

/// Witness for #441: a rule denial *inside a best-of-N candidate* must
/// reach the journal as a typed `PolicyDecision`, not just as a tool error.
/// Candidate workspaces are the primary real users of the rule-guard bus,
/// so before this the typed record existed for the session and was missing
/// exactly where most denials happen.
#[tokio::test]
async fn a_candidate_rule_denial_reaches_the_journal_as_a_policy_decision() {
    let root = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t.t"]);
    git(&["config", "user.name", "t"]);
    std::fs::write(root.path().join("base.txt"), "base\n").unwrap();
    git(&["add", "base.txt"]);
    git(&["commit", "-q", "-m", "base"]);

    let rule_path = root.path().join(".stella/rules/protect-session.md");
    std::fs::create_dir_all(rule_path.parent().unwrap()).unwrap();
    std::fs::write(
        &rule_path,
        "---\nguard-tool: Write\nguard-deny-path: protected/**\n---\nSession guard.",
    )
    .unwrap();
    let mut cfg = cfg_for("zai");
    cfg.workspace_root = root.path().to_path_buf();
    cfg.authority.project_prompts_allowed = true;
    let parent_rules = crate::rules::load_workspace_rules(root.path(), &cfg.authority);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
    let ws_ports = workspace_ports(
        root.path().to_path_buf(),
        &cfg,
        stella_tools::RegistryOptions::default(),
        parent_rules.clone(),
        None,
        Some(stella_core::EventSender::new(tx)),
    )
    .unwrap();

    let candidate = ws_ports.candidate_workspaces.create().await.unwrap();
    let output = candidate
        .tools()
        .execute(
            "write_file",
            &serde_json::json!({"path": "protected/candidate.txt", "content": "no\n"}),
        )
        .await;
    candidate.remove().await;
    drop(ws_ports);

    assert!(
        output.is_error(),
        "precondition: the guard must still deny inside the candidate"
    );

    let mut decisions = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let AgentEvent::PolicyDecision { kind, subject, .. } = event {
            decisions.push((kind, subject));
        }
    }
    assert!(
        decisions
            .iter()
            .any(|(kind, _)| *kind == stella_protocol::PolicyKind::Blocked),
        "the candidate's denial never reached the journal: {decisions:?}"
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

    unsafe {
        std::env::remove_var("AWS_SECRET_ACCESS_KEY");
    }
}

/// A `ConfiguredProvider` for `provider_id` at its default model with a
/// dummy key — the offline analogue of `cfg_for` for judge routing. The
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
    }
}

#[test]
fn single_configured_provider_reuses_the_worker_as_judge() {
    // (a) Only the worker's own provider is configured: no distinct
    // family exists, so the router degrades to the worker and we build no
    // second provider — the judge IS the worker (identical to the
    // pre-routing behavior, no extra cost).
    let configured = vec![configured_provider("zai")];
    assert!(
        resolve_cross_family_judge("zai", "glm-5.2", &configured).is_none(),
        "a single configured family must leave the judge as the worker provider"
    );
}

#[test]
fn same_family_providers_reuse_the_worker_as_judge() {
    // Two providers but ONE family (Gemini and Gemini-via-Vertex both
    // group under `google`): still no bias-resistant judge available, so
    // it stays the worker — proves `provider_family` grouping gates the
    // cross-family judge, not the raw provider count.
    let configured = vec![configured_provider("gemini"), configured_provider("vertex")];
    assert!(
        resolve_cross_family_judge("gemini", "gemini-3-pro", &configured).is_none(),
        "same-vendor providers share a family and must not route a cross-family judge"
    );
}

#[test]
fn distinct_families_route_a_cross_family_judge() {
    // (b) Worker on Z.ai with Anthropic also configured: the router picks
    // the distinct family and we build that concrete adapter. No network
    // — only construction and `.id()`.
    let configured = vec![configured_provider("zai"), configured_provider("anthropic")];
    let (judge, judge_id) = resolve_cross_family_judge("zai", "glm-5.2", &configured)
        .expect("a distinct family must route a cross-family judge");
    assert_eq!(judge_id, "anthropic", "judge must be the distinct family");
    assert_eq!(judge.id(), "anthropic", "judge adapter must be Anthropic's");
    assert_ne!(
        judge.id(),
        "zai",
        "judge must differ from the worker's family"
    );
}

#[test]
fn judge_build_failure_falls_back_to_the_worker() {
    // (c) The router selects a distinct family, but building that judge
    // adapter fails (an unknown model slug the catalog rejects). Judge
    // routing must never break the loop: it falls back to the worker
    // provider (`None`). Fully offline and race-free — no shared env, no
    // network — unlike an env-gated Vertex/Bedrock build failure.
    let faux = ConfiguredProvider {
        config: ProviderConfig {
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
    };
    let configured = vec![configured_provider("zai"), faux];
    assert!(
        resolve_cross_family_judge("zai", "glm-5.2", &configured).is_none(),
        "a judge adapter that fails to build must fall back to the worker provider"
    );
}

#[test]
fn reflection_json_preserves_full_paid_call_envelope_and_cost() {
    let report = ReflectionReport {
        recorded: 1,
        model_error: None,
        cost_usd: 0.0042,
        events: vec![AgentEvent::StepUsage {
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
fn budget_flag_configures_the_session_axis_not_the_turn_axis() {
    // `--budget` must cap the whole run, so its limit lives on the session
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
fn budget_cap_holds_across_turns_rather_than_resetting_each_one() {
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

#[path = "agent_tests/usage_completeness.rs"]
mod usage_completeness;

#[path = "agent_tests/engine_wiring.rs"]
mod engine_wiring;

/// Issue #272: `stella init`'s summary line must surface generated/minified
/// exclusion count, not let excluded files silently vanish from the totals.
/// Tested on the pure builder/output functions, never a live TTY.
#[test]
fn format_graph_stats_reports_generated_skip_count_when_nonzero() {
    let summary = GraphSummary {
        total_symbols: 12,
        total_imports: 4,
        total_files: 3,
        files_parsed: 2,
        files_unchanged: 1,
        files_skipped_generated: 5,
    };
    let line = format_graph_stats(&summary);
    assert!(
        line.contains("skipped 5 generated files"),
        "line should surface the skip count: {line}"
    );
    assert!(line.contains("12 symbols"), "{line}");
}

#[test]
fn format_graph_stats_omits_the_skip_clause_when_nothing_was_skipped() {
    let summary = GraphSummary {
        total_symbols: 1,
        total_imports: 0,
        total_files: 1,
        files_parsed: 1,
        files_unchanged: 0,
        files_skipped_generated: 0,
    };
    let line = format_graph_stats(&summary);
    assert!(
        !line.contains("skipped"),
        "no skip clause when nothing was excluded: {line}"
    );
}

#[test]
fn format_graph_stats_uses_singular_file_for_a_count_of_one() {
    let summary = GraphSummary {
        total_symbols: 0,
        total_imports: 0,
        total_files: 0,
        files_parsed: 0,
        files_unchanged: 0,
        files_skipped_generated: 1,
    };
    let line = format_graph_stats(&summary);
    assert!(line.contains("skipped 1 generated file"), "{line}");
    assert!(
        !line.contains("generated files"),
        "singular, not plural, for a count of one: {line}"
    );
}

/// End-to-end through the real builder `stella init` calls
/// ([`index_workspace_graph_blocking`]): a `*.min.*` file sitting at the
/// workspace root (no denied directory involved) must be excluded and
/// counted, while an ordinary file alongside it indexes normally.
#[test]
fn index_workspace_graph_blocking_reports_generated_skips_end_to_end() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("app.min.js"), "function refresh(){}\n").unwrap();
    std::fs::write(ws.path().join("main.rs"), "pub fn run() {}\n").unwrap();

    let summary = index_workspace_graph_blocking(ws.path()).expect("index build succeeds");
    assert_eq!(summary.total_files, 1, "the minified file is never indexed");
    assert_eq!(summary.files_skipped_generated, 1);

    let line = format_graph_stats(&summary);
    assert!(line.contains("skipped 1 generated file"), "{line}");
}

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
/// customs, interactive, the policy filter, discovery on top — and return the
/// advertised names plus a closure-free handle for calling into it.
///
/// Deliberately not a mock: the point of these witnesses is *where* the
/// decorator sits in the real chain, which a fake inner executor cannot show.
async fn stack_names_and_execute(
    root: &std::path::Path,
    policy: stella_tools::policy::ToolPolicy,
    custom_tools: Vec<stella_tools::custom::CustomTool>,
    call: &str,
) -> (Vec<String>, ToolOutput) {
    let registry =
        ToolRegistry::with_backends_and_options(root.to_path_buf(), None, None, Default::default());
    let customs = CustomToolSet::new(&registry, custom_tools, root.to_path_buf());
    let (event_tx, _event_rx) = mpsc::unbounded_channel();
    let interactive = InteractiveToolSet::new(&customs, event_tx, default_ask_io(false));
    let permitted = PolicyToolSet::new(&interactive, policy);
    let tools = crate::discovery::DiscoveryToolSet::new(&permitted, root.to_path_buf());
    let names = tools
        .schemas()
        .into_iter()
        .map(|schema| schema.name)
        .collect();
    let output = tools
        .execute(call, &serde_json::json!({"command": "echo hi"}))
        .await;
    (names, output)
}

/// **Witness for the default flip, at the session boundary.** With the
/// shipped policy (no settings at all), the assembled stack advertises `bash`
/// and runs it. On the old code the registry was constructed with
/// `RegistryOptions::bash = false` unless a settings key said otherwise, so
/// the schema was absent and the call was an unknown tool.
#[tokio::test]
async fn the_session_stack_ships_with_bash_available() {
    let root = tempfile::tempdir().unwrap();
    let (names, output) = stack_names_and_execute(
        root.path(),
        stella_tools::policy::ToolPolicy::allow_all(),
        vec![],
        "bash",
    )
    .await;

    assert!(
        names.iter().any(|name| name == "bash"),
        "bash must be advertised with no settings at all: {names:?}"
    );
    match output {
        ToolOutput::Ok { content } => assert!(content.contains("hi"), "{content}"),
        ToolOutput::Error { message } => panic!("default bash must run: {message}"),
    }
}

/// **Witness: `{"bash": "off"}` hides AND refuses.** Hiding alone is a
/// prompt-budget measure; a capability gate has to hold when the model calls
/// the name anyway, from a stale prompt or a replayed trajectory.
#[tokio::test]
async fn a_settings_entry_hides_and_refuses_bash_in_the_real_stack() {
    let root = tempfile::tempdir().unwrap();
    let policy = serde_json::from_str::<crate::settings::Settings>(r#"{"tools": {"bash": "off"}}"#)
        .unwrap()
        .tool_policy();
    let (names, output) = stack_names_and_execute(root.path(), policy, vec![], "bash").await;

    assert!(
        !names.iter().any(|name| name == "bash"),
        "a switched-off tool must not be advertised: {names:?}"
    );
    assert!(
        names.iter().any(|name| name == "read_file"),
        "and nothing else is withheld"
    );
    match output {
        ToolOutput::Error { message } => assert!(
            message.contains("unknown tool"),
            "a disabled tool must not announce itself: {message}"
        ),
        other => panic!("a disabled tool must be refused, got {other:?}"),
    }
}

/// **Witness: a group key disables the whole family in the real stack.**
#[tokio::test]
async fn a_group_entry_disables_every_process_tool_in_the_real_stack() {
    let root = tempfile::tempdir().unwrap();
    let policy =
        serde_json::from_str::<crate::settings::Settings>(r#"{"tools": {"process": "off"}}"#)
            .unwrap()
            .tool_policy();
    let (names, output) =
        stack_names_and_execute(root.path(), policy, vec![], "start_process").await;

    for withheld in stella_tools::catalog::names_in_group("process") {
        assert!(
            !names.iter().any(|name| name == withheld),
            "`{withheld}` must be withheld by the group switch: {names:?}"
        );
    }
    assert!(
        names.iter().any(|name| name == "bash"),
        "bash is its own group"
    );
    assert!(matches!(output, ToolOutput::Error { .. }));
}

/// **Witness: the decorator sits ABOVE the custom-tool layer.** A customer's
/// registered tool is not in any compile-time table and never passed through
/// `RegistryOptions`, so the old per-capability booleans could not reach it at
/// all. `tool_search` must not advertise it either — which is why the policy
/// filter goes *below* the discovery layer, not on top of it.
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
    let custom_tools = stella_tools::custom::discover_in_scopes(root.path(), None, true).tools;
    assert_eq!(custom_tools.len(), 1, "fixture must register one tool");

    // On: the tool is there, so the fixture proves the *policy* withheld it
    // below, not a broken manifest.
    let (names, _) = stack_names_and_execute(
        root.path(),
        stella_tools::policy::ToolPolicy::allow_all(),
        custom_tools.clone(),
        "read_file",
    )
    .await;
    assert!(names.iter().any(|name| name == "deploy_to_staging"));

    let policy = serde_json::from_str::<crate::settings::Settings>(
        r#"{"tools": {"deploy_to_staging": "off"}}"#,
    )
    .unwrap()
    .tool_policy();
    let (names, output) =
        stack_names_and_execute(root.path(), policy, custom_tools, "deploy_to_staging").await;
    assert!(
        !names.iter().any(|name| name == "deploy_to_staging"),
        "a custom tool named in settings must be withheld: {names:?}"
    );
    match output {
        ToolOutput::Error { message } => assert!(message.contains("unknown tool"), "{message}"),
        other => panic!("a disabled custom tool must be refused, got {other:?}"),
    }
}
