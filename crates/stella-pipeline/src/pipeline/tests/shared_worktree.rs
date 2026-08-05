//! Attribution in a shared worktree (#1553): the diff probe reads the
//! WORKING TREE, and a tree shared with a human can move under a run. The
//! verifier must judge only what the run itself did — `mutating_actions`,
//! counted off the calls the pipeline dispatched, is the one signal foreign
//! motion cannot forge. A child module, so it reaches the scripted ports
//! above via `super::*`; the worktree-relocation policy tests live here for
//! the same reason — they are the other half of "whose tree is this?".

use super::*;

/// The witness #1553 demanded: a run that touches nothing, with an unrelated
/// tracked edit appearing mid-run, must not be failed for that edit.
///
/// This fixture is byte-for-byte the shape `misclassified_lookup_…` used to
/// model "the turn touched files" — a non-empty diff with ZERO dispatched
/// tool calls. That conjunction can only mean the motion is not the run's:
/// the engine changes the world exclusively through tool calls, so a turn
/// that dispatched none cannot own a diff. Before the fix, the zero-diff
/// guard read the foreign edit as the run's, revoked the verifier skip, and
/// handed someone else's diff to the verifier.
#[tokio::test]
async fn a_foreign_edit_beside_an_idle_lookup_does_not_drag_it_into_verification() {
    // triage → "lookup"; worker answers with NO tool calls. No verifier
    // response is scripted, because none must be consulted.
    let provider = ScriptedProvider::new(vec![text_result("lookup"), text_result("the answer")]);
    let resolver = OneProvider(&provider);
    // The tree shows an edit — made by someone else while the run read code.
    let runner = ScriptedRunner::new(vec![], "@@ -1 +1 @@\n-a\n+b");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let config = PipelineConfig {
        test_command: None,
        diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
        ..PipelineConfig::default()
    };
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
        config,
    );

    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Explain the retry policy", &mut messages, &mut budget)
        .await
        .expect("a foreign edit must not break a read-only run");

    assert_eq!(outcome.task_class, TaskClass::SimpleLookup);
    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert!(
        outcome.verdict.is_none(),
        "someone else's edit is not this run's work to verify: {:?}",
        outcome.verdict
    );
    assert_eq!(outcome.final_text, "the answer");

    let s = stages(&drain(&mut rx));
    assert!(
        !s.contains(&StageKind::Verdict),
        "no verifier may be consulted over foreign motion: {s:?}"
    );
}

/// `create_worktrees` precedence, without a gate in the way.
///
/// The case that matters most is the first one: a lookup never raises the
/// question. `ask` is the default, so a policy that asked before knowing the
/// class would put a prompt in front of every `stella "what does this do"` —
/// about relocating work the run is never going to do.
#[test]
fn the_worktree_policy_defers_to_the_class_then_to_what_is_available() {
    use crate::pipeline::{Decided, worktree_decision_without_asking as decide};
    use crate::ports::WorktreePolicy::{Always, Ask, Never};

    // A run that changes nothing is never isolated and never asks — not even
    // under `always`.
    for policy in [Always, Ask, Never] {
        assert_eq!(
            decide(policy, false, true),
            Decided::No,
            "{policy:?} must not isolate a run that changes nothing"
        );
    }

    // No isolation available: silent for the two policies that did not ask for
    // it, and told for the one that did.
    assert_eq!(decide(Ask, true, false), Decided::No);
    assert_eq!(decide(Never, true, false), Decided::No);
    assert!(
        matches!(decide(Always, true, false), Decided::NoAndSayWhy(_)),
        "an operator who configured `always` and cannot have it must be told"
    );

    // And with everything available, the policy decides.
    assert_eq!(decide(Always, true, true), Decided::Yes);
    assert_eq!(decide(Never, true, true), Decided::No);
    assert_eq!(decide(Ask, true, true), Decided::MustAsk);
}

/// An approval gate that cannot ask answers "no", so an unwired or headless
/// host keeps doing exactly what it did before the question existed.
#[tokio::test]
async fn a_gate_with_nobody_behind_it_declines_to_relocate_the_work() {
    use crate::ports::{AlwaysAbortGate, ApprovalGate};
    assert!(
        !AlwaysAbortGate.confirm("anything at all").await,
        "a headless run must not silently relocate somebody's work"
    );
}
