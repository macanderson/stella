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


// ── The errored-command census (#2125) ────────────────────────────────────

/// The shell tool the census reads results from. Not `read_only`, so the call
/// also counts as a mutating action and the ladder escalates instead of
/// writing the turn off as a no-op.
const CENSUS_SHELL: &str = "bash";

/// A shell whose every command answers with `content`, rendered the way the
/// real bash tool renders one (`stella-tools/src/bash.rs`): stdout, then a
/// `[stderr]` block, then the trailing exit marker. Nothing weaker will do —
/// the census is gated on those two markers precisely so a tool result that
/// is not a command chain says nothing.
struct FixedShell(&'static str);
#[async_trait]
impl ToolExecutor for FixedShell {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: CENSUS_SHELL.into(),
            description: "run a shell command".into(),
            input_schema: serde_json::json!({ "type": "object" }),
            read_only: false,
            speculation_safe: false,
        }]
    }
    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: self.0.into(),
        }
    }
}

/// A diff probe that reports a real one-line change, so the ladder finds the
/// turn observable and escalates to the model verifier — the only path on
/// which a verifier prompt exists to assert against.
struct MeasuringDiff;
#[async_trait]
impl DiagnosticRunner for MeasuringDiff {
    async fn run_diagnostic(&self, _invocation: &DiagnosticInvocation) -> CmdOutcome {
        CmdOutcome {
            exit_code: 0,
            stdout_tail: "diff --git a/README.md b/README.md\n--- a/README.md\n\
                          +++ b/README.md\n@@ -1 +1,2 @@\n line\n+The hook runs in 70 ms.\n"
                .to_string(),
            stderr_tail: String::new(),
            kind: CmdKind::Completed,
        }
    }
}

/// Run one scripted candidate whose worker turn makes a single shell call
/// answering with `shell_output`, and hand back the prompt the model verifier
/// was given.
async fn verifier_prompt_for_shell_output(shell_output: &'static str) -> String {
    let provider = ScriptedProvider::new(vec![
        text_result("CLASS: single\nWITNESS: no\nVERIFIER: yes"),
        // The worker measures...
        CompletionResult {
            tool_calls: vec![ToolCall {
                call_id: "call-measure".into(),
                name: CENSUS_SHELL.into(),
                input: serde_json::json!({ "command": "echo \"scale=0; $t/1\" | bc" }),
            }],
            ..text_result("")
        },
        // ...and reports the number it read.
        text_result("The hook runs in 70 ms."),
        text_result("PASS the change documents the measured latency"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = MeasuringDiff;
    let tests = ScriptedRunner::new(vec![], "");
    let tools = FixedShell(shell_output);
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = SeqRepoStatus::new(vec![vec![], vec![]]);
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
            roster: Roster::default().with_enabled(ModelCallRole::WitnessAuthor, false),
            ..PipelineConfig::default()
        },
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run(
            "Measure the hook and note it in README.md",
            &mut messages,
            &mut budget,
        )
        .await
        .expect("run completes");

    provider
        .prompts()
        .into_iter()
        .find(|p| p.contains("independent code reviewer"))
        .expect("the verifier was asked")
}

/// The trusted zone of a verdict prompt: the deterministic evidence the
/// pipeline states in its own voice, between the goal and the worker-authored
/// diff. Sliced out because the fixed instruction block *defines* the census
/// key on every call — a whole-prompt search for it would find the definition
/// and never notice a missing observation.
fn evidence_section(prompt: &str) -> &str {
    let (_, rest) = prompt
        .split_once("## Deterministic evidence gathered")
        .expect("the verdict prompt carries a trusted evidence section");
    rest.split_once("\n\nThe diff follows below")
        .expect("the evidence section ends where the untrusted diff begins")
        .0
}


/// The other half of the witness: a clean run's verifier evidence is
/// unchanged. An `errored_commands=0` printed on every verdict would read as
/// "this run's commands were verified clean" — a claim a closed signature
/// vocabulary cannot make — and would put a number a reader must learn to
/// ignore in front of every verifier.
#[tokio::test]
async fn a_clean_run_carries_no_errored_command_census() {
    let prompt = verifier_prompt_for_shell_output("70\n[exit code: 0]").await;
    let evidence = evidence_section(&prompt);
    assert!(
        !evidence.contains("errored_commands"),
        "silence means the probe had nothing to say: {evidence}"
    );
}
