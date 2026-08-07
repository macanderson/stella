//! The orchestrator MCP pre-fetch hook (issue #248): [`McpPrefetchPort::prefetch`]
//! is consulted once at the top of `run_best_of_n`, and its result — when
//! `Some` — rides in every candidate's shared message history rather than
//! each candidate independently paying to look it up. The sweep is
//! goal-blind by contract (#1779): `prefetch` takes no arguments, so no
//! double here can even observe what goal the run carries.

use super::*;

/// A [`McpPrefetchPort`] that always returns a fixed sentinel string —
/// proves the orchestrator calls it and folds the result into the shared
/// history, independent of how the real CLI adapter gathers context.
struct FixedPrefetch(&'static str);

#[async_trait]
impl McpPrefetchPort for FixedPrefetch {
    async fn prefetch(&self) -> Option<String> {
        Some(self.0.to_string())
    }
}

#[tokio::test]
async fn best_of_n_folds_the_mcp_prefetch_into_every_candidate() {
    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("cand0 done"),
        text_result("cand1 done"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, false, false, true], "@@ -1 +1 @@\n-a\n+b");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let prefetch = FixedPrefetch("SENTINEL-SHARED-CONTEXT");
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
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: Some(&prefetch),
            steering: None,
        },
        tx,
        isolated_config(2),
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");
    assert!(
        messages
            .iter()
            .any(|m| m.content.contains("SENTINEL-SHARED-CONTEXT")),
        "the once-fetched context must ride in the winning candidate's history: {messages:?}"
    );
}

/// The pre-fetched block is budgeted like recall: past the fan-out budget it
/// is head-kept with an in-band marker. This block rides ahead of every
/// candidate's every turn, so an unbounded server answer multiplied itself
/// by N candidates × every revision.
#[tokio::test]
async fn an_oversized_prefetch_is_clamped_with_a_visible_marker() {
    /// [`FixedPrefetch`] with an owned answer, sized at runtime.
    struct OwnedPrefetch(String);
    #[async_trait]
    impl McpPrefetchPort for OwnedPrefetch {
        async fn prefetch(&self) -> Option<String> {
            Some(self.0.clone())
        }
    }
    let port = OwnedPrefetch("x".repeat(60_000));
    let folded = crate::mcp_prefetch::fold(Some(&port), 2, &[])
        .await
        .expect("a hit folds a message");
    let msg = &folded.last().expect("one folded message").content;
    assert!(msg.contains("truncated at the fan-out budget"));
    assert!(
        msg.chars().count() < 21_000,
        "the folded block is bounded by the budget, not the server's appetite"
    );
}

/// A prefetch miss (`None`) must never abort the run — best-of-N proceeds
/// exactly as if no [`McpPrefetchPort`] were wired at all.
#[tokio::test]
async fn a_prefetch_miss_never_aborts_the_run() {
    struct EmptyPrefetch;
    #[async_trait]
    impl McpPrefetchPort for EmptyPrefetch {
        async fn prefetch(&self) -> Option<String> {
            None
        }
    }

    let provider = ScriptedProvider::new(vec![
        text_result("single"),
        text_result("cand0 done"),
        text_result("cand1 done"),
    ]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, false, false, true], "@@ -1 +1 @@\n-a\n+b");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let prefetch = EmptyPrefetch;
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
            tests: &runner,
            lint: None,
            mutation: None,
            coverage: None,
            approvals: &approvals,
            sleeper: &sleeper,
            hooks: None,
            candidate_workspaces: None,
            mcp_prefetch: Some(&prefetch),
            steering: None,
        },
        tx,
        isolated_config(2),
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("a prefetch miss must not fail the run");
    assert_eq!(outcome.status, PipelineStatus::Completed);
}
