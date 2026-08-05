//! Witness for #1477: a "confident zero" — a turn that pokes at the
//! workspace read-only, then trails off on a short, unterminated line
//! standing in for a result — must not report `Completed`.
//!
//! On `main` (before the fix in `driver::confident_zero`) the run below
//! reaches `TurnOutcome::Completed`, indistinguishable from a genuine
//! answer. That is worse than a timeout: a timeout at least looks like a
//! failure, whereas this looks like a successful run right up until an
//! external grader scores it zero. The companion tests pin the two cases the
//! fix must NOT touch — a direct zero-tool-call short answer, and any turn
//! that actually changed something — per the issue's own constraint:
//! "the fix must not become a hard gate that refuses to finish legitimately
//! short tasks. Some tasks genuinely are three steps."

use super::*;

/// A `ToolExecutor` whose every declared tool is read-only — used to prove
/// that investigation alone (no matter how much of it) never produces an
/// "artifact" the confident-zero check would credit as real work.
struct ReadOnlyTools {
    calls: Arc<AtomicU32>,
}
#[async_trait]
impl ToolExecutor for ReadOnlyTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![
            ToolSchema {
                name: "read_file".into(),
                description: "read a file".into(),
                input_schema: serde_json::json!({"type": "object"}),
                read_only: true,
                speculation_safe: true,
            },
            ToolSchema {
                name: "glob".into(),
                description: "list files".into(),
                input_schema: serde_json::json!({"type": "object"}),
                read_only: true,
                speculation_safe: true,
            },
        ]
    }
    async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ToolOutput::Ok {
            content: format!("contents of {name}"),
        }
    }
}

/// THE reported shape (issue #1477's ArenaBench trace): two read-only tool
/// calls, then a short line with no terminal punctuation standing in for a
/// result. This must never reach `Completed`.
#[tokio::test]
async fn a_confident_zero_never_reports_as_completed() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(tool_call_result("c1", "read_file")),
            Ok(tool_call_result("c2", "glob")),
            Ok(text_result("I'm working with a checksum file")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = ReadOnlyTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("check the gcov coverage"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    match outcome {
        TurnOutcome::Aborted { reason, .. } => {
            assert!(
                reason.contains("abandoned attempt"),
                "reason should name the confident-zero pattern: {reason}"
            );
            assert!(
                reason.contains('2'),
                "reason should cite the tool-call tally: {reason}"
            );
        }
        other => panic!("a confident zero must not report Completed: {other:?}"),
    }
    // Never a silent abort — the caller sees why, same as every other abort
    // path in `dispatch_completion`.
    let events = drain_events(&mut rx);
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Error {
                retryable: true,
                ..
            }
        )),
        "confident-zero abort must surface a retryable error: {events:?}"
    );
}

/// Zero tool calls this turn must never trip the check, however short or
/// unterminated the answer — a task answerable without investigation at all
/// is an ordinary short completion, not an abstain.
#[tokio::test]
async fn a_direct_zero_tool_answer_still_completes() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Ok(text_result("4"))]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("what's 2+2"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "a direct answer with no investigation must not be gated: {outcome:?}"
    );
    drain_events(&mut rx);
}

/// A turn that actually changed the workspace completes normally even when
/// its closing line is a bare fragment — real work happened, whatever the
/// last line says. `CountingTools` (shared by other driver tests) declares
/// its one tool, `bash`, as NOT read-only.
#[tokio::test]
async fn a_turn_that_did_mutating_work_still_completes_despite_a_bare_closing_line() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(tool_call_result("c1", "bash")),
            Ok(text_result("done")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("run the build"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "a turn with real workspace-changing work must not be gated: {outcome:?}"
    );
    drain_events(&mut rx);
}

/// A properly terminated short answer following read-only investigation is
/// not a confident zero — only an UNTERMINATED line trips the check.
#[tokio::test]
async fn a_terminated_short_answer_after_investigation_still_completes() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(tool_call_result("c1", "read_file")),
            Ok(text_result("No, it does not exist.")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = ReadOnlyTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("does config.toml exist?"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "a stated, punctuated result must not be gated: {outcome:?}"
    );
    drain_events(&mut rx);
}
