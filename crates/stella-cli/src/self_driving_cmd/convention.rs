//! Learning how *this* repository writes issues.
//!
//! `doc:backlog-self-driving` §3.1a. [`stella_autonomy::conform`] is the pure
//! decision; this is the I/O half that finds out what to decide against.
//!
//! # Precedence, and why the enforced source wins
//!
//! **Enforced > declared > observed.** A workflow that adds `triage` to an
//! untyped issue is not *describing* a convention — it **is** one, and it will
//! do that to the loop's filings whatever any document says. So the enforced
//! source is read first and is the only one that can be wrong about itself in
//! no way at all.
//!
//! # Why this scans rather than parses
//!
//! It reads two declared facts out of `.github/workflows/issue-triage.yml` — the
//! `TYPE_LABELS` list and whether `triage` is applied by automation — and it
//! does **not** parse the workflow. That is a deliberate trade against adding a
//! YAML dependency to the workspace for two lines (AGENTS.md: no new
//! dependencies casually), and it is safe only because of how it fails:
//!
//! **A scan that finds nothing yields a [`Acceptance::Proposed`] convention,
//! which refuses every filing.** So the failure mode of not understanding this
//! repository is "file nothing and say so", never "file something unclassified".
//! A fragile reader whose failure direction is refusal is a different thing from
//! a fragile reader that guesses.
//!
//! If a workspace needs something this cannot discover, it writes the manifest
//! by hand and [`load`] reads that instead — which is also how a non-GitHub
//! tracker is described.

use std::path::Path;

use stella_autonomy::{
    Acceptance, AxisRequirement, BacklogConvention, ConventionSource, LabelAxis,
};

/// Where a workspace pins its convention, overriding discovery.
const MANIFEST: &str = ".stella/issues/convention.json";

/// The workflow whose behaviour *is* this repository's type convention.
const TRIAGE_WORKFLOW: &str = ".github/workflows/issue-triage.yml";

/// How the convention in force was arrived at, for the human reading a refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Provenance {
    /// Read from the workspace's checked-in manifest.
    Manifest,
    /// Discovered from repository automation.
    Discovered,
    /// Nothing could be learned, so a proposal was made and nothing may be
    /// filed under it until a human accepts.
    Proposed,
}

/// The convention in force, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Bound {
    /// The convention itself.
    pub convention: BacklogConvention,
    /// How it was arrived at.
    pub provenance: Provenance,
}

/// Resolve the convention for a workspace root.
///
/// Never fails: an unreadable repository yields a proposal, and a proposal
/// files nothing. The caller renders the difference to a human rather than
/// having to handle an error it could not act on anyway.
pub(super) fn load(root: &Path) -> Bound {
    if let Some(convention) = from_manifest(root) {
        return Bound {
            convention,
            provenance: Provenance::Manifest,
        };
    }
    if let Some(convention) = discover(root) {
        return Bound {
            convention,
            provenance: Provenance::Discovered,
        };
    }
    Bound {
        convention: propose(),
        provenance: Provenance::Proposed,
    }
}

