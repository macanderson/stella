//! `extensions.rs`'s tests.
//!
//! Split into their own file when the module crossed the 1500-line ceiling
//! (#5232 added the invocation-reporting seam and its witness); the parent is
//! the loaders and the sync, this is everything that exercises them.

use super::*;

fn write(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn skill_md(name: &str) -> String {
    format!("---\nname: {name}\ndescription: about {name}\n---\nbody of {name}\n")
}

#[test]
fn sync_adopts_real_entries_and_skips_symlinked_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // `.agents/skills/shared` is the real home; `.claude/skills/shared`
    // is another agent's symlink to it — the exact in-the-wild shape.
    write(
        &root.join(".agents/skills/shared/SKILL.md"),
        &skill_md("shared"),
    );
    write(
        &root.join(".claude/skills/local/SKILL.md"),
        &skill_md("local"),
    );
    std::os::unix::fs::symlink(
        root.join(".agents/skills/shared"),
        root.join(".claude/skills/shared"),
    )
    .unwrap();
    write(
        &root.join(".claude/commands/deploy.md"),
        "---\ndescription: ship it\n---\nDeploy $ARGUMENTS now.",
    );
    write(
        &root.join(".claude/agents/reviewer.md"),
        "---\nname: reviewer\ndescription: reviews\n---\nYou review.",
    );

    let outcome = sync_into(
        &root.join(".stella"),
        &[root.join(".claude"), root.join(".agents")],
    );
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    let mut linked = outcome.linked.clone();
    linked.sort_by(|a, b| a.1.cmp(&b.1));
    // Flat files keep their `.md` basename so the loaders' stem-derived
    // slugs survive the link; skill directories link by directory name.
    assert_eq!(
        linked,
        vec![
            (ExtensionKind::Commands, "deploy.md".to_string()),
            (ExtensionKind::Skills, "local".to_string()),
            (ExtensionKind::Agents, "reviewer.md".to_string()),
            (ExtensionKind::Skills, "shared".to_string()),
        ]
    );

    // `shared` was adopted from its real `.agents` home, not through the
    // `.claude` symlink — and as a relative link.
    let link = root.join(".stella/skills/shared");
    let target = std::fs::read_link(&link).unwrap();
    assert!(target.is_relative());
    assert_eq!(
        std::fs::canonicalize(&link).unwrap(),
        std::fs::canonicalize(root.join(".agents/skills/shared")).unwrap()
    );
    // The adopted definitions are readable through the links.
    assert!(
        std::fs::read_to_string(root.join(".stella/skills/shared/SKILL.md"))
            .unwrap()
            .contains("body of shared")
    );
}

/// The witness for namespaced commands. This is the shape issue #104
/// documented as *unloadable* — `.claude/commands/vercel/deploy.md` with
/// no `vercel.md` and no `vercel/COMMAND.md`. It was unloadable because
/// nothing read it; now `read_command_files` does, so the sync links it
/// and it invokes as `/vercel:deploy`.
#[test]
fn a_namespace_directory_is_adopted_and_loads_as_ns_colon_name() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(
        &root.join(".claude/commands/vercel/deploy.md"),
        "---\nname: deploy\n---\nDeploy $ARGUMENTS.",
    );
    write(&root.join(".claude/commands/build.md"), "Build it.");

    let outcome = sync_into(
        &root.join(".stella"),
        &[root.join(".claude"), root.join(".agents")],
    );
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    assert!(
        outcome
            .linked
            .contains(&(ExtensionKind::Commands, "vercel".to_string())),
        "the namespace directory is adopted now: {:?}",
        outcome.linked
    );
    assert!(
        outcome.unloadable.is_empty(),
        "nothing is unloadable any more: {:?}",
        outcome.unloadable
    );
    assert!(root.join(".stella/commands/vercel").exists());

    let mut problems = Vec::new();
    let commands = load_commands_from(&[root.join(".stella/commands")], &mut problems);
    assert!(problems.is_empty(), "{problems:?}");
    let deploy = commands
        .iter()
        .find(|c| c.invocation() == "vercel:deploy")
        .unwrap_or_else(|| {
            panic!(
                "no /vercel:deploy in {:?}",
                commands.iter().map(|c| c.invocation()).collect::<Vec<_>>()
            )
        });
    assert_eq!(deploy.name, "deploy", "the bare name keeps no prefix");
    assert_eq!(deploy.namespace.as_deref(), Some("vercel"));
}

