// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What a plugin *ships*, as opposed to what it *does* in the turn loop
//! (#3380).
//!
//! A plugin's manifest declares a say in the turn: hook points, wrapper
//! points, an oracle, a process. That is the whole of `plugin.toml` and it
//! was never the whole of a package. The three surfaces a workspace already
//! steers itself with — script tools, skills, and context records — had no
//! way to arrive with a plugin, so "install the review plugin" could deliver
//! an arbiter that holds a turn open but not the `lint_fix` tool it wants to
//! call, the skill that explains the house style, or the record that steers
//! toward it. This module makes a package able to carry all three.
//!
//! # A package is `.stella`-shaped, and that is the whole format
//!
//! ```text
//! <plugin_dir>/
//!   plugin.toml          the say in the loop
//!   tools/*.toml         the same manifests `.stella/tools/*.toml` holds
//!   skills/<slug>/SKILL.md   the same files `.stella/skills/` holds
//!   rules/*.toml         the same records `.stella/rules/` holds
//! ```
//!
//! There is deliberately no second format and no second loader: each
//! directory is handed to the loader that already reads that surface, as an
//! additional source. A plugin's tool is a custom tool
//! ([`stella_tools::custom`]); its skill is a skill
//! (`stella_core::skills`); its record is a context record
//! (`crate::context_records`). A parallel plane would be a second set of
//! precedence rules, a second set of diagnostics, and a second thing to
//! remember to gate.
//!
//! # Derived, never copied — which is what makes retraction total
//!
//! Nothing here writes into `.stella/tools`, `.stella/skills` or
//! `.stella/rules`. The contributions are **recomputed from the installed
//! packages on every load**, exactly as [`super::roster`] recomputes hook
//! routes, and for exactly the reason its module docs give: a loader that
//! copied a plugin's entries into the user's own directories would have to
//! find and delete precisely those entries at uninstall, and there is no
//! expression that removes one owner's rows from a merged list nobody
//! stamped with an owner. That failure already happened once here with hook
//! matchers, where the result was a removed plugin whose process still ran
//! on every `PreToolUse`.
//!
//! So `stella plugin remove` does nothing about tools, skills or records —
//! and that is the guarantee, not a gap. The package directory is gone, so
//! the next load derives nothing from it. There is nothing to clean up and
//! therefore nothing to forget to clean up.
//!
//! # Three properties, and where each one lives
//!
//! - **Provenance.** Every contribution knows its plugin. For a tool it is
//!   stamped by discovery from the directory it read
//!   ([`stella_tools::custom::CustomTool::contributed_by`]) and it is what
//!   [`stella_tools::custom::CustomTool::principal`] turns into
//!   `Principal::Plugin`. For skills and records it is the source path,
//!   which lies inside the package.
//! - **Consent.** [`Inventory::consent_addendum`] renders what a package
//!   ships into the *same* install transaction as
//!   [`stella_plugin::consent_text`] — see that function's own docs for why
//!   the host renders this half and the plugin crate does not.
//! - **Retraction.** Structural, per above.
//!
//! # The trust gate is the roster's, not a second one
//!
//! Every function here takes contributions from a [`PluginRoster`], which is
//! the only way to get one: [`PluginRoster::load`] refuses the project tier
//! outright in an untrusted workspace
//! (`roster::read_project_tier`) and drops any plugin an operator
//! switched off. A contributed **tool** is executable code that arrived with
//! a `git clone`, so it must sit behind that gate — and it does, by
//! construction, because there is no path from a package directory to a
//! contribution that does not pass through the roster.

use std::path::{Path, PathBuf};

use super::roster::{InstalledPlugin, PluginRoster};
use crate::settings::Settings;

/// `<plugin_dir>/tools` — custom script-tool manifests, the format
/// [`stella_tools::custom`] documents.
pub(crate) const TOOLS_DIR: &str = "tools";
/// `<plugin_dir>/skills` — `<slug>/SKILL.md` files, the layout
/// `stella_core::skills` reads.
pub(crate) const SKILLS_DIR: &str = "skills";
/// `<plugin_dir>/rules` — context records, the format
/// [`crate::context_records`] reads. Named `rules` rather than `records`
/// because that is what the directory holding them is called everywhere else
/// (`.stella/rules`, `~/.stella/rules`), and a package author copying their
/// workspace's directory across should not have to rename it.
pub(crate) const RECORDS_DIR: &str = "rules";

/// One plugin's contributed directory of a single kind, ready to hand to the
/// loader that owns that surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContributedDir {
    /// The contributing plugin's manifest `name` — the provenance, and the
    /// string `Principal::Plugin` carries for its tools.
    pub(crate) plugin: String,
    /// The directory itself. It may not exist: a plugin shipping no skills
    /// has no `skills/`, and every loader here treats an absent directory as
    /// an empty one rather than an error.
    pub(crate) dir: PathBuf,
}

