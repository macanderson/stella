//! Stella's own `.stella/` state, excluded from the verifier-facing diff and
//! from the warrant's path set (#2038).
//!
//! The defect this pins: in a workspace whose `.gitignore` does not exclude
//! `.stella/private/`, the agent's own SQLite state appeared in the change
//! stream the verifier and the deck were shown, as if it were the agent's
//! work. One observed turn carried three entries — `codegraph.db-shm`,
//! `codegraph.db-wal`, and `test_regex.py`, the only real work — with the two
//! sidecars' bodies full of binary escape bytes.
//!
//! Two consequences, one per channel this module covers:
//!
//! 1. **Evidence noise.** Binary escape bytes in a verifier-facing payload.
//! 2. **Warrant classification.** `warrant::changed_paths` holds every changed
//!    path to the every-path-must-agree rules, and a `.db-wal` is neither docs
//!    nor test — so the agent's own bookkeeping silently defeated a
//!    `DocsOnly`/`TestsOnly` waiver the change had earned.
//!
//! The exclusion is this crate's own (`warrant::is_stella_state_path`): the
//! verification plane reaches these files through
//! `git ls-files --others --exclude-standard`, which honours `.gitignore`
//! and nothing else, so nothing upstream can be relied on to filter them.
//!
//! **There is no `.gitignore` anywhere in this test.** That absence is the
//! failing configuration: this repository ships a generated `.stella/.gitignore`
//! and is the only reason the defect does not reproduce here, and #2038's
//! definition of done is that the exclusion is a property of the code rather
//! than of a generated file that may be missing.
//!
//! A child module for the same two reasons `flip_halt_arming` is: it reaches
//! the shared fakes through the parent's own `use super::*`, and the
//! already-oversized `tests.rs` does not grow another module declaration.

use super::*;

/// Exactly the turn #2038 observed: one real created file beside the code
/// graph's two SQLite sidecars, as `git ls-files --others` reports them where
/// nothing excludes `.stella/private/`.
const OBSERVED_TURN: [(&str, &str); 3] = [
    (".stella/private/codegraph.db-wal", "h1"),
    (".stella/private/codegraph.db-shm", "h2"),
    ("test_regex.py", "h3"),
];

/// Added lines each untracked path reports, so a leaked sidecar is visible in
/// the probe's `lines` count — the diff-size budget's input — and not only in
/// its text.
const ADDED_PER_PATH: u32 = 7;

/// A prose-only tracked edit, for the classification half below. Docs-only is
/// the waiver #2038 observed being defeated, and it is only reachable when the
/// TRACKED diff carries the change — an untracked-only diff stays `Required`
/// by design, because `changed_paths` finds no header to read.
const DOCS_ONLY_TRACKED: &str = "--- a/docs/guide.md\n\
                                 +++ b/docs/guide.md\n\
                                 @@ -1 +1 @@\n\
                                 -old wording\n\
                                 +new wording\n";

/// A [`DiagnosticRunner`] over a tree whose tracked diff is caller-supplied
/// and whose untracked paths each answer with a numstat plus a real patch
/// body.
///
/// It answers for `.stella/` paths as readily as for any other — the exclusion
/// under test must come from the pipeline, never from a probe that declined.
struct ScriptedTree {
    tracked_diff: &'static str,
}

#[async_trait]
impl DiagnosticRunner for ScriptedTree {
    async fn run_diagnostic(&self, invocation: &DiagnosticInvocation) -> CmdOutcome {
        let stdout = match invocation {
            DiagnosticInvocation::GitDiff => self.tracked_diff.to_string(),
            DiagnosticInvocation::UntrackedNumstat { path } => {
                format!("{ADDED_PER_PATH}\t0\t{path}")
            }
            DiagnosticInvocation::UntrackedPatch { path } => format!(
                "diff --git a/{path} b/{path}\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+content\n"
            ),
        };
        CmdOutcome {
            exit_code: 0,
            stdout_tail: stdout,
            stderr_tail: String::new(),
            kind: CmdKind::Completed,
        }
    }
}

