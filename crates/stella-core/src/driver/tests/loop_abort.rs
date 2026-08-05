//! The steer→abort ladder across a *recovery*: the shape a real stuck turn
//! actually takes.
//!
//! `stuck_loop_steers_once_then_aborts_on_re_detection` pins the ladder when
//! the model ignores the warning immediately — three identical calls, then a
//! fourth. That is the easy half. The half seen in the field is the one where
//! the steer WORKS: the model breaks the streak, does one different thing, and
//! then falls into a second loop on a different tool. The turn's steer is
//! already spent at that point, so the very next detection is supposed to be
//! the kill.
//!
//! # Confident-zero tests (#1477) live in this file too
//!
//! The tests below `stuck_loop_...` and its siblings are unrelated to loop
//! detection — they cover `driver::confident_zero` (#1477): a turn that
//! investigates read-only and then trails off with no artifact must abort
//! rather than report `Completed`. They landed here, reusing this file's
//! `ConstantTools` as a read-only tool executor, rather than in their own
//! `driver/tests/confident_zero.rs` submodule, because both `driver.rs` and
//! `driver/tests.rs` sit exactly at their file-size ratchet ceiling (see
//! `scripts/file-size-baseline.txt`) — a new top-level test submodule costs
//! `driver/tests.rs` one `mod` line it has no room for, the same constraint
//! `driver::settlement`'s budget-check split and this PR's own
//! `driver::confident_zero` extraction out of `dispatch_completion` exist to
//! satisfy. Reusing an already-registered submodule is the same trade PR
//! #1504 made for `driver/tests/budget_boundaries.rs`.

use super::*;

/// A tool executor that answers with a constant per tool name, so two calls
/// with the same name AND the same input are byte-identical — the condition
/// `loop_detect` requires before it will call anything a loop. Real `grep` is
/// deterministic in exactly this way: same pattern, same path, unchanged tree,
/// same bytes out.
struct ConstantTools {
    calls: Arc<AtomicU32>,
}

#[async_trait]
impl ToolExecutor for ConstantTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![
            ToolSchema {
                name: "grep".into(),
                description: "search".into(),
                input_schema: serde_json::json!({"type": "object"}),
                read_only: true,
                speculation_safe: true,
            },
            ToolSchema {
                name: "read_file".into(),
                description: "read".into(),
                input_schema: serde_json::json!({"type": "object"}),
                read_only: true,
                speculation_safe: true,
            },
        ]
    }
    async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ToolOutput::Ok {
            content: match name {
                "grep" => "stella-tui/src/deck.rs:118: /// Cumulative prompt-cache".into(),
                other => format!("contents of {other}"),
            },
        }
    }
}

fn call_of(call_id: &str, name: &str, input: Value) -> CompletionResultAlias {
    CompletionResultAlias {
        text: String::new(),
        tool_calls: vec![ToolCall {
            call_id: call_id.into(),
            name: name.into(),
            input,
        }],
        usage: CompletionUsage::reported_zero(),
        model: "scripted".into(),
        cost_usd: 0.0001,
        finish_reason: None,
    }
}

fn grep_call() -> CompletionResultAlias {
    call_of(
        "c_grep",
        "grep",
        serde_json::json!({"pattern": "cache_write_tokens", "path": "stella-tui/src/deck.rs"}),
    )
}

/// THE field repro. The model loops, gets steered, actually course-corrects
/// with one different call — and then falls straight into a second loop. The
/// turn already spent its one warning, so the second loop's first detection
/// must abort rather than grind to the step cap.
#[tokio::test]
async fn a_loop_that_resumes_after_a_successful_course_correction_still_aborts() {
    // grep ×3 (detect + steer) → read_file (the course correction) → grep
    // forever (ScriptedProvider repeats its last entry).
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(grep_call()),
            Ok(grep_call()),
            Ok(grep_call()),
            Ok(call_of(
                "c_read",
                "read_file",
                serde_json::json!({"path": "stella-tui/src/deck.rs"}),
            )),
            Ok(grep_call()),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tool_calls = Arc::new(AtomicU32::new(0));
    let tools = ConstantTools {
        calls: tool_calls.clone(),
    };
    let sleeper = NoopSleeper;
    // A low cap so a broken ladder shows up as "ground to the cap" in a
    // second rather than 200 steps of grinding.
    let config = EngineConfig {
        max_steps: 30,
        ..EngineConfig::default()
    };
    let engine = Engine::with_sleeper(&provider, &tools, config, &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("why is the cache write count wrong?"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    let events = drain_events(&mut rx);
    let detections: Vec<bool> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::LoopDetected { aborted, .. } => Some(*aborted),
            _ => None,
        })
        .collect();

    assert!(
        matches!(outcome, TurnOutcome::Aborted { ref reason, .. } if reason.contains("stuck-loop")),
        "the second loop must abort the turn, got {outcome:?} after \
         {} tool calls and detections {detections:?}",
        tool_calls.load(Ordering::SeqCst),
    );
    assert!(
        tool_calls.load(Ordering::SeqCst) <= 8,
        "the second loop must die on its FIRST detection (3 + 1 + 3 calls), \
         not grind to the cap: {} calls, detections {detections:?}",
        tool_calls.load(Ordering::SeqCst),
    );
    assert_eq!(
        detections,
        vec![false, true],
        "one steer, then the kill — the spent warning is not re-earned"
    );
}