/// Two namespaces may hold the same bare name — merging on the bare name
/// would silently drop whichever loaded second.
#[test]
fn the_same_command_name_in_two_namespaces_is_two_commands() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("commands");
    write(&dir.join("vercel/deploy.md"), "Ship to vercel.");
    write(&dir.join("fly/deploy.toml"), "prompt = \"Ship to fly.\"");

    let mut problems = Vec::new();
    let commands = load_commands_from(&[dir], &mut problems);
    assert!(problems.is_empty(), "{problems:?}");
    let mut names: Vec<String> = commands.iter().map(|c| c.invocation()).collect();
    names.sort();
    assert_eq!(names, vec!["fly:deploy", "vercel:deploy"]);
}

/// A directory holding `COMMAND.md` is still ONE command, not a namespace
/// — the two directory shapes are told apart by content, not by naming.
#[test]
fn a_nested_command_directory_is_not_read_as_a_namespace() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("commands");
    write(&dir.join("deploy/COMMAND.md"), "Ship it.");
    write(&dir.join("deploy/notes.md"), "Not a command.");

    let mut problems = Vec::new();
    let commands = load_commands_from(&[dir], &mut problems);
    let names: Vec<String> = commands.iter().map(|c| c.invocation()).collect();
    assert_eq!(names, vec!["deploy"], "{problems:?}");
}

#[test]
fn a_toml_command_loads_alongside_markdown() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("commands");
    write(&dir.join("build.md"), "Build it.");
    write(
        &dir.join("review.toml"),
        "description = \"Review a PR\"\nargument-hint = \"<pr>\"\nprompt = \"Review $1.\"\n",
    );

    let mut problems = Vec::new();
    let commands = load_commands_from(&[dir], &mut problems);
    assert!(problems.is_empty(), "{problems:?}");
    let review = commands.iter().find(|c| c.name == "review").unwrap();
    assert_eq!(review.argument_hint.as_deref(), Some("<pr>"));
    assert_eq!(expand_command(&review.body, "142"), "Review 142.");
    assert!(commands.iter().any(|c| c.name == "build"));
}

#[test]
fn sync_reports_an_unloadable_namespace_directory_even_when_nothing_else_is_found() {
    // A workspace whose ONLY entry is a directory the loader cannot read:
    // `outcome.summary()` is `None` since nothing linked, so this must not
    // go silent (issue #104's actual bug).
    //
    // Uses SKILLS, not commands: a commands namespace directory is
    // loadable now (`/vercel:deploy`), while skills and agents have no
    // namespace syntax and keep the stricter rule. The guard being
    // protected here is the reporting path, not the kind.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join(".claude/skills/vercel/deploy.md"), "Deploy.");

    let outcome = sync_into(
        &root.join(".stella"),
        &[root.join(".claude"), root.join(".agents")],
    );
    assert!(outcome.linked.is_empty());
    assert!(outcome.summary().is_none());
    assert_eq!(outcome.unloadable.len(), 1, "{:?}", outcome.unloadable);

    let mut lines = Vec::new();
    emit_sync_outcome("workspace", &outcome, &mut |line| lines.push(line));
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("vercel"));
    assert!(lines[0].contains("not loadable"));
    assert!(lines[0].contains("workspace scope"));
}

