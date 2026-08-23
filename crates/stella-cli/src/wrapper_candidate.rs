// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The candidate grant and the tamper snapshot the CLI's wrapper driver owes a
//! plugin (#3553).
//!
//! # What was broken
//!
//! `crate::wrapper_plugin` sent `RoundInput { candidate: None, .. }` and
//! reported [`TamperFinding::NotChecked`], and both were honest — the host had
//! neither. The consequence was not honest at all: `judge` reads the flip first
//! and `FlipObservation::Unobservable` under `flip = "required"` is
//! `UndecidedReason::FlipUnobservable`, so **every witness-flavoured wrapper
//! reported `Undecided` on every run, forever**. Only a measurement oracle
//! (`flip = "not-applicable"`) worked. A plugin with no root to read and no
//! test to run cannot observe a flip, and a flip nobody vouched for cannot be
//! credited.
//!
//! # The grant names the tree the turn actually ran in
//!
//! `stella run --pipeline <variant>` drives the raw step loop **in the shared
//! work tree** (#3494). So the grant this module mints is over that tree, via
//! [`stella_plugin::host_tree_grant`] — the minting logic moved here (rather
//! than staying duplicated) before the staged pipeline's own
//! `CandidateHandles::grant` (formerly `stella-pipeline`'s `ports/handle.rs`)
//! was deleted along with that crate (#3865), so there was one fence, not
//! two, for as long as both callers coexisted, and this module is now the
//! only one.
//!
//! Minting a *worktree* instead would be worse than the gap it closes: the turn
//! would still run in the shared tree, the plugin would read and test an
//! isolated copy nothing had touched, and its flip observation would be a
//! confident answer about the wrong directory. The handle the grant carries is
//! therefore [`stella_plugin::HOST_TREE_HANDLE`], which addresses no
//! workspace in any table — a plugin that asks to seal or adopt it is told the
//! handle is unknown, which is true.
//!
//! Running the raw turn *inside* an isolated candidate, with adoption behind
//! the wrapper's verdict, is a different feature with its own blast radius (a
//! failed adoption is the user's work stranded in a shadow worktree) and is not
//! this slice.
//!
//! # The tamper watch, and exactly what it covers
//!
//! `TamperPolicy::ArtifactIdentity` asks the **host** to snapshot artifact
//! identity before the work and compare after, so that a worker which rewrote
//! the witness does not win the flip. The host has to know which artifacts
//! those are, and on this path it knows exactly one thing about the witness:
//! the test invocation the user gave it. So the watch covers **the files that
//! invocation names** — `sh tests/witness_flip.sh`, `pytest
//! tests/test_regression.py` — resolved through the same fence every other
//! plugin-supplied path crosses ([`stella_plugin::resolve_in_root`]),
//! snapshotted before the turn with the same no-follow identity
//! (`crate::agent::tools::fs_artifact_identity`) and compared with the same
//! comparator (`stella_plugin::witness_identity_matches`) the staged
//! pipeline itself used.
//!
//! What it does **not** cover is stated rather than implied: an invocation that
//! names no path — `cargo test --test flip`, whose artifact is `tests/flip.rs`
//! by cargo's convention and not by anything in the argv — snapshots nothing
//! and the finding is [`TamperFinding::NotChecked`]. That is a refusal to
//! credit, not a pass. Widening it needs the plugin to declare its own witness
//! artifacts on the wire, which is a wire change and is #3587.

use std::path::{Path, PathBuf};

use stella_plugin::{
    ArtifactIdentity, CandidateGrant, TamperFinding, host_tree_grant, parse_test_invocation,
    resolve_in_root, test_plan, witness_identity_matches,
};

/// The grant a wrapper plugin receives, and the artifacts the host will vouch
/// for.
///
/// The two are minted together because they come from one fact — the test
/// invocation — and a grant carrying a test plan whose artifacts nobody
/// snapshotted is exactly the half-answer that made every witness wrapper
/// abstain.
#[derive(Debug)]
pub(crate) struct GrantedCandidate {
    /// What rides on [`RoundInput::candidate`](stella_runtime::wrapper::RoundInput).
    pub(crate) grant: CandidateGrant,
    /// The artifacts whose identity the host pinned before the turn.
    pub(crate) watch: TamperWatch,
}

/// The host's own tamper baseline: workspace-relative path → the identity
/// observed before the turn ran.
///
/// Empty is a real state and the reason [`TamperFinding::NotChecked`] exists —
/// see this module's docs for the invocation shapes that produce it.
#[derive(Debug)]
pub(crate) struct TamperWatch {
    root: PathBuf,
    pinned: Vec<(String, ArtifactIdentity)>,
}

