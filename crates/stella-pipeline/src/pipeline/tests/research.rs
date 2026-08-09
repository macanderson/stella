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

/// A provider that records the `effort` of every request beside its text.
///
/// [`ScriptedProvider`] records messages only, which is enough for every other
/// question this file asks and not enough for the one below: `agents.research`
/// is a claim about what goes ON THE WIRE, and a knob that resolves correctly
/// in `stella-cli` and never reaches a request is exactly the "parses but
/// wires nothing" failure. Local to this file rather than folded into
/// `ScriptedProvider` because that fixture's module sits at its file-size
/// ceiling.
struct EffortRecordingProvider {
    script: TokioMutex<VecDeque<CompletionResult>>,
    calls: std::sync::Mutex<Vec<(Option<stella_protocol::ReasoningEffort>, String)>>,
}

impl EffortRecordingProvider {
    fn new(results: Vec<CompletionResult>) -> Self {
        Self {
            script: TokioMutex::new(results.into_iter().collect()),
            calls: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// The effort of every call whose prompt contains `needle`.
    fn efforts_for(&self, needle: &str) -> Vec<Option<stella_protocol::ReasoningEffort>> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, text)| text.contains(needle))
            .map(|(effort, _)| *effort)
            .collect()
    }
}

#[async_trait]
impl Provider for EffortRecordingProvider {
    fn id(&self) -> &str {
        "effort-recording"
    }
    async fn complete_ref(
        &self,
        req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResult, ProviderError> {
        self.calls.lock().unwrap().push((
            req.effort,
            req.messages
                .iter()
                .map(|m| m.content.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        let mut q = self.script.lock().await;
        q.pop_front()
            .ok_or_else(|| ProviderError::Terminal("scripted provider exhausted".into()))
    }
}

struct OneEffortProvider<'p>(&'p EffortRecordingProvider);
impl ProviderResolver for OneEffortProvider<'_> {
    fn provider_for(&self, _model: &ModelRef) -> Option<&dyn Provider> {
        Some(self.0)
    }
}

/// **The pipeline half of #2374's witness.** `agents.research` must reach the
/// research children's requests, and must not disturb the worker's.
///
/// The reason this needs its own test rather than riding the CLI's: a research
/// child is an engine SUB-AGENT turn, not a raw call, so it never passes
/// through `metered_raw_call` where every other role's overrides are applied.
/// Its shaping has to be written onto the `EngineConfig` the stage builds, and
/// nothing but a recorded request proves that happened.
#[tokio::test]
async fn the_research_row_reaches_the_childrens_requests_and_not_the_workers() {
    let provider = EffortRecordingProvider::new(vec![
        text_result(TRIAGE_WITH_QUESTIONS),
        text_result("driver.rs owns retries."),
        text_result("The retry tests live in driver/tests.rs."),
        text_result(r#"["update retry.rs"]"#),
        text_result("PLAN COMPLETE: retry layer refactored."),
    ]);
    let resolver = OneEffortProvider(&provider);
    let runner = ScriptedRunner::new(vec![false, true, true], "@@ -1 +1 @@\n-old\n+new");
    let tools = EmptyTools;
    let recall = NoContextRecall;
    let repo = NoRepoStructure;
    let repo_status = NoRepoStatus;
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
            // The worker's own effort rides `engine`, which is what the CLI
            // builds from `agents.worker` — so this pair is the posture the
            // `fix-git` trace showed, with research turned down.
            engine: EngineConfig {
                effort: Some(stella_protocol::ReasoningEffort::Xhigh),
                ..EngineConfig::default()
            },
            role_overrides: PipelineRoleOverrides {
                research: RoleCallOverrides {
                    effort: Some(stella_protocol::ReasoningEffort::Low),
                    ..RoleCallOverrides::default()
                },
                ..PipelineRoleOverrides::default()
            },
            ..PipelineConfig::default()
        },
    );
    let mut messages = vec![CompletionMessage::system("sys")];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    pipeline
        .run(
            "Refactor the retry layer end to end",
            &mut messages,
            &mut budget,
        )
        .await
        .expect("run succeeds");
    drop(pipeline);

    // Matched on the child's own system prompt, not on the question text: the
    // planner and worker messages quote the question back inside their
    // findings section, so a needle of "Which module owns retries?" also
    // catches two calls that are emphatically not research children.
    let research = provider.efforts_for("You are a read-only research agent");
    assert!(
        !research.is_empty(),
        "a research child must have been called"
    );
    assert!(
        research
            .iter()
            .all(|e| *e == Some(stella_protocol::ReasoningEffort::Low)),
        "every research child must carry the pinned effort, got {research:?}"
    );

    let worker = provider.efforts_for("## Plan (JSON array of step strings)");
    assert!(
        worker
            .iter()
            .all(|e| *e == Some(stella_protocol::ReasoningEffort::Xhigh)),
        "turning research down must leave every other role where it was, got {worker:?}"
    );
}

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

/// The worker's own first user message, from the recorded per-call shapes:
/// the one carrying the `## Task` heading `assemble_user_message` writes.
fn worker_user_message(shapes: &[Vec<(stella_protocol::MessageRole, String)>]) -> String {
    shapes
        .iter()
        .flatten()
        .filter(|(role, _)| *role == stella_protocol::MessageRole::User)
        .map(|(_, text)| text.as_str())
        .find(|text| text.contains("## Task"))
        .expect("a worker turn was dispatched")
        .to_string()
}

/// #2415's witness. Findings reached the planner and no further: the worker
/// saw them only as whatever residue the planner encoded into a step string.
/// They now ride the worker's own user message, as their own section, with
/// the question that grounds each one.
#[tokio::test]
async fn the_worker_user_message_carries_the_research_findings() {
    let (_outcome, _events, shapes) = research_scenario!(
        TRIAGE_WITH_QUESTIONS,
        vec![
            text_result("ANSWER-ONE: driver.rs owns retries."),
            text_result("ANSWER-TWO: tests live in driver/tests.rs."),
            // A plan whose steps mention neither answer — so the assertion
            // cannot pass on the planner's residue leaking through.
            text_result(r#"["do it"]"#),
            text_result("PLAN COMPLETE: done."),
        ]
    );

    let worker = worker_user_message(&shapes);
    assert!(
        worker.contains("## Research findings"),
        "the findings are their own section in the worker's message: {worker}"
    );
    assert!(worker.contains("ANSWER-ONE"), "{worker}");
    assert!(worker.contains("ANSWER-TWO"), "{worker}");
    assert!(
        worker.contains("Which module owns retries?"),
        "each finding is grounded by its question: {worker}"
    );
    let findings_at = worker.find("## Research findings").unwrap();
    let task_at = worker.find("## Task").unwrap();
    assert!(
        findings_at < task_at,
        "grounding rides before the goal, as recall does: {worker}"
    );
}

/// The advisory contract, in its strongest form: with no findings the
/// worker's message is byte-for-byte what it was before the second sink
/// existed. Compared against the function's own output for the same inputs
/// rather than a transcribed literal, so it stays true as the message grows.
#[tokio::test]
async fn no_findings_leaves_the_worker_message_byte_identical() {
    let (_outcome, _events, shapes) = research_scenario!(
        "CLASS: multi\nWITNESS: yes\nVERIFIER: yes",
        vec![
            text_result(r#"["update retry.rs"]"#),
            text_result("PLAN COMPLETE: done."),
        ]
    );

    let worker = worker_user_message(&shapes);
    assert!(!worker.contains("## Research findings"));
    assert_eq!(
        worker,
        assemble_user_message(
            "Refactor the retry layer end to end",
            &[],
            &[],
            VerificationContract::Oracle("cargo test -p x"),
        ),
        "an empty findings list must not change a single byte"
    );
}
