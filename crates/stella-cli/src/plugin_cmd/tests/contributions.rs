// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A plugin is a package: the tools, skills and records it ships (#3380), and
//! what happens to each when the package is installed, drifts, or is removed.
//!
//! Split out of `tests.rs` when that file crossed the 1500-line ceiling
//! (#4440). The fixtures it shares with the parent reach here through
//! `super::*`.

use std::path::Path;
use stella_core::ports::ToolExecutor as _;

use super::super::roster::PluginScope;
use super::super::*;
use super::*;
/// Add the three contributed directories to a package source tree.
///
/// One tool (a real spawnable program, so dispatch can be witnessed rather
/// than inferred from a schema list), one skill, one context record carrying
/// a marker the system prompt would have to contain.
fn ships_a_package(source: &Path, plugin: &str) {
    let tools = source.join(package::TOOLS_DIR);
    std::fs::create_dir_all(&tools).expect("tools dir");
    std::fs::write(
        tools.join(format!("{plugin}_review.toml")),
        format!(
            "name = \"{plugin}_review\"\n\
             description = \"review the diff the {plugin} way\"\n\
             command = [\"/bin/echo\", \"reviewed\"]\n"
        ),
    )
    .expect("tool manifest");

    let skill = source.join(package::SKILLS_DIR).join("house-style");
    std::fs::create_dir_all(&skill).expect("skill dir");
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: house-style\ndescription: how this shop writes code\n---\n\nPACKAGE_SKILL_MARKER\n",
    )
    .expect("skill file");

    let rules = source.join(package::RECORDS_DIR);
    std::fs::create_dir_all(&rules).expect("rules dir");
    std::fs::write(
        rules.join("acme.web.toml"),
        "schema = \"context-record/v0.1\"\nset_id = \"acme.web\"\n\
         \n[[record]]\nlineage_id = \"ctx.acme.web.marker\"\nkind = \"rule\"\n\
         statement = \"PACKAGE_RECORD_MARKER\"\n\
         \n[record.steering]\nforce = \"must\"\n",
    )
    .expect("record file");

    // And the manifest declares all three (#3565). Not optional decoration:
    // `install` reconciles the declaration against these very directories and
    // refuses any disagreement, so a fixture that shipped without declaring is
    // exactly the package that can no longer be installed.
    let manifest = source.join(roster::MANIFEST_FILE);
    let declared = format!(
        "{}\n\
         [[tools]]\nname = \"{plugin}_review\"\n\
         description = \"review the diff the {plugin} way\"\n\n\
         [[skills]]\nslug = \"house-style\"\n\
         description = \"how this shop writes code\"\n\n\
         [[records]]\nlineage = \"ctx.acme.web.marker\"\n\
         statement = \"PACKAGE_RECORD_MARKER\"\n",
        std::fs::read_to_string(&manifest).expect("the fixture manifest exists")
    );
    std::fs::write(&manifest, declared).expect("declared manifest");
}

/// **The reconciliation witness (#3565 item 2).** A package whose `tools/`
/// holds a manifest the declaration does not name is **refused at install**,
/// with both sides in the message and nothing copied.
///
/// The refusal is what makes `consent_text` provably complete: the document a
/// user reads is rendered from the declaration alone, so a directory holding
/// anything the declaration omits would put executable code into the agent's
/// surface that nobody consented to. Before this, a package's contributions
/// were discovered by directory convention and the manifest could not describe
/// them at all.
#[test]
fn a_package_that_ships_more_than_it_declares_is_refused_at_install() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("package-undeclared");
    let _paths = crate::paths::test_user_home(root.join("home"));
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };

    let source = package(&root, "vera");
    ships_a_package(&source, "vera");
    // One more tool than the manifest declares — the whole difference.
    std::fs::write(
        source.join(package::TOOLS_DIR).join("deploy.toml"),
        "name = \"deploy\"\ndescription = \"ship it\"\ncommand = [\"/bin/true\"]\n",
    )
    .expect("undeclared tool");

    let refusal = install(
        &root,
        &source,
        PluginScope::Project,
        true,
        &Settings::default(),
    )
    .expect_err("a package that ships an undeclared tool must be refused");
    assert!(refusal.contains("deploy"), "{refusal}");
    assert!(refusal.contains("[[tools]]"), "{refusal}");
    assert!(refusal.contains("Nothing was copied"), "{refusal}");
    assert!(
        !stella_home::resolve_project_plugins_dir(&root)
            .join("vera")
            .exists(),
        "and nothing was: a refused install leaves no directory behind"
    );

    // Declaring it is what makes the same bytes installable.
    let manifest = source.join(roster::MANIFEST_FILE);
    let declared = format!(
        "{}\n[[tools]]\nname = \"deploy\"\ndescription = \"ship it\"\n",
        std::fs::read_to_string(&manifest).expect("the fixture manifest exists")
    );
    std::fs::write(&manifest, declared).expect("declared manifest");
    install(
        &root,
        &source,
        PluginScope::Project,
        true,
        &Settings::default(),
    )
    .expect("a package that declares what it ships installs");

    let _ = std::fs::remove_dir_all(&root);
}

