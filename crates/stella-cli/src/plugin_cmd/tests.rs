// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The loader's round trip, over real directories.
//!
//! These drive `install`/`remove` against a temporary workspace root rather
//! than the process's cwd, so nothing here reads or writes the developer's
//! own `.stella/`.

use std::collections::BTreeMap;

use std::path::{Path, PathBuf};
use stella_core::ports::ToolExecutor as _;

use super::roster::{PluginRoster, PluginScope};
use super::*;

/// The manifest text of the hostile package in #3509's repro: an `arbiter`
/// that arbitrates the loop and spawns a process at two hook points.
fn manifest_text(name: &str) -> String {
    format!(
        "name = \"{name}\"\n\
         description = \"a fixture\"\n\n\
         [loop]\nparticipation = \"arbiter\"\nhooks = [\"PreToolUse\", \"Stop\"]\n\n\
         [requirements]\nr = \"the tests pass\"\n\n\
         [runtime]\nargv = [\"python3\", \"${{plugin_dir}}/main.py\"]\ntimeout_secs = 30\nenv = [\"PATH\"]\n"
    )
}

/// A manifest that declares both halves of a dispatch: the grant, and the
/// process to dispatch into.
fn package(dir: &Path, name: &str) -> PathBuf {
    let source = dir.join(format!("src-{name}"));
    std::fs::create_dir_all(source.join("lib")).expect("fixture dirs");
    std::fs::write(source.join(roster::MANIFEST_FILE), manifest_text(name))
        .expect("fixture manifest");
    std::fs::write(source.join("main.py"), "print('hi')\n").expect("fixture entrypoint");
    std::fs::write(source.join("lib").join("helper.py"), "\n").expect("fixture nested file");
    source
}

fn temp_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "stella-plugin-{label}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp root");
    root
}

/// The roster as `list` and a host would see it, over one workspace.
fn roster_at(root: &Path) -> PluginRoster {
    PluginRoster::compose(Vec::new(), read_project_tier(root), &BTreeMap::new())
}

/// `PluginRoster::load` reads the *user* tier through `crate::paths`, which a
/// unit test must not touch — so the project tier is read directly here.
///
/// Through `roster::read_tier`, never a second scan of its own: what a tier
/// admits is the policy under test in more than one place here, and a helper
/// with a private copy of that filter would answer for a build that does not
/// have it (#3530).
fn read_project_tier(root: &Path) -> Vec<super::roster::InstalledPlugin> {
    let dir = stella_home::resolve_project_plugins_dir(root);
    roster::read_tier(&dir, PluginScope::Project, &mut Vec::new())
}

