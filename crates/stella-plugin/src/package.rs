//! What a plugin *ships*, declared — the package half of the manifest
//! (#3565, `doc:pipeline-as-plugins` §13).
//!
//! The rest of this crate answers "what say does this plugin have in the turn
//! loop?". This module answers the other question a human installing one has:
//! **what arrives on my machine besides a process?** A package may carry
//! script tools, skills, context records and MCP servers, and each of those is
//! a real power — a tool most of all, because it is executable code entering
//! the agent's tool surface that the model may call on its own initiative for
//! the rest of the session, and a server for the same reason one step removed:
//! it decides its own tool list when it connects.
//!
//! # Declared here, discovered by the host, and the two must agree
//!
//! The loader half (#3380) discovers contributions **by directory
//! convention**: `<plugin_dir>/tools/*.toml`, `<plugin_dir>/skills/<slug>/
//! SKILL.md`, `<plugin_dir>/rules/*.toml`, `<plugin_dir>/mcp.toml`, each
//! handed to the loader that already reads that surface. That gives one property worth keeping —
//! provenance cannot be forged, because a tool's contributing plugin is
//! stamped from the directory that was read and never from anything inside the
//! file — and one that cost a consent defect: this crate is pure, so
//! [`crate::consent_text`] could not see a directory, and an embedding host
//! calling it showed a document that omitted every one of those contributions.
//!
//! Neither half alone is sufficient, which is the whole design here:
//!
//! - **A declaration nobody checks is a promise.** So the host supplies what it
//!   actually found as a [`PackageListing`] and
//!   [`PluginManifest::reconcile`] refuses any disagreement.
//! - **A directory nobody declared cannot be consented to.** Same function,
//!   other direction: a `tools/` entry the manifest does not name is a
//!   refusal, not a silent extra tool.
//! - **Provenance still comes from the directory, never the file.** Nothing
//!   here is an input to attribution. A declaration names entries; it cannot
//!   claim to have shipped one it did not, because the listing it is checked
//!   against is the host's own read.
//!
//! # Names, never paths — so nothing here needs interpolating
//!
//! A contribution is declared by the identity its own surface knows it by: a
//! tool by the name the model calls (`stella_tools::custom`'s `name`), a skill
//! by its slug, a record by its lineage id, an MCP server by its `[servers]`
//! key. No entry carries a path, so the
//! `${plugin_dir}` placeholder [`crate::Runtime`] and
//! [`OracleCommand`](crate::OracleCommand) use does not arise — and where a
//! path genuinely does (a tool's argv), it stays in the tool manifest the host
//! reads, which is the one file that can be checked against what runs.
//!
//! That is also why a `[[tools]]` entry deliberately does **not** restate the
//! argv. Author prose is labelled as the author's throughout this crate
//! ([`Capability::purpose`](crate::Capability)), but an argv reads as a *fact
//! about what will run*, and this crate cannot check it against the file that
//! decides. Rendering an unverified command line at a consent prompt is the
//! "claimed limit shown as an enforced one" mistake wearing different clothes.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::error::ManifestError;
use crate::manifest::PluginManifest;

/// One script tool a package ships — the sharpest thing in a consent document
/// after a process.
///
/// A contributed tool is discovered by `stella_tools::custom::discover_with_plugins`
/// from `<plugin_dir>/tools/*.toml`, advertised to the model beside the
/// built-ins, and executed as `Principal::Plugin(<this manifest's name>)`. The
/// user's own manifests keep their names on a collision, so a package can never
/// replace a tool its host already defines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolContribution {
    /// The name the model calls, as the shipped tool manifest's own `name`
    /// field spells it — not the file stem, because the stem is not what the
    /// model sees and a consent document must name the thing that runs.
    pub name: String,
    /// What the tool does, in the author's words. Shown at consent, flattened
    /// to one line.
    ///
    /// Required rather than optional, on [`Capability::purpose`](crate::Capability)'s
    /// reasoning: "this package installs a tool called `deploy`" is not
    /// something a human can consent to.
    pub description: String,
}

/// One skill a package ships — `<plugin_dir>/skills/<slug>/SKILL.md`.
///
/// A skill is never enforced: it is selected against the prompt and injected as
/// volatile context. That makes it the mildest of the three, and the consent
/// text says so rather than levelling the language across all of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillContribution {
    /// The slug — the `<slug>` directory name under `skills/`, which is the
    /// identity `stella_core::skills` knows it by.
    pub slug: String,
    /// What the skill teaches, in the author's words.
    pub description: String,
}

