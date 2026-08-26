// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The port through which a host tells the dashboard what a workspace's
//! installed plugins contribute (#4917, #4974), and the witnesses for the two
//! views that read it.
//!
//! A sibling of [`super`] rather than part of it because the two answer
//! different questions. Everything in `fsview` derives what it shows from
//! files this process can see; nothing here can be derived that way at all —
//! a package's skills and records are contributed by derivation from
//! `<plugin_dir>`, and reaching them means resolving a roster and a trust
//! gate that live in `stella-cli`, which this crate may not link. The
//! trait's own doc comment carries that argument in full.

use std::path::{Path, PathBuf};

/// The tier an installed plugin sits at, which is also the tier its
/// contributed skills are governed by (`stella_cli::skill_manager`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginTier {
    /// `<workspace>/.stella/plugins` — the tier behind the project trust gate.
    Project,
    /// `~/.stella/plugins` — the operator's own machine-scope tier.
    User,
}

impl PluginTier {
    /// The scope label the skills view already uses for the user's own rows.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
        }
    }
}

/// One installed plugin's contributed directory of one kind, as the host that
/// resolved the roster reports it.
///
/// Mirrors `stella_cli::plugin_cmd::package::ContributedDir`, which is what
/// every host answering this port already holds.
#[derive(Debug, Clone)]
pub struct ContributedDir {
    /// The contributing plugin's manifest `name`.
    pub plugin: String,
    /// The tier its package is installed at.
    pub tier: PluginTier,
    /// `<plugin_dir>/skills` or `<plugin_dir>/rules`. It need not exist; an
    /// absent directory is an empty one, the same as every other root this
    /// module scans.
    pub dir: PathBuf,
}

/// The port a host implements to say what a session started in this workspace
/// would load out of its installed plugins (#4917, #4974).
///
/// # Why a port and not a directory scan
///
/// A plugin's skills live under `<plugin_dir>/skills` and its context records
/// under `<plugin_dir>/rules`, and both are contributed *by derivation* —
/// nothing is ever copied into the user's own `.stella/`, which is the
/// property that makes `stella plugin remove` total
/// (`stella_cli::plugin_cmd::roster`). So the dashboard cannot find them by
/// widening the roots it walks: it would have to resolve the roster itself,
/// and with it the project-tier trust gate (#3509), the `plugins.<name> =
/// "off"` retraction, the install receipt, and the reconcile-on-every-load
/// check. Every one of those is a security answer, and a second implementation
/// of a security answer is a second answer — the shape this area exists to
/// avoid.
///
/// So the host that already computes that answer hands it over instead —
/// ports, not direct dependencies (AGENTS.md rule 1). `stella observe`
/// implements this over `plugin_cmd::package::contributed_skill_dirs` and
/// `contributed_record_dirs`, the same two calls the session's own skill load
/// and context-record registry make, so the dashboard and the loader cannot
/// disagree about what is in force — including about a workspace nobody has
/// trusted, where both answer nothing.
///
/// # Why one port with two questions rather than two ports
///
/// Both questions have one answer-holder — the resolved roster — and one
/// wrong answer: a roster the host resolved for skills and did not resolve for
/// records would put the dashboard's two panels on different trust decisions.
/// A host implements the roster once and answers both from it. It is also what
/// keeps [`crate::serve`] to one `Arc`: a third contributed surface already
/// exists (`contributed_mcp_servers`, #4733), and a port per surface would
/// have the plumbing grow with it.
///
/// The port is asked per request rather than once at startup, so a package
/// installed while the dashboard is open shows up on the next refresh.
///
/// **The observer's environment is the observer's.** `STELLA_TRUST_PROJECT`
/// is read from the process asking, so a `stella observe` launched without it
/// reports the project tier empty while an agent session launched with it
/// loads those skills. That is the same authority answering in two
/// environments rather than two authorities, and what the dashboard reports is
/// what a session started *here* would load.
pub trait PluginContributions: Send + Sync {
    /// The contributed skills directories in force for `workspace_root`.
    fn contributed_skill_dirs(&self, workspace_root: &Path) -> Vec<ContributedDir>;

    /// The contributed context-record directories in force for
    /// `workspace_root`.
    fn contributed_record_dirs(&self, workspace_root: &Path) -> Vec<ContributedDir>;
}

/// The answer for a caller with no roster to report — every unit test that
/// drives [`crate::respond`], and any host that does not implement the port.
pub struct NoContributions;

impl PluginContributions for NoContributions {
    fn contributed_skill_dirs(&self, _workspace_root: &Path) -> Vec<ContributedDir> {
        Vec::new()
    }

    fn contributed_record_dirs(&self, _workspace_root: &Path) -> Vec<ContributedDir> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::super::{rules_files, skills};
    use super::*;
    /// A fake roster, so the contributed views can be tested without this
    /// crate linking the one that resolves rosters (which it may not).
    ///
    /// The two kinds are held separately because the real roster answers them
    /// from two different package subdirectories, and a fake that returned one
    /// list for both would let a view read the other kind's directory and
    /// still pass.
    #[derive(Default)]
    struct FakeRoster {
        skills: Vec<ContributedDir>,
        records: Vec<ContributedDir>,
    }

