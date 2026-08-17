// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What is installed, what it is allowed to be invoked at, and as whom.
//!
//! Two tiers — `~/.stella/plugins` and `<workspace>/.stella/plugins`, both
//! resolved by `stella-home` so nothing here spells `.stella` itself — folded
//! into one roster, then turned into the set of hook dispatches a host may
//! make.
//!
//! # Why plugin hooks are derived and never written into `hooks`
//!
//! The obvious loader writes a plugin's hook grants into a settings file's
//! `hooks` block at install and deletes them at uninstall. It cannot work,
//! and the reason is structural rather than a matter of care:
//! `stella_core::hooks::HookMatcher` carries no owner, so once several
//! plugins' entries and the operator's own have been concatenated by
//! `settings::merge`, there is no expression that removes one plugin's
//! entries and nobody else's (`doc:pipeline-as-plugins` §A4). An uninstall
//! that leaves a third party's process wired into every `PreToolUse` is a
//! security defect, not a cosmetic one.
//!
//! So the roster is the source of truth and it is recomputed from disk: a
//! plugin's directory is gone, its routes are gone, with nothing to clean up
//! and nothing to forget to clean up. `Settings::plugins` covers the one case
//! deletion cannot reach — retracting a plugin the *other* scope installed —
//! and it is per owner, so it removes an owner's entries whole.
//!
//! # `permits_hook` is the filter, and it is the only filter
//!
//! [`PluginRoster::hook_routes`] asks
//! [`LoopGrant::permits_hook`](stella_plugin::LoopGrant::permits_hook) for
//! every event, rather than trusting anything the plugin's own process says
//! at runtime. A process that registers for `Stop` without declaring it is
//! simply never called, because no route to it was ever built.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use stella_core::ports::Principal;
use stella_plugin::{HookEvent, PluginManifest};

use crate::settings::{Settings, Toggle};

/// The manifest file inside a plugin directory.
pub(crate) const MANIFEST_FILE: &str = "plugin.toml";

/// Every hook event, in the fixed order routes are emitted in. Exhaustive by
/// construction: [`HookEvent`] is `stella-protocol`'s, and a sixth event
/// added there without a line here would be silently unroutable, so the
/// `event_order_is_exhaustive` test destructures the enum against this list.
const EVENT_ORDER: [HookEvent; 5] = [
    HookEvent::SessionStart,
    HookEvent::PreToolUse,
    HookEvent::PostToolUse,
    HookEvent::Stop,
    HookEvent::PreCompact,
];

/// Which tier a plugin was installed into.
///
/// Ordered weakest-first so `Ord` is the precedence: a project install
/// shadows a user install of the same name, the same way a project settings
/// scope wins per field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PluginScope {
    /// `~/.stella/plugins` — installed once, visible in every workspace.
    User,
    /// `<workspace>/.stella/plugins` — installed for this repository alone.
    Project,
}

impl PluginScope {
    /// The word `stella plugin list` prints and `--scope` accepts.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}

/// One plugin found on disk, with its manifest already validated.
#[derive(Debug, Clone)]
pub(crate) struct InstalledPlugin {
    /// The validated manifest — construction goes through
    /// `PluginManifest::from_toml_str`, so every cross-field rule has passed.
    pub(crate) manifest: PluginManifest,
    /// The directory holding `plugin.toml`, and what `${plugin_dir}`
    /// interpolates to.
    pub(crate) dir: PathBuf,
    /// The tier it was found in.
    pub(crate) scope: PluginScope,
}

impl InstalledPlugin {
    /// The caller a gate sees for anything this plugin does.
    ///
    /// [`Principal::Plugin`], never [`Principal::User`]: a plugin is a third
    /// party's code that arrived after install, and a gate that could not
    /// tell it from the human driving the session would grant every installed
    /// plugin the operator's authority (`doc:pipeline-as-plugins` §A1).
    pub(crate) fn principal(&self) -> Principal {
        Principal::Plugin(self.manifest.name.clone())
    }
}

/// One dispatch a host is permitted to make into a plugin's process.
///
/// A route exists only where the manifest declared *both* halves: the grant
/// (`[loop] hooks`) and the process (`[runtime]`). A grant with no process has
/// nothing to invoke, and a process with no grant has nowhere to be invoked —
/// neither is an error, and neither produces a route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginHookRoute {
    /// The manifest name, as the principal spells it.
    pub(crate) plugin: String,
    /// The caller a gate sees — always [`Principal::Plugin`].
    pub(crate) principal: Principal,
    /// The declared event this route dispatches.
    pub(crate) event: HookEvent,
    /// The program and arguments, with `${plugin_dir}` already interpolated
    /// to the install directory. Never a shell string.
    pub(crate) argv: Vec<String>,
    /// Seconds before the host kills the process.
    pub(crate) timeout_secs: u64,
    /// The environment variable names — exactly these — the child may
    /// inherit. Resolved to values at spawn time by
    /// [`super::process::prepare_command`], never here: a route is a
    /// declaration, and holding resolved credentials in one would put them
    /// in every debug print of it.
    pub(crate) env_allowlist: Vec<String>,
}