#[test]
fn sync_is_idempotent_and_never_clobbers_user_definitions() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(&root.join(".claude/commands/deploy.md"), "Ship.");
    // A user-authored definition already occupies the name.
    write(&root.join(".stella/commands/deploy.md"), "MY deploy.");

    let dest = root.join(".stella");
    let sources = vec![root.join(".claude"), root.join(".agents")];
    let first = sync_into(&dest, &sources);
    assert!(
        first.linked.is_empty(),
        "existing names are never clobbered"
    );
    assert_eq!(
        std::fs::read_to_string(root.join(".stella/commands/deploy.md")).unwrap(),
        "MY deploy."
    );

    write(&root.join(".claude/commands/fresh.md"), "New.");
    let second = sync_into(&dest, &sources);
    assert_eq!(second.linked.len(), 1);
    let third = sync_into(&dest, &sources);
    assert!(third.linked.is_empty(), "re-running links nothing new");
}

/// **Witness (#3675).** `sync_extensions` symlinks into the user tier it
/// is *given*, and touches none at all when given none.
///
/// The witness is the signature: on the base commit this function resolves
/// `paths::home()` and `user_config_root()` inside its own body, so there
/// is no tier to name and this does not compile — which is the defect,
/// because it means every test that drove `agent::init_workspace` created
/// symlinks under the developer's real `~/.stella/{commands,skills,agents}/`
/// however carefully the workspace root was sandboxed. The assertions are
/// what pin the behaviour once it does.
#[test]
fn sync_adopts_into_the_user_tier_it_is_given_and_none_otherwise() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("ws");
    let home = tmp.path().join("home");
    let stella_root = tmp.path().join("elsewhere/.stella");
    write(&home.join(".claude/commands/deploy.md"), "Ship.");

    let mut lines: Vec<String> = Vec::new();
    sync_extensions(&workspace, None, &mut |line| lines.push(line));
    assert!(
        !stella_root.exists() && !home.join(".stella").exists(),
        "no tier given, no tier written: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.contains("user scope")),
        "and nothing claims to have adopted one: {lines:?}"
    );

    lines.clear();
    sync_extensions(
        &workspace,
        Some(UserScope {
            home: &home,
            stella_root: &stella_root,
        }),
        &mut |line| lines.push(line),
    );
    assert!(
        stella_root.join("commands/deploy.md").exists(),
        "the link lands in the NAMED root, not beside the source: {lines:?}"
    );
    assert!(
        !home.join(".stella").exists(),
        "and the root is not derived from the home — `STELLA_HOME` moves \
         one and not the other"
    );
}

#[test]
fn sync_resolves_frontmatter_name_collisions_by_source_precedence() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    // Different file names, same frontmatter `name: deploy` — only the
    // earlier source (.claude) is adopted, so the loaded `/deploy` is
    // decided by source precedence, not destination file-name order.
    write(
        &root.join(".claude/commands/ship.md"),
        "---\nname: deploy\ndescription: from claude\n---\nShip.",
    );
    write(
        &root.join(".agents/commands/release.md"),
        "---\nname: deploy\ndescription: from agents\n---\nRelease.",
    );
    let outcome = sync_into(
        &root.join(".stella"),
        &[root.join(".claude"), root.join(".agents")],
    );
    assert_eq!(
        outcome.linked,
        vec![(ExtensionKind::Commands, "ship.md".to_string())]
    );
    assert_eq!(outcome.skipped, 1);
}

#[test]
fn relative_target_walks_up_and_back_down() {
    assert_eq!(
        relative_symlink_target(
            Path::new("/ws/.stella/skills"),
            Path::new("/ws/.agents/skills/x"),
        ),
        PathBuf::from("../../.agents/skills/x")
    );
    assert_eq!(
        relative_symlink_target(Path::new("/a/b"), Path::new("/a/b/c")),
        PathBuf::from("c")
    );
}

