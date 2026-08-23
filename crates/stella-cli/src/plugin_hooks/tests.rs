// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The dispatch side of the plugin hook plane, driven through
//! [`PluginRoster::load`] over real directories — the door the binary uses,
//! so the #3509 trust gate and the user tier are both in the picture and
//! nothing here can be answered by a fixture reading a directory itself.

use std::path::{Path, PathBuf};

use super::*;

/// An arbiter that declares a `Stop` hook and a process to dispatch into.
fn manifest_text(name: &str) -> String {
    format!(
        "name = \"{name}\"\n\
         description = \"a fixture\"\n\n\
         [loop]\nparticipation = \"arbiter\"\nhooks = [\"Stop\"]\n\n\
         [requirements]\nr = \"the tests pass\"\n\n\
         [runtime]\nargv = [\"python3\", \"${{plugin_dir}}/main.py\"]\ntimeout_secs = 30\nenv = [\"PATH\"]\n"
    )
}

pub(crate) fn temp_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "stella-plugin-hooks-{label}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp root");
    root
}

pub(crate) fn plant(plugins_dir: &Path, name: &str) {
    let dir = plugins_dir.join(name);
    std::fs::create_dir_all(&dir).expect("fixture plugin dir");
    std::fs::write(dir.join("plugin.toml"), manifest_text(name)).expect("fixture manifest");
}

/// The `Stop` actions in a plane, in the order the engine would run them.
pub(crate) fn stop_actions(hooks: &Hooks) -> Vec<HookAction> {
    hooks
        .stop
        .as_deref()
        .unwrap_or_default()
        .iter()
        .flat_map(|matcher| matcher.hooks.iter().cloned())
        .collect()
}

