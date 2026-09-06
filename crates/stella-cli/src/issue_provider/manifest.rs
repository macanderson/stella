//! What a tracker is, kept in a file.
//!
//! GitHub ships as a manifest, the way any other tracker has to. `github.toml`
//! sits next to this file. Stella builds it into the binary. A workspace file
//! at `.stella/issues/github.toml` takes its place. So the built-in path and
//! the file path are one path.
//!
//! # What the file holds
//!
//! The kernel's [`Vocabulary`]: the open and closed words, the resolution
//! spellings, the field names. Plus `[classes]`, which maps labels onto
//! [`IssueClass`]. The name, the kind and the schema version are read too.
//! Each one this build cannot honour prints a line. None of them stops the
//! loop.
//!
//! # What the file does not hold
//!
//! `[connection]`, `[states]`, `[capabilities]` and `[fields.write]` are the
//! rest of `doc:agent-native-delivery` §4.1. `#1281` owns them, and Linear's
//! move onto the same file. No second schema for them is defined here.
//!
//! # A missing file is still an answer
//!
//! No file, a bad file, a key left out: each one gives a working provider.
//! The compiled table below covers one case. That case is a built-in file this
//! build cannot parse. A test pins that the shipped file parses, so a shipped
//! build never reaches it.

use std::path::Path;

use serde::Deserialize;
use stella_protocol::issue::{IssueClass, Vocabulary};

use crate::settings::toml_config::{IssuesSection, TomlConfig};

/// The shipped GitHub manifest. A workspace with no file of its own still
/// reads a real one.
pub(crate) const EMBEDDED_GITHUB: &str = include_str!("github.toml");

/// The manifest schema this build reads.
const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// One tracker, as a file says it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderManifest {
    /// Which schema version the file was written for.
    pub schema_version: u32,
    /// What the file calls this provider.
    pub name: String,
    /// Which transport reaches it — `github`, `jira`, `linear`, `exec`.
    pub kind: String,
    /// The words this tracker uses for the ideas every tracker has.
    pub vocabulary: Vocabulary,
    /// Which labels mean which class.
    pub classes: ClassMap,
}

/// Which labels mean which [`IssueClass`].
///
/// Empty means the file said nothing. It does not mean no label counts. A
/// file with no `[classes]` block keeps the shipped map. See
/// [`ProviderManifest::inherit`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub(crate) struct ClassMap {
    /// Labels meaning something is broken.
    pub bug: Vec<String>,
    /// Labels meaning something should exist that does not.
    pub feature: Vec<String>,
    /// Labels meaning work that is neither — a migration, a cleanup, a chore.
    pub task: Vec<String>,
}

impl ClassMap {
    /// Whether this file said anything about classes.
    fn is_empty(&self) -> bool {
        self.bug.is_empty() && self.feature.is_empty() && self.task.is_empty()
    }

    /// Which class these labels mean.
    ///
    /// Bug beats feature, and feature beats task. An issue with two of them
    /// is a defect first. The rule for a fix is the strict one, and the strict
    /// rule is the safe one to apply.
    ///
    /// Labels in none of the lists give [`IssueClass::Other`], not `Task`. A
    /// class that says "not mapped" can be seen. A wrong class hides.
    pub(crate) fn class_of(&self, labels: &[&str]) -> IssueClass {
        let has = |declared: &[String]| declared.iter().any(|name| labels.contains(&name.as_str()));
        if has(&self.bug) {
            IssueClass::Bug
        } else if has(&self.feature) {
            IssueClass::Feature
        } else if has(&self.task) {
            IssueClass::Task
        } else {
            IssueClass::Other
        }
    }

    /// The compiled table. It is read only when the built-in file will not
    /// parse. A test below holds it equal to `github.toml`.
    fn compiled_github() -> Self {
        Self {
            bug: vec!["bug".to_owned()],
            feature: vec!["feature".to_owned()],
            task: vec!["chore".to_owned(), "task".to_owned(), "refactor".to_owned()],
        }
    }
}

/// Everything in a manifest that is not the kernel's [`Vocabulary`].
///
/// A second struct, not a `#[serde(flatten)]` field. `Vocabulary` stays the
/// one place its own words are declared. A field added there needs no copy
/// here.
#[derive(Debug, Deserialize)]
#[serde(default)]
struct Header {
    schema_version: u32,
    name: String,
    kind: String,
    classes: ClassMap,
}