#[test]
fn loads_commands_and_agents_with_workspace_precedence() {
    let tmp = tempfile::tempdir().unwrap();
    let user = tmp.path().join("user-scope");
    let ws = tmp.path().join("ws-scope");
    write(
        &user.join("deploy.md"),
        "---\ndescription: user version\n---\nuser body",
    );
    write(
        &ws.join("deploy.md"),
        "---\ndescription: workspace version\n---\nworkspace body",
    );
    write(&ws.join("review/COMMAND.md"), "Review the diff.");

    let mut problems = Vec::new();
    let commands = load_commands_from(&[user.clone(), ws.clone()], &mut problems);
    let deploy = commands.iter().find(|c| c.name == "deploy").unwrap();
    assert_eq!(deploy.description, "workspace version");
    assert!(
        commands.iter().any(|c| c.name == "review"),
        "nested layout loads"
    );
    assert!(problems.is_empty(), "{problems:?}");

    let agent_dir = tmp.path().join("ws-agents");
    write(
        &agent_dir.join("reviewer.md"),
        "---\nname: reviewer\ndescription: reviews\n---\nYou review.",
    );
    let agents = load_agents_from(&[agent_dir], &mut problems);
    assert_eq!(agents.len(), 1, "flat .md files parse as agents");
    assert_eq!(agents[0].name, "reviewer");
}

#[test]
fn untrusted_project_extensions_are_excluded_while_user_extensions_remain() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let workspace = tmp.path().join("workspace");
    write(
        &home.join(".stella/commands/user.md"),
        "---\ndescription: user command\n---\nUSER_COMMAND_BODY",
    );
    write(
        &workspace.join(".stella/commands/project.md"),
        "---\ndescription: project command\n---\nPROJECT_COMMAND_BODY",
    );
    write(
        &home.join(".stella/agents/user.md"),
        "---\nname: user-agent\ndescription: user agent\n---\nUSER_AGENT_BODY",
    );
    write(
        &workspace.join(".stella/agents/project.md"),
        "---\nname: project-agent\ndescription: project agent\n---\nPROJECT_AGENT_BODY",
    );
    let _home = crate::paths::test_user_home(home.clone());

    let custom = CustomExtensions::load_with_authority(
        &workspace,
        &crate::settings::AuthorityPolicy::default(),
    );
    let trusted = CustomExtensions::load_with_authority(
        &workspace,
        &crate::settings::AuthorityPolicy {
            project_prompts_allowed: true,
            ..crate::settings::AuthorityPolicy::default()
        },
    );

    let names: Vec<&str> = custom
        .commands
        .iter()
        .map(|command| command.name.as_str())
        .collect();
    assert_eq!(names, vec!["user"], "loaded commands: {names:?}");
    let trusted_names: Vec<&str> = trusted
        .commands
        .iter()
        .map(|command| command.name.as_str())
        .collect();
    assert_eq!(trusted_names, vec!["user", "project"]);
    let agent_names: Vec<&str> = custom
        .agents
        .iter()
        .map(|agent| agent.name.as_str())
        .collect();
    assert_eq!(agent_names, vec!["user-agent"]);
    let trusted_agent_names: Vec<&str> = trusted
        .agents
        .iter()
        .map(|agent| agent.name.as_str())
        .collect();
    assert_eq!(trusted_agent_names, vec!["user-agent", "project-agent"]);
}

#[test]
fn loader_reports_malformed_and_unreadable_definitions() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("commands");
    write(&dir.join("empty.md"), "---\nname: empty\n---\n");
    // A dangling symlink: the file exists as an entry but cannot be read.
    std::os::unix::fs::symlink(tmp.path().join("gone.md"), dir.join("dangling.md")).unwrap();

    let mut problems = Vec::new();
    let commands = load_commands_from(&[dir], &mut problems);
    assert!(commands.is_empty());
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("empty body")));
    assert!(problems.iter().any(|p| p.contains("dangling.md")));
}