/// One MCP server a package ships — an entry in `<plugin_dir>/mcp.toml`, the
/// format `.stella/mcp.toml` holds.
///
/// **The sharpest contribution in the package, and the one that was refused
/// until it could be enumerated.** A server's tools enter the registry as
/// `mcp__<server>__<tool>`, so installing one hands the model a set of calls
/// whose membership the server decides *at connect time* — and
/// [`crate::configure`] refused an `mcp` section for exactly that reason: "the
/// effect of one line is a set of tools the consent document never listed".
///
/// Declaring the server by name is what answers that. It does not make the
/// tool list knowable in advance — nothing can, short of connecting — so the
/// consent text says what is true: this package starts this named server, and
/// the tools it adds are whatever it advertises. That is a real disclosure
/// (a human can look the server up, and sees it named at install and in
/// `stella plugin list`) rather than a false precision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpContribution {
    /// The server name — the key under `[servers]` in the shipped `mcp.toml`,
    /// and the `<server>` half of every `mcp__<server>__<tool>` the model
    /// sees. Declared rather than derived, on this module's "names, never
    /// paths" rule, and checked against the host's own read of the file.
    pub server: String,
    /// What the server is for, in the author's words — the same one-line
    /// disclosure every other contribution carries, and required for
    /// [`ToolContribution::description`]'s reason: "this package installs an
    /// MCP server called `acme`" is not something a human can consent to.
    pub description: String,
}

/// One context record a package ships — `<plugin_dir>/rules/*.toml`, the
/// format `.stella/rules/` holds.
///
/// **A quieter power than a tool, and a wider one.** A tool runs when the model
/// chooses to call it; a record steers *every* future turn in the workspace
/// that matches it, without ever appearing in a transcript as an action. That
/// is why it is disclosed in its own words rather than folded into a count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordContribution {
    /// The record's `lineage_id`, which is the identity the registry merges,
    /// the promotion ledger names, and `stella context explain` prints.
    pub lineage: String,
    /// The record's statement, in the author's words — what it will steer
    /// toward.
    pub statement: String,
    /// What the record asks to be able to do. Defaults to
    /// [`RecordEnforcement::Advisory`], and see that type for why the other
    /// variant is declarable but never loadable.
    #[serde(default)]
    pub enforcement: RecordEnforcement,
}

/// What a contributed record asks to be able to do.
///
/// # Why `blocking` is spelled and then refused
///
/// A record with `enforcement.mode = "blocking"` can deny a tool call. In the
/// regulated governance tier that authority is granted only through the
/// repository-visible, hash-chained promotion ledger
/// (`.stella/rules/promotions.jsonl`), where each grant names its approver and
/// its reason and travels through the same review as the record it arms.
///
/// A plugin's records are not in that repository and are not covered by that
/// chain: the package is installed, not reviewed, and its files are outside the
/// Git tree the ledger is verified against. So a package may ship records that
/// *steer*, and may never ship one that *denies* —
/// [`ManifestError::PluginRecordCannotEnforce`] is that rule.
///
/// The variant exists so the refusal can be a named error explaining the
/// governance rule, rather than a deserializer's "unknown variant". A manifest
/// that asks for it is not making a typo; it is asking for something the
/// governance model does not grant, and it deserves to be told which.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordEnforcement {
    /// It steers the model and denies nothing. The only loadable value, and
    /// the default.
    #[default]
    Advisory,
    /// It asks to deny tool calls — refused at load, per this type's docs.
    Blocking,
}

impl std::fmt::Display for RecordEnforcement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RecordEnforcement::Advisory => "advisory",
            RecordEnforcement::Blocking => "blocking",
        })
    }
}

/// Which of the four surfaces a contribution enters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ContributionKind {
    /// A script tool the model may call.
    Tool,
    /// A skill selected into the prompt.
    Skill,
    /// A context record that steers turns.
    Record,
    /// An MCP server whose tools join the agent's surface.
    Mcp,
}

