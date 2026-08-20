//! The loop's configuration, read where all stella configuration is read.
//!
//! **Everything configurable about stella comes from `stella.toml`.** That is
//! a rule about where a person looks, not about where bytes live: a second
//! config system is a second place to search, and the only thing worse than a
//! setting nobody can find is two settings that disagree.
//!
//! Two things are read here, and the split between them is the design:
//!
//! - **`[self_driving]`** — how the loop identifies itself. Signature text,
//!   branch prefix. This is *stella's* behaviour, so it lives in
//!   `stella.toml` directly.
//! - **`[issues]`** — which tracker is active, and a pointer to its manifest.
//!   The manifest holds the *tracker's* vocabulary: what marks an issue
//!   closed, how a resolution is spelled, what the fields are called.
//!
//! # Why the vocabulary is not in `stella.toml` too
//!
//! Because it is not stella's vocabulary. `stella.toml` says *which* tracker;
//! the tracker's manifest says how that tracker talks. Collapsing them would
//! mean a workspace with two trackers — a migration, a monorepo spanning two
//! teams — had nowhere to put the second one's words, and it would put a
//! vendor's spellings in the file a person edits to configure stella.
//!
//! `doc:agent-native-delivery` §4 already specifies provider manifests under
//! `.stella/issues/`, so this is the existing seam rather than a new one.
//!
//! # Absent is always answerable
//!
//! No `stella.toml`, no `[issues]`, no manifest file — every one of those
//! yields a working default rather than an error. "Everything is configurable"
//! must not become "everything must be configured", and a loop that refused to
//! start because nobody had written a vocabulary file would be useless on the
//! tracker it is most likely pointed at.

use std::path::Path;

use stella_autonomy::Attribution;
use stella_protocol::issue::Vocabulary;

use crate::settings::toml_config::{IssuesSection, TomlConfig};

/// Everything the self-driving verbs read out of configuration.
#[derive(Debug, Clone)]
pub(crate) struct LoopConfig {
    /// What the loop appends to what it writes, and how it names branches.
    pub attribution: Attribution,
    /// How the active tracker spells the concepts every tracker has.
    pub vocabulary: Vocabulary,
    /// Which labels mean urgent, which mean "ours", which mean "not ours".
    pub triage: stella_autonomy::priority::TriagePolicy,
    /// How the loop decides where two operators would decide differently.
    pub doctrine: stella_autonomy::Doctrine,
    /// Which checks are allowed to block a merge.
    pub merge: stella_autonomy::BlockingPolicy,
    /// The command that proves a change when CI cannot, and its ceiling.
    pub verify_command: Option<String>,
    /// Seconds before local verification is abandoned.
    pub verify_timeout_secs: u64,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            attribution: Attribution::default(),
            vocabulary: Vocabulary::default(),
            triage: stella_autonomy::priority::TriagePolicy::default(),
            doctrine: stella_autonomy::Doctrine::default(),
            merge: stella_autonomy::BlockingPolicy::default(),
            verify_command: None,
            verify_timeout_secs: 1800,
        }
    }
}

/// Read the loop's configuration for a workspace.
///
/// Never fails. A malformed document is reported on stderr and the defaults
/// apply — the loop should say loudly that it could not read a setting and
/// then keep working, rather than refuse to run because of a typo in a section
/// it might not even use.
#[must_use]
pub(crate) fn load(root: &Path) -> LoopConfig {
    let Some(parsed) = read_toml(root) else {
        return LoopConfig::default();
    };

    LoopConfig {
        attribution: parsed.self_driving.attribution.clone(),
        vocabulary: vocabulary_for(root, &parsed.issues),
        triage: parsed.self_driving.triage.policy(),
        doctrine: parsed.self_driving.doctrine,
        merge: parsed.self_driving.merge.policy(),
        verify_command: parsed.self_driving.verify.command.clone(),
        verify_timeout_secs: parsed.self_driving.verify.timeout_secs.unwrap_or(1800),
    }
}

fn read_toml(root: &Path) -> Option<TomlConfig> {
    let path = crate::settings::toml_config::project_toml_path(root);
    let raw = std::fs::read_to_string(&path).ok()?;
    match TomlConfig::parse(&raw, &path) {
        Ok(parsed) => Some(parsed),
        Err(error) => {
            eprintln!("warning: {error}; self-driving is using its defaults");
            None
        }
    }
}

/// Load the active provider's vocabulary.
///
/// The manifest path is the one `[issues] manifest` names, or
/// `.stella/issues/<provider>.toml`. An absent file is the built-in default
/// for that provider rather than an error.
fn vocabulary_for(root: &Path, issues: &IssuesSection) -> Vocabulary {
    let default = builtin_vocabulary(&issues.provider);

    let path = issues.manifest.clone().unwrap_or_else(|| {
        format!(
            ".stella/issues/{}.toml",
            issues.provider.trim().to_ascii_lowercase()
        )
    });

    let Ok(raw) = std::fs::read_to_string(root.join(&path)) else {
        return default;
    };

    match toml::from_str::<Vocabulary>(&raw) {
        Ok(vocabulary) => vocabulary,
        Err(error) => {
            eprintln!(
                "warning: {path} could not be read ({error}); using the built-in \
                 vocabulary for `{}`",
                issues.provider
            );
            default
        }
    }
}

