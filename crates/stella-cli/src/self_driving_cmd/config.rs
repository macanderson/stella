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
//!
//! The default is a file too. GitHub's manifest ships inside the binary, next
//! to its adapter (`crate::issue_provider::manifest`). A workspace that set
//! nothing still reads a real file. A workspace file at
//! `.stella/issues/github.toml` takes its place.

use std::path::Path;

use stella_autonomy::Attribution;
use stella_protocol::issue::Vocabulary;

use crate::issue_provider::ProviderManifest;
use crate::settings::toml_config::TomlConfig;

/// Everything the self-driving verbs read out of configuration.
#[derive(Debug, Clone)]
pub(crate) struct LoopConfig {
    /// What the loop appends to what it writes, and how it names branches.
    pub attribution: Attribution,
    /// The active tracker's declaration: its vocabulary and its class
    /// mapping, resolved once here so two readers cannot see two files.
    pub manifest: ProviderManifest,
    /// Which labels mean urgent, which mean "ours", which mean "not ours".
    pub triage: stella_autonomy::priority::TriagePolicy,
    /// Labels marking a tracking/container issue — a checklist of other
    /// issues, not workable itself. `drive --backlog` reads this beside
    /// `triage.ladder`, because the ready queue is a different generator
    /// from the defect queue `triage` feeds and needs its own copy of the
    /// same policy. See `stella_autonomy::ready::DEFAULT_CONTAINER_LABELS`.
    pub container_labels: Vec<String>,
    /// How the loop decides where two operators would decide differently.
    pub doctrine: stella_autonomy::Doctrine,
    /// How long an escalated issue waits before the loop may take it again,
    /// and how many escalations end in it being parked for good.
    pub escalation: stella_autonomy::escalation::EscalationPolicy,
    /// Which checks are allowed to block a merge.
    pub merge: stella_autonomy::BlockingPolicy,
    /// The command that proves a change when CI cannot, and its ceiling.
    pub verify_command: Option<String>,
    /// Seconds before local verification is abandoned.
    pub verify_timeout_secs: u64,
    /// Whether the end-of-turn residue gate scans and files leftover work
    /// (`residue_gate = "on" | "off"`, absent means on).
    pub residue_gate: bool,
    /// Whether the drive loop also watches the release workflow's latest run
    /// and files on red. On unless `stella.toml` says `deploy_watch = "off"`.
    pub deploy_watch: bool,
    /// Where the loop looks for work once the ranked queue is empty. Every
    /// supply is shut unless `[self_driving.supply]` opens one.
    pub supply: stella_autonomy::supply::SupplyPolicy,
    /// Which coding agent performs issue work.
    pub worker: crate::settings::toml_config::WorkerSection,
}

impl LoopConfig {
    /// How the active tracker spells the concepts every tracker has.
    pub fn vocabulary(&self) -> &Vocabulary {
        &self.manifest.vocabulary
    }
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            attribution: Attribution::default(),
            manifest: ProviderManifest::default(),
            triage: stella_autonomy::priority::TriagePolicy::default(),
            container_labels: default_container_labels(),
            doctrine: stella_autonomy::Doctrine::default(),
            escalation: stella_autonomy::escalation::EscalationPolicy::default(),
            merge: stella_autonomy::BlockingPolicy::default(),
            verify_command: None,
            verify_timeout_secs: 1800,
            residue_gate: true,
            deploy_watch: true,
            supply: stella_autonomy::supply::SupplyPolicy::default(),
            worker: crate::settings::toml_config::WorkerSection::default(),
        }
    }
}