impl Default for Header {
    fn default() -> Self {
        Self {
            schema_version: SUPPORTED_SCHEMA_VERSION,
            name: super::GITHUB.to_owned(),
            kind: super::GITHUB.to_owned(),
            classes: ClassMap::default(),
        }
    }
}

impl Default for ProviderManifest {
    /// The shipped GitHub manifest. It is what a bare workspace gets.
    fn default() -> Self {
        Self::embedded()
    }
}

impl ProviderManifest {
    /// Parse one manifest document.
    ///
    /// Two passes over the same text, one per struct. TOML says what it is,
    /// so a second pass costs one more parse of a small file. It buys the
    /// split above: no field is spelled twice.
    pub(crate) fn parse(raw: &str) -> Result<Self, toml::de::Error> {
        let vocabulary: Vocabulary = toml::from_str(raw)?;
        let header: Header = toml::from_str(raw)?;
        Ok(Self {
            schema_version: header.schema_version,
            name: header.name,
            kind: header.kind,
            vocabulary,
            classes: header.classes,
        })
    }

    /// The shipped GitHub manifest.
    pub(crate) fn embedded() -> Self {
        match Self::parse(EMBEDDED_GITHUB) {
            Ok(manifest) => manifest,
            Err(error) => {
                eprintln!(
                    "warning: the built-in GitHub manifest did not parse ({error}); using the \
                     compiled table. This is a defect in the build, not in your workspace."
                );
                Self::compiled_github()
            }
        }
    }

    /// The compiled table. It is read only when the built-in file will not
    /// parse. A test below holds it equal to `github.toml`.
    fn compiled_github() -> Self {
        Self {
            schema_version: SUPPORTED_SCHEMA_VERSION,
            name: super::GITHUB.to_owned(),
            kind: super::GITHUB.to_owned(),
            vocabulary: Vocabulary::github(),
            classes: ClassMap::compiled_github(),
        }
    }

    /// The manifest a workspace resolves for its provider.
    ///
    /// The path is what `[issues] manifest` names, or
    /// `.stella/issues/<provider>.toml`. A file that is missing or unreadable
    /// gives the shipped manifest, not an error.
    pub(crate) fn resolve(root: &Path, issues: &IssuesSection) -> Self {
        let embedded = Self::embedded();
        let provider = issues.provider.trim().to_ascii_lowercase();
        if !provider.is_empty() && provider != super::GITHUB {
            eprintln!(
                "warning: no built-in manifest for issue provider `{provider}`; using GitHub's. \
                 Declare it in `.stella/issues/{provider}.toml` to say how that tracker spells \
                 open, closed, its resolutions, and its classes."
            );
        }

        let path = issues
            .manifest
            .clone()
            .unwrap_or_else(|| format!(".stella/issues/{provider}.toml"));

        let Ok(raw) = std::fs::read_to_string(root.join(&path)) else {
            return embedded;
        };

        match Self::parse(&raw) {
            Ok(manifest) => manifest.inherit(embedded, &path),
            Err(error) => {
                eprintln!(
                    "warning: {path} could not be read ({error}); using the built-in manifest \
                     for `{}`",
                    issues.provider
                );
                embedded
            }
        }
    }

    /// The manifest this workspace resolves, from `stella.toml` and the file
    /// it names.
    ///
    /// For a caller that holds a root and no loop config. The deck's issues
    /// tab is one. A bad `stella.toml` is quiet here. The loop's own loader
    /// already reports it, and a copy of that line per keystroke would be
    /// noise.
    pub(crate) fn for_workspace(root: &Path) -> Self {
        Self::resolve(root, &issues_section(root))
    }

    /// Fill in what a file left out. Name what this build cannot honour.
    ///
    /// A file with no `[classes]` block keeps the shipped map. Without that
    /// rule, a file written before the block existed would class every issue
    /// as `Other`.
    fn inherit(mut self, embedded: Self, path: &str) -> Self {
        if self.classes.is_empty() {
            self.classes = embedded.classes;
        }
        if self.schema_version > SUPPORTED_SCHEMA_VERSION {
            eprintln!(
                "warning: {path} declares manifest schema {} and this build reads {}; \
                 anything newer in it is ignored",
                self.schema_version, SUPPORTED_SCHEMA_VERSION
            );
        }
        if self.kind != super::GITHUB {
            eprintln!(
                "warning: {path} declares provider `{}` with `kind = \"{}\"`, and this build \
                 ships only the `{}` transport; GitHub's adapter is what will run",
                self.name,
                self.kind,
                super::GITHUB
            );
        }
        self
    }
}