/// Every plugin's contributed directory of one kind, in roster order.
///
/// Roster order is name order, and it is the precedence order for
/// plugin-versus-plugin name collisions: the first plugin to claim a name
/// keeps it. Deterministic rather than fair, which is the property that
/// matters — two installs of the same set must produce the same surface.
fn dirs_of(roster: &PluginRoster, kind: &str) -> Vec<ContributedDir> {
    roster
        .plugins()
        .iter()
        .map(|plugin| ContributedDir {
            plugin: plugin.manifest.name.clone(),
            dir: plugin.dir.join(kind),
        })
        .collect()
}

/// The roster this workspace's session runs under, or an empty one.
///
/// The single place the package surfaces resolve what is installed, so the
/// trust gate, the `plugins.<name> = "off"` retraction and the
/// filesystem-isolation boundary are asked once and answered the same way
/// for tools, skills and records. Notices are dropped here deliberately:
/// `stella plugin list` and the session's own plugin load already print
/// them, and a malformed package must not make a skill lookup noisy three
/// times over.
fn session_roster(workspace_root: &Path) -> PluginRoster {
    let settings = Settings::load(workspace_root).unwrap_or_default();
    PluginRoster::load(workspace_root, &settings).0
}

/// The `<plugin_dir>/tools` directories a session's custom-tool discovery
/// must scan, in roster order.
///
/// Handed to [`stella_tools::custom::discover_with_plugins`], which scans
/// them **last** so the user's own manifests keep their names — see that
/// function for the precedence argument.
pub(crate) fn contributed_tool_dirs(
    workspace_root: &Path,
) -> Vec<stella_tools::custom::PluginToolDir> {
    dirs_of(&session_roster(workspace_root), TOOLS_DIR)
        .into_iter()
        .map(|contributed| stella_tools::custom::PluginToolDir {
            plugin: contributed.plugin,
            dir: contributed.dir,
        })
        .collect()
}

/// The `<plugin_dir>/skills` directories a session's skill load must read,
/// in roster order.
pub(crate) fn contributed_skill_dirs(workspace_root: &Path) -> Vec<ContributedDir> {
    dirs_of(&session_roster(workspace_root), SKILLS_DIR)
}

/// The `<plugin_dir>/rules` directories a session's context-record registry
/// must read, in roster order.
pub(crate) fn contributed_record_dirs(workspace_root: &Path) -> Vec<ContributedDir> {
    dirs_of(&session_roster(workspace_root), RECORDS_DIR)
}

/// What one package ships, counted and named — the answer to "what am I
/// installing?" beyond the say in the loop.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Inventory {
    /// Tool manifest stems, sorted. The stem rather than the manifest's
    /// `name` field on purpose: reading the name means parsing every
    /// manifest, and a package whose TOML does not parse must still be
    /// *described* to the human deciding whether to install it. A stem that
    /// disagrees with the manifest's `name` is surfaced later, by
    /// `stella plugin list`, off the real discovery.
    pub(crate) tools: Vec<String>,
    /// Skill slugs (the `<slug>/SKILL.md` directory names), sorted.
    pub(crate) skills: Vec<String>,
    /// Context-record file stems, sorted.
    pub(crate) records: Vec<String>,
}

impl Inventory {
    /// Read one package directory's inventory. Never fails: an unreadable
    /// directory is an empty one, because a consent prompt that cannot be
    /// rendered must not be the thing that stops an install from being
    /// *refused*.
    pub(crate) fn of_package(dir: &Path) -> Self {
        Self {
            tools: entries(&dir.join(TOOLS_DIR), Kind::TomlFile),
            skills: entries(&dir.join(SKILLS_DIR), Kind::SkillDir),
            records: entries(&dir.join(RECORDS_DIR), Kind::TomlFile),
        }
    }

    /// Whether the package ships nothing at all beyond its manifest.
    pub(crate) fn is_empty(&self) -> bool {
        self.tools.is_empty() && self.skills.is_empty() && self.records.is_empty()
    }

    /// The lines the install prompt prints beside
    /// [`stella_plugin::consent_text`], or `None` when the package ships
    /// nothing.
    ///
    /// # Why the host renders this and the plugin crate does not
    ///
    /// `stella-plugin` is pure — it parses borrowed text and performs no
    /// I/O — and what a package ships is a fact about a *directory*. It
    /// cannot see one, and giving it a filesystem to answer this would cost
    /// that crate the property its whole boundary is built on.
    ///
    /// This is the same seam [`super::install`] already uses for the
    /// credential correction: the crate renders what the manifest declared,
    /// and the host prints what only the host can know, in the same
    /// transaction, before the one y/N. **One prompt, one answer** — the
    /// thing that would be wrong is a second consent *decision*, not a
    /// paragraph the host contributes to the first.
    ///
    /// The sharpest line is the tools one, and it says so: a contributed
    /// tool is executable code entering the agent's surface, which the model
    /// may call on its own initiative for the rest of the session.
    pub(crate) fn consent_addendum(&self, plugin: &str) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut lines = vec![format!("`{plugin}` also installs:")];
        if !self.tools.is_empty() {
            lines.push(format!(
                "  - {} tool(s) the model may call by itself, each a script this package \
                 ships: {}",
                self.tools.len(),
                self.tools.join(", ")
            ));
            lines.push(
                "      every one runs as `{plugin}`, not as you: the authorization gate sees \
                 the plugin as the caller"
                    .replace("{plugin}", plugin),
            );
        }
        if !self.skills.is_empty() {
            lines.push(format!(
                "  - {} skill(s), injected into your prompts when they match: {}",
                self.skills.len(),
                self.skills.join(", ")
            ));
        }
        if !self.records.is_empty() {
            lines.push(format!(
                "  - {} context record(s), which steer the model in this workspace: {}",
                self.records.len(),
                self.records.join(", ")
            ));
        }
        lines.push(
            "  All of it is removed by `stella plugin remove`, because none of it is copied \
             into your own .stella/ — it is read from the package."
                .into(),
        );
        Some(lines.join("\n"))
    }
}

