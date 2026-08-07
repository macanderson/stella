//! The pre-plan research stage (#1778) observed end to end: triage names
//! questions, parallel read-only sub-agents answer them between `Triage` and
//! `Plan`, the planner prompt carries the bounded findings as their own
//! section — and every degraded round (no questions, empty answers) leaves
//! the turn exactly as it was before the stage existed.

use super::*;

/// One scripted multi-step run over `extra` provider results: triage's reply
/// is `triage`, then whatever `extra` scripts (research children, the plan,
/// the worker's close-out). Returns the outcome, the event stream, and the
/// per-call message shapes.
macro_rules! research_scenario {
    ($triage:expr, $extra:expr) => {{
        let mut script = vec![text_result($triage)];
        script.extend($extra);
        let provider = ScriptedProvider::new(script);
        let resolver = OneProvider(&provider);
        let runner = ScriptedRunner::new(vec![false, true, true], "@@ -1 +1 @@\n-old\n+new");
        let tools = EmptyTools;
        let recall = NoContextRecall;
        let repo = NoRepoStructure;
        let repo_status = NoRepoStatus;
        let approvals = AutoApproveGate;
        let sleeper = NoopSleeper;
        let router = router();
        let (tx, mut rx) = mpsc::unbounded_channel();
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
            PipelineConfig {
                test_command: Some("cargo test -p x".into()),
                diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
                ..PipelineConfig::default()
            },
        );
        let mut messages = vec![CompletionMessage::system("sys")];
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let outcome = pipeline
            .run(
                "Refactor the retry layer end to end",
                &mut messages,
                &mut budget,
            )
            .await
            .expect("run succeeds");
        (outcome, drain(&mut rx), provider.shapes())
    }};
}

/// A triage reply naming two research questions on the structured protocol.
const TRIAGE_WITH_QUESTIONS: &str = "CLASS: multi\nWITNESS: yes\nVERIFIER: yes\n\
     RESEARCH: Which module owns retries? | Where are the retry tests?";

/// The planner prompt of a run, from the recorded per-call shapes: the one
/// user message carrying the `## Plan` instruction.
fn planner_prompt(shapes: &[Vec<(stella_protocol::MessageRole, String)>]) -> String {
    shapes
        .iter()
        .flatten()
        .map(|(_, text)| text.as_str())
        .find(|text| text.contains("## Plan (JSON array of step strings)"))
        .expect("a planner call was made")
        .to_string()
}

/// The issue's definition of done, first half: a multi-step goal whose triage
/// names questions produces N>1 concurrent `SubAgent` brackets between
/// `Stage::Triage` and `Stage::Plan`, under a `Stage::Research` bookend.
#[tokio::test]
async fn triage_questions_fan_out_as_sub_agents_between_triage_and_plan() {
    let (outcome, events, _shapes) = research_scenario!(
        TRIAGE_WITH_QUESTIONS,
        vec![
            text_result("driver.rs owns retries; retry.rs holds the policy."),
            text_result("The retry tests live in driver/tests.rs."),
            text_result(r#"["update retry.rs","update driver.rs"]"#),
            text_result("PLAN COMPLETE: retry layer refactored."),
        ]
    );

    assert_eq!(outcome.status, PipelineStatus::Completed);
    let stage_list = stages(&events);
    let research_at = stage_list
        .iter()
        .position(|s| *s == StageKind::Research)
        .expect("the research stage is emitted");
    let plan_at = stage_list
        .iter()
        .position(|s| *s == StageKind::Plan)
        .expect("the plan stage is emitted");
    assert!(
        research_at < plan_at,
        "research precedes plan: {stage_list:?}"
    );

    let research_idx = events
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::Stage {
                    name: StageKind::Research
                }
            )
        })
        .expect("research stage event");
    let plan_idx = events
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::Stage {
                    name: StageKind::Plan
                }
            )
        })
        .expect("plan stage event");
    let started: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(
                e,
                AgentEvent::SubAgent {
                    phase: stella_protocol::SubAgentPhase::Started { .. }
                }
            )
        })
        .map(|(i, _)| i)
        .collect();
    let finished = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::SubAgent {
                    phase: stella_protocol::SubAgentPhase::Finished { .. }
                }
            )
        })
        .count();
    assert_eq!(
        started.len(),
        2,
        "one sub-agent bracket per question: {events:?}"
    );
    assert_eq!(finished, 2, "every bracket closes");
    assert!(
        started.iter().all(|&i| i > research_idx && i < plan_idx),
        "the fan-out runs between Research and Plan: started at {started:?}, \
         research at {research_idx}, plan at {plan_idx}"
    );
}

