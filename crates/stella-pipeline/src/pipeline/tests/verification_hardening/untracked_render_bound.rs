//! The untracked-diff read bound (#2110) — the witness and the counting
//! diagnostic runner only it scripts.
//!
//! The defect this pins: `gather_diff`'s untracked pass read every fresh
//! untracked path, two `git diff --no-index` subprocesses apiece, with no
//! bound of its own. On a task that unpacks and builds a source tree that is
//! thousands of paths — Terminal-Bench's `sqlite-with-gcov` extracts SQLite
//! into the workspace and builds it with `--coverage`, and the harness
//! commits a git baseline first, so every extracted and generated file is
//! untracked and fresh. The pass then spent the trial's whole remaining wall
//! clock reading a build tree, and the journal ended after the worker's final
//! text with no warrant, no verify, no verdict and no `complete` event.
//!
//! The fix bounds what is READ without loosening what is KNOWN: the tail past
//! [`super::super::super::verify_probes`]'s budget is still named (the
//! warrant parses those markers into its path rules) and still counted (the
//! diff-size budget must stay tripped), so the only thing given up is content
//! nothing could have used.

use super::*;

/// A [`DiagnosticRunner`] that answers any untracked path and COUNTS the
/// subprocesses it was asked for.
///
/// The count is the whole point: the bound under test is a cost bound, and a
/// test that only inspected the resulting text would pass just as happily on
/// a probe that read all ten thousand files and then rendered a few.
struct CountingDiagnostics {
    calls: std::sync::atomic::AtomicUsize,
}

impl CountingDiagnostics {
    fn new() -> Self {
        Self {
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait]
impl DiagnosticRunner for CountingDiagnostics {
    async fn run_diagnostic(&self, invocation: &DiagnosticInvocation) -> CmdOutcome {
        let stdout = match invocation {
            // A clean tracked tree: the whole change is the untracked files.
            DiagnosticInvocation::GitDiff => String::new(),
            DiagnosticInvocation::UntrackedNumstat { path } => {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                format!("7\t0\t{path}")
            }
            DiagnosticInvocation::UntrackedPatch { path } => {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                format!("diff --git a/{path} b/{path}\n+++ b/{path}\n@@ -0,0 +1 @@\n+content\n")
            }
        };
        CmdOutcome {
            exit_code: 0,
            stdout_tail: stdout,
            stderr_tail: String::new(),
            kind: CmdKind::Completed,
        }
    }
}

/// How many fresh untracked paths the scripted tree grows — comfortably past
/// the budget, and the same order of magnitude as a built SQLite tree.
const FRESH_PATHS: usize = 3000;

/// #2110's witness: a turn that leaves thousands of fresh untracked files
/// must cost a BOUNDED number of diff subprocesses, while still naming and
/// counting every one of those paths.
///
/// On `main` the pass reads all of them — `2 * FRESH_PATHS` subprocesses —
/// which is the unbounded cost that rode Terminal-Bench trials into the
/// harness kill before the warrant could be emitted.
#[tokio::test]
async fn a_tree_sized_untracked_change_is_read_within_a_bounded_number_of_probes() {
    let provider = ScriptedProvider::new(vec![]);
    let resolver = OneProvider(&provider);
    let diagnostics = CountingDiagnostics::new();
    // An unpacked, built source tree: every path fresh this turn.
    let paths: Vec<String> = (0..FRESH_PATHS)
        .map(|i| format!("sqlite/src/file{i:05}.c"))
        .collect();
    let repo_status = FakeRepoStatus::new(paths.iter().map(|p| (p.as_str(), "v1")).collect());
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
            touches: &NoFileTouches,
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

    let probe = pipeline.gather_diff(surface, &HashMap::new()).await;

    // THE BOUND. Two subprocesses per path read; unbounded, this is 6000.
    let calls = diagnostics.calls();
    assert!(
        calls < 2 * FRESH_PATHS,
        "the untracked pass must not read every path of an unpacked tree — \
         it read {calls} of a possible {} (#2110)",
        2 * FRESH_PATHS
    );

    // ...and the bound costs nothing the warrant or the budget depended on.
    assert!(
        probe.available,
        "a readable tree still reports itself readable"
    );
    // Counted through the shared prefix the warrant's own parser keys on, so
    // this asserts the property that parser depends on rather than a shape
    // this test invented.
    let named = probe
        .text
        .lines()
        .filter(|line| line.starts_with(crate::witness::warrant::UNTRACKED_CHANGE_PREFIX))
        .count();
    assert_eq!(
        named, FRESH_PATHS,
        "every fresh path is still NAMED: the warrant parses these markers \
         into the path set it holds to its 'every path must agree' rules, so \
         an unnamed tail could relax exactly when a witness is owed"
    );
    assert!(
        probe.lines as usize >= FRESH_PATHS,
        "every fresh path still contributes at least its floor of 1 line, so \
         a tree-sized change stays OVER the diff-size budget rather than \
         slipping under it: {} lines for {FRESH_PATHS} paths",
        probe.lines
    );
    assert!(
        probe.text.contains("were not read"),
        "the truncation is declared, not silent — the tail's counts are \
         floors and a reader must be told so: {}",
        &probe.text[probe.text.len().saturating_sub(400)..]
    );
}