impl TamperWatch {
    /// Pin the identity of every artifact in `paths` that exists inside `root`.
    ///
    /// A path the fence refuses, or one whose identity cannot be observed
    /// without following a link, is left out of the watch entirely rather than
    /// recorded as absent: this is a baseline of things that *were there*, and
    /// an entry meaning "nothing was here" would make a file the worker
    /// legitimately created read as a change to a witness.
    pub(crate) fn pin(root: &Path, paths: &[String]) -> Self {
        let mut pinned = Vec::new();
        for path in paths {
            let Some(identity) = observe(root, path) else {
                continue;
            };
            if !pinned.iter().any(|(seen, _): &(String, _)| seen == path) {
                pinned.push((path.clone(), identity));
            }
        }
        Self {
            root: root.to_path_buf(),
            pinned,
        }
    }

    /// Re-observe every pinned artifact and report what the host found.
    ///
    /// Fails closed in both directions a comparison can fail: an artifact that
    /// no longer matches its pin **and** one that can no longer be observed at
    /// all are both [`TamperFinding::Tampered`] — a witness that was deleted or
    /// replaced with a symlink is not a witness that is unchanged.
    /// `witness_identity_matches` is the pipeline's own comparator, so a rename
    /// is tampering here for the same reason it is there: the identity carries
    /// the location the observation was made at.
    pub(crate) fn finding(&self) -> TamperFinding {
        if self.pinned.is_empty() {
            return TamperFinding::NotChecked;
        }
        for (path, expected) in &self.pinned {
            let current = observe(&self.root, path);
            if !witness_identity_matches(expected, current.as_ref()) {
                return TamperFinding::Tampered {
                    artifact: path.clone(),
                };
            }
        }
        TamperFinding::Clean
    }
}

/// One artifact's no-follow identity, or `None` when it cannot be established
/// inside `root`.
fn observe(root: &Path, path: &str) -> Option<ArtifactIdentity> {
    let root_text = root.to_str()?;
    // The fence before the filesystem, every time: the argv came from a user's
    // `--test-command` and the parser's own `validate_local_args` already
    // refuses absolute and `..` arguments, but a second reader of the same text
    // must not be the place that stops checking.
    resolve_in_root(root_text, path).ok()?;
    crate::agent::tools::fs_artifact_identity(root, path)
}

/// Mint the grant for the shared work tree, and pin whatever witness artifacts
/// the test command names.
///
/// `test_command` is the oracle for this turn, unparsed — `stella run`'s
/// `--test-command`, or the fleet task's own `test_command` plan field
/// (#3884). It crosses [`parse_test_invocation`] — the host's strict, closed
/// runner vocabulary — before anything is put on the wire, so a plugin
/// receives an argv the host itself would run and never a line to hand to a
/// shell (#1400).
///
/// # Errors
///
/// A message naming what a user can fix: a workspace root the filesystem will
/// not resolve, or a test command the parser refuses. The refusal quotes the
/// command rather than the flag that carried it, because two doors carry it
/// and a message naming `--test-command` is wrong on the one whose oracle came
/// out of a plan file. `stella-cli` is a binary, so a `String` here is the
/// finished product (AGENTS.md invariant 5).
pub(crate) fn grant_shared_tree(
    workspace_root: &Path,
    test_command: Option<&str>,
) -> Result<GrantedCandidate, String> {
    let root_text = workspace_root
        .to_str()
        .ok_or_else(|| "the workspace root is not valid UTF-8".to_string())?;

    let invocation = match test_command {
        Some(command) => Some(parse_test_invocation(command).map_err(|error| {
            format!("the test command `{command}` cannot be given to a wrapper plugin: {error}")
        })?),
        None => None,
    };

    // No baseline run: the host does not spend a test run the plugin is about
    // to spend itself. A wrapper observes red in `before_turn` — it holds the
    // root and the plan there — and green in `after_turn`, which is the flip
    // the oracle is for. `TestBaseline::NotRun` says exactly that, and a plugin
    // that wanted the host's own observation instead reads it as absent rather
    // than as passing.
    let plan = invocation.as_ref().map(|parsed| test_plan(parsed, None));
    let grant = host_tree_grant(root_text, plan)
        .map_err(|denial| format!("no candidate grant could be minted: {denial}"))?;

    // Pinned against the *canonical* root the grant carries, so the host
    // fences and observes in the same directory it told the plugin about.
    let watch = TamperWatch::pin(
        Path::new(&grant.root),
        &invocation
            .map(|parsed| named_artifacts(&parsed.args))
            .unwrap_or_default(),
    );
    Ok(GrantedCandidate { grant, watch })
}

