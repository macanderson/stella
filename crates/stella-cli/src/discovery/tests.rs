use super::*;

/// A fake session stack: a few natives plus two MCP servers' tools.
struct FakeInner;

fn schema(name: &str, description: &str, read_only: bool) -> ToolSchema {
    ToolSchema {
        name: name.into(),
        description: description.into(),
        input_schema: serde_json::json!({"type": "object"}),
        read_only,
        // Irrelevant to these discovery tests - the searches never speculate.
        speculation_safe: false,
    }
}

#[async_trait]
impl ToolExecutor for FakeInner {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![
            schema("read_file", "Read a file with line numbers", true),
            schema("bash", "Run a shell command", false),
            schema("screenshot", "Capture a screenshot", true),
            schema(
                "search_issues",
                "Search the configured issue tracker by keywords",
                true,
            ),
            schema(
                "mcp__github__list_commits",
                "List commits on a GitHub branch",
                true,
            ),
            schema(
                "mcp__linear__list_issues",
                "List Linear issues in the workspace",
                true,
            ),
        ]
    }
    async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: format!("inner ran {name}"),
        }
    }
}

/// A scripted registry: returns a fixed page, records nothing.
struct StubRegistry;

#[async_trait]
impl McpRegistrySearch for StubRegistry {
    async fn search(&self, _query: &str, _limit: u32) -> Result<stella_mcp::RegistryPage, String> {
        Ok(stella_mcp::RegistryPage {
            entries: vec![stella_mcp::RegistryEntry {
                server: stella_mcp::RegistryServer {
                    name: "io.github.acme/postgres-mcp".into(),
                    description: Some("Query Postgres databases over MCP".into()),
                    title: None,
                    version: Some("1.2.0".into()),
                    repository: None,
                    packages: Vec::new(),
                    remotes: Vec::new(),
                },
                status: Some("active".into()),
                is_latest: Some(true),
                updated_at: None,
            }],
            next_cursor: None,
            count: Some(1),
        })
    }
}

fn full_set(inner: &FakeInner, root: PathBuf) -> DiscoveryToolSet<'_> {
    DiscoveryToolSet::new(inner, root)
        .with_lean(None)
        .with_registry_search(Box::new(StubRegistry))
}

fn content(out: ToolOutput) -> String {
    match out {
        ToolOutput::Ok { content } => content,
        ToolOutput::Error { message, .. } => panic!("expected Ok, got error: {message}"),
    }
}

/// The mount is where `ToolExecutor::active_skill_slugs` stops being the
/// empty default (#2685): a span begun on the shared tracking handle must
/// surface to the engine through the port, and end structurally with its
/// guard — this is the wiring that lets a live skill body survive an
/// overflow-summarization splice.
#[test]
fn a_live_invocation_span_surfaces_through_the_executor_port() {
    let inner = FakeInner;
    let set = full_set(&inner, std::env::temp_dir());
    assert!(set.active_skill_slugs().is_empty(), "nothing live yet");
    let span = set.active_skill_invocations().begin("release-notes");
    assert_eq!(
        set.active_skill_slugs(),
        vec!["release-notes".to_string()],
        "the engine-facing port must see the live invocation"
    );
    drop(span);
    assert!(
        set.active_skill_slugs().is_empty(),
        "teardown is structural — the dropped guard ends the span"
    );
}

#[tokio::test]
async fn schemas_add_the_discovery_trio_on_top_of_inner() {
    let inner = FakeInner;
    let set = full_set(&inner, std::env::temp_dir());
    let names: Vec<String> = set.schemas().into_iter().map(|s| s.name).collect();
    for expected in [TOOL_SEARCH, SKILL_SEARCH, MCP_SEARCH, "bash", "read_file"] {
        assert!(names.contains(&expected.to_string()), "missing {expected}");
    }
    // All three are read-only, so the goal verifier's ReadOnlyTools keeps them.
    for s in set.schemas() {
        if [TOOL_SEARCH, SKILL_SEARCH, MCP_SEARCH].contains(&s.name.as_str()) {
            assert!(s.read_only, "{} must be read-only", s.name);
        }
    }
}

#[tokio::test]
async fn non_discovery_tools_fall_through_to_inner() {
    let inner = FakeInner;
    let set = full_set(&inner, std::env::temp_dir());
    match set.execute("bash", &Value::Null).await {
        ToolOutput::Ok { content } => assert_eq!(content, "inner ran bash"),
        other => panic!("expected fallthrough, got {other:?}"),
    }
}