/// Definition of done, second half: the planner prompt carries the findings
/// as a distinct section — question and answer — separate from recall frames.
#[tokio::test]
async fn the_planner_prompt_carries_the_bounded_findings_section() {
    let (_outcome, _events, shapes) = research_scenario!(
        TRIAGE_WITH_QUESTIONS,
        vec![
            text_result("ANSWER-ONE: driver.rs owns retries."),
            text_result("ANSWER-TWO: tests live in driver/tests.rs."),
            text_result(r#"["update retry.rs"]"#),
            text_result("PLAN COMPLETE: done."),
        ]
    );

    let prompt = planner_prompt(&shapes);
    assert!(
        prompt.contains("## Research findings"),
        "the findings are their own section: {prompt}"
    );
    // Both answers arrive whatever order the concurrent children finished in.
    assert!(prompt.contains("ANSWER-ONE"), "{prompt}");
    assert!(prompt.contains("ANSWER-TWO"), "{prompt}");
    assert!(
        prompt.contains("Which module owns retries?"),
        "each finding is grounded by its question: {prompt}"
    );
}

/// A triage reply with no `RESEARCH:` line skips the stage byte-for-byte:
/// no `Research` stage event, no sub-agents, no findings section — today's
/// behavior exactly (L-E2).
#[tokio::test]
async fn no_questions_means_no_stage_no_sub_agents_no_section() {
    let (outcome, events, shapes) = research_scenario!(
        "CLASS: multi\nWITNESS: yes\nVERIFIER: yes",
        vec![
            text_result(r#"["update retry.rs"]"#),
            text_result("PLAN COMPLETE: done."),
        ]
    );

    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert!(
        !stages(&events).contains(&StageKind::Research),
        "no research stage on the skip path: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::SubAgent { .. })),
        "no sub-agents on the skip path"
    );
    assert!(!planner_prompt(&shapes).contains("## Research findings"));
}

/// One scripted entry of [`HangTailProvider`]: serve a result, or hang.
enum HangScript {
    Serve(CompletionResult),
    Hang,
}

/// Serves its script in order; a [`HangScript::Hang`] entry parks that call
/// forever (the research latency ceiling is what ends it), and later calls
/// keep serving the rest of the script — so the run can continue past the
/// cancelled child.
struct HangTailProvider {
    script: TokioMutex<VecDeque<HangScript>>,
}

#[async_trait]
impl Provider for HangTailProvider {
    fn id(&self) -> &str {
        "hang-tail"
    }

    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        let next = self.script.lock().await.pop_front();
        match next {
            Some(HangScript::Serve(result)) => Ok(result),
            Some(HangScript::Hang) => std::future::pending().await,
            None => Err(ProviderError::Terminal("script exhausted".into())),
        }
    }
}

/// #1954's witness, verbatim: a research child that never answers within the
/// ceiling produces a **balanced** Started/Finished bracket, a
/// `UsageIncomplete` with reason `cancelled`, and `Finished.steps` equal to
/// the child's committed `StepUsage` count — while the turn itself degrades
/// to a missing finding and completes. Before the primitive owned the
/// cancel bracket, the stage forged `Finished { steps: 0 }` and this failed.
#[tokio::test]
async fn a_child_past_the_ceiling_closes_its_bracket_with_committed_steps() {
    // Sequence: triage (one question) → child call 1 commits a tool step →
    // child call 2 hangs until the ceiling cancels it → plan → close-out.
    let mut committed_step = text_result("");
    committed_step.tool_calls = vec![ToolCall {
        call_id: "r1".into(),
        name: "read_file".into(),
        input: serde_json::json!({ "path": "src/lib.rs" }),
    }];
    committed_step.cost_usd = 0.002;
    let provider = HangTailProvider {
        script: TokioMutex::new(VecDeque::from([
            HangScript::Serve(text_result(
                "CLASS: multi\nWITNESS: yes\nVERIFIER: yes\nRESEARCH: Which module owns retries?",
            )),
            HangScript::Serve(committed_step),
            HangScript::Hang,
            HangScript::Serve(text_result(r#"["update retry.rs"]"#)),
            HangScript::Serve(text_result("PLAN COMPLETE: done.")),
        ])),
    };
    let resolver = OneHangProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, true, true], "@@ -1 +1 @@\n-old\n+new");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
    let approvals = AutoApproveGate;
    let sleeper = NoopSleeper;
    let router = router();
    let (tx, mut rx) = mpsc::unbounded_channel();
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
        PipelineConfig {
            test_command: Some("cargo test -p x".into()),
            diff_diagnostic: Some(DiagnosticInvocation::GitDiff),
            research_latency_ceiling: std::time::Duration::from_millis(200),
            ..PipelineConfig::default()
        },
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let outcome = pipeline
        .run(
            "Refactor the retry layer end to end",
            &mut messages,
            &mut budget,
        )
        .await
        .expect("run succeeds");
    let events = drain(&mut rx);

    assert_eq!(
        outcome.status,
        PipelineStatus::Completed,
        "a cancelled child degrades to a missing finding, never a wedged turn"
    );
    let started = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::SubAgent {
                    phase: stella_protocol::SubAgentPhase::Started { .. }
                }
            )
        })
        .count();
    let finished: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::SubAgent {
                phase:
                    stella_protocol::SubAgentPhase::Finished {
                        steps,
                        status,
                        reason,
                        ..
                    },
            } => Some((*steps, *status, reason.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(started, 1);
    assert_eq!(finished.len(), 1, "a balanced bracket: {events:?}");
    let (steps, status, reason) = &finished[0];
    assert_eq!(
        *steps, 1,
        "Finished.steps is the committed StepUsage count, not a forged zero"
    );
    assert_eq!(*status, stella_protocol::SubAgentStatus::Incomplete);
    assert!(
        reason.as_deref().unwrap_or_default().contains("cancelled"),
        "the close says why: {reason:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::UsageIncomplete {
                reason: stella_protocol::UsageIncompleteReason::Cancelled,
                ..
            }
        )),
        "the abandoned in-flight call owes its envelope: {events:?}"
    );
}