/// The workspace-relative paths a parsed invocation's arguments name.
///
/// Deliberately syntactic and deliberately narrow: an argument that starts with
/// `-` is a flag, and everything else is a candidate path that
/// [`TamperWatch::pin`] then has to actually find on disk. Nothing here infers
/// a path a runner would derive by convention — `cargo test --test flip` names
/// `flip`, not `tests/flip.rs`, and turning one into the other is the host
/// guessing at a witness rather than watching one.
fn named_artifacts(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|arg| !arg.starts_with('-') && !arg.is_empty())
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp workspace");
        std::fs::create_dir_all(dir.path().join("tests")).expect("tests dir");
        std::fs::write(
            dir.path().join("tests/witness_flip.sh"),
            "#!/bin/sh\nexit 1\n",
        )
        .expect("the witness");
        dir
    }

    /// **Witness (grant).** The driver mints a real grant over the tree the
    /// turn runs in, carrying the parsed test plan a plugin needs to observe a
    /// flip at all. Before #3553 this value did not exist and `candidate` was
    /// `None` on every round.
    #[test]
    fn the_shared_tree_is_granted_with_the_test_the_host_would_run() {
        let dir = workspace();
        let granted = grant_shared_tree(dir.path(), Some("sh tests/witness_flip.sh"))
            .expect("the root resolves and the command parses");

        assert_eq!(
            granted.grant.root,
            std::fs::canonicalize(dir.path())
                .expect("canonical")
                .to_string_lossy(),
            "the plugin is told the same path the host fences against"
        );
        assert_eq!(
            granted.grant.handle.as_str(),
            stella_plugin::HOST_TREE_HANDLE,
            "a tree the host already runs in is not a workspace any table minted"
        );
        let plan = granted.grant.test.as_ref().expect("a test plan");
        assert_eq!(plan.program, "sh");
        assert_eq!(plan.args, vec!["tests/witness_flip.sh".to_string()]);
        assert_eq!(
            plan.baseline,
            stella_plugin::TestBaseline::NotRun,
            "the host did not run it, and says so rather than claiming red"
        );
    }

    /// **Witness (tamper).** The host vouches for an untouched witness and
    /// refuses one the turn rewrote — the two findings `judge` needs to credit
    /// or deny a flip. Both were unreachable before: this host only ever said
    /// `NotChecked`.
    #[test]
    fn the_host_vouches_for_an_untouched_witness_and_names_a_rewritten_one() {
        let dir = workspace();
        let granted = grant_shared_tree(dir.path(), Some("sh tests/witness_flip.sh"))
            .expect("the grant mints");
        assert_eq!(
            granted.watch.finding(),
            TamperFinding::Clean,
            "the named artifact is pinned, and nothing has touched it yet"
        );

        std::fs::write(
            dir.path().join("tests/witness_flip.sh"),
            "#!/bin/sh\nexit 0\n",
        )
        .expect("the worker rewrites the witness");
        assert_eq!(
            granted.watch.finding(),
            TamperFinding::Tampered {
                artifact: "tests/witness_flip.sh".to_string(),
            },
            "a worker that rewrote the witness does not win the flip"
        );
    }

    /// A deleted witness is tampering, not an unchanged one: the comparison
    /// fails closed on an artifact it can no longer observe.
    #[test]
    fn a_witness_that_vanished_is_refused_rather_than_vouched_for() {
        let dir = workspace();
        let granted =
            grant_shared_tree(dir.path(), Some("sh tests/witness_flip.sh")).expect("the grant");
        std::fs::remove_file(dir.path().join("tests/witness_flip.sh")).expect("removed");
        assert!(matches!(
            granted.watch.finding(),
            TamperFinding::Tampered { .. }
        ));
    }

    /// An invocation that names no file on disk watches nothing, and says so.
    /// `NotChecked` is not a pass — `judge` reads it as `TamperUnchecked` — so
    /// this is a declared limit rather than a hidden credit.
    #[test]
    fn an_invocation_that_names_no_artifact_checks_nothing() {
        let dir = workspace();
        let granted = grant_shared_tree(dir.path(), Some("cargo test --test witness_flip"))
            .expect("the grant mints");
        assert_eq!(granted.watch.finding(), TamperFinding::NotChecked);

        let none = grant_shared_tree(dir.path(), None).expect("no test command is fine");
        assert!(none.grant.test.is_none(), "no plan is not an empty plan");
        assert_eq!(none.watch.finding(), TamperFinding::NotChecked);
    }

    /// A test command the host's own parser refuses produces no grant carrying
    /// it — the plugin never receives an invocation this host would not run.
    /// The refusal quotes the command, which is the half a user can act on
    /// whichever door supplied it (`--test-command` or a fleet plan file).
    #[test]
    fn a_refused_test_command_mints_no_grant() {
        let dir = workspace();
        let error = grant_shared_tree(dir.path(), Some("rm -rf /"))
            .expect_err("`rm` is not in the runner vocabulary");
        assert!(error.contains("rm -rf /"), "{error}");
    }

    /// The fence is asked before the filesystem is, even though the parser
    /// already refused these shapes: two readers of the same untrusted text,
    /// and neither gets to be the one that stopped checking.
    #[test]
    fn an_escaping_artifact_path_is_never_observed() {
        let dir = workspace();
        let watch = TamperWatch::pin(
            dir.path(),
            &[
                "../outside.sh".to_string(),
                "/etc/passwd".to_string(),
                "tests/witness_flip.sh".to_string(),
            ],
        );
        assert_eq!(
            watch.pinned.len(),
            1,
            "only the path inside the root is watched"
        );
    }
}