/// What an entry in a package's contributed directory looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A `*.toml` file; the stem names the entry.
    TomlFile,
    /// A `<slug>/SKILL.md` directory; the directory name names the entry.
    SkillDir,
}

/// The named entries of one contributed directory, sorted, or empty when the
/// directory is absent or unreadable.
fn entries(dir: &Path, kind: Kind) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<String> = read
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            match kind {
                Kind::TomlFile => {
                    let is_toml = path.extension().and_then(|ext| ext.to_str()) == Some("toml");
                    (is_toml && path.is_file())
                        .then(|| path.file_stem()?.to_str().map(str::to_string))
                        .flatten()
                }
                Kind::SkillDir => (path.is_dir() && path.join("SKILL.md").is_file())
                    .then(|| path.file_name()?.to_str().map(str::to_string))
                    .flatten(),
            }
        })
        .collect();
    found.sort();
    found
}

/// The inventory of every plugin in a roster, in roster order — what
/// `stella plugin list` prints under each package.
pub(crate) fn inventories(roster: &PluginRoster) -> Vec<(&InstalledPlugin, Inventory)> {
    roster
        .plugins()
        .iter()
        .map(|plugin| (plugin, Inventory::of_package(&plugin.dir)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(dir: &Path) {
        std::fs::create_dir_all(dir.join(TOOLS_DIR)).expect("tools dir");
        std::fs::create_dir_all(dir.join(SKILLS_DIR).join("house-style")).expect("skill dir");
        std::fs::create_dir_all(dir.join(RECORDS_DIR)).expect("rules dir");
        std::fs::write(dir.join(TOOLS_DIR).join("lint_fix.toml"), "").expect("tool");
        std::fs::write(
            dir.join(SKILLS_DIR).join("house-style").join("SKILL.md"),
            "# style\n",
        )
        .expect("skill");
        std::fs::write(dir.join(RECORDS_DIR).join("no-force-push.toml"), "").expect("record");
    }

    /// The three directories are read as the three surfaces, by the names
    /// the rest of the tree already uses for them.
    #[test]
    fn a_package_inventory_names_all_three_surfaces() {
        let dir = tempfile::tempdir().expect("a temp dir");
        package(dir.path());
        let inventory = Inventory::of_package(dir.path());
        assert_eq!(inventory.tools, vec!["lint_fix".to_string()]);
        assert_eq!(inventory.skills, vec!["house-style".to_string()]);
        assert_eq!(inventory.records, vec!["no-force-push".to_string()]);
        assert!(!inventory.is_empty());
    }

    /// A package that ships nothing renders no addendum — the ordinary
    /// wrapper plugin's prompt is unchanged.
    #[test]
    fn a_package_that_ships_nothing_adds_nothing_to_the_prompt() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let inventory = Inventory::of_package(dir.path());
        assert!(inventory.is_empty());
        assert_eq!(inventory.consent_addendum("vera"), None);
    }

    /// **The consent witness.** Everything a package ships is named in the
    /// install prompt, and the tool line says the thing a user most needs to
    /// know: the code runs as the plugin, and the model calls it unprompted.
    #[test]
    fn the_consent_addendum_names_every_contribution_before_install() {
        let dir = tempfile::tempdir().expect("a temp dir");
        package(dir.path());
        let text = Inventory::of_package(dir.path())
            .consent_addendum("vera")
            .expect("a package that ships something must say so");
        for expected in [
            "lint_fix",
            "house-style",
            "no-force-push",
            "runs as `vera`, not as you",
            "the model may call by itself",
            "stella plugin remove",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }

    /// A skill directory with no `SKILL.md` is not a skill, and a stray
    /// non-TOML file is not a tool: the inventory must describe what will
    /// actually load, or it is a prompt that overstates the grant.
    #[test]
    fn only_loadable_entries_are_inventoried() {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::create_dir_all(dir.path().join(SKILLS_DIR).join("empty")).expect("dir");
        std::fs::create_dir_all(dir.path().join(TOOLS_DIR)).expect("dir");
        std::fs::write(dir.path().join(TOOLS_DIR).join("README.md"), "").expect("stray");
        let inventory = Inventory::of_package(dir.path());
        assert!(inventory.is_empty(), "{inventory:?}");
    }
}
