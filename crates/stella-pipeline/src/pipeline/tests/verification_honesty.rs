//! The verification-honesty guard (`verify_probes::verification_honest_diff`)
//! observed at its seam: an empty diff beside real file-change events must
//! read as "couldn't capture", never "verified nothing". Split out of
//! `tests.rs`, which is closed to growth.

use super::super::verify_probes::verification_honest_diff;

/// The archetypal lie: the turn emitted file-change events, but the diff
/// came back empty (committed work, a baseline miss, an uncaptured file).
/// A bare empty string reads to a verifier as "no changes were made" — the
/// signal that once drove an agent to reinitialize git. The guard must
/// turn it into an honest "couldn't capture", never "verified nothing".
#[test]
fn a_blind_empty_diff_with_file_changes_is_reported_as_uncaptured_not_absent() {
    let out = verification_honest_diff(String::new(), 3);
    assert!(
        !out.trim().is_empty(),
        "an empty diff with file changes must not stay empty"
    );
    assert!(out.contains("could not be captured"), "{out}");
    assert!(
        out.contains("NOT evidence that nothing changed"),
        "the marker must foreclose the 'no changes' reading: {out}"
    );
    assert!(out.contains('3'), "it names the file-change count: {out}");
}

/// A genuinely empty diff — no file-change events — is the truth and stays
/// empty. The guard must not invent changes that did not happen.
#[test]
fn a_truly_empty_diff_with_no_file_changes_stays_empty() {
    assert_eq!(verification_honest_diff(String::new(), 0), "");
    assert_eq!(verification_honest_diff("   \n".to_string(), 0), "   \n");
}

/// A real diff is passed through untouched, regardless of the count.
#[test]
fn a_real_diff_passes_through_unchanged() {
    let diff = "@@ -1 +1 @@\n-a\n+b".to_string();
    assert_eq!(verification_honest_diff(diff.clone(), 0), diff);
    assert_eq!(verification_honest_diff(diff.clone(), 5), diff);
}

use super::*;

/// The regex the fixture task writes. Distinctive so an assertion that finds
/// it in a prompt cannot be finding something else.
const REGEX: &str = r"^\d{4}-\d{2}-\d{2}$";

/// A diagnostics double for the untracked-only shape: `git diff` sees nothing
/// (the file was never staged), the numstat probe counts one added line, and
/// the patch probe serves the file under the header preamble real git emits.
struct PatchRunner;

#[async_trait]
impl DiagnosticRunner for PatchRunner {
    async fn run_diagnostic(&self, invocation: &DiagnosticInvocation) -> CmdOutcome {
        let stdout = match invocation {
            DiagnosticInvocation::GitDiff => String::new(),
            DiagnosticInvocation::UntrackedNumstat { .. } => "1\t0\tregex.txt".to_string(),
            DiagnosticInvocation::UntrackedPatch { .. } => format!(
                "diff --git a/dev/null b/regex.txt\nnew file mode 100644\n\
                 index 0000000..1111111\n--- /dev/null\n+++ b/regex.txt\n\
                 @@ -0,0 +1 @@\n+{REGEX}\n"
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

/// The witness: a task whose entire deliverable is an untracked file must
/// reach the verifier as its CONTENT, not as its name.
///
/// Before the `UntrackedPatch` probe, `gather_diff` described such a file with
/// one marker line — the path and an added-line count — because `git diff`
/// cannot see an unstaged file and nothing else was asked about it. A real run
/// of exactly this shape returned PASS reasoning that "the unseen content
/// cannot itself justify a FAIL": the model was grading a filename.
///
/// Asserting on the verifier's own prompt is the point. An assertion on the
/// probe's return value would still pass if the text were dropped anywhere
/// between the probe and the model.
#[tokio::test]
async fn the_verifier_reads_an_untracked_files_content_not_just_its_name() {
    let provider = ScriptedProvider::new(vec![
        text_result("CLASS: single\nWITNESS: no\nVERIFIER: yes"),
        text_result("Wrote the regex to regex.txt."),
        text_result("PASS the regex handles the stated cases"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = PatchRunner;
    let tests = ScriptedRunner::new(vec![], "");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    // Empty before the turn, carrying the new file after it — the fingerprint
    // delta that makes `gather_diff` bill the file to this turn.
    let repo_status = SeqRepoStatus::new(vec![vec![], vec![("regex.txt", "sha256:a")]]);
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
            diagnostics: &runner,
            tests: &tests,
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
        PipelineConfig {
            test_command: None,
            diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
            witness_writer: false,
            ..PipelineConfig::default()
        },
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run("Write a regex to regex.txt", &mut messages, &mut budget)
        .await
        .expect("run completes");

    let verifier_prompt = provider
        .prompts()
        .into_iter()
        .find(|p| p.contains("independent code reviewer"))
        .expect("the verifier was asked");
    assert!(
        verifier_prompt.contains(REGEX),
        "the verifier graded the change without ever seeing it: {verifier_prompt}"
    );
    assert!(
        verifier_prompt.contains("untracked change: regex.txt"),
        "the marker still names the file the content belongs to: {verifier_prompt}"
    );
}