/// #2038's witness: Stella's own per-workspace state never reaches the
/// verifier's diff, its line count, its rendered bodies, or the warrant's path
/// set — with no `.gitignore` doing the work.
///
/// On `main` the untracked delta carries no identity filter at all, so all
/// three paths are read: the sidecars' markers and hunk bodies land in `text`,
/// their added lines are billed to `lines`, and `untracked_marker_paths`
/// parses their markers straight into the every-path-must-agree rules.
#[tokio::test]
async fn stella_private_state_never_reaches_the_verifier_diff_or_the_warrant() {
    let probe = gather(ScriptedTree { tracked_diff: "" }, OBSERVED_TURN.to_vec()).await;

    // Channel 1: the verifier-facing text.
    assert!(
        probe.text.contains("test_regex.py"),
        "the turn's real work is still reported: {}",
        probe.text
    );
    assert!(
        !probe.text.contains(".db-wal") && !probe.text.contains(".db-shm"),
        "Stella's own SQLite sidecars must not appear in a verifier-facing \
         payload — neither as a marker line nor as a hunk body: {}",
        probe.text
    );
    assert_eq!(
        probe.lines, ADDED_PER_PATH,
        "the diff-size budget sees ONE file's lines: the agent's bookkeeping \
         is not the agent's change, so it must not be billed to the turn"
    );

    // Channel 2: the path set the every-path-must-agree rules run over. An
    // untracked file reaches those rules through its marker line, not through
    // a `+++` header — `verify_probes::patch_body` strips those deliberately,
    // so `changed_paths` is empty for an untracked-only turn by design and
    // `untracked_marker_paths` is the seam that actually carries the paths.
    assert_eq!(
        crate::witness::warrant::untracked_marker_paths(&probe.text),
        vec!["test_regex.py".to_string()],
        "a `.db-wal` is neither docs nor test, so leaving one in this set \
         defeats a DocsOnly/TestsOnly waiver the change had earned"
    );
}

/// The same exclusion seen through the decision it changes (#2038's second
/// consequence): a turn that edits prose while Stella's code graph checkpoints
/// itself underneath is a documentation change, and must classify as one.
///
/// On `main` the two sidecars ride into `untracked_marker_paths`, neither is
/// docs, the every-path-must-agree rule breaks, and the run buys a witness for
/// a change that needed none — the agent's own bookkeeping deciding whether a
/// witness is owed.
#[tokio::test]
async fn stella_state_churn_does_not_defeat_a_docs_only_waiver() {
    let probe = gather(
        ScriptedTree {
            tracked_diff: DOCS_ONLY_TRACKED,
        },
        vec![
            (".stella/private/codegraph.db-wal", "h1"),
            (".stella/private/codegraph.db-shm", "h2"),
        ],
    )
    .await;

    assert_eq!(
        crate::witness::warrant::warrant(&probe.text, ChangeSignals::default()),
        crate::witness::warrant::WitnessWarrant::NotRequired(
            crate::witness::warrant::NoWitnessReason::DocsOnly
        ),
        "prose changed and nothing else did: {}",
        probe.text
    );
}

/// Drive [`Pipeline::gather_diff`] over the given tree and untracked
/// snapshot, against an empty before-snapshot — every path is fresh this
/// turn, which is what a first observation in a workspace with no exclusions
/// looks like.
async fn gather(diagnostics: ScriptedTree, untracked: Vec<(&str, &str)>) -> DiffProbe {
    let provider = ScriptedProvider::new(vec![]);
    let resolver = OneProvider(&provider);
    let repo_status = FakeRepoStatus::new(untracked);
    let runner = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, _rx) = mpsc::unbounded_channel();
    let pipeline = Pipeline::new(
        PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tools,
            recall: &recall,
            repo: &repo,
            repo_status: &repo_status,
            diagnostics: &diagnostics,
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: None,
            steering: None,
        },
        tx,
        PipelineConfig::default(),
    );

    let surface = CandidateSurface {
        diagnostics: &diagnostics,
        tests: &runner,
        lint: None,
        mutation: None,
        coverage: None,
        repo_status: &repo_status,
        cwd: None,
        hook_runner: None,
        workspace: None,
    };

    pipeline.gather_diff(surface, &HashMap::new()).await
}