#[tokio::test]
async fn tool_search_ranks_and_labels_mcp_tools() {
    let inner = FakeInner;
    let set = full_set(&inner, std::env::temp_dir());
    let out = content(
        set.execute(
            TOOL_SEARCH,
            &serde_json::json!({"query": "+github commits"}),
        )
        .await,
    );
    let first_hit = out.lines().nth(1).expect("a first hit line");
    assert!(
        first_hit.contains("mcp__github__list_commits") && first_hit.contains("(MCP: github)"),
        "got: {out}"
    );
    assert!(
        !out.contains("mcp__linear__"),
        "+github must exclude linear tools: {out}"
    );
}

#[tokio::test]
async fn tool_search_select_form_reports_unknown_names() {
    let inner = FakeInner;
    let set = full_set(&inner, std::env::temp_dir());
    let out = content(
        set.execute(
            TOOL_SEARCH,
            &serde_json::json!({"query": "select:bash,no_such_tool"}),
        )
        .await,
    );
    assert!(out.contains("1. bash"), "got: {out}");
    assert!(out.contains("not found: no_such_tool"), "got: {out}");
}

#[tokio::test]
async fn tool_search_requires_a_query() {
    let inner = FakeInner;
    let set = full_set(&inner, std::env::temp_dir());
    assert!(
        set.execute(TOOL_SEARCH, &serde_json::json!({}))
            .await
            .is_error()
    );
    assert!(
        set.execute(TOOL_SEARCH, &serde_json::json!({"query": "  "}))
            .await
            .is_error()
    );
}

#[tokio::test]
async fn lean_mode_advertises_only_core_plus_discovery_until_a_search_activates() {
    let inner = FakeInner;
    let set = DiscoveryToolSet::new(&inner, std::env::temp_dir())
        .with_registry_search(Box::new(StubRegistry))
        .with_lean(Some(LeanConfig {
            core: ["read_file", "bash"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            discovery: true,
        }));

    let names: Vec<String> = set.schemas().into_iter().map(|s| s.name).collect();
    assert!(names.contains(&"read_file".to_string()));
    assert!(names.contains(&TOOL_SEARCH.to_string()));
    assert!(
        !names.iter().any(|n| n.starts_with("mcp__")),
        "lean mode must hide non-core tools: {names:?}"
    );

    // A search for the hidden tool activates it…
    let _ = set
        .execute(TOOL_SEARCH, &serde_json::json!({"query": "github commits"}))
        .await;
    let names: Vec<String> = set.schemas().into_iter().map(|s| s.name).collect();
    assert!(
        names.contains(&"mcp__github__list_commits".to_string()),
        "a searched tool must be advertised afterwards: {names:?}"
    );
}

/// #896: lean mode used to advertise three text-search tools and hide both
/// structural ones behind `tool_search`. A model that does not know the
/// code graph exists has no reason to search for it, so the discovery step
/// never fires and lean sessions are pinned to grep by construction.
#[test]
fn the_lean_core_advertises_the_structural_retrieval_tools() {
    let core = LeanConfig::default_core().core;
    for name in ["graph_query", "read_symbol"] {
        assert!(
            core.contains(name),
            "{name} must be advertised without a tool_search first: {core:?}"
        );
    }
    // Still a *lean* core — the text tools it competes with stay, and the
    // long tail stays behind discovery.
    assert!(core.contains("grep") && core.contains("glob"));
    assert!(!core.contains("screenshot"));
}

#[tokio::test]
async fn lean_mode_still_executes_hidden_tools_by_name() {
    let inner = FakeInner;
    let set = DiscoveryToolSet::new(&inner, std::env::temp_dir())
        .with_registry_search(Box::new(StubRegistry))
        .with_lean(Some(LeanConfig {
            core: HashSet::new(),
            discovery: true,
        }));
    match set.execute("screenshot", &Value::Null).await {
        ToolOutput::Ok { content } => assert_eq!(content, "inner ran screenshot"),
        other => panic!("hidden tools must still execute, got {other:?}"),
    }
}

#[tokio::test]
async fn lean_activation_survives_a_tool_stack_rebuild_via_the_shared_handle() {
    let inner = FakeInner;
    let activation = new_activation();
    let lean = Some(LeanConfig {
        core: HashSet::new(),
        discovery: true,
    });
    {
        let set = DiscoveryToolSet::new(&inner, std::env::temp_dir())
            .with_registry_search(Box::new(StubRegistry))
            .with_lean(lean.clone())
            .with_activation(activation.clone());
        let _ = set
            .execute(TOOL_SEARCH, &serde_json::json!({"query": "github commits"}))
            .await;
    }
    // A NEW per-turn stack with the same session handle still advertises
    // the activated tool.
    let set = DiscoveryToolSet::new(&inner, std::env::temp_dir())
        .with_registry_search(Box::new(StubRegistry))
        .with_lean(lean)
        .with_activation(activation);
    let names: Vec<String> = set.schemas().into_iter().map(|s| s.name).collect();
    assert!(names.contains(&"mcp__github__list_commits".to_string()));
}

/// Write a skill in the ecosystem layout under the given root.
fn write_skill(root: &std::path::Path, slug: &str, description: &str, body: &str) {
    let dir = root.join(".stella/skills").join(slug);
    std::fs::create_dir_all(&dir).expect("skill dir");
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {slug}\ndescription: {description}\n---\n{body}\n"),
    )
    .expect("skill file");
}