/// The `[issues]` section of a workspace's `stella.toml`, or its default.
fn issues_section(root: &Path) -> IssuesSection {
    let path = crate::settings::toml_config::project_toml_path(root);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return IssuesSection::default();
    };
    TomlConfig::parse(&raw, &path)
        .map(|parsed| parsed.issues)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The shipping witness.** The built-in file parses, and it declares
    /// what the compiled table holds.
    ///
    /// If the file stopped parsing, every workspace would fall back to that
    /// table. One line on stderr would be the only sign. If the two drifted,
    /// the fallback would answer one way and the file another.
    #[test]
    fn the_shipped_manifest_parses_and_matches_the_compiled_table() {
        let shipped = ProviderManifest::parse(EMBEDDED_GITHUB).expect("the shipped file parses");
        assert_eq!(shipped, ProviderManifest::compiled_github());
        assert_eq!(shipped.vocabulary, Vocabulary::github());
        assert_eq!(shipped.name, "github");
        assert_eq!(shipped.kind, "github");
        assert_eq!(shipped.schema_version, SUPPORTED_SCHEMA_VERSION);
    }

    /// The class map is data. A team that spells `bug` its own way says so in
    /// the file.
    #[test]
    fn a_renamed_label_set_changes_the_class() {
        let manifest = ProviderManifest::parse(
            r#"
schema_version = 1
name = "github"
kind = "github"
open = ["open"]
closed = ["closed"]

[classes]
bug = ["kind/defect"]
feature = ["kind/enhancement"]
"#,
        )
        .expect("parses");

        assert_eq!(manifest.classes.class_of(&["kind/defect"]), IssueClass::Bug);
        assert_eq!(
            manifest.classes.class_of(&["kind/enhancement"]),
            IssueClass::Feature
        );
        assert_eq!(
            manifest.classes.class_of(&["bug"]),
            IssueClass::Other,
            "the shipped label must not survive a mapping that replaced it"
        );
    }

    /// A defect that also carries a feature label is a defect. The strict
    /// rule is the safe one.
    #[test]
    fn a_bug_label_wins_over_the_others() {
        let classes = ClassMap::compiled_github();
        assert_eq!(classes.class_of(&["feature", "bug"]), IssueClass::Bug);
        assert_eq!(classes.class_of(&["task", "feature"]), IssueClass::Feature);
        assert_eq!(classes.class_of(&["refactor"]), IssueClass::Task);
        assert_eq!(classes.class_of(&[]), IssueClass::Other);
    }

    fn workspace() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, body).expect("write");
    }

    /// **The shadow witness.** A workspace file of the same name takes the
    /// place of the built-in one.
    #[test]
    fn a_workspace_manifest_shadows_the_embedded_one() {
        let ws = workspace();
        write(
            ws.path(),
            ".stella/issues/github.toml",
            "open = [\"triage\"]\nclosed = [\"shipped\"]\n\n[classes]\nbug = [\"kind/defect\"]\n",
        );

        let manifest = ProviderManifest::for_workspace(ws.path());
        assert!(manifest.vocabulary.is_open("triage"));
        assert!(!manifest.vocabulary.is_open("open"));
        assert_eq!(manifest.classes.class_of(&["kind/defect"]), IssueClass::Bug);
    }

    /// A file with no class block keeps the shipped map. A read must not
    /// start returning `Other` for every issue.
    #[test]
    fn a_manifest_with_no_classes_inherits_the_shipped_ones() {
        let ws = workspace();
        write(
            ws.path(),
            ".stella/issues/github.toml",
            "open = [\"triage\"]\nclosed = [\"shipped\"]\n",
        );

        let manifest = ProviderManifest::for_workspace(ws.path());
        assert!(manifest.vocabulary.is_open("triage"));
        assert_eq!(manifest.classes.class_of(&["bug"]), IssueClass::Bug);
    }

    /// A workspace that set nothing gets the shipped manifest.
    #[test]
    fn an_unconfigured_workspace_gets_the_shipped_manifest() {
        assert_eq!(
            ProviderManifest::for_workspace(workspace().path()),
            ProviderManifest::embedded()
        );
    }
}