/// The custom-tool surface a session would assemble for `root`, gated
/// exactly as `agent::tool_stack` gates it.
fn contributed_tools(root: &Path) -> Vec<stella_tools::custom::CustomTool> {
    // Through the foundry gate, exactly as a session's discovery is: a tool a
    // package ships is no more exempt from it than one the user wrote.
    crate::tool_foundry::adopt::gate_discovery(
        stella_tools::custom::discover_with_plugins(
            root,
            None,
            true,
            &package::contributed_tool_dirs(root),
        ),
        root,
    )
    .tools
}

/// A base that answers nothing, so a name the custom layer stopped handling
/// is visibly unhandled rather than quietly served by a built-in.
struct EmptyBase;

#[async_trait::async_trait]
impl stella_core::ports::ToolExecutor for EmptyBase {
    fn schemas(&self) -> Vec<stella_protocol::tool::ToolSchema> {
        Vec::new()
    }
    async fn execute(
        &self,
        name: &str,
        _input: &serde_json::Value,
    ) -> stella_protocol::tool::ToolOutput {
        stella_protocol::tool::ToolOutput::Error {
            message: format!("no tool named `{name}`"),
            class: None,
        }
    }
}

/// **The package witness: a plugin's tool installs with it, runs as the
/// plugin, and is gone the moment the plugin is.**
///
/// Every clause fails before #3380: a package's `tools/` directory was read
/// by nothing, so the tool did not exist, could not be attributed, and had
/// no removal to survive.
///
/// The removal half is asserted through *dispatch*, not through a listing.
/// The hook lesson this loader already paid for (see the module header) is
/// that a plugin absent from a list can still be a plugin whose code runs —
/// so the assertion is that the assembled stack no longer executes it.
///
/// Synchronous with an explicit runtime, not `#[tokio::test]`: the process
/// environment lock has to be held across the dispatch, and a `MutexGuard`
/// held across an `.await` is a real hazard the lint is right about.
#[test]
fn a_plugins_tool_installs_runs_as_the_plugin_and_retracts_with_it() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("package-tool");
    let _paths = crate::paths::test_user_home(root.join("home"));
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };

    let source = package(&root, "vera");
    ships_a_package(&source, "vera");

    assert!(
        contributed_tools(&root).is_empty(),
        "anti-vacuity: nothing is contributed before install"
    );

    install(
        &root,
        &source,
        PluginScope::Project,
        true,
        &Settings::default(),
    )
    .expect("install must succeed");

    let installed = contributed_tools(&root);
    let tool = installed
        .iter()
        .find(|tool| tool.name == "vera_review")
        .expect("the package's tool joins the surface");
    assert_eq!(
        tool.contributed_by.as_deref(),
        Some("vera"),
        "and it is attributed to the package that shipped it"
    );
    assert_eq!(
        tool.principal(&stella_core::ports::Principal::User),
        stella_core::ports::Principal::Plugin("vera".into()),
        "a plugin's script is authorized as the plugin, never as the human"
    );

    // It really dispatches: the schema is advertised and the program runs.
    let base = EmptyBase;
    let stack = crate::agent::tool_stack::session_stack_with_gate(
        &base,
        installed.clone(),
        root.clone(),
        stella_tools::policy::ToolPolicy::allow_all(),
        crate::agent::tool_stack::session_gate(&root),
        stella_core::ports::Principal::User,
    );
    match runtime.block_on(stack.execute("vera_review", &serde_json::json!({}))) {
        stella_protocol::tool::ToolOutput::Ok { content, .. } => {
            assert!(content.contains("reviewed"), "{content}");
        }
        other => panic!("the contributed tool must run: {other:?}"),
    }

    remove(&root, "vera").expect("remove must succeed");

    assert!(
        contributed_tools(&root).is_empty(),
        "the contribution is derived from the package, so removing it removes them"
    );
    let after = crate::agent::tool_stack::session_stack_with_gate(
        &EmptyBase,
        contributed_tools(&root),
        root.clone(),
        stella_tools::policy::ToolPolicy::allow_all(),
        crate::agent::tool_stack::session_gate(&root),
        stella_core::ports::Principal::User,
    );
    match runtime.block_on(after.execute("vera_review", &serde_json::json!({}))) {
        stella_protocol::tool::ToolOutput::Error { message, .. } => {
            assert!(message.contains("no tool named"), "{message}");
        }
        other => panic!("a removed plugin's tool must stop dispatching: {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// **The end-to-end witness #3579 asked for and #4301 carried forward.** A
/// package ships its own executable script, names it through `${plugin_dir}`,
/// and the script's own stdout comes back through dispatch.
///
/// The test above proves the wiring with `/bin/echo` — an absolute path that
/// would still run if the placeholder expanded to nothing, or to the wrong
/// directory. This one cannot: the script exists only inside the installed
/// package, so either mistake is a spawn failure rather than a green run. It
/// also covers the `[env]` half of the expansion, which nothing reached.
#[cfg(unix)]
#[test]
fn a_packages_own_script_runs_through_the_expanded_plugin_dir() {
    use std::os::unix::fs::PermissionsExt;

    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("package-script");
    let _paths = crate::paths::test_user_home(root.join("home"));
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };

    let source = package(&root, "vera");
    let scripts = source.join("scripts");
    std::fs::create_dir_all(&scripts).expect("scripts dir");
    let script = scripts.join("review.sh");
    std::fs::write(&script, "#!/bin/sh\necho \"ran $0 RULESET=$RULESET\"\n").expect("script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");

    let tools = source.join(package::TOOLS_DIR);
    std::fs::create_dir_all(&tools).expect("tools dir");
    std::fs::write(
        tools.join("vera_review.toml"),
        "name = \"vera_review\"\n\
         description = \"review the diff the vera way\"\n\
         command = [\"${plugin_dir}/scripts/review.sh\"]\n\
         \n[env]\nRULESET = \"${plugin_dir}/rules/strict.yaml\"\n",
    )
    .expect("tool manifest");

    let manifest = source.join(roster::MANIFEST_FILE);
    let declared = format!(
        "{}\n[[tools]]\nname = \"vera_review\"\n\
         description = \"review the diff the vera way\"\n",
        std::fs::read_to_string(&manifest).expect("the fixture manifest exists")
    );
    std::fs::write(&manifest, declared).expect("declared manifest");

    install(
        &root,
        &source,
        PluginScope::Project,
        true,
        &Settings::default(),
    )
    .expect("install must succeed");

    let stack = crate::agent::tool_stack::session_stack_with_gate(
        &EmptyBase,
        contributed_tools(&root),
        root.clone(),
        stella_tools::policy::ToolPolicy::allow_all(),
        crate::agent::tool_stack::session_gate(&root),
        stella_core::ports::Principal::User,
    );
    let dir = stella_home::resolve_project_plugins_dir(&root).join("vera");
    let dir = dir.to_string_lossy().into_owned();
    match runtime.block_on(stack.execute("vera_review", &serde_json::json!({}))) {
        stella_protocol::tool::ToolOutput::Ok { content, .. } => {
            assert!(
                content.contains(&format!("ran {dir}/scripts/review.sh")),
                "the script did not run from the installed package: {content}"
            );
            assert!(
                content.contains(&format!("RULESET={dir}/rules/strict.yaml")),
                "the `[env]` placeholder was not expanded: {content}"
            );
        }
        other => panic!("the package's own script must run: {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&root);
}

/// **The re-check witness.** `reconcile` used to run only once, from
/// `stella plugin install` — which makes the consent document "provably
/// complete" true for exactly one instant. A plugin runs as an ordinary
/// subprocess with no filesystem sandbox beyond the env-var allowlist, so its
/// own process can write a new `tools/*.toml` into its own installed
/// directory at any point after install completes, with no new consent
/// transaction anywhere on the path. This test simulates exactly that write
/// rather than trusting a hostile plugin's process to demonstrate it.
///
/// Before the fix, `contributed_tools` reads the installed directory straight
/// off disk and the `backdoor` assertion fails. After the fix, a plugin whose
/// directories no longer agree with its manifest contributes nothing at all —
/// not even `vera_review`, which it still declares truthfully — because a
/// package that grew one undeclared entry is not a package whose other
/// entries can still be trusted to be the ones a human consented to.
#[test]
fn a_plugin_that_drifts_from_its_declaration_after_install_contributes_nothing() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("package-drift");
    let _paths = crate::paths::test_user_home(root.join("home"));
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };

    let source = package(&root, "vera");
    ships_a_package(&source, "vera");
    install(
        &root,
        &source,
        PluginScope::Project,
        true,
        &Settings::default(),
    )
    .expect("install must succeed");

    assert_eq!(
        contributed_tools(&root)
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>(),
        vec!["vera_review".to_string()],
        "the declared tool loads normally right after install"
    );

    // What a hostile plugin's own subprocess can do at any time while it
    // runs: write a new file into its own installed `tools/` directory.
    let installed_dir = stella_home::resolve_project_plugins_dir(&root).join("vera");
    std::fs::write(
        installed_dir.join(package::TOOLS_DIR).join("backdoor.toml"),
        "name = \"backdoor\"\ndescription = \"not consented to\"\ncommand = [\"/bin/true\"]\n",
    )
    .expect("simulated post-install write");

    let after = contributed_tools(&root);
    assert!(
        after.iter().all(|tool| tool.name != "backdoor"),
        "an undeclared tool written after install must never be loaded: {after:?}"
    );
    assert!(
        after.iter().all(|tool| tool.name != "vera_review"),
        "a package that drifted from its declaration is withheld whole, not just the \
         undeclared entry: {after:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **The clone witness, extended to the sharpest contribution (#3509 +
/// #3380).** A contributed tool is executable code that a `git clone`
/// carried in, so it must sit behind exactly the same trust gate the
/// package's `[runtime]` process does — and it does, because there is no
/// path to a contribution that does not go through the roster.
#[test]
fn an_untrusted_projects_contributed_tool_never_loads() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("package-untrusted");
    let _paths = crate::paths::test_user_home(root.join("home"));

    let planted = stella_home::resolve_project_plugins_dir(&root).join("vera");
    plant(&stella_home::resolve_project_plugins_dir(&root), "vera");
    ships_a_package(&planted, "vera");

    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe {
        std::env::remove_var("STELLA_TRUST_PROJECT");
        std::env::remove_var("STELLA_PROJECT_HOOKS");
    }
    assert!(
        package::contributed_tool_dirs(&root).is_empty(),
        "an untrusted checkout contributes no tool directory at all"
    );
    assert!(
        contributed_tools(&root).is_empty(),
        "so no tool is discovered from it"
    );

    // SAFETY: as above.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };
    assert_eq!(
        contributed_tools(&root)
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>(),
        vec!["vera_review".to_string()],
        "the same bytes load once the operator trusts the workspace"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **The record witness.** A context record a plugin ships steers this
/// workspace's session, and stops the moment the plugin is removed — the
/// same derive-never-copy guarantee, on the surface where "left behind"
/// would mean a third party's policy still shaping the prompt.
#[test]
fn a_plugins_context_record_steers_and_stops_on_remove() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("package-record");
    let _paths = crate::paths::test_user_home(root.join("home"));
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };

    let authority = crate::settings::AuthorityPolicy {
        project_prompts_allowed: true,
        ..Default::default()
    };
    let steered = |root: &Path| {
        crate::rules::load_workspace_rules(root, &authority)
            .registry()
            .entries
            .iter()
            .any(|entry| {
                entry
                    .record
                    .record
                    .statement
                    .contains("PACKAGE_RECORD_MARKER")
            })
    };

    assert!(
        !steered(&root),
        "anti-vacuity: nothing steers before install"
    );

    let source = package(&root, "vera");
    ships_a_package(&source, "vera");
    install(
        &root,
        &source,
        PluginScope::Project,
        true,
        &Settings::default(),
    )
    .expect("install must succeed");
    assert!(
        steered(&root),
        "a package's record must reach the registry the session is steered by"
    );

    remove(&root, "vera").expect("remove must succeed");
    assert!(
        !steered(&root),
        "and must stop the moment the package is gone — nothing was copied into .stella/rules"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **The skill witness.** Same shape, on the surface recall injects from.
#[test]
fn a_plugins_skill_is_selectable_and_stops_on_remove() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("package-skill");
    let _paths = crate::paths::test_user_home(root.join("home"));
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };

    let loaded = |root: &Path| {
        crate::memory::load_workspace_skills_with_authority(root, true)
            .skills
            .iter()
            .any(|skill| skill.name == "house-style")
    };
    assert!(!loaded(&root), "anti-vacuity");

    let source = package(&root, "vera");
    ships_a_package(&source, "vera");
    install(
        &root,
        &source,
        PluginScope::Project,
        true,
        &Settings::default(),
    )
    .expect("install must succeed");
    assert!(loaded(&root), "a package's skill joins the selectable set");

    remove(&root, "vera").expect("remove must succeed");
    assert!(
        !loaded(&root),
        "and leaves with it — the skill lived in the package, not in .stella/skills"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **The listing witness** (#4734). A skill that steers a turn has to be
/// visible in the tab a user opens to ask what is steering them, saying whose
/// it is — a contributed skill used to be absent from the listing entirely and
/// to report the origin `workspace`, which is the user's own.
///
/// Asserted through `skill_manager::enumerate` — the rows the SKILLS tab
/// renders — rather than through the loader, because the loader half already
/// passes above and passed while the tab showed nothing.
#[test]
fn a_plugins_skill_is_listed_as_the_plugins_and_is_not_removable_there() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("package-skill-listing");
    let _paths = crate::paths::test_user_home(root.join("home"));
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };

    let listed = |root: &Path| {
        crate::skill_manager::enumerate(root)
            .into_iter()
            .find(|row| row.name == "house-style")
    };
    assert!(listed(&root).is_none(), "anti-vacuity");

    let source = package(&root, "vera");
    ships_a_package(&source, "vera");
    install(
        &root,
        &source,
        PluginScope::Project,
        true,
        &Settings::default(),
    )
    .expect("install must succeed");

    let row = listed(&root).expect("a package's skill appears in the skills listing");
    assert_eq!(
        row.contributed_by.as_deref(),
        Some("vera"),
        "and names the package that shipped it"
    );
    assert_eq!(
        row.origin, "plugin",
        "not `workspace` — that is a claim to be the user's own"
    );
    assert!(
        !row.removable,
        "uninstall unlinks an entry under a scope root and this has none; \
         retraction is `stella plugin remove`"
    );
    assert!(row.enabled, "and it is in force until switched off");

    // The tab's own switch reaches it: the state file that governs the tier
    // the package is installed at governs its contributed skills too, so the
    // row can be turned off and the recall path stops injecting it.
    crate::skill_manager::set_enabled(
        stella_tui::SkillScope::Project,
        "house-style",
        false,
        &root,
    )
    .expect("disable must succeed");
    assert!(
        !listed(&root).expect("still listed while disabled").enabled,
        "the row shows it off rather than dropping it"
    );
    assert!(
        !crate::memory::load_workspace_skills_with_authority(&root, true)
            .skills
            .iter()
            .any(|skill| skill.name == "house-style"),
        "and a disabled contributed skill stops reaching the prompt"
    );

    remove(&root, "vera").expect("remove must succeed");
    assert!(listed(&root).is_none(), "and leaves the listing with it");

    let _ = std::fs::remove_dir_all(&root);
}

/// **The precedence witness.** A package may not silently take over a skill
/// the user wrote: the user's body is the one that loads, and the plugin's
/// same-named skill is dropped.
#[test]
fn a_plugins_skill_never_displaces_one_the_user_wrote() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("package-skill-precedence");
    let _paths = crate::paths::test_user_home(root.join("home"));
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };

    let mine = root.join(".stella").join("skills").join("house-style");
    std::fs::create_dir_all(&mine).expect("my skill dir");
    std::fs::write(
        mine.join("SKILL.md"),
        "---\nname: house-style\ndescription: mine\n---\n\nMY_OWN_BODY\n",
    )
    .expect("my skill");

    let source = package(&root, "vera");
    ships_a_package(&source, "vera");
    install(
        &root,
        &source,
        PluginScope::Project,
        true,
        &Settings::default(),
    )
    .expect("install must succeed");

    let skills = crate::memory::load_workspace_skills_with_authority(&root, true).skills;
    let held: Vec<&stella_core::skills::Skill> = skills
        .iter()
        .filter(|skill| skill.name == "house-style")
        .collect();
    assert_eq!(held.len(), 1, "one name, one skill: {held:?}");
    assert!(
        held[0].body.contains("MY_OWN_BODY"),
        "the user's own skill is the one that loads: {}",
        held[0].body
    );

    let _ = std::fs::remove_dir_all(&root);
}
