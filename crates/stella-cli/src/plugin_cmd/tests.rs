// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The loader's round trip, over real directories.
//!
//! These drive `install`/`remove` against a temporary workspace root rather
//! than the process's cwd, so nothing here reads or writes the developer's
//! own `.stella/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::roster::{PluginRoster, PluginScope};
use super::*;

/// A manifest that declares both halves of a dispatch: the grant, and the
/// process to dispatch into.
fn package(dir: &Path, name: &str) -> PathBuf {
    let source = dir.join(format!("src-{name}"));
    std::fs::create_dir_all(source.join("lib")).expect("fixture dirs");
    std::fs::write(
        source.join(roster::MANIFEST_FILE),
        format!(
            "name = \"{name}\"\n\
             description = \"a fixture\"\n\n\
             [loop]\nparticipation = \"arbiter\"\nhooks = [\"PreToolUse\", \"Stop\"]\n\n\
             [requirements]\nr = \"the tests pass\"\n\n\
             [runtime]\nargv = [\"python3\", \"${{plugin_dir}}/main.py\"]\ntimeout_secs = 30\nenv = [\"PATH\"]\n"
        ),
    )
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
fn read_project_tier(root: &Path) -> Vec<super::roster::InstalledPlugin> {
    let dir = stella_home::resolve_project_plugins_dir(root);
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return found;
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    paths.sort();
    for path in paths {
        if let Some(manifest) = roster::load_manifest(&path).expect("a fixture manifest must load")
        {
            found.push(super::roster::InstalledPlugin {
                manifest,
                dir: path,
                scope: PluginScope::Project,
            });
        }
    }
    found
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
/// then withholds has to be corrected at the prompt. This pins the two to one
/// implementation: whatever `prepare_command` refuses is exactly what
/// `refused_credentials` names, so the correction cannot drift from the
/// withholding it describes.
#[test]
fn what_install_corrects_is_exactly_what_the_spawn_withholds() {
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

    let route = super::roster::PluginHookRoute {
        plugin: "vera".into(),
        principal: stella_core::ports::Principal::Plugin("vera".into()),
        event: stella_plugin::HookEvent::PreToolUse,
        argv: vec!["node".into()],
        timeout_secs: 30,
        env_allowlist: declared,
    };
    let prepared = process::prepare_command(&route, |name| Some(format!("value-of-{name}")));
    assert_eq!(
        prepared.refused, refused,
        "the correction the prompt prints is the withholding the spawn performs"
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
