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
//! - **Consent.** [`stella_plugin::consent_text`] names every contribution,
//!   because the manifest declares them (#3565). This module's job is the
//!   other side of that: [`Inventory::listing`] hands the host's own read of
//!   the directories to [`stella_plugin::PluginManifest::reconcile`], which
//!   refuses a package whose declaration and directories disagree in either
//!   direction. The host renders **nothing** of its own about what a package
//!   ships; it used to, and the cost was an embedding host showing a document
//!   that omitted executable code entering the agent's tool surface.
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

/// What one package ships, named as each surface names it — the host's read,
/// and the thing a manifest's declaration is reconciled against.
///
/// **Every field holds the surface's identity, not the filesystem's** (#3565).
/// This began as file stems, which is cheaper and was enough while the
/// inventory only had to *describe* a package to a human. It is not enough to
/// check a declaration against: a stem is not what the model calls, so a
/// package shipping `tools/lint_fix.toml` whose manifest inside says
/// `name = "deploy"` would reconcile against a declared `lint_fix` and still
/// hand the model a tool the consent document never named. Reading the real
/// identity costs one parse per file and closes that.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Inventory {
    /// Tool names as the model will see them — each shipped manifest's own
    /// `name` field — sorted. A manifest that does not parse contributes its
    /// file stem instead, so a broken package is still *described* rather than
    /// silently shrinking to nothing (and, being described wrongly, is
    /// refused at install rather than installed quietly).
    pub(crate) tools: Vec<String>,
    /// Skill slugs (the `<slug>/SKILL.md` directory names), sorted.
    pub(crate) skills: Vec<String>,
    /// Context-record lineage ids — the identity the registry merges on and
    /// the promotion ledger names — sorted and deduplicated. One rules file is
    /// a record *set* and may declare several, so the stem is not the identity
    /// here either. An unparsable file contributes its stem, for the tools
    /// rule's reason.
    pub(crate) records: Vec<String>,
}

impl Inventory {
    /// Read one package directory's inventory. Never fails: an unreadable
    /// directory is an empty one, because a package that cannot be described
    /// must not be the thing that stops an install from being *refused*.
    pub(crate) fn of_package(dir: &Path) -> Self {
        Self {
            tools: tool_names(&dir.join(TOOLS_DIR)),
            skills: skill_slugs(&dir.join(SKILLS_DIR)),
            records: record_lineages(&dir.join(RECORDS_DIR)),
        }
    }

    /// This read, as the listing
    /// [`stella_plugin::PluginManifest::reconcile`] checks a declaration
    /// against.
    ///
    /// The host's half of the #3565 reconciliation, and the reason every field
    /// above carries the identity its *surface* uses rather than the one the
    /// filesystem does: the agreement worth enforcing is between the manifest
    /// and what the model will actually be offered, not between the manifest
    /// and a set of filenames.
    pub(crate) fn listing(&self) -> stella_plugin::PackageListing {
        stella_plugin::PackageListing {
            tools: self.tools.clone(),
            skills: self.skills.clone(),
            records: self.records.clone(),
        }
    }
}

/// The tool names a package's `tools/` directory will contribute — each
/// manifest's own `name`, which is the string the model calls.
///
/// Parsing is delegated to [`stella_tools::custom::parse_manifest`], the same
/// function discovery uses, so this can never disagree with what actually
/// loads. A file it rejects falls back to the stem: the entry stays visible in
/// `stella plugin list` and in the reconciliation, where a broken manifest
/// surfaces as a refusal naming it rather than as a package that quietly ships
/// one fewer tool than it declared.
fn tool_names(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = toml_files(dir)
        .into_iter()
        .map(|(path, stem)| {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|text| stella_tools::custom::parse_manifest(&text, &path).ok())
                .map_or(stem, |tool| tool.name)
        })
        .collect();
    found.sort();
    found.dedup();
    found
}

/// The record lineages a package's `rules/` directory will contribute.
///
/// One file is a record *set*, so the answer is per record and not per file.
/// [`stella_core::records::load_context_file`] is the same reader
/// [`crate::context_records`] uses, for `tool_names`' reason; an unreadable
/// file falls back to the stem for the same one.
fn record_lineages(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = toml_files(dir)
        .into_iter()
        .flat_map(|(path, stem)| {
            let parsed = std::fs::read_to_string(&path).ok().and_then(|text| {
                stella_core::records::load_context_file(&path.display().to_string(), &text).ok()
            });
            match parsed {
                Some(records) if !records.is_empty() => records
                    .into_iter()
                    .map(|loaded| loaded.record.lineage_id)
                    .collect::<Vec<_>>(),
                _ => vec![stem],
            }
        })
        .collect();
    found.sort();
    found.dedup();
    found
}

/// Every `*.toml` file in `dir` with its stem, or nothing when the directory
/// is absent or unreadable.
fn toml_files(dir: &Path) -> Vec<(PathBuf, String)> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    read.flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let is_toml = path.extension().and_then(|ext| ext.to_str()) == Some("toml");
            if !is_toml || !path.is_file() {
                return None;
            }
            let stem = path.file_stem()?.to_str()?.to_string();
            Some((path, stem))
        })
        .collect()
}