/// The installed plugins that are actually in force, in name order.
#[derive(Debug, Clone, Default)]
pub(crate) struct PluginRoster {
    plugins: Vec<InstalledPlugin>,
}

impl PluginRoster {
    /// Fold the two tiers and the retraction switches into the roster in
    /// force. Pure — the caller supplies what it read.
    ///
    /// Project shadows user **per plugin name**, wholesale. That is the
    /// property `hooks` does not have and cannot be given: an owned unit can
    /// be replaced or removed as a unit, so "this workspace pins a different
    /// build of `vera`" removes the user-scope `vera` entirely rather than
    /// running both.
    pub(crate) fn compose(
        user: Vec<InstalledPlugin>,
        project: Vec<InstalledPlugin>,
        retractions: &BTreeMap<String, Toggle>,
    ) -> Self {
        let mut by_name: BTreeMap<String, InstalledPlugin> = BTreeMap::new();
        for plugin in user.into_iter().chain(project) {
            // `insert` overwrites, and the project tier is iterated second, so
            // the higher scope replaces the lower one whole.
            by_name.insert(plugin.manifest.name.clone(), plugin);
        }
        by_name.retain(|name, _| !matches!(retractions.get(name), Some(Toggle::Off)));
        Self {
            plugins: by_name.into_values().collect(),
        }
    }

    /// Read both tiers off disk and compose them.
    ///
    /// Returns the roster and the notices a caller should print: a directory
    /// that is not a plugin, or a manifest that does not load, is reported and
    /// skipped rather than failing the session. A malformed third-party plugin
    /// must not be able to stop Stella from starting — but it must also never
    /// be silently ignored, because "I installed it and nothing happened" is
    /// unanswerable without the reason.
    pub(crate) fn load(workspace_root: &Path, settings: &Settings) -> (Self, Vec<String>) {
        let mut notices = Vec::new();
        let user = match crate::paths::stella_root() {
            Some(root) => read_tier(
                &stella_home::resolve_user_plugins_dir(Some(root)).unwrap_or_default(),
                PluginScope::User,
                &mut notices,
            ),
            None => Vec::new(),
        };
        let project = read_tier(
            &stella_home::resolve_project_plugins_dir(workspace_root),
            PluginScope::Project,
            &mut notices,
        );
        (Self::compose(user, project, &settings.plugins), notices)
    }

    /// The plugins in force, in name order.
    pub(crate) fn plugins(&self) -> &[InstalledPlugin] {
        &self.plugins
    }

    /// The plugin installed under `name`, if any.
    pub(crate) fn get(&self, name: &str) -> Option<&InstalledPlugin> {
        self.plugins
            .iter()
            .find(|plugin| plugin.manifest.name == name)
    }

    /// Every hook dispatch a host is permitted to make, in a deterministic
    /// order (plugin name, then [`EVENT_ORDER`]).
    ///
    /// **This is the authoritative routing filter.** Nothing downstream is
    /// expected to re-check the grant, so nothing here may emit a route the
    /// manifest did not declare.
    pub(crate) fn hook_routes(&self) -> Vec<PluginHookRoute> {
        let mut routes = Vec::new();
        for plugin in &self.plugins {
            let Some(runtime) = &plugin.manifest.runtime else {
                // A hook grant with no `[runtime]` has no process to call.
                continue;
            };
            let dir = plugin.dir.to_string_lossy().into_owned();
            for event in EVENT_ORDER {
                if !plugin.manifest.loop_grant.permits_hook(event) {
                    continue;
                }
                routes.push(PluginHookRoute {
                    plugin: plugin.manifest.name.clone(),
                    principal: plugin.principal(),
                    event,
                    argv: runtime
                        .argv
                        .iter()
                        .map(|arg| arg.replace("${plugin_dir}", &dir))
                        .collect(),
                    timeout_secs: runtime.timeout_secs,
                    env_allowlist: runtime.env.clone(),
                });
            }
        }
        routes
    }
}