/// A command carrying none of the optional parity fields — the shape a
/// plain prompt file loads as, and what these menu/collision tests are
/// actually about.
fn bare_command(name: &str, description: &str, body: &str, source: &str) -> CommandDef {
    CommandDef {
        name: name.to_string(),
        namespace: None,
        description: description.to_string(),
        argument_hint: None,
        allowed_tools: None,
        model: None,
        model_invocable: true,
        body: body.to_string(),
        source_path: source.to_string(),
    }
}

fn custom_fixture() -> CustomExtensions {
    CustomExtensions {
        commands: vec![bare_command(
            "fix-bug",
            "fix the named bug",
            "Fix $ARGUMENTS end to end.",
            "x/fix-bug.md",
        )],
        skills: vec![Skill {
            name: "sql-style".to_string(),
            description: "format sql".to_string(),
            domains: vec![],
            body: "Lowercase keywords.".to_string(),
            source_path: "x/sql-style/SKILL.md".to_string(),
            origin: stella_core::skills::SkillOrigin::Workspace,
            contributed_by: None,
        }],
        agents: vec![AgentDef {
            name: "reviewer".to_string(),
            description: "reviews diffs".to_string(),
            tools: None,
            model: None,
            body: "You review.".to_string(),
            source_path: "x/reviewer.md".to_string(),
        }],
        problems: Vec::new(),
    }
}

#[test]
fn slash_entries_are_custom_rows_that_never_shadow_builtins() {
    let mut custom = custom_fixture();
    // collides with the builtin /help
    custom
        .commands
        .push(bare_command("help", "shadowed", "body", "x/help.md"));
    let reserved = vec![SlashCommand::new("/help", "show commands")];
    let rows = custom.slash_entries(&reserved);
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    // Commands, then skills, then agents — the fixture's `reviewer`
    // agent is offered as an invocable ⚡ row too.
    assert_eq!(names, vec!["/fix-bug", "/sql-style", "/reviewer"]);
    assert!(rows.iter().all(|r| r.kind == stella_tui::SlashKind::Custom));
}

#[test]
fn agent_invocations_expand_to_the_persona_prompt() {
    let custom = custom_fixture();
    let prompt = custom
        .expand("/reviewer check the diff", &[])
        .expect("the agent is invocable");
    assert!(prompt.contains("# Agent: reviewer"), "{prompt}");
    assert!(prompt.contains("You review."), "{prompt}");
    assert!(prompt.contains("## Task\ncheck the diff"), "{prompt}");
    // Commands and skills still shadow agents by name.
    match custom.lookup("/reviewer") {
        Some(Invocation::Agent(agent)) => assert_eq!(agent.name, "reviewer"),
        _ => panic!("expected the agent invocation"),
    }
}

#[test]
fn expand_substitutes_command_arguments() {
    let custom = custom_fixture();
    assert_eq!(
        custom.expand("/fix-bug issue-42", &[]).as_deref(),
        Some("Fix issue-42 end to end.")
    );
    assert!(custom.expand("/unknown thing", &[]).is_none());
}

#[test]
fn expand_never_runs_a_custom_definition_under_a_reserved_name() {
    let mut custom = custom_fixture();
    custom
        .commands
        .push(bare_command("help", "shadowed", "hijacked", "x/help.md"));
    // Hidden from the menu AND unreachable at invocation time — the
    // argument-carrying form included, which bypasses whole-input
    // builtin matching in both surfaces.
    assert!(custom.expand("/help", &["/help"]).is_none());
    assert!(custom.expand("/help topic", &["/help"]).is_none());
    // Unreserved names still expand.
    assert!(custom.expand("/fix-bug x", &["/help"]).is_some());
}

#[test]
fn expand_wraps_a_skill_invocation_with_its_body_and_task() {
    let custom = custom_fixture();
    let prompt = custom.expand("/sql-style tidy my query", &[]).unwrap();
    assert!(prompt.contains("# Skill: sql-style"));
    assert!(prompt.contains("Lowercase keywords."));
    assert!(prompt.contains("## Task\ntidy my query"));
    // Bare invocation: no task section.
    let bare = custom.expand("/sql-style", &[]).unwrap();
    assert!(!bare.contains("## Task"));
}