/// Read the loop's configuration for a workspace.
///
/// Never fails. A malformed document is reported on stderr and the defaults
/// apply — the loop should say loudly that it could not read a setting and
/// then keep working, rather than refuse to run because of a typo in a section
/// it might not even use.
///
/// A workspace with no `stella.toml` still resolves its provider manifest.
/// `.stella/issues/github.toml` is the file the tracker's words and classes
/// are edited in, and requiring a second file beside it to make the first one
/// count would make the manifest unreachable for the workspace that has
/// configured nothing else — which is the workspace it exists for.
///
/// The worker is the exception, and it fails closed. A document that does not
/// parse but does declare `[self_driving.worker]` resolves
/// [`crate::settings::toml_config::WorkerKind::Unreadable`], and the work path
/// refuses the turn. Falling back to the default there would run stella under
/// a file that asked for Claude Code, which is the one substitution the typed
/// setting exists to stop, and `stella self-driving drive` runs unattended, so
/// a warning on stderr is not a stop.
#[must_use]
pub(crate) fn load(root: &Path) -> LoopConfig {
    let parsed = match read_toml(root) {
        LoopDocument::Parsed(parsed) => parsed,
        LoopDocument::Absent => {
            return LoopConfig {
                manifest: ProviderManifest::for_workspace(root),
                ..LoopConfig::default()
            };
        }
        LoopDocument::Unparsed { names_a_worker } => {
            return LoopConfig {
                manifest: ProviderManifest::for_workspace(root),
                worker: crate::settings::toml_config::WorkerSection {
                    kind: if names_a_worker {
                        crate::settings::toml_config::WorkerKind::Unreadable
                    } else {
                        crate::settings::toml_config::WorkerKind::default()
                    },
                    ..crate::settings::toml_config::WorkerSection::default()
                },
                ..LoopConfig::default()
            };
        }
    };

    LoopConfig {
        attribution: parsed.self_driving.attribution.clone(),
        manifest: ProviderManifest::resolve(root, &parsed.issues),
        triage: parsed.self_driving.triage.policy(),
        container_labels: if parsed.self_driving.container_labels.is_empty() {
            default_container_labels()
        } else {
            parsed.self_driving.container_labels.clone()
        },
        doctrine: parsed.self_driving.doctrine,
        escalation: parsed.self_driving.escalation,
        merge: parsed.self_driving.merge.policy(),
        verify_command: parsed.self_driving.verify.command.clone(),
        verify_timeout_secs: parsed.self_driving.verify.timeout_secs.unwrap_or(1800),
        residue_gate: parsed.self_driving.residue_gate.enabled(),
        deploy_watch: parsed.self_driving.deploy_watch.enabled(),
        supply: parsed.self_driving.supply.policy(),
        worker: parsed.self_driving.worker,
    }
}

/// The built-in tracking-label set, as the config layer's owned copy.
///
/// `stella_autonomy::ready::DEFAULT_CONTAINER_LABELS` is `&'static [&'static
/// str]`; `LoopConfig::container_labels` is `Vec<String>` so an operator's
/// `stella.toml` list and the shipped default share one field and one type.
fn default_container_labels() -> Vec<String> {
    stella_autonomy::ready::DEFAULT_CONTAINER_LABELS
        .iter()
        .map(|label| (*label).to_owned())
        .collect()
}

/// What a workspace's `stella.toml` gave the loop.
enum LoopDocument {
    /// No file, or one this process cannot read.
    Absent,
    /// The document parsed.
    Parsed(Box<TomlConfig>),
    /// The document did not parse. Every setting falls back to its default
    /// except the worker, which fails closed when the file declared one.
    Unparsed {
        /// Whether the raw text opens a `[self_driving.worker]` table.
        names_a_worker: bool,
    },
}

fn read_toml(root: &Path) -> LoopDocument {
    let path = crate::settings::toml_config::project_toml_path(root);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return LoopDocument::Absent;
    };
    match TomlConfig::parse(&raw, &path) {
        Ok(parsed) => LoopDocument::Parsed(Box::new(parsed)),
        Err(error) => {
            eprintln!("warning: {error}; self-driving is using its defaults");
            LoopDocument::Unparsed {
                names_a_worker: declares_a_worker(&raw),
            }
        }
    }
}