/// The vocabulary shipped for a provider this build knows.
///
/// An unknown provider gets GitHub's, **and says so**: the shapes are close
/// enough that the loop will mostly work, and a silent empty vocabulary would
/// make every status unmapped and every read fail with no hint why.
fn builtin_vocabulary(provider: &str) -> Vocabulary {
    match provider.trim().to_ascii_lowercase().as_str() {
        "github" | "" => Vocabulary::github(),
        other => {
            eprintln!(
                "warning: no built-in vocabulary for issue provider `{other}`; using GitHub's. \
                 Declare it in `.stella/issues/{other}.toml` to say how that tracker spells \
                 open, closed, and its resolutions."
            );
            Vocabulary::github()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, body).expect("write");
    }

    /// **The "never stuck" witness.** A workspace that has configured nothing
    /// gets a working loop against GitHub. "Everything is configurable" must
    /// not become "everything must be configured".
    #[test]
    fn a_workspace_with_no_config_still_works() {
        let cfg = load(workspace().path());

        assert_eq!(cfg.attribution.branch_prefix(), "stella/");
        assert!(cfg.vocabulary.is_open("open"));
        assert_eq!(
            cfg.vocabulary
                .resolution(stella_protocol::issue::RESOLUTION_COMPLETED),
            "completed"
        );
    }

    /// The signature and branch prefix come from `stella.toml`, which is where
    /// a person looks for anything configurable about stella.
    #[test]
    fn attribution_is_read_from_stella_toml() {
        let ws = workspace();
        write(
            ws.path(),
            "stella.toml",
            r#"
[meta]
schema_version = 1
scope = "project"

[self_driving.attribution]
commit = "Created by oxagen."
branch_prefix = "oxagen/"
"#,
        );

        let cfg = load(ws.path());
        assert_eq!(cfg.attribution.commit, "Created by oxagen.");
        assert_eq!(cfg.attribution.branch_prefix(), "oxagen/");
        // Unmentioned surfaces keep identifying the loop rather than blanking.
        assert_eq!(cfg.attribution.issue, stella_autonomy::SIGNATURE);
    }

    /// **The portability witness.** `stella.toml` says *which* tracker; the
    /// tracker's own manifest says how it talks. A different issue system is a
    /// configuration, not a fork.
    #[test]
    fn a_different_tracker_is_pointed_at_and_described_separately() {
        let ws = workspace();
        write(
            ws.path(),
            "stella.toml",
            r#"
[meta]
schema_version = 1
scope = "project"

[issues]
provider = "jira"
manifest = ".stella/issues/jira.toml"
"#,
        );
        write(
            ws.path(),
            ".stella/issues/jira.toml",
            r#"
open = ["To Do", "In Progress"]
closed = ["Done", "Won't Do"]

[resolutions]
fixed = "Done"
not_planned = "Won't Do"

[fields]
body = "description"
"#,
        );

        let cfg = load(ws.path());
        assert!(cfg.vocabulary.is_open("In Progress"));
        assert!(!cfg.vocabulary.is_open("Done"));
        assert_eq!(
            cfg.vocabulary
                .resolution(stella_protocol::issue::RESOLUTION_NOT_PLANNED),
            "Won't Do"
        );
        assert_eq!(cfg.vocabulary.fields.body, "description");
    }

    /// The manifest path defaults from the provider name, so naming a provider
    /// is enough — a customer does not have to say the path twice.
    #[test]
    fn the_manifest_path_defaults_from_the_provider_name() {
        let ws = workspace();
        write(
            ws.path(),
            "stella.toml",
            "[meta]\nschema_version = 1\nscope = \"project\"\n\n[issues]\nprovider = \"jira\"\n",
        );
        write(
            ws.path(),
            ".stella/issues/jira.toml",
            "open = [\"To Do\"]\nclosed = [\"Done\"]\n",
        );

        assert!(load(ws.path()).vocabulary.is_open("To Do"));
    }

    /// A malformed manifest falls back to a working vocabulary rather than
    /// stopping the loop, and the operator is told.
    #[test]
    fn a_malformed_manifest_falls_back_rather_than_failing() {
        let ws = workspace();
        write(
            ws.path(),
            "stella.toml",
            "[meta]\nschema_version = 1\nscope = \"project\"\n\n[issues]\nprovider = \"github\"\n",
        );
        write(
            ws.path(),
            ".stella/issues/github.toml",
            "this is not toml {{",
        );

        assert!(load(ws.path()).vocabulary.is_open("open"));
    }
}