/// **The witness (#5232).** A `/slug` expansion hands back the skill it
/// used, so the caller can record that it was used.
///
/// Fails on the base commit for the reason the issue names: there was no
/// second return value at all. `expand` produced prompt text and told
/// nobody a skill had produced it, so an explicitly invoked skill entered
/// the prompt without an event — and `skill_usage`, which appraisal reads
/// before retiring a skill, counts only what the auto path reports.
#[test]
fn an_invoked_skill_is_handed_back_to_be_recorded() {
    let custom = custom_fixture();

    let expansion = custom.expansion("/sql-style tidy my query", &[]).unwrap();
    let skill = expansion
        .skill
        .expect("a skill invocation reports the skill it invoked");
    assert_eq!(skill.name, "sql-style");
    assert_eq!(skill.summary, "format sql");
    assert!(
        skill.tokens > 0,
        "the expansion put a body in the prompt and it cost something"
    );

    // The two invocation kinds that are not a skill report none, so a
    // caller cannot bill a command or a persona as skill usage.
    assert!(
        custom
            .expansion("/fix-bug issue-42", &[])
            .unwrap()
            .skill
            .is_none()
    );
    assert!(
        custom
            .expansion("/reviewer check the diff", &[])
            .unwrap()
            .skill
            .is_none()
    );
}

#[test]
fn agent_list_names_every_agent_and_hints_when_empty() {
    let custom = custom_fixture();
    let list = custom.render_agent_list();
    assert!(list.contains("⚡ reviewer — reviews diffs"));
    let empty = CustomExtensions::default();
    assert!(empty.render_agent_list().contains("no custom agents"));
}

/// **Witness (#3864).** One loader, one policy: with the user extension
/// tier hidden, `CustomExtensions` reads no user-scope command or agent —
/// exactly as it already read no user-scope skill.
///
/// The redirect installs a real root and then hides the tier, so the
/// assertion separates "the loader honours the policy" from "there was
/// nothing in that directory". Fails on the base commit: commands and
/// agents resolved through `paths::stella_root`, which ignores the policy
/// and answers with the developer's own `~/.stella` in a test build —
/// while skills, resolving through `paths::user_extension_root`, correctly
/// saw no user tier at all.
#[test]
fn a_hidden_user_tier_hides_commands_and_agents_as_it_already_hid_skills() {
    let user = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    write(
        &user.path().join(".stella/commands/deploy.md"),
        "---\ndescription: ship it\n---\nDeploy $ARGUMENTS now.",
    );
    write(
        &user.path().join(".stella/agents/reviewer.md"),
        "---\nname: reviewer\ndescription: reviews\n---\nYou review.",
    );
    write(
        &user.path().join(".stella/skills/sql-style/SKILL.md"),
        &skill_md("sql-style"),
    );

    // The control: with the tier visible, all three load, so the
    // assertions below cannot pass because the fixture is empty.
    let _home = crate::paths::test_user_home(user.path().to_path_buf());
    let visible = CustomExtensions::load_with_workspace_extensions(workspace.path(), false);
    assert_eq!(visible.commands.len(), 1, "premise: the command is there");
    assert_eq!(visible.agents.len(), 1, "premise: the agent is there");
    assert_eq!(visible.skills.len(), 1, "premise: the skill is there");

    let _hidden = crate::paths::test_extensions_visible(false);
    let loaded = CustomExtensions::load_with_workspace_extensions(workspace.path(), false);
    assert!(
        loaded.commands.is_empty(),
        "a hidden user tier must yield no commands, got {:?}",
        loaded
            .commands
            .iter()
            .map(|c| c.invocation())
            .collect::<Vec<_>>()
    );
    assert!(
        loaded.agents.is_empty(),
        "and no agents — `stella agents` installs into this same directory"
    );
    assert!(
        loaded.skills.is_empty(),
        "premise: skills already honoured the policy"
    );
}