impl ContributionKind {
    /// The directory a package ships this kind in, relative to the package
    /// root — named here so a refusal can point at the place to fix.
    #[must_use]
    pub fn dir(self) -> &'static str {
        match self {
            // `rules`, not `records`, because that is what the directory is
            // called everywhere else (`.stella/rules`, `~/.stella/rules`).
            ContributionKind::Tool => "tools",
            ContributionKind::Skill => "skills",
            ContributionKind::Record => "rules",
            // A file, not a directory — the one kind a package ships as a
            // single `mcp.toml`, because that is the unit `.stella/mcp.toml`
            // is and a server is a table entry rather than a file.
            ContributionKind::Mcp => "mcp.toml",
        }
    }

    /// The manifest table that declares it.
    #[must_use]
    pub fn table(self) -> &'static str {
        match self {
            ContributionKind::Tool => "[[tools]]",
            ContributionKind::Skill => "[[skills]]",
            ContributionKind::Record => "[[records]]",
            ContributionKind::Mcp => "[[mcp]]",
        }
    }
}

impl std::fmt::Display for ContributionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ContributionKind::Tool => "tool",
            ContributionKind::Skill => "skill",
            ContributionKind::Record => "record",
            ContributionKind::Mcp => "MCP server",
        })
    }
}

/// What a host actually found in a package's directories, by the identity each
/// surface knows an entry by.
///
/// The host's half of the reconciliation. This crate performs no I/O, so it
/// cannot look; the caller looks and hands the answer over. Building one is the
/// host's whole responsibility, and the identities have to be the *effective*
/// ones — a tool's own `name` field, not its file stem — or the agreement this
/// checks would be between the manifest and a filename rather than between the
/// manifest and what the model will be offered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackageListing {
    /// Tool names found under `tools/`.
    pub tools: Vec<String>,
    /// Skill slugs found under `skills/`.
    pub skills: Vec<String>,
    /// Record lineage ids found under `rules/`.
    pub records: Vec<String>,
    /// MCP server names found in `mcp.toml` — the `[servers]` keys.
    pub mcp: Vec<String>,
}

impl PackageListing {
    /// The listing for a package that ships nothing.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// What was found for one kind.
    #[must_use]
    fn of(&self, kind: ContributionKind) -> &[String] {
        match kind {
            ContributionKind::Tool => &self.tools,
            ContributionKind::Skill => &self.skills,
            ContributionKind::Record => &self.records,
            ContributionKind::Mcp => &self.mcp,
        }
    }
}

/// One surface on which a package's declaration and its directory disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindMismatch {
    /// The surface.
    pub kind: ContributionKind,
    /// Found on disk, named by no declaration — the direction that would
    /// otherwise put an unconsented contribution into the agent's surface.
    /// Sorted.
    pub undeclared: Vec<String>,
    /// Declared, present in no directory — a consent document describing
    /// something that will never exist. Sorted.
    pub unshipped: Vec<String>,
}

/// A package whose declaration and directories disagree.
///
/// Its own error type rather than a [`ManifestError`] variant, because it
/// answers a different question: `ManifestError` is "this manifest is not
/// well-formed", which a plugin author fixes by reading the manifest alone,
/// while this is "this manifest does not describe this directory", which needs
/// both sides in hand. A host renders it verbatim at the refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageMismatch {
    /// Every surface that disagrees, in [`ContributionKind`] order and never
    /// empty — an agreement returns `Ok`, so a value of this type always names
    /// at least one disagreement.
    pub kinds: Vec<KindMismatch>,
}