/// Run one skill_search against a workspace with HOME redirected to an
/// empty tempdir, so the developer's real user-global skills can't leak
/// into assertions. Sync on purpose: the env lock must be held across
/// the whole mutation window, and holding a `MutexGuard` across an
/// `.await` trips `clippy::await_holding_lock` — so the async execute
/// runs on a local runtime inside the sync scope instead.
fn skill_search_with_isolated_home(ws: &std::path::Path, input: Value) -> String {
    let _lock = crate::test_env::lock();
    let _restore = crate::test_env::EnvRestore::capture(&["HOME"]);
    let home = tempfile::tempdir().expect("home");
    unsafe { std::env::set_var("HOME", home.path()) };

    let inner = FakeInner;
    let set = full_set(&inner, ws.to_path_buf()).with_project_prompts_allowed(true);
    let out = tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(set.execute(SKILL_SEARCH, &input));

    content(out)
}

#[test]
fn skill_search_finds_installed_skills_and_returns_the_body_on_request() {
    let ws = tempfile::tempdir().expect("ws");
    write_skill(
        ws.path(),
        "sql-formatting",
        "How this repo formats SQL migrations",
        "Always uppercase keywords.",
    );
    write_skill(
        ws.path(),
        "release-notes",
        "Writing release notes for the changelog",
        "Lead with user impact.",
    );

    let out = skill_search_with_isolated_home(
        ws.path(),
        serde_json::json!({"query": "sql migrations", "include_body": true}),
    );

    let first_hit = out.lines().nth(1).expect("a first hit line");
    assert!(first_hit.contains("sql-formatting"), "got: {out}");
    assert!(
        out.contains("Always uppercase keywords."),
        "include_body must return the top match's instructions: {out}"
    );
}

#[test]
fn skill_search_with_no_match_points_at_the_public_registry() {
    let ws = tempfile::tempdir().expect("ws");
    let out =
        skill_search_with_isolated_home(ws.path(), serde_json::json!({"query": "kubernetes"}));
    assert!(out.contains("search_skills"), "got: {out}");
}

#[tokio::test]
async fn mcp_search_workspace_scope_merges_config_and_connected_servers() {
    let ws = tempfile::tempdir().expect("ws");
    std::fs::create_dir_all(ws.path().join(".stella")).expect("dir");
    std::fs::write(
        ws.path().join(".stella/mcp.toml"),
        "[servers.linear]\ntransport = \"http\"\nurl = \"https://mcp.linear.app/mcp\"\n\
         [servers.offline]\ntransport = \"stdio\"\ncmd = \"npx\"\n",
    )
    .expect("mcp.toml");

    let inner = FakeInner;
    let set = full_set(&inner, ws.path().to_path_buf());
    let out = content(
        set.execute(MCP_SEARCH, &serde_json::json!({"query": "linear issues"}))
            .await,
    );
    assert!(out.contains("linear"), "got: {out}");
    assert!(
        out.contains("mcp__linear__list_issues"),
        "the server's matching tools must be listed: {out}"
    );
    // `github` is connected but absent from mcp.toml — still findable.
    let out = content(
        set.execute(MCP_SEARCH, &serde_json::json!({"query": "github commits"}))
            .await,
    );
    assert!(out.contains("github"), "got: {out}");
}

#[tokio::test]
async fn mcp_search_registry_scope_lists_installable_servers() {
    let ws = tempfile::tempdir().expect("ws");
    let inner = FakeInner;
    let set = full_set(&inner, ws.path().to_path_buf());
    let out = content(
        set.execute(
            MCP_SEARCH,
            &serde_json::json!({"query": "postgres", "scope": "registry"}),
        )
        .await,
    );
    assert!(out.contains("io.github.acme/postgres-mcp"), "got: {out}");
    assert!(
        out.contains("stella mcp install io.github.acme/postgres-mcp"),
        "an install hint must be present: {out}"
    );
}