/// **Witness (c).** install → list → remove round-trips, and the removed
/// plugin's hooks stop being routed.
///
/// "Stop being routed" is the load-bearing half: `permits_hook` is the
/// authoritative filter and `hook_routes` is the only thing that consults it,
/// so a plugin absent from that list cannot be dispatched no matter what its
/// process registers for.
#[test]
fn install_list_remove_round_trips_and_the_removed_hooks_stop_routing() {
    let root = temp_root("roundtrip");
    let source = package(&root, "vera");
    let settings = Settings::default();

    assert!(
        roster_at(&root).plugins().is_empty(),
        "nothing is installed before install runs"
    );

    install(&root, &source, PluginScope::Project, true, &settings).expect("install must succeed");

    let after_install = roster_at(&root);
    let installed = after_install.get("vera").expect("`vera` must be listed");
    assert_eq!(installed.scope, PluginScope::Project);
    assert_eq!(
        installed.dir,
        stella_home::resolve_project_plugins_dir(&root).join("vera")
    );
    assert!(
        installed.dir.join("lib").join("helper.py").is_file(),
        "the whole package is copied, not just the manifest"
    );
    let routes = after_install.hook_routes();
    assert_eq!(routes.len(), 2, "one route per declared hook: {routes:?}");
    assert!(
        routes
            .iter()
            .all(|route| route.principal == stella_core::ports::Principal::Plugin("vera".into())),
        "a loaded plugin acts as itself"
    );

    remove(&root, "vera").expect("remove must succeed");

    let after_remove = roster_at(&root);
    assert!(after_remove.get("vera").is_none(), "still listed");
    assert!(
        after_remove.hook_routes().is_empty(),
        "a removed plugin's hooks must stop firing — this is the security half \
         of uninstall, not a cosmetic one"
    );
    assert!(
        !stella_home::resolve_project_plugins_dir(&root)
            .join("vera")
            .exists(),
        "and its files are gone"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Installing twice is refused rather than silently overwriting: the second
/// package's consent text is not the one the user accepted for the first.
#[test]
fn installing_over_an_existing_plugin_is_refused() {
    let root = temp_root("double");
    let source = package(&root, "vera");
    let settings = Settings::default();
    install(&root, &source, PluginScope::Project, true, &settings).expect("first install");
    let error = install(&root, &source, PluginScope::Project, true, &settings)
        .expect_err("the second must be refused");
    assert!(error.contains("already installed"), "{error}");
    assert!(error.contains("stella plugin remove"), "{error}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn removing_something_that_was_never_installed_says_so() {
    let root = temp_root("absent");
    let error = remove(&root, "ghost").expect_err("must fail");
    assert!(error.contains("not installed"), "{error}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A directory with no `plugin.toml` is not a plugin, and install says that
/// rather than copying an arbitrary tree into the plugins directory.
#[test]
fn a_directory_without_a_manifest_is_not_a_plugin() {
    let root = temp_root("bare");
    let source = root.join("not-a-plugin");
    std::fs::create_dir_all(&source).expect("fixture");
    let error = install(
        &root,
        &source,
        PluginScope::Project,
        true,
        &Settings::default(),
    )
    .expect_err("must fail");
    assert!(error.contains("is not a plugin"), "{error}");
    let _ = std::fs::remove_dir_all(&root);
}

/// **The path-traversal witness.** A manifest `name` becomes a directory
/// name, and it is third-party text: `../../evil` must be refused before it
/// is joined to anything.
#[test]
fn a_manifest_name_that_escapes_the_plugins_directory_is_refused() {
    for hostile in ["../escape", "..", ".", ".hidden", "a/b", "a\\b", " pad"] {
        let error = checked_name(hostile).expect_err("`{hostile}` must be refused");
        assert!(
            error.contains("not usable as a plugin directory name"),
            "{hostile}: {error}"
        );
    }
    assert_eq!(checked_name("vera").expect("a plain name is fine"), "vera");
}

/// The consent prompt and the spawn must describe the same program.
///
/// `stella_plugin::consent_text` prints the allowlist verbatim — that crate
/// has no credential vocabulary and must not grow one — so anything the host
/// then withholds has to be corrected at the prompt. The withholding itself is
/// no longer this binary's (#3512): it is
/// `stella_runtime::wrapper::SubprocessWrapper::declare`, the boundary every
/// driver crosses. That makes drift a *cross-crate* hazard, so this asserts
/// across the boundary — what the prompt corrects is exactly what the socket
/// then refuses to pass on.
#[test]
fn what_install_corrects_is_exactly_what_the_socket_withholds() {
    let declared: Vec<String> = ["PATH", "ANTHROPIC_API_KEY", "AWS_SECRET_ACCESS_KEY"]
        .into_iter()
        .map(str::to_string)
        .collect();
    let refused = process::refused_credentials(&declared);
    assert_eq!(
        refused,
        vec![
            "ANTHROPIC_API_KEY".to_string(),
            "AWS_SECRET_ACCESS_KEY".to_string()
        ]
    );

    let admitted = stella_runtime::wrapper::SubprocessWrapper::declare(
        vec!["node".into()],
        declared
            .iter()
            .map(|name| (name.clone(), format!("value-of-{name}")))
            .collect(),
        stella_runtime::wrapper::DEFAULT_WRAPPER_TIMEOUT,
    )
    .expect("the transport is declared with a program and a budget");
    assert_eq!(
        admitted.refused, refused,
        "the correction the prompt prints is the withholding the socket performs"
    );
}

/// A symlink inside a package is refused rather than followed: install must
/// not become a read primitive aimed at whatever the link names.
#[cfg(unix)]
#[test]
fn a_symlink_in_a_package_is_refused() {
    let root = temp_root("symlink");
    let source = package(&root, "sneaky");
    std::os::unix::fs::symlink("/etc/passwd", source.join("stolen")).expect("fixture symlink");
    let error = install(
        &root,
        &source,
        PluginScope::Project,
        true,
        &Settings::default(),
    )
    .expect_err("must fail");
    assert!(error.contains("symlink"), "{error}");
    let _ = std::fs::remove_dir_all(&root);
}

/// Plant a plugin in a tier the way a `git clone` does — files appearing on
/// disk, with no `install` and so no consent transaction anywhere.
fn plant(plugins_dir: &Path, name: &str) {
    let dir = plugins_dir.join(name);
    std::fs::create_dir_all(&dir).expect("fixture plugin dir");
    std::fs::write(dir.join(roster::MANIFEST_FILE), manifest_text(name)).expect("fixture manifest");
    std::fs::write(dir.join("main.py"), "print('pwned')\n").expect("fixture entrypoint");
}

/// **The clone witness (#3509).** A plugin that arrived with the repository is
/// **not loaded, not listed, and dispatches no hook** until the operator
/// trusts the workspace — and the same bytes load once they do.
///
/// The third assertion is the one that matters. `hook_routes` is what a host
/// spawns from, so a test asserting only on the roster would pass against a
/// build that still handed five dispatchable `argv`s to the loop. The refusal
/// notice is asserted too: a plugin that vanishes silently is indistinguishable
/// from one that is broken.
#[test]
fn an_untrusted_projects_plugins_do_not_load_are_not_listed_and_dispatch_no_hook() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("untrusted");
    // A home of our own, so the *user* tier cannot answer for the project one.
    let _paths = crate::paths::test_user_home(root.join("home"));
    plant(&stella_home::resolve_project_plugins_dir(&root), "vera");
    let settings = Settings::default();

    // What a freshly cloned repository sees: neither trust flag set.
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe {
        std::env::remove_var("STELLA_TRUST_PROJECT");
        std::env::remove_var("STELLA_PROJECT_HOOKS");
    }
    let (untrusted, notices) = PluginRoster::load(&root, &settings);
    assert!(untrusted.get("vera").is_none(), "not loaded");
    assert!(
        untrusted.plugins().is_empty(),
        "not listed: {:?}",
        untrusted.plugins()
    );
    assert!(
        untrusted.hook_routes().is_empty(),
        "and above all dispatches nothing — a route is an argv the host spawns: {:?}",
        untrusted.hook_routes()
    );
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains("STELLA_TRUST_PROJECT")),
        "the refusal is spoken, not silent: {notices:?}"
    );

    // The opt-in is what changes the answer — nothing on disk moved.
    // SAFETY: as above.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };
    let (trusted, _) = PluginRoster::load(&root, &settings);
    let loaded = trusted.get("vera").expect("a trusted workspace loads it");
    assert_eq!(loaded.scope, PluginScope::Project);
    assert_eq!(
        trusted.hook_routes().len(),
        2,
        "one route per declared hook, once trusted"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **The symlink witness (#3530).** A symlinked tier entry is **not** a
/// plugin: it is not loaded, not listed, dispatches no hook, and `remove`
/// reports it as not installed instead of erroring on it.
///
/// The route assertion is the load-bearing one — a route is an `argv` a host
/// spawns, with `${plugin_dir}` interpolated to the *link* path, so a test
/// asserting only on the roster would pass against a build that still handed
/// out dispatchable commands into a tree this CLI never copied. `install` has
/// always refused a symlink (`a_symlink_in_a_package_is_refused`); the tier
/// reader now holds the same line, so the two can no longer disagree in the
/// direction that left a package loadable, routable and un-uninstallable.
///
/// Driven through `PluginRoster::load` rather than the `roster_at` helper on
/// purpose: `load` is what the binary calls, so the trust gate and the user
/// tier are in the picture and nothing here can be answered by a fixture's
/// own reading of the directory.
#[cfg(unix)]
#[test]
fn a_symlinked_tier_entry_is_not_loaded_listed_or_routed() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("symlinked-entry");
    // A home of our own, so the developer's `~/.stella/plugins` cannot answer.
    let _paths = crate::paths::test_user_home(root.join("home"));

    // A complete, loadable package that lives *outside* the tier, reachable
    // only through the link — what a hand-run `ln -s` produces.
    let source = package(&root, "vera");
    let tier = stella_home::resolve_project_plugins_dir(&root);
    std::fs::create_dir_all(&tier).expect("fixture tier");
    std::os::unix::fs::symlink(&source, tier.join("vera")).expect("fixture symlink");

    // Trusted, so the project-tier gate cannot be what refuses it (#3509).
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe {
        std::env::set_var("STELLA_TRUST_PROJECT", "1");
        std::env::remove_var("STELLA_PROJECT_HOOKS");
    }
    let (roster, notices) = PluginRoster::load(&root, &Settings::default());
    assert!(roster.get("vera").is_none(), "not loaded");
    assert!(
        roster.plugins().is_empty(),
        "not listed: {:?}",
        roster.plugins()
    );
    assert!(
        roster.hook_routes().is_empty(),
        "and above all dispatches nothing — a route is an argv the host spawns: {:?}",
        roster.hook_routes()
    );
    assert!(
        notices.iter().any(|notice| notice.contains("symlink")),
        "the skip is spoken, not silent: {notices:?}"
    );

    let error = remove(&root, "vera").expect_err("there is nothing installed to remove");
    assert!(
        error.contains("is not installed in either scope"),
        "the link is reported as not installed, not refused as a removal: {error}"
    );
    assert!(
        std::fs::symlink_metadata(tier.join("vera")).is_ok(),
        "and the link itself is left for the operator to delete by hand"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// The legacy hooks-only flag opens the same door, because
/// `project_code_execution_trusted` is the *hooks* half of the trust pair —
/// pinned so a future reader does not "tidy" plugins onto the credentials half
/// and quietly change which flag admits them.
#[test]
fn the_legacy_project_hooks_flag_also_admits_a_projects_plugins() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("legacy-flag");
    let _paths = crate::paths::test_user_home(root.join("home"));
    plant(&stella_home::resolve_project_plugins_dir(&root), "vera");

    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe {
        std::env::remove_var("STELLA_TRUST_PROJECT");
        std::env::set_var("STELLA_PROJECT_HOOKS", "1");
    }
    let (roster, _) = PluginRoster::load(&root, &Settings::default());
    assert!(
        roster.get("vera").is_some(),
        "the legacy flag still opens it"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// The user tier is **not** gated: `~/.stella/plugins` is the operator's own
/// directory, reached only through `stella plugin install`'s consent
/// transaction, so an untrusted *workspace* says nothing about it.
///
/// This is the half a blanket gate would break, and it is why the gate lives in
/// `read_project_tier` rather than in `load`.
#[test]
fn the_user_tier_loads_and_routes_even_in_an_untrusted_workspace() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("user-tier");
    let home = root.join("home");
    let _paths = crate::paths::test_user_home(home.clone());
    plant(
        &stella_home::resolve_user_plugins_dir(Some(home.join(".stella")))
            .expect("a home was installed, so the user tier resolves"),
        "vera",
    );

    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe {
        std::env::remove_var("STELLA_TRUST_PROJECT");
        std::env::remove_var("STELLA_PROJECT_HOOKS");
    }
    let (roster, _) = PluginRoster::load(&root, &Settings::default());
    let loaded = roster.get("vera").expect("the operator's own plugin loads");
    assert_eq!(loaded.scope, PluginScope::User);
    assert_eq!(
        roster.hook_routes().len(),
        2,
        "and it still dispatches — the workspace's trust is not its business"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// A package that configures two keys: one the workspace already set, one it
/// did not. The pair is the point — "unset" is the case a user cannot recover
/// from `stella.toml` at all.
fn configuring_package(dir: &Path, name: &str) -> PathBuf {
    let source = package(dir, name);
    let manifest = std::fs::read_to_string(source.join(roster::MANIFEST_FILE))
        .expect("the fixture manifest was just written");
    std::fs::write(
        source.join(roster::MANIFEST_FILE),
        format!(
            "{manifest}\n\
             [[configure]]\n\
             key = \"self_driving.attribution.commit\"\n\
             value = \"Generated by Oxagen\"\n\
             purpose = \"sign this workspace's commits in the distribution's name\"\n\n\
             [[configure]]\n\
             key = \"agents.default_model\"\n\
             value = \"oxagen/vera\"\n\
             purpose = \"pin the model this distribution is calibrated against\"\n"
        ),
    )
    .expect("fixture manifest");
    source
}

/// **Witness (#4018).** `stella plugin list` names every key an installed
/// package configured and what each one replaced.
///
/// The three states are asserted together because they are one question asked
/// of three keys: a key that replaced a value, a key that replaced nothing, and
/// a key the user has edited by hand since. The last is the one that changes
/// what `remove` will do, and reporting it as though nothing had happened would
/// promise a restoration to the wrong value.
#[test]
fn list_names_every_key_a_package_configured_and_what_it_replaced() {
    let root = temp_root("configures");
    let config = root.join("stella.toml");
    std::fs::write(&config, "[agents]\ndefault_model = \"anthropic/sonnet\"\n")
        .expect("seed config");
    let source = configuring_package(&root, "oxagen");

    install(
        &root,
        &source,
        PluginScope::Project,
        true,
        &Settings::default(),
    )
    .expect("the package installs");

    let installed = read_project_tier(&root);
    let plugin = installed.first().expect("it is on disk");
    let lines = configure_lines(&root, plugin);
    assert_eq!(lines.len(), 2, "one line per declared key: {lines:#?}");
    assert!(
        lines[0].contains("configures: self_driving.attribution.commit = \"Generated by Oxagen\"")
            && lines[0].contains("(was: unset)"),
        "a key the workspace never had says so, because nothing else can: {}",
        lines[0]
    );
    assert!(
        lines[1].contains("configures: agents.default_model = \"oxagen/vera\"")
            && lines[1].contains("(was: \"anthropic/sonnet\")"),
        "and a key it did have names what removal would put back: {}",
        lines[1]
    );
    for line in &lines {
        assert!(
            !line.contains("edited since"),
            "nothing has been edited yet: {line}"
        );
    }

    // The user edits one of them by hand. `remove` will still restore the
    // prior value, discarding this — so `list` says so.
    let edited = std::fs::read_to_string(&config)
        .expect("read back")
        .replace("oxagen/vera", "openai/gpt-6");
    std::fs::write(&config, edited).expect("hand edit");

    let lines = configure_lines(&root, plugin);
    assert!(
        !lines[0].contains("edited since"),
        "the untouched key is unaffected: {}",
        lines[0]
    );
    assert!(
        lines[1].contains("edited since to \"openai/gpt-6\"")
            && lines[1].contains("still restores \"anthropic/sonnet\""),
        "an edited key names both what it holds now and what removal will do: {}",
        lines[1]
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **Witness (#3520).** `install --scope user` and `PluginRoster::load`
/// resolve one directory, so a unit test can reach neither the developer's own
/// `~/.stella/plugins` nor a tier the loader would decline to read.
///
/// Either case alone passes on the defect: with a home installed the two
/// accessors agree anyway, and without one only `tier_dir` used to answer at
/// all. So both are asserted.
#[test]
fn the_user_tier_installs_where_the_loader_reads_and_nowhere_else() {
    let root = temp_root("user-tier-accessor");

    // No home installed: an un-redirected unit test gets the refusal, not a
    // path into whatever `$HOME` the developer happens to have.
    let refusal = tier_dir(&root, PluginScope::User).expect_err(
        "an un-redirected test has no visible user tier, so there is nothing to install into",
    );
    assert!(
        refusal.contains("--scope project"),
        "the refusal names the remedy: {refusal}"
    );

    // A home installed: install and load land on the same directory.
    let home = root.join("home");
    let _paths = crate::paths::test_user_home(home.clone());
    let installs_into = tier_dir(&root, PluginScope::User).expect("a home was installed");
    let loads_from = stella_home::resolve_user_plugins_dir(crate::paths::user_extension_root())
        .expect("the loader sees the installed home");
    assert_eq!(installs_into, loads_from);
    assert!(
        installs_into.starts_with(&home),
        "and it is under the test's own home, not the developer's: {}",
        installs_into.display()
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Plant a package whose directory name is **not** its manifest `name` — the
/// shape a tarball, a `git subtree`, or a rename produces, and the shape the
/// loader has to resolve by manifest rather than by path.
///
/// Written as plant-then-rename rather than a second fixture writer so there
/// is still exactly one place that says what a planted package contains.
fn plant_named(plugins_dir: &Path, dir_name: &str, manifest_name: &str) {
    plant(plugins_dir, manifest_name);
    std::fs::rename(plugins_dir.join(manifest_name), plugins_dir.join(dir_name))
        .expect("fixture rename");
}

/// **Witness (#3380, defect 1).** `remove` uninstalls the name from *every*
/// tier that holds it, not merely the first one it finds.
///
/// Installing at both scopes is the ordinary case, not a contrived one:
/// pinning a workspace to a different build of a globally installed plugin is
/// the reason project scope exists. The load-bearing assertion is
/// `hook_routes` **after** the removal — a `remove` that deletes the project
/// copy, prints "removed `vera` (project)" and returns `Ok` has told the user
/// a third party's process is gone while the user-tier copy is still
/// dispatched on every tool call.
#[test]
fn remove_uninstalls_every_tier_that_holds_the_name() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("both-tiers");
    let home = root.join("home");
    let _paths = crate::paths::test_user_home(home.clone());
    let source = package(&root, "vera");
    let settings = Settings::default();
    // Trusted, so the project tier genuinely loads and the assertions below
    // are about `remove` rather than about the trust gate.
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };

    install(&root, &source, PluginScope::User, true, &settings).expect("the user install");
    install(&root, &source, PluginScope::Project, true, &settings).expect("the project install");
    let user_dir = stella_home::resolve_user_plugins_dir(Some(home.join(".stella")))
        .expect("a home was installed, so the user tier resolves")
        .join("vera");
    let project_dir = stella_home::resolve_project_plugins_dir(&root).join("vera");
    assert!(user_dir.is_dir(), "the user tier holds a copy");
    assert!(project_dir.is_dir(), "and so does the project tier");
    let (before, _) = PluginRoster::load(&root, &settings);
    assert_eq!(
        before.hook_routes().len(),
        2,
        "the project copy shadows the user one and dispatches its two declared hooks"
    );

    remove(&root, "vera").expect("remove must succeed");

    let (after, _) = PluginRoster::load(&root, &settings);
    assert!(
        after.hook_routes().is_empty(),
        "a removed plugin must stop dispatching from EVERY tier — reporting success while \
         another tier's copy is still wired into every tool call is the failure the roster \
         exists to prevent: {:?}",
        after.hook_routes()
    );
    assert!(!project_dir.exists(), "the project copy is gone");
    assert!(
        !user_dir.exists(),
        "and so is the user copy — `remove` does not stop at the first tier"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **Witness (#3380, defect 2).** A package whose directory name disagrees
/// with its manifest is listed and routed under the manifest name — so it must
/// be removable under that name too.
///
/// Resolved by directory name, `remove vera` reported that `vera` was not
/// installed at the same moment `list` was showing it and `hook_routes` was
/// dispatching it: an uninstallable plugin, with no spelling of the command
/// that reached it. The notice is asserted as well, because a name that exists
/// nowhere on disk is otherwise something a user can only discover by reading
/// the manifest themselves.
#[test]
fn a_package_whose_directory_disagrees_with_its_manifest_is_still_removable() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("dir-name");
    let _paths = crate::paths::test_user_home(root.join("home"));
    let tier = stella_home::resolve_project_plugins_dir(&root);
    plant_named(&tier, "pkg", "vera");
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };

    let (roster, notices) = PluginRoster::load(&root, &Settings::default());
    assert!(
        roster.get("vera").is_some(),
        "the manifest name is the identity, everywhere"
    );
    assert_eq!(roster.hook_routes().len(), 2, "and it dispatches under it");
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains("pkg") && notice.contains("vera")),
        "the disagreement is spoken: the name a user is shown is on no directory: {notices:?}"
    );

    remove(&root, "vera").expect("what `list` shows under a name must be removable under it");

    assert!(!tier.join("pkg").exists(), "the directory is gone");
    let (after, _) = PluginRoster::load(&root, &Settings::default());
    assert!(after.hook_routes().is_empty(), "and it dispatches nothing");

    let _ = std::fs::remove_dir_all(&root);
}

/// **Witness (#3380, defect 2, the collision half).** Two directories in one
/// tier claiming one name are reported, and `remove` takes both.
///
/// `PluginRoster::compose` folds by name, so one of the two silently does not
/// run — it is installed, inert, and one rename away from being the copy in
/// force. Leaving it behind on `remove` would be the same defect as leaving
/// the other tier's.
#[test]
fn two_directories_claiming_one_name_are_reported_and_both_removed() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("collision");
    let _paths = crate::paths::test_user_home(root.join("home"));
    let tier = stella_home::resolve_project_plugins_dir(&root);
    plant_named(&tier, "a-copy", "vera");
    plant_named(&tier, "b-copy", "vera");
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };

    let (roster, notices) = PluginRoster::load(&root, &Settings::default());
    assert_eq!(
        roster.plugins().len(),
        1,
        "only one of them is in force — which is precisely why it is said"
    );
    assert!(
        notices
            .iter()
            .any(|notice| notice.contains("a-copy") && notice.contains("b-copy")),
        "the collapse is reported rather than silent: {notices:?}"
    );

    remove(&root, "vera").expect("remove must succeed");

    assert!(
        !tier.join("a-copy").exists(),
        "the shadowed copy is removed"
    );
    assert!(!tier.join("b-copy").exists(), "and so is the one in force");

    let _ = std::fs::remove_dir_all(&root);
}

/// **Witness (#3380, defect 3).** A failed install leaves nothing behind and
/// leaves the name free.
///
/// The copy used to create `<tier>/<name>` first and fill it in `read_dir`
/// order, so an error part-way through left a directory the roster loads and
/// routes with an `argv` naming files that were never copied — a live hook
/// dispatch into nothing — and took the name, so every later install of it was
/// refused as "already installed" until someone deleted the directory by hand.
/// The symlink is simply a copy failure that is deterministic; the assertions
/// are about the residue, not about symlinks.
#[cfg(unix)]
#[test]
fn a_failed_install_leaves_no_directory_and_no_claim_on_the_name() {
    let root = temp_root("atomic");
    let source = package(&root, "vera");
    let stolen = source.join("stolen");
    std::os::unix::fs::symlink("/etc/passwd", &stolen).expect("fixture symlink");
    let settings = Settings::default();
    let tier = stella_home::resolve_project_plugins_dir(&root);

    let error = install(&root, &source, PluginScope::Project, true, &settings)
        .expect_err("the symlink must abort the copy");
    assert!(error.contains("symlink"), "{error}");
    assert!(
        !tier.join("vera").exists(),
        "a half-copied package must never appear under the plugin's name: the roster loads \
         whatever is there and routes an argv at files that were never copied"
    );
    let leftovers: Vec<PathBuf> = std::fs::read_dir(&tier)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "and the staging tree is discarded with it: {leftovers:?}"
    );

    // The other half: the name is still free, so the fix is repairing the
    // package rather than deleting a directory the user never chose to create.
    std::fs::remove_file(&stolen).expect("repair the package");
    install(&root, &source, PluginScope::Project, true, &settings)
        .expect("a repaired package installs under the same name");
    assert!(tier.join("vera").join("main.py").is_file());

    let _ = std::fs::remove_dir_all(&root);
}

/// The trusted launcher's filesystem-isolation boundary closes **both** tiers,
/// the same answer `load_mcp_plan` and the rules/skills/extensions loaders
/// give. A plugin is the most executable extension there is, so it cannot be
/// the one that survives a boundary drawn to exclude executable ones.
#[test]
fn filesystem_isolation_closes_both_tiers() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("isolated");
    let home = root.join("home");
    let _paths = crate::paths::test_user_home(home.clone());
    plant(&stella_home::resolve_project_plugins_dir(&root), "vera");
    plant(
        &stella_home::resolve_user_plugins_dir(Some(home.join(".stella")))
            .expect("a home was installed, so the user tier resolves"),
        "lint-gate",
    );
    // Trusted, so only the isolation boundary can be what closes the project
    // tier — otherwise this would pass for the wrong reason.
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };

    let (open, _) = PluginRoster::load(&root, &Settings::default());
    assert_eq!(
        open.plugins().len(),
        2,
        "both tiers load with the boundary open"
    );

    let _isolation = crate::paths::test_filesystem_isolation(true);
    let (closed, notices) = PluginRoster::load(&root, &Settings::default());
    assert!(closed.plugins().is_empty(), "{:?}", closed.plugins());
    assert!(closed.hook_routes().is_empty());
    assert!(
        notices.is_empty(),
        "an isolated run reports nothing: {notices:?}"
    );

    let _ = std::fs::remove_dir_all(&root);
}

// A plugin is a package: the tools, skills and records it ships (#3380)

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
        crate::agent::tool_stack::session_gate(),
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
        crate::agent::tool_stack::session_gate(),
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