    impl FakeRoster {
        fn with_skills(dirs: Vec<ContributedDir>) -> Self {
            Self {
                skills: dirs,
                records: Vec::new(),
            }
        }

        fn with_records(dirs: Vec<ContributedDir>) -> Self {
            Self {
                skills: Vec::new(),
                records: dirs,
            }
        }
    }

    impl PluginContributions for FakeRoster {
        fn contributed_skill_dirs(&self, _workspace_root: &Path) -> Vec<ContributedDir> {
            self.skills.clone()
        }

        fn contributed_record_dirs(&self, _workspace_root: &Path) -> Vec<ContributedDir> {
            self.records.clone()
        }
    }

    /// Write a `<dir>/<slug>/SKILL.md`, the layout a package ships.
    fn plant_skill(dir: &Path, slug: &str, description: &str, body: &str) {
        let entry = dir.join(slug);
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(
            entry.join("SKILL.md"),
            format!("---\nname: {slug}\ndescription: {description}\n---\n\n{body}\n"),
        )
        .unwrap();
    }

    /// **The parity witness** (#4917). A plugin's skill is listed in the
    /// dashboard's skills view and names the package that shipped it.
    ///
    /// #3567 gave a contributed skill an attribution field and put it on
    /// screen in the SKILLS tab; this view scanned exactly two roots —
    /// `<workspace>/.stella/skills` and `<user_config>/skills` — and a
    /// package's skills live under `<plugin_dir>/skills` and are never copied
    /// into either. So a user who installed a package for its skills saw them
    /// in the deck and could not find them in the dashboard, which reads as
    /// the complete list.
    #[test]
    fn a_plugins_skill_is_listed_and_names_the_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let own = root.join(".stella/skills");
        std::fs::create_dir_all(&own).unwrap();
        plant_skill(&own, "our-own", "the workspace's own", "OWN_BODY");

        let package = root.join(".stella/plugins/vera/skills");
        std::fs::create_dir_all(&package).unwrap();
        plant_skill(
            &package,
            "house-style",
            "how this shop writes code",
            "PKG_BODY",
        );

        // Named rather than counted: the user-scope root this view also scans
        // is the developer's real `~/.stella/skills`, and this crate has no
        // env lock to move it.
        let named = |rows: &Value, name: &str| -> Vec<Value> {
            rows.as_array()
                .expect("skills returns an array")
                .iter()
                .filter(|row| row["name"] == name)
                .cloned()
                .collect()
        };
        let empty = skills(root, &NoContributions);
        assert!(
            named(&empty, "house-style").is_empty(),
            "anti-vacuity — the package's skill is absent before the roster is asked"
        );
        assert_eq!(
            named(&empty, "our-own").len(),
            1,
            "and the workspace's own is there either way"
        );