/// The skill slugs one contributed `skills/` directory holds — a `<slug>`
/// directory with a `SKILL.md` in it — sorted, or empty when the directory is
/// absent or unreadable.
///
/// The slug *is* the filesystem identity here, unlike a tool's name or a
/// record's lineage: `stella_core::skills` names a skill by its directory, so
/// there is nothing inside the file to read it from.
fn skill_slugs(dir: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<String> = read
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            (path.is_dir() && path.join("SKILL.md").is_file())
                .then(|| path.file_name()?.to_str().map(str::to_string))
                .flatten()
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

    /// A tool manifest of the shape `stella_tools::custom` reads, whose file
    /// stem is deliberately **not** its `name` — the divergence the inventory
    /// must resolve in the model's favour.
    const TOOL: &str = "name = \"lint_fix\"\ndescription = \"run the fixer\"\n\
                        command = [\"./scripts/lint-fix.sh\"]\n";

    /// A record set of the shape `.stella/rules/*.toml` holds. Two records in
    /// one file, so a stem cannot be the identity.
    const RECORDS: &str = "schema = \"context-record/v0.1\"\nset_id = \"vera\"\n\n\
         [[record]]\nlineage_id = \"ctx.vera.no-force-push\"\n\
         record_id = \"rec_vera_1\"\nkind = \"rule\"\n\
         statement = \"never force-push a shared branch\"\n\
         origin = \"imported\"\nsharing_scope = \"repository\"\nstatus = \"active\"\n\n\
         [[record]]\nlineage_id = \"ctx.vera.review-before-merge\"\n\
         record_id = \"rec_vera_2\"\nkind = \"rule\"\n\
         statement = \"a change is reviewed before it merges\"\n\
         origin = \"imported\"\nsharing_scope = \"repository\"\nstatus = \"active\"\n";

    fn package(dir: &Path) {
        std::fs::create_dir_all(dir.join(TOOLS_DIR)).expect("tools dir");
        std::fs::create_dir_all(dir.join(SKILLS_DIR).join("house-style")).expect("skill dir");
        std::fs::create_dir_all(dir.join(RECORDS_DIR)).expect("rules dir");
        std::fs::write(dir.join(TOOLS_DIR).join("fixer.toml"), TOOL).expect("tool");
        std::fs::write(
            dir.join(SKILLS_DIR).join("house-style").join("SKILL.md"),
            "# style\n",
        )
        .expect("skill");
        std::fs::write(dir.join(RECORDS_DIR).join("set.toml"), RECORDS).expect("record");
    }

    /// **The #3565 host-side witness.** The three directories are read as the
    /// three surfaces, and each entry is named by the identity **its surface**
    /// uses — a tool by the name the model calls, a record by its lineage —
    /// never by the filename. `fixer.toml` declaring `lint_fix`, and one
    /// rules file holding two lineages, both answer differently under a
    /// stem-based read.
    #[test]
    fn a_package_inventory_names_all_three_surfaces_as_the_surface_names_them() {
        let dir = tempfile::tempdir().expect("a temp dir");
        package(dir.path());
        let inventory = Inventory::of_package(dir.path());
        assert_eq!(
            inventory.tools,
            vec!["lint_fix".to_string()],
            "the tool's own name, not its file stem `fixer`"
        );
        assert_eq!(inventory.skills, vec!["house-style".to_string()]);
        assert_eq!(
            inventory.records,
            vec![
                "ctx.vera.review-before-merge".to_string(),
                "ctx.vera.no-force-push".to_string()
            ]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>(),
            "both lineages of one set file, not the stem `set`"
        );
        assert_ne!(inventory, Inventory::default());
    }

    /// **The reconciliation witness (#3565 item 2).** A manifest that names
    /// exactly what the directories hold installs; one that names less, or
    /// more, is refused with both sides in the message.
    #[test]
    fn a_declaration_must_agree_with_the_directories() {
        let dir = tempfile::tempdir().expect("a temp dir");
        package(dir.path());
        let listing = Inventory::of_package(dir.path()).listing();

        let complete = stella_plugin::PluginManifest::from_toml_str(
            "name = \"vera\"\n\n\
             [[tools]]\nname = \"lint_fix\"\ndescription = \"run the fixer\"\n\n\
             [[skills]]\nslug = \"house-style\"\ndescription = \"how we write\"\n\n\
             [[records]]\nlineage = \"ctx.vera.no-force-push\"\nstatement = \"no force-push\"\n\n\
             [[records]]\nlineage = \"ctx.vera.review-before-merge\"\nstatement = \"review it\"\n",
        )
        .expect("a complete declaration must load");
        complete
            .reconcile(&listing)
            .expect("a declaration that names everything must reconcile");

        let silent = stella_plugin::PluginManifest::from_toml_str("name = \"vera\"")
            .expect("a manifest declaring nothing still loads");
        let refusal = silent
            .reconcile(&listing)
            .expect_err("a package whose directories nobody declared must be refused")
            .to_string();
        for expected in [
            "lint_fix",
            "house-style",
            "ctx.vera.no-force-push",
            "[[tools]]",
            "tools/",
        ] {
            assert!(
                refusal.contains(expected),
                "missing {expected:?} in:\n{refusal}"
            );
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
        assert_eq!(inventory, Inventory::default(), "{inventory:?}");
    }

    /// A manifest that does not parse still names its entry, by stem — so the
    /// package is described rather than silently shrinking, and the
    /// reconciliation refuses it instead of installing a tool nobody could
    /// read.
    #[test]
    fn an_unparsable_contribution_falls_back_to_its_stem() {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::create_dir_all(dir.path().join(TOOLS_DIR)).expect("dir");
        std::fs::write(
            dir.path().join(TOOLS_DIR).join("broken.toml"),
            "not = [toml",
        )
        .expect("broken");
        assert_eq!(
            Inventory::of_package(dir.path()).tools,
            vec!["broken".to_string()]
        );
    }
}