/// **Witness for #4417.** A plugin declaring a `Stop` hook reaches the
/// engine's hook plane, carrying its own identity and its own argv.
///
/// Before this module the same manifest produced a route that `stella plugin
/// list` printed and nothing dispatched: `hook_routes()` had no production
/// consumer at all, so the declared hook was advertised and never fired.
#[test]
fn a_declared_stop_hook_reaches_the_session_hook_plane() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("declared");
    let _paths = crate::paths::test_user_home(root.join("home"));
    plant(&stella_home::resolve_project_plugins_dir(&root), "vera");
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };

    let plane = session_hook_plane(&root, &Settings::default()).expect("a plane with one route");
    let actions = stop_actions(&plane);
    assert_eq!(
        actions.len(),
        1,
        "one declared hook, one action: {actions:?}"
    );
    let origin = actions[0]
        .plugin
        .as_ref()
        .expect("the plugin's identity rides the action");
    assert_eq!(origin.plugin, "vera");
    assert_eq!(
        origin.argv,
        vec![
            "python3".to_string(),
            stella_home::resolve_project_plugins_dir(&root)
                .join("vera")
                .join("main.py")
                .display()
                .to_string(),
        ],
        "${{plugin_dir}} is interpolated by the roster, not by the runner"
    );
    assert_eq!(origin.env_allowlist, vec!["PATH".to_string()]);
    assert_eq!(
        actions[0].effective_timeout_ms(),
        30_000,
        "the manifest's seconds, in the engine's milliseconds"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// **Witness for #3521.** The #3509 trust gate holds at the *dispatch* site,
/// not only at the roster: a plugin a `git clone` carried in produces no
/// action for the engine to run, and the same bytes produce one once the
/// operator trusts the workspace.
///
/// This is the assertion that distinguishes a wired dispatch from a wired
/// dispatch with a hole in it — a host reaching
/// `stella_home::resolve_project_plugins_dir` directly would pass every other
/// test in this file and fail this one.
#[test]
fn an_untrusted_project_tier_contributes_no_hook_route() {
    let _env = crate::test_env::lock();
    let _restore =
        crate::test_env::EnvRestore::capture(&["STELLA_TRUST_PROJECT", "STELLA_PROJECT_HOOKS"]);
    let root = temp_root("untrusted");
    let _paths = crate::paths::test_user_home(root.join("home"));
    plant(&stella_home::resolve_project_plugins_dir(&root), "vera");

    // What a freshly cloned repository sees: neither trust flag set.
    // SAFETY: the env lock is held for the whole mutate-read-restore window.
    unsafe {
        std::env::remove_var("STELLA_TRUST_PROJECT");
        std::env::remove_var("STELLA_PROJECT_HOOKS");
    }
    assert!(
        session_hook_plane(&root, &Settings::default()).is_none(),
        "an untrusted tier contributes nothing, so a hook-free session stays hook-free"
    );

    // SAFETY: as above.
    unsafe { std::env::set_var("STELLA_TRUST_PROJECT", "1") };
    let plane = session_hook_plane(&root, &Settings::default()).expect("trusted: one route");
    assert_eq!(stop_actions(&plane).len(), 1, "nothing on disk moved");

    let _ = std::fs::remove_dir_all(&root);
}

/// The operator's own matchers stay ahead of every plugin's, so a plugin can
/// never displace a hook the user wrote.
#[test]
fn the_operators_hooks_run_before_a_plugins() {
    let operator = Hooks {
        stop: Some(vec![HookMatcher {
            matcher: None,
            hooks: vec![HookAction::new("echo mine")],
        }]),
        ..Hooks::default()
    };
    let routes = vec![PluginHookRoute {
        plugin: "vera".to_string(),
        principal: stella_core::ports::Principal::Plugin("vera".to_string()),
        event: HookEvent::Stop,
        argv: vec!["python3".to_string()],
        timeout_secs: 30,
        env_allowlist: Vec::new(),
    }];
    let plane = fold_plugin_routes(Some(operator), &routes).expect("a plane");
    let owners: Vec<Option<String>> = stop_actions(&plane)
        .iter()
        .map(|action| action.plugin.as_ref().map(|origin| origin.plugin.clone()))
        .collect();
    assert_eq!(owners, vec![None, Some("vera".to_string())]);
}

/// No routes means no plane, rather than an empty one: a hook-free session
/// must carry no hooks handle at all so the engine takes its pre-hooks path.
#[test]
fn no_routes_leaves_the_operators_plane_exactly_as_it_was() {
    assert!(fold_plugin_routes(None, &[]).is_none());
    let operator = Hooks {
        stop: Some(vec![HookMatcher {
            matcher: None,
            hooks: vec![HookAction::new("echo mine")],
        }]),
        ..Hooks::default()
    };
    assert_eq!(
        fold_plugin_routes(Some(operator.clone()), &[]),
        Some(operator),
        "byte for byte, not merely equivalent"
    );
}

/// **The chokepoint is the only door (#3521).** `PluginRoster::load` is where
/// the #3509 trust gate lives, so a production site reaching the plugins tier
/// any other way re-opens arbitrary code execution on `git clone` with
/// nothing failing.
///
/// A grep guard rather than a narrowed visibility, and the choice is forced:
/// `stella plugin install`/`remove` must be able to name the tier of a
/// workspace the operator has *not* trusted — deleting a package is the one
/// operation an untrusted tier may never refuse — so the function cannot be
/// made unreachable from `plugin_cmd`. What can be enforced is that nothing
/// *else* reaches it, which is what this asserts.
#[test]
fn no_other_production_site_reads_the_plugins_tier() {
    let sites = crate::source_scan::production_files_mentioning("resolve_project_plugins_dir");
    assert_eq!(
        sites,
        [
            // `install`/`remove` name the tier to copy into and delete from.
            "plugin_cmd.rs".to_string(),
            // The chokepoint itself, where the trust gate is applied.
            "plugin_cmd/roster.rs".to_string(),
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>(),
        "a new site must go through PluginRoster::load, or justify itself here"
    );
}