        let listed = skills(
            root,
            &FakeRoster::with_skills(vec![ContributedDir {
                plugin: "vera".to_string(),
                tier: PluginTier::Project,
                dir: package.clone(),
            }]),
        );
        let found = named(&listed, "house-style");
        let row = found
            .first()
            .unwrap_or_else(|| panic!("the package's skill is listed: {listed:#?}"));
        assert_eq!(row["contributed_by"], "vera", "and names the package");
        assert_eq!(
            row["scope"], "project",
            "at the tier its package is installed at, which is the tier whose \
             state file governs it"
        );
        assert_eq!(
            row["learned"], false,
            "a package's skill was not mined by this workspace's loop, whatever \
             its frontmatter says — counted as learned it would inflate the tile"
        );
        assert!(
            row["evidence_grade"].is_null(),
            "no proposal here to grade it"
        );
        assert!(
            named(&listed, "our-own")[0]["contributed_by"].is_null(),
            "and the user's own row names nobody"
        );
    }

    /// **The precedence witness.** A package may not silently take over a name
    /// the user wrote: the recall loader drops the plugin's same-named skill
    /// before it reaches the prompt, so listing it would show a steer that is
    /// not in force.
    #[test]
    fn a_plugins_skill_never_displaces_one_the_user_wrote_in_the_listing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let own = root.join(".stella/skills");
        std::fs::create_dir_all(&own).unwrap();
        plant_skill(&own, "house-style", "the user's own wording", "OWN_BODY");

        let package = root.join(".stella/plugins/vera/skills");
        std::fs::create_dir_all(&package).unwrap();
        plant_skill(&package, "house-style", "the package's wording", "PKG_BODY");

        let listed = skills(
            root,
            &FakeRoster::with_skills(vec![ContributedDir {
                plugin: "vera".to_string(),
                tier: PluginTier::Project,
                dir: package,
            }]),
        );
        let rows: Vec<&Value> = listed
            .as_array()
            .expect("skills returns an array")
            .iter()
            .filter(|row| row["name"] == "house-style")
            .collect();
        assert_eq!(rows.len(), 1, "one row for one name: {rows:#?}");
        assert_eq!(rows[0]["description"], "the user's own wording");
        assert!(rows[0]["contributed_by"].is_null());
    }

    /// Write a record set at `<dir>/<file>.toml` — one file, one
    /// `[[record]]` per `(lineage, statement)` pair.
    fn plant_records(dir: &Path, file: &str, records: &[(&str, &str)]) {
        std::fs::create_dir_all(dir).unwrap();
        let mut text = String::from("schema = \"context-record/v0.1\"\nset_id = \"house\"\n");
        for (lineage, statement) in records {
            text.push_str(&format!(
                "\n[[record]]\nlineage_id = \"{lineage}\"\nkind = \"constraint\"\n\
                 statement = \"{statement}\"\n\n  [record.steering]\n  force = \"must\"\n"
            ));
        }
        std::fs::write(dir.join(format!("{file}.toml")), text).unwrap();
    }

    /// **The parity witness** (#4974). A plugin's context record is listed in
    /// the dashboard's rules panel and names the package that shipped it.
    ///
    /// A package's `rules/` steers every matching turn — `stella context
    /// explain` prints `contributed by the \`vera\` plugin` — and this panel
    /// scanned exactly `.stella/rules`. A record is the surface that steers
    /// *quietly*, without appearing in a transcript as anything the agent did,
    /// so a panel that reads as the complete list of what steers this
    /// workspace and omits it is worse than one that showed nothing.
    #[test]
    fn a_plugins_record_is_listed_and_names_the_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        plant_records(
            &root.join(".stella/rules"),
            "ctx.house.ours",
            &[("ctx.house.ours", "OUR_OWN_MARKER")],
        );

        let package = root.join(".stella/plugins/vera/rules");
        plant_records(
            &package,
            "ctx.house.theirs",
            &[("ctx.house.theirs", "PACKAGE_MARKER")],
        );

        let named = |rows: &Value, lineage: &str| -> Vec<Value> {
            rows.as_array()
                .expect("rules_files returns an array")
                .iter()
                .filter(|row| row["lineage_id"] == lineage)
                .cloned()
                .collect()
        };

        let empty = rules_files(root, &NoContributions);
        assert!(
            named(&empty, "ctx.house.theirs").is_empty(),
            "anti-vacuity — the package's record is absent before the roster is asked"
        );
        assert_eq!(
            named(&empty, "ctx.house.ours").len(),
            1,
            "and the workspace's own is there either way"
        );

        let listed = rules_files(
            root,
            &FakeRoster::with_records(vec![ContributedDir {
                plugin: "vera".to_string(),
                tier: PluginTier::Project,
                dir: package,
            }]),
        );
        let found = named(&listed, "ctx.house.theirs");
        let row = found
            .first()
            .unwrap_or_else(|| panic!("the package's record is listed: {listed:#?}"));
        assert_eq!(row["contributed_by"], "vera", "and names the package");
        assert_eq!(
            row["statement"], "PACKAGE_MARKER",
            "with the fields the panel renders, not just a title"
        );
        assert!(
            named(&listed, "ctx.house.ours")[0]["contributed_by"].is_null(),
            "and the workspace's own row names nobody"
        );
    }

    /// **The precedence witness.** A rules file is a record *set*, so the
    /// panel dedupes by lineage and not by file: a package whose set shares
    /// one lineage with the workspace's own must lose that lineage and keep
    /// every other record in the same file.
    ///
    /// `stella_cli::context_records::plugin_first` reads contributed
    /// directories first and every merge below it is later-wins by lineage, so
    /// the workspace's copy is what steers — listing the package's would show
    /// a steer that is not in force. Dropping the whole file instead would
    /// hide records nothing collided with, which is the failure mode the set
    /// shape introduces and the skills half never had.
    #[test]
    fn a_plugins_record_loses_a_shared_lineage_and_keeps_the_rest_of_its_set() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        plant_records(
            &root.join(".stella/rules"),
            "ctx.house.shared",
            &[("ctx.house.shared", "OURS_WINS")],
        );

        let package = root.join(".stella/plugins/vera/rules");
        plant_records(
            &package,
            "ctx.house.set",
            &[
                ("ctx.house.shared", "THEIRS_LOSES"),
                ("ctx.house.only-theirs", "THEIRS_SURVIVES"),
            ],
        );

        let listed = rules_files(
            root,
            &FakeRoster::with_records(vec![ContributedDir {
                plugin: "vera".to_string(),
                tier: PluginTier::Project,
                dir: package,
            }]),
        );
        let rows = listed.as_array().expect("rules_files returns an array");

        let shared: Vec<&Value> = rows
            .iter()
            .filter(|row| row["lineage_id"] == "ctx.house.shared")
            .collect();
        assert_eq!(shared.len(), 1, "one row for one lineage: {shared:#?}");
        assert_eq!(shared[0]["statement"], "OURS_WINS");
        assert!(shared[0]["contributed_by"].is_null());

        let survivor: Vec<&Value> = rows
            .iter()
            .filter(|row| row["lineage_id"] == "ctx.house.only-theirs")
            .collect();
        assert_eq!(
            survivor.len(),
            1,
            "the rest of the package's set is not collateral: {rows:#?}"
        );
        assert_eq!(survivor[0]["contributed_by"], "vera");
    }
}