impl std::fmt::Display for PackageMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "this package's manifest does not describe what it ships, so what it \
             installs cannot be consented to",
        )?;
        for mismatch in &self.kinds {
            let kind = mismatch.kind;
            if !mismatch.undeclared.is_empty() {
                write!(
                    f,
                    "\n  - {}/ ships {} the manifest does not declare: {} — add {} entries for \
                     them, or delete them",
                    kind.dir(),
                    plural(mismatch.undeclared.len(), kind),
                    mismatch.undeclared.join(", "),
                    kind.table(),
                )?;
            }
            if !mismatch.unshipped.is_empty() {
                write!(
                    f,
                    "\n  - {} declares {} that {}/ does not ship: {} — ship them, or drop the \
                     declaration",
                    kind.table(),
                    plural(mismatch.unshipped.len(), kind),
                    kind.dir(),
                    mismatch.unshipped.join(", "),
                )?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for PackageMismatch {}

/// `1 tool` / `2 tools` — a count with the kind's noun, agreeing in number.
fn plural(count: usize, kind: ContributionKind) -> String {
    if count == 1 {
        format!("{count} {kind}")
    } else {
        format!("{count} {kind}s")
    }
}

impl PluginManifest {
    /// Refuse a package whose declaration and directories disagree — the
    /// #3565 rule, in the one place both sides are in hand.
    ///
    /// Pure: the host supplies its own read as `listing`, so this crate still
    /// touches no filesystem. Both directions are checked, because each covers
    /// a different failure. An **undeclared** entry is the security one: it
    /// enters the agent's surface without appearing in the consent document a
    /// human read. An **unshipped** one is the honesty half: a consent document
    /// promising a tool that does not exist teaches a reader that the document
    /// is decorative.
    ///
    /// Names are compared as sets, so declaration order is free — the ordering
    /// in the manifest is the author's and is what [`crate::consent_text`]
    /// renders.
    ///
    /// # Errors
    ///
    /// [`PackageMismatch`], naming every surface that disagrees and both sides
    /// of each disagreement.
    pub fn reconcile(&self, listing: &PackageListing) -> Result<(), PackageMismatch> {
        let declared = [
            (
                ContributionKind::Tool,
                self.tools
                    .iter()
                    .map(|tool| tool.name.clone())
                    .collect::<Vec<_>>(),
            ),
            (
                ContributionKind::Skill,
                self.skills.iter().map(|skill| skill.slug.clone()).collect(),
            ),
            (
                ContributionKind::Record,
                self.records
                    .iter()
                    .map(|record| record.lineage.clone())
                    .collect(),
            ),
            (
                ContributionKind::Mcp,
                self.mcp.iter().map(|mcp| mcp.server.clone()).collect(),
            ),
        ];

        let kinds: Vec<KindMismatch> = declared
            .into_iter()
            .filter_map(|(kind, declared)| {
                let found: HashSet<&str> = listing.of(kind).iter().map(String::as_str).collect();
                let declared: HashSet<&str> = declared.iter().map(String::as_str).collect();
                let undeclared = sorted_difference(&found, &declared);
                let unshipped = sorted_difference(&declared, &found);
                (!undeclared.is_empty() || !unshipped.is_empty()).then_some(KindMismatch {
                    kind,
                    undeclared,
                    unshipped,
                })
            })
            .collect();

        if kinds.is_empty() {
            Ok(())
        } else {
            Err(PackageMismatch { kinds })
        }
    }
}

/// The entries of `left` that `right` does not hold, sorted so a refusal reads
/// the same twice.
fn sorted_difference(left: &HashSet<&str>, right: &HashSet<&str>) -> Vec<String> {
    let mut out: Vec<String> = left
        .difference(right)
        .map(|name| (*name).to_string())
        .collect();
    out.sort();
    out
}

/// The cross-field rules for the three package tables, kept beside the types
/// they govern exactly as [`crate::consent`]'s and [`crate::wrapper`]'s are.
pub(crate) fn validate_contributions(manifest: &PluginManifest) -> Result<(), ManifestError> {
    let mut tools = HashSet::with_capacity(manifest.tools.len());
    for tool in &manifest.tools {
        let name = tool.name.trim();
        if name.is_empty() {
            return Err(ManifestError::EmptyContributionName {
                kind: ContributionKind::Tool,
            });
        }
        if tool.description.trim().is_empty() {
            return Err(ManifestError::EmptyContributionDescription {
                kind: ContributionKind::Tool,
                name: tool.name.clone(),
            });
        }
        if !tools.insert(name) {
            return Err(ManifestError::DuplicateContribution {
                kind: ContributionKind::Tool,
                name: tool.name.clone(),
            });
        }
    }

    let mut skills = HashSet::with_capacity(manifest.skills.len());
    for skill in &manifest.skills {
        let slug = skill.slug.trim();
        if slug.is_empty() {
            return Err(ManifestError::EmptyContributionName {
                kind: ContributionKind::Skill,
            });
        }
        if skill.description.trim().is_empty() {
            return Err(ManifestError::EmptyContributionDescription {
                kind: ContributionKind::Skill,
                name: skill.slug.clone(),
            });
        }
        if !skills.insert(slug) {
            return Err(ManifestError::DuplicateContribution {
                kind: ContributionKind::Skill,
                name: skill.slug.clone(),
            });
        }
    }

    let mut records = HashSet::with_capacity(manifest.records.len());
    for record in &manifest.records {
        let lineage = record.lineage.trim();
        if lineage.is_empty() {
            return Err(ManifestError::EmptyContributionName {
                kind: ContributionKind::Record,
            });
        }
        if record.statement.trim().is_empty() {
            return Err(ManifestError::EmptyContributionDescription {
                kind: ContributionKind::Record,
                name: record.lineage.clone(),
            });
        }
        // The governance rule, enforced at the earliest point it can be: a
        // package's records are outside the repository the promotion ledger
        // covers, so none of them may ask to deny a tool call.
        if record.enforcement == RecordEnforcement::Blocking {
            return Err(ManifestError::PluginRecordCannotEnforce {
                lineage: record.lineage.clone(),
            });
        }
        if !records.insert(lineage) {
            return Err(ManifestError::DuplicateContribution {
                kind: ContributionKind::Record,
                name: record.lineage.clone(),
            });
        }
    }

    let mut servers = HashSet::with_capacity(manifest.mcp.len());
    for mcp in &manifest.mcp {
        let server = mcp.server.trim();
        if server.is_empty() {
            return Err(ManifestError::EmptyContributionName {
                kind: ContributionKind::Mcp,
            });
        }
        if mcp.description.trim().is_empty() {
            return Err(ManifestError::EmptyContributionDescription {
                kind: ContributionKind::Mcp,
                name: mcp.server.clone(),
            });
        }
        if !servers.insert(server) {
            return Err(ManifestError::DuplicateContribution {
                kind: ContributionKind::Mcp,
                name: mcp.server.clone(),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<PluginManifest, ManifestError> {
        PluginManifest::from_toml_str(text)
    }

    const PACKAGE: &str = "name = \"vera\"\n\n\
         [[tools]]\nname = \"lint_fix\"\ndescription = \"run the fixer\"\n\n\
         [[skills]]\nslug = \"house-style\"\ndescription = \"how we write\"\n\n\
         [[records]]\nlineage = \"ctx.vera.no-force-push\"\nstatement = \"never force-push\"\n";

    fn listing() -> PackageListing {
        PackageListing {
            tools: vec!["lint_fix".into()],
            skills: vec!["house-style".into()],
            records: vec!["ctx.vera.no-force-push".into()],
            mcp: Vec::new(),
        }
    }

    /// **Witness for #3565, half one.** A manifest can say what the package
    /// ships, in the identities each surface already knows.
    #[test]
    fn a_package_declares_a_tool_a_skill_and_a_record() {
        let manifest = parse(PACKAGE).expect("a package declaration must load");
        assert_eq!(manifest.tools.len(), 1);
        assert_eq!(manifest.tools[0].name, "lint_fix");
        assert_eq!(manifest.skills[0].slug, "house-style");
        assert_eq!(manifest.records[0].lineage, "ctx.vera.no-force-push");
        assert_eq!(
            manifest.records[0].enforcement,
            RecordEnforcement::Advisory,
            "an undeclared enforcement is advisory, never blocking"
        );
    }

    /// The #1400 rule this crate inherits, on every new table.
    #[test]
    fn an_unknown_key_in_any_package_table_is_a_load_error() {
        for table in [
            "[[tools]]\nname = \"t\"\ndescription = \"d\"\nargv = [\"x\"]",
            "[[skills]]\nslug = \"s\"\ndescription = \"d\"\npriority = 1",
            "[[records]]\nlineage = \"l\"\nstatement = \"s\"\nprecedence = 10",
        ] {
            let err = parse(&format!("name = \"x\"\n\n{table}")).unwrap_err();
            assert!(
                matches!(err, ManifestError::Parse(_)),
                "{table} must not half-load: {err:?}"
            );
        }
    }

    /// **Witness for #3565, half two.** Both directions of the disagreement
    /// are refused, and the refusal names both sides.
    #[test]
    fn a_declaration_that_disagrees_with_the_directory_is_refused() {
        let manifest = parse(PACKAGE).expect("fixture");
        manifest
            .reconcile(&listing())
            .expect("an agreeing package reconciles");

        // Shipped but not declared — the security direction.
        let mut extra = listing();
        extra.tools.push("deploy".into());
        let err = manifest
            .reconcile(&extra)
            .expect_err("an undeclared tool must be refused");
        assert_eq!(err.kinds.len(), 1);
        assert_eq!(err.kinds[0].kind, ContributionKind::Tool);
        assert_eq!(err.kinds[0].undeclared, ["deploy"]);
        assert!(err.kinds[0].unshipped.is_empty());
        let text = err.to_string();
        assert!(text.contains("tools/ ships 1 tool"), "{text}");
        assert!(text.contains("deploy"), "{text}");
        assert!(text.contains("[[tools]]"), "{text}");

        // Declared but not shipped — the honesty direction.
        let err = manifest
            .reconcile(&PackageListing::empty())
            .expect_err("a declaration with nothing behind it must be refused");
        assert_eq!(
            err.kinds
                .iter()
                .map(|mismatch| mismatch.kind)
                .collect::<Vec<_>>(),
            [
                ContributionKind::Tool,
                ContributionKind::Skill,
                ContributionKind::Record
            ],
            "every disagreeing surface is named in one refusal, in kind order"
        );
        for mismatch in &err.kinds {
            assert!(mismatch.undeclared.is_empty());
            assert_eq!(mismatch.unshipped.len(), 1);
        }
    }

    /// Declaration order is the author's and reconciliation is a set
    /// comparison, so a re-ordered manifest is the same package.
    #[test]
    fn reconciliation_does_not_depend_on_order() {
        let manifest = parse(
            "name = \"vera\"\n\n\
             [[tools]]\nname = \"b\"\ndescription = \"d\"\n\n\
             [[tools]]\nname = \"a\"\ndescription = \"d\"\n",
        )
        .expect("fixture");
        assert!(
            manifest
                .reconcile(&PackageListing {
                    tools: vec!["a".into(), "b".into()],
                    ..PackageListing::empty()
                })
                .is_ok()
        );
    }

    /// A package with nothing to declare and nothing on disk reconciles, and
    /// nothing about the existing manifest shape changed.
    #[test]
    fn a_package_that_ships_nothing_reconciles_with_an_empty_listing() {
        let manifest = parse("name = \"x\"").expect("fixture");
        assert!(manifest.tools.is_empty());
        assert!(manifest.reconcile(&PackageListing::empty()).is_ok());
    }

    /// **The governance witness.** A package may ship a record that steers and
    /// may never ship one that denies: enforcement is granted only through the
    /// repository's own promotion ledger, which a package's files are outside
    /// of.
    #[test]
    fn a_contributed_record_may_not_ask_for_enforcement() {
        let err = parse(
            "name = \"x\"\n\n[[records]]\nlineage = \"ctx.x.no-force-push\"\n\
             statement = \"never force-push\"\nenforcement = \"blocking\"",
        )
        .unwrap_err();
        assert!(
            matches!(err, ManifestError::PluginRecordCannotEnforce { ref lineage }
                if lineage == "ctx.x.no-force-push"),
            "{err:?}"
        );
        let text = err.to_string();
        assert!(text.contains("promotion ledger"), "{text}");
    }

    #[test]
    fn a_contribution_must_be_named_described_and_unique() {
        let blank_name =
            parse("name = \"x\"\n\n[[tools]]\nname = \" \"\ndescription = \"d\"").unwrap_err();
        assert!(matches!(
            blank_name,
            ManifestError::EmptyContributionName {
                kind: ContributionKind::Tool
            }
        ));

        let blank_description =
            parse("name = \"x\"\n\n[[skills]]\nslug = \"s\"\ndescription = \"  \"").unwrap_err();
        assert!(matches!(
            blank_description,
            ManifestError::EmptyContributionDescription {
                kind: ContributionKind::Skill,
                ..
            }
        ));

        let dupe = parse(
            "name = \"x\"\n\n[[records]]\nlineage = \"l\"\nstatement = \"s\"\n\n\
             [[records]]\nlineage = \"l\"\nstatement = \"s2\"",
        )
        .unwrap_err();
        assert!(matches!(
            dupe,
            ManifestError::DuplicateContribution {
                kind: ContributionKind::Record,
                ..
            }
        ));
    }

    /// The enforcement vocabulary's wire strings, pinned on both sides — the
    /// [`crate::FlipPolicy`] discipline. `blocking` must stay *spellable*, or
    /// its refusal degrades into an unknown-variant parse error that explains
    /// nothing about governance.
    #[test]
    fn the_enforcement_vocabulary_is_pinned() {
        for (mode, wire) in [
            (RecordEnforcement::Advisory, "advisory"),
            (RecordEnforcement::Blocking, "blocking"),
        ] {
            assert_eq!(serde_json::to_value(mode).unwrap(), wire);
            assert_eq!(
                serde_json::from_value::<RecordEnforcement>(wire.into()).unwrap(),
                mode
            );
            assert_eq!(mode.to_string(), wire);
        }
    }
}