/// A provider that never repeats an argument: every call is `grep` with a
/// freshly mutated pattern, exactly as the field transcript showed (a regex
/// alternation the model kept reshuffling). The tool answers identically
/// every time, so not one of these calls learns anything.
struct MutatingGrepProvider {
    calls: Arc<AtomicU32>,
}

#[async_trait]
impl Provider for MutatingGrepProvider {
    fn id(&self) -> &str {
        "mutating-grep"
    }
    async fn complete_ref(
        &self,
        _req: CompletionRequestRef<'_>,
    ) -> Result<CompletionResultAlias, ProviderError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        // A different alternation every call — and, like the real provider
        // (Moonshot mints `<tool>:<n>`), a different call_id too.
        let pattern = (0..=n)
            .map(|i| format!("cache_write_{i}"))
            .collect::<Vec<_>>()
            .join("|");
        Ok(call_of(
            &format!("grep:{n}"),
            "grep",
            serde_json::json!({"pattern": pattern, "path": "stella-tui/src/deck.rs"}),
        ))
    }
}

/// THE bug this change exists for. Before stagnation detection, this turn
/// ran to the step cap: every `input` differed, so `detect_exact_repeat`
/// never matched two records, and the arguments never settled into a period
/// so `detect_short_cycle` never matched either — while every single call
/// returned the same bytes. On the live turn that produced this shape the
/// tool ran 38 times before an accidental exact triple finally tripped the
/// old detector.
#[tokio::test]
async fn a_tool_answering_identically_to_every_new_argument_is_steered_then_killed() {
    let provider = MutatingGrepProvider {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tool_calls = Arc::new(AtomicU32::new(0));
    let tools = ConstantTools {
        calls: tool_calls.clone(),
    };
    let sleeper = NoopSleeper;
    let config = EngineConfig {
        max_steps: 60,
        ..EngineConfig::default()
    };
    let engine = Engine::with_sleeper(&provider, &tools, config, &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("where is the cache write count summed?"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    let calls = tool_calls.load(Ordering::SeqCst);

    let events = drain_events(&mut rx);
    let detections: Vec<(String, usize, bool)> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::LoopDetected {
                kind,
                repeats,
                aborted,
                ..
            } => Some((kind.clone(), *repeats, *aborted)),
            _ => None,
        })
        .collect();

    assert!(
        matches!(outcome, TurnOutcome::Aborted { ref reason, .. } if reason.contains("stuck-loop")),
        "a tool that answers every new argument identically must end the turn, \
         got {outcome:?} after {calls} calls, detections {detections:?}"
    );
    assert_eq!(detections.len(), 2, "steer then kill: {detections:?}");
    assert_eq!(detections[0].0, "stagnation", "classified by the new rung");
    assert!(!detections[0].2, "first detection steers");
    assert!(detections[1].2, "second detection aborts");
    // The default threshold is 6, so the steer lands around the 7th call
    // (detection runs at the top of a step, on the PREVIOUS step's results)
    // and the kill one threshold later. The old detector needed 38.
    assert!(
        calls <= 16,
        "must die within a couple of thresholds, not grind to the cap: {calls} calls"
    );
}

/// THE reported shape (issue #1477's ArenaBench trace): two read-only tool
/// calls, then a short line with no terminal punctuation standing in for a
/// result. This must never reach `Completed`.
#[tokio::test]
async fn a_confident_zero_never_reports_as_completed() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(call_of(
                "c1",
                "read_file",
                serde_json::json!({"path": "a.rs"}),
            )),
            Ok(grep_call()),
            Ok(text_result("I'm working with a checksum file")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = ConstantTools {
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
/// last line says. `CountingTools` (shared by other driver tests, declared
/// in the parent `tests.rs`) declares its one tool, `bash`, as NOT
/// read-only.
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
/// not a confident zero — only an UNTERMINATED, orientation-shaped line
/// trips the check.
#[tokio::test]
async fn a_terminated_short_answer_after_investigation_still_completes() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(call_of(
                "c1",
                "read_file",
                serde_json::json!({"path": "config.toml"}),
            )),
            Ok(text_result("No, it does not exist.")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = ConstantTools {
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