fn from_manifest(root: &Path) -> Option<BacklogConvention> {
    let raw = std::fs::read_to_string(root.join(MANIFEST)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Read the enforced convention out of repository automation.
///
/// Returns `None` when nothing enforced can be found, which is what makes the
/// caller propose rather than guess.
fn discover(root: &Path) -> Option<BacklogConvention> {
    let workflow = std::fs::read_to_string(root.join(TRIAGE_WORKFLOW)).ok()?;

    let members = type_labels(&workflow)?;

    // A label the automation applies is a label the loop must not: it means
    // "arrived from outside without a type", and the loop knows what it found.
    let mut reserved = Vec::new();
    if workflow.contains("--add-label triage") {
        reserved.push("triage".to_owned());
    }

    Some(BacklogConvention {
        axes: vec![LabelAxis {
            name: "type".to_owned(),
            members,
            // The workflow's whole behaviour is "an issue without one of these
            // gets stamped", which is `ExactlyOne` stated as an action.
            requirement: AxisRequirement::ExactlyOne,
            source: ConventionSource::Enforced,
        }],
        reserved,
        acceptance: Acceptance::Bound,
    })
}

/// Extract the `TYPE_LABELS` list — the one fact this scan is for.
///
/// The workflow declares it as a space-separated scalar, and its own comment
/// says why: *"Space-separated; adding any of these counts as typing the
/// issue."* An empty or missing list yields `None` rather than an empty axis,
/// because an axis with no members would accept every filing — the failure
/// direction this module exists to avoid.
fn type_labels(workflow: &str) -> Option<Vec<String>> {
    let line = workflow
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("TYPE_LABELS:"))?;

    let members: Vec<String> = line
        .trim_start_matches("TYPE_LABELS:")
        .trim()
        .trim_matches(['"', '\''])
        .split_whitespace()
        .map(str::to_owned)
        .collect();

    (!members.is_empty()).then_some(members)
}

/// The minimal proposal for a repository with no discoverable standard.
///
/// **Inert by construction** — [`Acceptance::Proposed`] makes
/// [`stella_autonomy::conform`] refuse every filing against it, including one
/// that would otherwise pass. The loop may propose any convention and grant
/// itself none; acceptance is a human's, and under governance mode `regulated`
/// it stays one.
///
/// Deliberately small. A proposal is a thing a human has to read and agree to,
/// and a large one is a thing they will skim.
fn propose() -> BacklogConvention {
    BacklogConvention {
        axes: vec![LabelAxis {
            name: "type".to_owned(),
            members: ["bug", "feature", "task"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            requirement: AxisRequirement::ExactlyOne,
            source: ConventionSource::Observed,
        }],
        reserved: Vec::new(),
        acceptance: Acceptance::Proposed,
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

    /// This repository's real workflow, abridged to the two facts that are read.
    const REAL_WORKFLOW: &str = r#"
env:
  GH_TOKEN: ${{ github.token }}
  # Space-separated; adding any of these counts as typing the issue.
  TYPE_LABELS: bug feature chore documentation epic
jobs:
  triage:
    steps:
      - run: gh issue edit "$ISSUE" --repo "$REPO" --add-label triage
"#;

    /// **The discovery witness.** The convention is learned from the automation
    /// that enforces it, not from a table hardcoded in Stella — so a repository
    /// that types its issues differently gets its own answer.
    #[test]
    fn the_type_axis_is_learned_from_the_workflow_that_enforces_it() {
        let ws = workspace();
        write(ws.path(), TRIAGE_WORKFLOW, REAL_WORKFLOW);

        let bound = load(ws.path());

        assert_eq!(bound.provenance, Provenance::Discovered);
        assert_eq!(bound.convention.acceptance, Acceptance::Bound);
        let axis = &bound.convention.axes[0];
        assert_eq!(axis.source, ConventionSource::Enforced);
        assert_eq!(
            axis.members,
            vec!["bug", "feature", "chore", "documentation", "epic"]
        );
        assert_eq!(bound.convention.reserved, vec!["triage".to_owned()]);
    }

    /// A different repository, a different vocabulary. Nothing about Stella's
    /// own labels is baked in.
    #[test]
    fn a_repository_with_its_own_vocabulary_gets_its_own_axis() {
        let ws = workspace();
        write(
            ws.path(),
            TRIAGE_WORKFLOW,
            "env:\n  TYPE_LABELS: defect enhancement chore\n",
        );

        let bound = load(ws.path());

        assert_eq!(
            bound.convention.axes[0].members,
            vec!["defect", "enhancement", "chore"]
        );
        // No `--add-label triage` in this one, so nothing is reserved.
        assert!(bound.convention.reserved.is_empty());
    }

    /// **The fail-safe witness.** A repository whose standard cannot be
    /// discovered yields a proposal, and a proposal files nothing — so the
    /// failure mode of not understanding a repository is "file nothing and say
    /// so", never "file something unclassified".
    #[test]
    fn an_undiscoverable_repository_proposes_and_files_nothing() {
        let bound = load(workspace().path());

        assert_eq!(bound.provenance, Provenance::Proposed);
        assert_eq!(bound.convention.acceptance, Acceptance::Proposed);

        // The operative half: even a well-formed draft is refused.
        assert!(!stella_autonomy::conform(&bound.convention, &["bug"]).is_conformant());
    }

    /// An empty list would make an axis that accepts every filing, which is the
    /// exact failure direction this module exists to avoid — so it is treated
    /// as "nothing discovered" rather than as a permissive convention.
    #[test]
    fn an_empty_type_list_is_not_a_permissive_convention() {
        let ws = workspace();
        write(ws.path(), TRIAGE_WORKFLOW, "env:\n  TYPE_LABELS:\n");

        assert_eq!(load(ws.path()).provenance, Provenance::Proposed);
    }

    /// A checked-in manifest overrides discovery — how a workspace describes a
    /// tracker this scan could never learn.
    #[test]
    fn a_checked_in_manifest_outranks_discovery() {
        let ws = workspace();
        write(ws.path(), TRIAGE_WORKFLOW, REAL_WORKFLOW);
        write(
            ws.path(),
            MANIFEST,
            r#"{"axes":[{"name":"kind","members":["defect"],
                 "requirement":"exactly_one","source":"declared"}],
                 "reserved":[],"acceptance":"bound"}"#,
        );

        let bound = load(ws.path());

        assert_eq!(bound.provenance, Provenance::Manifest);
        assert_eq!(bound.convention.axes[0].name, "kind");
    }

    /// Reads this repository's own checked-in
    /// `.github/workflows/issue-triage.yml` — not a fixture — and checks
    /// `TYPE_LABELS` against `/backlog-triage`'s five valid types
    /// (`bug feature chore documentation epic`), no more and no less: a
    /// `question` entry or a missing `chore` fails this test.
    #[test]
    fn the_real_workflow_type_labels_match_the_five_valid_types() {
        // crates/stella-cli -> repository root.
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");

        let bound = load(&repo_root);

        assert_eq!(
            bound.provenance,
            Provenance::Discovered,
            "expected TYPE_LABELS to be discoverable from the real workflow"
        );
        let mut members = bound.convention.axes[0].members.clone();
        members.sort();
        assert_eq!(
            members,
            vec!["bug", "chore", "documentation", "epic", "feature"]
        );
    }

    /// **The drift guard.** Four places hand-mirror
    /// `.github/workflows/issue-triage.yml`'s `TYPE_LABELS`: the
    /// `REAL_WORKFLOW` fixture above, `stella_autonomy`'s `stella()` helper,
    /// and `backlog.rs`'s and `residue.rs`'s `convention()` helpers. None of
    /// them reads the workflow, so an edit to the real one can reach some and
    /// miss the rest, leaving a fixture describing a convention this
    /// repository does not enforce. `#6036` had to edit all four by hand in one
    /// pull request, with nothing to catch a partial pass.
    ///
    /// The copies stay. Each proves something the others do not: the fixture
    /// proves the parser, and the helpers give the axis a value to conform
    /// against. What they cannot do any more is diverge in silence.
    ///
    /// `include_str!` across a crate boundary is the shape
    /// `the_two_copies_of_this_policy_have_not_drifted` uses for `accept.rs`'s
    /// twins, and it carries that shape's hazard: these files change
    /// atomically, so a mechanical sweep must not split them across pull
    /// requests (AGENTS.md, "A partition can also split something atomic").
    #[test]
    fn every_hand_written_copy_of_type_labels_matches_the_real_workflow() {
        const AUTONOMY: &str = include_str!("../../../stella-autonomy/src/convention.rs");
        const BACKLOG: &str = include_str!("backlog.rs");
        const RESIDUE: &str = include_str!("residue.rs");
        const WORKFLOW: &str = include_str!("../../../../.github/workflows/issue-triage.yml");

        let declared = WORKFLOW
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix("TYPE_LABELS:"))
            .expect("issue-triage.yml declares TYPE_LABELS");
        let members: Vec<&str> = declared.split_whitespace().collect();
        assert!(!members.is_empty(), "TYPE_LABELS names no type");

        // The one spelling every Rust copy uses: `["bug", "feature", ...]`.
        let rust_literal = format!(
            "[{}]",
            members
                .iter()
                .map(|m| format!("{m:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        for (path, text) in [
            ("crates/stella-autonomy/src/convention.rs", AUTONOMY),
            ("crates/stella-cli/src/self_driving_cmd/backlog.rs", BACKLOG),
            ("crates/stella-cli/src/self_driving_cmd/residue.rs", RESIDUE),
        ] {
            assert!(
                text.contains(&rust_literal),
                "{path} does not carry {rust_literal}, which is what \
                 .github/workflows/issue-triage.yml's TYPE_LABELS says today"
            );
        }

        assert!(
            REAL_WORKFLOW.contains(&format!("TYPE_LABELS: {}", members.join(" "))),
            "the REAL_WORKFLOW fixture above has stopped quoting the real TYPE_LABELS line"
        );
    }
}
