//! Triage classifies with workspace evidence, not the goal string alone:
//! the [`RepoStructurePort`] summary the planner and witness author already
//! receive also rides in the triage payload (bounded — see
//! `crate::triage::triage_prompt`). Triage decides the highest-leverage
//! questions in the pipeline — how much orchestration, whether a witness is
//! warranted, whether this is a task at all — and used to be the only stage
//! deciding anything blind.

use crate::ports::RepoStructurePort;

use super::*;

/// A [`RepoStructurePort`] answering with a fixed sentinel listing — proves
/// the orchestrator consults the port for triage and lands the summary in the
/// paid call's payload, independent of how the real CLI adapter renders it.
struct FixedStructure(&'static str);

#[async_trait]
impl RepoStructurePort for FixedStructure {
    async fn structure_summary(&self) -> String {
        self.0.to_string()
    }
}

#[tokio::test]
async fn the_triage_call_carries_the_workspace_listing_in_its_payload() {
    // triage → "single"; worker turn → final text (no tool calls).
    let provider = ScriptedProvider::new(vec![text_result("single"), text_result("done")]);
    let resolver = OneProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, true], "@@ -1 +1 @@\n-old\n+new");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = FixedStructure("src/lib.rs\nSENTINEL-STRUCTURE.rs\ntests/smoke.rs");
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, _rx) = mpsc::unbounded_channel();

    let config = PipelineConfig {
        test_command: Some("cargo test -p x".into()),
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
        .run("Fix the failing test", &mut messages, &mut budget)
        .await
        .expect("run succeeds");
    assert_eq!(outcome.status, PipelineStatus::Completed);

    // The first provider request is the triage call. Its volatile USER half
    // carries the listing and the goal; the cacheable SYSTEM half (#1434)
    // must stay workspace-independent or every triage call re-bills its
    // instruction block at the uncached rate.
    let shapes = provider.shapes();
    let triage_call = shapes.first().expect("a triage call was made");
    let user_payload: String = triage_call
        .iter()
        .filter(|(role, _)| *role == stella_protocol::MessageRole::User)
        .map(|(_, text)| text.as_str())
        .collect();
    assert!(
        user_payload.contains("SENTINEL-STRUCTURE.rs"),
        "the triage payload must carry the RepoStructurePort summary; got:\n{user_payload}"
    );
    assert!(user_payload.contains("Fix the failing test"));
    for (role, text) in triage_call {
        if *role == stella_protocol::MessageRole::System {
            assert!(
                !text.contains("SENTINEL-STRUCTURE.rs"),
                "workspace bytes in the system half would break the cacheable prefix"
            );
        }
    }
}