/// Read one tier's directory. A missing directory is an empty tier.
fn read_tier(dir: &Path, scope: PluginScope, notices: &mut Vec<String>) -> Vec<InstalledPlugin> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            notices.push(format!("  ! cannot read {}: {error}", dir.display()));
            return Vec::new();
        }
    };
    // Collected and sorted rather than taken in readdir order: the roster is
    // shown to humans and folded into routes, and directory order is not
    // stable across filesystems.
    let mut dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();

    let mut found = Vec::new();
    for path in dirs {
        match load_manifest(&path) {
            Ok(Some(manifest)) => found.push(InstalledPlugin {
                manifest,
                dir: path,
                scope,
            }),
            Ok(None) => {}
            Err(reason) => notices.push(format!("  ! {reason}")),
        }
    }
    found
}

/// Parse one plugin directory's manifest. `Ok(None)` = no `plugin.toml`, so
/// this directory is not a plugin at all.
pub(crate) fn load_manifest(dir: &Path) -> Result<Option<PluginManifest>, String> {
    let path = dir.join(MANIFEST_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    PluginManifest::from_toml_str(&text)
        .map(Some)
        .map_err(|error| format!("{} did not load: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use stella_plugin::Participation;

    fn manifest(text: &str) -> PluginManifest {
        PluginManifest::from_toml_str(text).expect("fixture must parse")
    }

    /// An arbiter that runs a process at every point it may declare.
    fn wired(name: &str) -> PluginManifest {
        manifest(&format!(
            "name = \"{name}\"\n\
             [loop]\nparticipation = \"arbiter\"\nhooks = [\"PreToolUse\", \"Stop\"]\n\n\
             [requirements]\nr = \"the tests pass\"\n\n\
             [runtime]\nargv = [\"python3\", \"${{plugin_dir}}/main.py\"]\ntimeout_secs = 30\nenv = [\"PATH\"]"
        ))
    }

    fn installed(name: &str, scope: PluginScope) -> InstalledPlugin {
        InstalledPlugin {
            manifest: wired(name),
            dir: PathBuf::from("/ws/.stella/plugins").join(name),
            scope,
        }
    }

    /// [`EVENT_ORDER`] must name every variant: an event missing from it is
    /// one a plugin can declare and no host will ever dispatch, which is the
    /// silent half of "an undeclared hook is never invoked".
    #[test]
    fn event_order_is_exhaustive() {
        for event in [
            HookEvent::SessionStart,
            HookEvent::PreToolUse,
            HookEvent::PostToolUse,
            HookEvent::Stop,
            HookEvent::PreCompact,
        ] {
            // The match is the assertion: adding a sixth variant to
            // `HookEvent` stops this compiling until it is added above too.
            let named = match event {
                HookEvent::SessionStart
                | HookEvent::PreToolUse
                | HookEvent::PostToolUse
                | HookEvent::Stop
                | HookEvent::PreCompact => EVENT_ORDER.contains(&event),
            };
            assert!(named, "{event} is declarable but unroutable");
        }
    }

    /// **The routing filter.** Only declared events produce a route, and the
    /// caller on each is the plugin — never the human.
    #[test]
    fn only_declared_hooks_are_routed_and_the_caller_is_the_plugin() {
        let roster = PluginRoster::compose(
            Vec::new(),
            vec![installed("vera", PluginScope::Project)],
            &BTreeMap::new(),
        );
        let routes = roster.hook_routes();
        let events: Vec<HookEvent> = routes.iter().map(|route| route.event).collect();
        assert_eq!(
            events,
            vec![HookEvent::PreToolUse, HookEvent::Stop],
            "the manifest declared exactly these two"
        );
        for route in &routes {
            assert_eq!(
                route.principal,
                Principal::Plugin("vera".into()),
                "a plugin acts as itself, never as the user"
            );
            assert_ne!(route.principal, Principal::User);
            assert_eq!(
                route.argv,
                vec!["python3", "/ws/.stella/plugins/vera/main.py"],
                "${{plugin_dir}} is interpolated by the host, per the manifest"
            );
            assert_eq!(route.env_allowlist, vec!["PATH".to_string()]);
        }
    }

    /// A grant with no `[runtime]` has nothing to invoke, so it routes
    /// nowhere — quietly, because it is a legal manifest, not an error.
    #[test]
    fn a_grant_without_a_process_routes_nowhere() {
        let plugin = InstalledPlugin {
            manifest: manifest(
                "name = \"quiet\"\n[loop]\nparticipation = \"steering\"\nhooks = [\"PreToolUse\"]",
            ),
            dir: PathBuf::from("/ws/.stella/plugins/quiet"),
            scope: PluginScope::Project,
        };
        let roster = PluginRoster::compose(Vec::new(), vec![plugin], &BTreeMap::new());
        assert_eq!(roster.plugins().len(), 1);
        assert!(roster.hook_routes().is_empty());
    }

    /// A project install replaces the user install of the same name **whole**
    /// — the per-owner replacement `hooks` concatenation cannot express.
    #[test]
    fn a_project_install_shadows_the_user_one_by_name() {
        let mut user = installed("vera", PluginScope::User);
        user.dir = PathBuf::from("/home/dev/.stella/plugins/vera");
        let roster = PluginRoster::compose(
            vec![user],
            vec![installed("vera", PluginScope::Project)],
            &BTreeMap::new(),
        );
        assert_eq!(roster.plugins().len(), 1, "one owner, one plugin");
        assert_eq!(roster.plugins()[0].scope, PluginScope::Project);
        assert!(
            roster
                .hook_routes()
                .iter()
                .all(|route| route.argv[1].starts_with("/ws/")),
            "no route survives from the shadowed install"
        );
    }

    /// **Witness (c), the retraction half.** A scope switching a plugin off
    /// removes that owner's routes and nobody else's — the thing no scope can
    /// do to another scope's hook matchers.
    #[test]
    fn a_retracted_plugin_contributes_no_routes() {
        let installed_both = || {
            vec![
                installed("vera", PluginScope::User),
                installed("lint-gate", PluginScope::User),
            ]
        };
        let running = PluginRoster::compose(installed_both(), Vec::new(), &BTreeMap::new());
        assert_eq!(running.plugins().len(), 2);
        assert_eq!(running.hook_routes().len(), 4);

        let retractions = BTreeMap::from([("vera".to_string(), Toggle::Off)]);
        let retracted = PluginRoster::compose(installed_both(), Vec::new(), &retractions);
        assert_eq!(
            retracted.plugins().len(),
            1,
            "the retracted owner is gone whole"
        );
        assert!(retracted.get("vera").is_none());
        assert!(
            retracted
                .hook_routes()
                .iter()
                .all(|route| route.plugin == "lint-gate"),
            "and the other owner's routes are untouched"
        );
    }

    /// An explicit `"on"` is not a grant of anything — it is the absence of a
    /// retraction, which is already the default. Pinned so a future reader
    /// does not add "on means install".
    #[test]
    fn an_on_switch_leaves_an_installed_plugin_installed() {
        let retractions = BTreeMap::from([("vera".to_string(), Toggle::On)]);
        let roster = PluginRoster::compose(
            vec![installed("vera", PluginScope::User)],
            Vec::new(),
            &retractions,
        );
        assert!(roster.get("vera").is_some());

        let none = PluginRoster::compose(Vec::new(), Vec::new(), &retractions);
        assert!(
            none.get("vera").is_none(),
            "a switch never conjures a plugin that is not on disk"
        );
    }

    /// The roster is name-ordered, and so are the routes: both are printed to
    /// humans and both decide dispatch order, so readdir order must not reach
    /// either.
    #[test]
    fn the_roster_and_its_routes_are_deterministically_ordered() {
        let roster = PluginRoster::compose(
            vec![
                installed("zeta", PluginScope::User),
                installed("alpha", PluginScope::User),
            ],
            Vec::new(),
            &BTreeMap::new(),
        );
        let names: Vec<&str> = roster
            .plugins()
            .iter()
            .map(|plugin| plugin.manifest.name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
        let routed: Vec<(String, HookEvent)> = roster
            .hook_routes()
            .into_iter()
            .map(|route| (route.plugin, route.event))
            .collect();
        assert_eq!(
            routed,
            vec![
                ("alpha".to_string(), HookEvent::PreToolUse),
                ("alpha".to_string(), HookEvent::Stop),
                ("zeta".to_string(), HookEvent::PreToolUse),
                ("zeta".to_string(), HookEvent::Stop),
            ]
        );
        // Guards the `BTreeSet` import earning its place: names are unique.
        let unique: BTreeSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), names.len());
    }

    #[test]
    fn an_observer_declares_no_hooks_and_so_routes_nowhere() {
        let plugin = InstalledPlugin {
            manifest: manifest(
                "name = \"watch\"\n[loop]\nparticipation = \"observer\"\n\n[runtime]\nargv = [\"node\"]\ntimeout_secs = 5",
            ),
            dir: PathBuf::from("/ws/.stella/plugins/watch"),
            scope: PluginScope::Project,
        };
        assert_eq!(
            plugin.manifest.loop_grant.participation,
            Participation::Observer
        );
        let roster = PluginRoster::compose(Vec::new(), vec![plugin], &BTreeMap::new());
        assert!(roster.hook_routes().is_empty());
    }
}