/// A resolver over the hanging double — [`OneProvider`] is typed to the
/// scripted one, and the harness needs exactly the same everything-is-this
/// behavior here.
struct OneHangProvider<'p>(&'p HangTailProvider);
impl ProviderResolver for OneHangProvider<'_> {
    fn provider_for(&self, _model: &ModelRef) -> Option<&dyn Provider> {
        Some(self.0)
    }
}

/// A research round that produces nothing usable — children answering empty —
/// degrades to exactly the no-research planner prompt: the stage may not
/// leave a half-empty section behind, and the turn still completes.
#[tokio::test]
async fn a_failed_research_round_degrades_to_the_no_research_prompt() {
    let (outcome, _events, with_failed_research) = research_scenario!(
        TRIAGE_WITH_QUESTIONS,
        vec![
            text_result(""),
            text_result(""),
            text_result(r#"["update retry.rs"]"#),
            text_result("PLAN COMPLETE: done."),
        ]
    );
    let (_outcome, _events, without_research) = research_scenario!(
        "CLASS: multi\nWITNESS: yes\nVERIFIER: yes",
        vec![
            text_result(r#"["update retry.rs"]"#),
            text_result("PLAN COMPLETE: done."),
        ]
    );

    assert_eq!(outcome.status, PipelineStatus::Completed);
    assert_eq!(
        planner_prompt(&with_failed_research),
        planner_prompt(&without_research),
        "zero findings must reproduce the pre-stage planner prompt byte for byte"
    );
}