/// Whether the text opens a `[self_driving.worker]` table.
///
/// Read from the raw text because the document did not parse, so there is no
/// tree to ask. A table header is the whole line, so a scan of the line
/// starts answers exactly: a `[` in the middle of a value cannot reach here,
/// and a header split across lines is not TOML.
fn declares_a_worker(raw: &str) -> bool {
    raw.lines()
        .map(str::trim)
        .any(|line| line.starts_with("[self_driving.worker]"))
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
        assert!(cfg.vocabulary().is_open("open"));
        assert_eq!(
            cfg.vocabulary()
                .resolution(stella_protocol::issue::RESOLUTION_COMPLETED),
            "completed"
        );
    }

    /// **The shipped-manifest witness.** GitHub's words and class map reach
    /// the loop from the built-in file. The default tracker is declared in a
    /// file, like any other.
    #[test]
    fn github_is_resolved_through_the_shipped_manifest() {
        use crate::issue_provider::ProviderManifest;

        // No `stella.toml` at all.
        assert_eq!(
            load(workspace().path()).manifest,
            ProviderManifest::embedded()
        );

        // A `stella.toml` naming github, with no manifest file beside it.
        let ws = workspace();
        write(
            ws.path(),
            "stella.toml",
            "[meta]\nschema_version = 1\nscope = \"project\"\n\n[issues]\nprovider = \"github\"\n",
        );
        let cfg = load(ws.path());
        assert_eq!(cfg.manifest, ProviderManifest::embedded());
        assert_eq!(cfg.manifest.name, "github");
        assert!(cfg.vocabulary().is_open("open"));
        assert_eq!(
            cfg.manifest.classes.class_of(&["bug"]),
            stella_protocol::issue::IssueClass::Bug
        );
    }

    /// A manifest is read on its own. `stella.toml` says which provider is
    /// active, and github is the default, so a workspace that edited only
    /// `.stella/issues/github.toml` has already said everything the loop needs
    /// — the shadow must not wait on a second file.
    #[test]
    fn a_manifest_shadows_the_shipped_one_with_no_stella_toml_beside_it() {
        let ws = workspace();
        write(
            ws.path(),
            ".stella/issues/github.toml",
            "open = [\"triage\"]\nclosed = [\"shipped\"]\n\n[classes]\nbug = [\"kind/defect\"]\n",
        );

        let cfg = load(ws.path());
        assert!(
            cfg.vocabulary().is_open("triage"),
            "the manifest's words are read without a stella.toml beside them"
        );
        assert_eq!(
            cfg.manifest.classes.class_of(&["kind/defect"]),
            stella_protocol::issue::IssueClass::Bug,
            "the manifest's classes are read too, not just its words"
        );
    }

    /// **The shadow witness.** A workspace `.stella/issues/github.toml` takes
    /// the place of the shipped one. Words and classes both.
    #[test]
    fn a_workspace_github_manifest_shadows_the_shipped_one() {
        let ws = workspace();
        write(
            ws.path(),
            "stella.toml",
            "[meta]\nschema_version = 1\nscope = \"project\"\n\n[issues]\nprovider = \"github\"\n",
        );
        write(
            ws.path(),
            ".stella/issues/github.toml",
            "open = [\"triage\"]\nclosed = [\"shipped\"]\n\n[classes]\nbug = [\"kind/defect\"]\n",
        );

        let cfg = load(ws.path());
        assert!(cfg.vocabulary().is_open("triage"));
        assert!(!cfg.vocabulary().is_open("open"));
        assert_eq!(
            cfg.manifest.classes.class_of(&["kind/defect"]),
            stella_protocol::issue::IssueClass::Bug
        );
        assert_eq!(
            cfg.manifest.classes.class_of(&["bug"]),
            stella_protocol::issue::IssueClass::Other,
            "the shipped label must not survive a mapping that replaced it"
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

    /// The witness for the worker seam: choosing claude, and the controls that
    /// bound it, survive parsing into the configuration the work path reads.
    /// Before this seam the worker was unconditionally a child `stella run`.
    #[test]
    fn claude_code_can_be_selected_as_the_issue_worker() {
        let ws = workspace();
        write(
            ws.path(),
            "stella.toml",
            r#"
[meta]
schema_version = 1
scope = "project"

[self_driving.worker]
kind = "claude"
command = "/opt/bin/claude"
model = "opus"
max_turns = 40
dangerously_skip_permissions = true
"#,
        );

        let worker = load(ws.path()).worker;
        assert_eq!(
            worker.kind,
            crate::settings::toml_config::WorkerKind::Claude
        );
        assert_eq!(worker.command, "/opt/bin/claude");
        assert_eq!(worker.model.as_deref(), Some("opus"));
        assert_eq!(worker.max_turns, Some(40));
        assert!(worker.dangerously_skip_permissions);
    }

    /// **The fail-closed witness.** A document that names a worker and does
    /// not parse resolves no worker at all.
    ///
    /// Fall back to the defaults here and this fails by construction: the
    /// whole-document parse fails, `load` returns `LoopConfig::default()`,
    /// and the operator who wrote `kind = "clade"` gets `WorkerKind::Stella`
    /// — the substitution the typed setting was chosen to stop. Every `match`
    /// on the enum has to answer for `Unreadable`, so the work path refuses
    /// instead.
    #[test]
    fn an_unreadable_worker_kind_resolves_no_worker() {
        let ws = workspace();
        write(
            ws.path(),
            "stella.toml",
            r#"
[meta]
schema_version = 1
scope = "project"

[self_driving.worker]
kind = "clade"
"#,
        );

        assert_eq!(
            load(ws.path()).worker.kind,
            crate::settings::toml_config::WorkerKind::Unreadable,
            "a worker the file cannot express must not resolve to one the operator did not name"
        );
    }

    /// A document that does not parse and names no worker keeps today's
    /// recovery: the loop says so and runs its default agent. Only the key
    /// that selects an executing agent fails closed.
    #[test]
    fn an_unparsable_document_with_no_worker_table_keeps_the_default() {
        let ws = workspace();
        write(
            ws.path(),
            "stella.toml",
            "[meta]\nschema_version = 1\nscope = \"project\"\n\n[self_driving]\ntriage = 7\n",
        );

        assert_eq!(
            load(ws.path()).worker.kind,
            crate::settings::toml_config::WorkerKind::Stella
        );
    }

    /// The default is unchanged by the seam existing: a workspace that says
    /// nothing about a worker still runs stella's own turn loop.
    #[test]
    fn the_worker_defaults_to_stella() {
        let ws = workspace();
        write(
            ws.path(),
            "stella.toml",
            r#"
[meta]
schema_version = 1
scope = "project"
"#,
        );

        let worker = load(ws.path()).worker;
        assert_eq!(
            worker.kind,
            crate::settings::toml_config::WorkerKind::Stella
        );
        assert!(!worker.dangerously_skip_permissions);
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
        assert!(cfg.vocabulary().is_open("In Progress"));
        assert!(!cfg.vocabulary().is_open("Done"));
        assert_eq!(
            cfg.vocabulary()
                .resolution(stella_protocol::issue::RESOLUTION_NOT_PLANNED),
            "Won't Do"
        );
        assert_eq!(cfg.vocabulary().fields.body, "description");
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

        assert!(load(ws.path()).vocabulary().is_open("To Do"));
    }

    /// The doctrine is read from `stella.toml`, like everything else
    /// configurable about stella, and an unconfigured workspace gets the
    /// shipped default.
    ///
    /// A regression guard on a seam nothing asserted, not a witness — `load`
    /// already carries the value. It is worth pinning because this axis decides
    /// whether the loop repairs a base somebody else broke, and because #3943
    /// proposes reading it from a `.stella/issues/` manifest instead: a second
    /// place to declare it is how the two would come to disagree.
    #[test]
    fn the_doctrine_is_read_from_stella_toml() {
        let ws = workspace();
        write(
            ws.path(),
            "stella.toml",
            r#"
[meta]
schema_version = 1
scope = "project"

[self_driving.doctrine]
foreign_breakage = "file_and_wait"
"#,
        );

        assert_eq!(
            load(ws.path()).doctrine.foreign_breakage,
            stella_autonomy::ForeignBreakage::FileAndWait,
            "an operator's declared axis must reach the loop"
        );
        assert_eq!(
            load(workspace().path()).doctrine,
            stella_autonomy::Doctrine::default(),
            "and a workspace that declares nothing gets the shipped default"
        );
    }

    /// The deploy watch is on for a workspace that says nothing, and an
    /// operator stands it down with one line — the default direction matters,
    /// because a watch that must be asked for is off exactly when nobody
    /// thought about it.
    #[test]
    fn the_deploy_watch_defaults_on_and_can_be_stood_down() {
        assert!(load(workspace().path()).deploy_watch);

        let ws = workspace();
        write(
            ws.path(),
            "stella.toml",
            r#"
[meta]
schema_version = 1
scope = "project"

[self_driving]
deploy_watch = "off"
"#,
        );
        assert!(!load(ws.path()).deploy_watch);
    }

    /// The tracking-label default is `epic` for a workspace that declares
    /// nothing, and an operator's own list replaces it wholesale — the
    /// same rule `[self_driving.triage]`'s lists already follow.
    #[test]
    fn container_labels_default_to_epic_and_can_be_overridden() {
        assert_eq!(
            load(workspace().path()).container_labels,
            vec!["epic".to_owned()],
            "an unconfigured workspace still skips the built-in tracking label"
        );

        let ws = workspace();
        write(
            ws.path(),
            "stella.toml",
            r#"
[meta]
schema_version = 1
scope = "project"

[self_driving]
container_labels = ["tracking"]
"#,
        );
        assert_eq!(
            load(ws.path()).container_labels,
            vec!["tracking".to_owned()],
            "a declared list replaces the built-in default rather than adding to it"
        );
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

        assert!(load(ws.path()).vocabulary().is_open("open"));
    }
}