#[tokio::test]
async fn mcp_search_rejects_an_unknown_scope() {
    let inner = FakeInner;
    let set = full_set(&inner, std::env::temp_dir());
    assert!(
        set.execute(
            MCP_SEARCH,
            &serde_json::json!({"query": "x", "scope": "everywhere"})
        )
        .await
        .is_error()
    );
}

#[test]
fn lean_env_parsing_covers_all_forms() {
    let _lock = crate::test_env::lock();
    let cases: &[(&str, Option<usize>, bool)] = &[
        ("0", None, false),
        ("false", None, false),
        ("", None, false),
        ("1", Some(DEFAULT_CORE_TOOLS.len()), true),
        ("true", Some(DEFAULT_CORE_TOOLS.len()), true),
        ("read_file, bash", Some(2), true),
        ("only:read_file, bash", Some(2), false),
    ];
    for (value, expected_core_len, expected_discovery) in cases {
        unsafe { std::env::set_var(LEAN_TOOLS_ENV, value) };
        let lean = lean_from_env();
        assert_eq!(
            lean.as_ref().map(|l| l.core.len()),
            *expected_core_len,
            "for env value {value:?}"
        );
        if let Some(lean) = &lean {
            assert_eq!(
                lean.discovery, *expected_discovery,
                "for env value {value:?}"
            );
        }
    }
    unsafe { std::env::remove_var(LEAN_TOOLS_ENV) };
    assert_eq!(lean_from_env(), None);
}

/// The lean core may not withhold a tool the static prompt *commands*.
///
/// Not "every tool the prompt names" — the prompts mention plenty that
/// lean mode deliberately hides behind `tool_search`, and asserting that
/// would be asserting #3033's unbuilt design. The narrow, true property is
/// about the imperative: the prompt opens by naming its five tools, so a
/// lean core missing any of them ships instructions the session cannot
/// obey. `search` joined the core with the five-tool prompt (#3079's
/// lean-core half: the prompt now advertises it first, so a core without
/// it would be the exact mismatch this test exists to prevent).
///
/// Anti-vacuity is the first assertion: the mandate is read out of
/// `prompt.rs` at compile time, so deleting the sentence fails this test
/// rather than silently satisfying it.
#[test]
fn the_lean_core_offers_every_tool_the_prompt_commands() {
    // Relative to this file (`src/discovery/tests.rs`), so one `..` up.
    const PROMPT_SOURCE: &str = include_str!("../agent/prompt.rs");
    const MANDATE: &str = "Your tools are search, read_file, edit_file, write_file, and bash.";
    assert!(
        PROMPT_SOURCE.contains(MANDATE),
        "the prompt no longer carries {MANDATE:?} — re-derive this test \
         from whatever replaced it rather than deleting it"
    );
    let core = LeanConfig::default_core().core;
    for tool in ["search", "read_file", "edit_file", "write_file", "bash"] {
        assert!(
            core.contains(tool),
            "the prompt names {tool} in its tool mandate and the lean \
             core does not advertise it — a session whose instructions \
             name a tool it cannot see"
        );
    }
}

/// The witness for `only:` (#3100, restoring the test #3081 deleted): a
/// closed set advertises exactly its members — no discovery layer — while
/// the plain-list form keeps `tool_search`, and, the load-bearing half,
/// execution stays permissive: withholding a schema is a prompt-budget
/// measure, never a capability gate, so a closed set cannot wedge a turn
/// on a tool the model knows from history but cannot see.
#[tokio::test]
async fn an_only_set_advertises_exactly_its_members_but_still_executes_a_withheld_tool() {
    let inner = FakeInner;
    let arm = || ["read_file", "bash"].map(String::from);
    let closed = full_set(&inner, std::env::temp_dir()).with_lean(Some(LeanConfig::closed(arm())));
    let mut names: Vec<String> = closed.schemas().into_iter().map(|s| s.name).collect();
    names.sort();
    assert_eq!(
        names,
        ["bash", "read_file"],
        "an `only:` set must advertise exactly its members — `screenshot` stays hidden"
    );
    // The contrast, so the two forms can never quietly converge.
    let open = full_set(&inner, std::env::temp_dir()).with_lean(Some(LeanConfig {
        core: arm().into_iter().collect(),
        discovery: true,
    }));
    let open_names: Vec<String> = open.schemas().into_iter().map(|s| s.name).collect();
    assert!(
        open_names.contains(&TOOL_SEARCH.to_string()),
        "the plain-list form must keep the discovery layer: {open_names:?}"
    );
    match closed.execute("screenshot", &Value::Null).await {
        ToolOutput::Ok { content } => assert_eq!(content, "inner ran screenshot"),
        other => panic!("a closed set must still execute a withheld tool, got {other:?}"),
    }
}
