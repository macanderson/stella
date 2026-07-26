//! Guards on how much redundant work one step does.
//!
//! These assert *pass counts*, not wall clock. Each whole-transcript walk and
//! each transcript-wide hash is Θ(history), so on a long turn the difference
//! between two of them per step and four is the difference between two and four
//! full re-reads of everything the model has seen — a cost that grows with the
//! turn while the per-pass constant stays flat. Shaving the constant inside a
//! pass cannot fix that; removing the pass can. A counter is the only way to
//! keep it removed.

use super::*;

/// How many times a single step re-reads the whole transcript to estimate it.
///
/// Four walks per step was the state of things: `compact`'s `before_tokens`, the
/// `else`-arm re-estimate that recovered the number `compact` had just discarded,
/// `run_model_call`'s pre-call estimate, and `emit_step_receipt` recomputing that
/// same pre-call estimate from the same unmutated slice. Two of the four were
/// pure recomputation of a value already in hand.
///
/// Each walk is Θ(transcript), so this is not a micro-cost: on a 200-step turn it
/// is the difference between two and four full re-reads of the entire history
/// before every model call. The remaining two are the honest ones — compaction
/// must measure before deciding, and the step must know its input size — and this
/// asserts the count so a future refactor cannot quietly reintroduce a third.
#[tokio::test]
async fn a_step_walks_the_transcript_to_estimate_it_at_most_twice() {
    const STEPS: usize = 3;
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(tool_call_result("call_1", "bash")),
            Ok(tool_call_result("call_2", "bash")),
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
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let _ = crate::estimator::take_conversation_walks();
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    let walks = crate::estimator::take_conversation_walks();
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));

    // The fixture stays far under budget, so no compaction pass fires and no
    // overflow summary runs — every walk counted here is on the plain step path.
    let compactions = drain_events(&mut rx)
        .iter()
        .filter(|e| matches!(e, AgentEvent::Compaction { .. }))
        .count();
    assert_eq!(compactions, 0, "fixture must not compact");
    assert!(
        walks <= 2 * STEPS,
        "{STEPS} steps performed {walks} whole-transcript estimate walks; at most \
         two per step are load-bearing (compaction's measurement and the step's \
         own input size). The other two were recomputations of a number the \
         caller already had."
    );
}

/// The receipt's estimate and the usage record's estimate must be the same
/// number for the same step.
///
/// They used to be produced by two independent `estimate_conversation_tokens`
/// walks over the same unmutated slice, one in the driver and one inside
/// `ReceiptLedger::emit_step_receipt` — agreeing only because both happened to
/// call the same function on the same input. The driver now passes its value in,
/// so agreement is structural; this pins it, because a future refactor that
/// re-derives the manifest's estimate from something else (post-compaction
/// messages, a calibrated figure) would silently desynchronize the pair that
/// `StepUsage`'s drift sampling compares.
#[tokio::test]
async fn the_receipt_and_the_usage_record_report_one_estimate_per_step() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(tool_call_result("call_1", "bash")),
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
        CompletionMessage::user("hi"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }));

    // Pair them by step so a missing or extra event fails rather than lines up
    // by accident.
    let mut manifest: HashMap<usize, u64> = HashMap::new();
    let mut usage: HashMap<usize, u64> = HashMap::new();
    for event in drain_events(&mut rx) {
        match event {
            AgentEvent::StepManifest {
                step,
                estimated_input_tokens,
                ..
            } => {
                manifest.insert(step, estimated_input_tokens);
            }
            AgentEvent::StepUsage {
                step,
                estimated_input_tokens,
                ..
            } => {
                usage.insert(step, estimated_input_tokens);
            }
            _ => {}
        }
    }
    assert_eq!(manifest.len(), 2, "one manifest per committed step");
    assert_eq!(
        manifest, usage,
        "the manifest's estimate and the usage record's estimate are the same \
         measurement of the same step and must not drift apart"
    );
    assert!(manifest.values().all(|&e| e > 0));
}
