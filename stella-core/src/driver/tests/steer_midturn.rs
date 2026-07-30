//! Mid-turn steering across a tool-call boundary. The existing witness
//! (`steered_messages_inject_before_the_next_model_call`) steers a turn that
//! never calls a tool, so it only proves the append happens. The interesting
//! case is the one the deck actually produces: the user types `>` while a tool
//! round is in flight, so the steer lands on a transcript whose tail is a
//! `Tool` message answering an assistant `tool_use`.
//!
//! Two invariants have to hold there. Every tool result must still be paired
//! with the call that produced it (an orphaned `tool_use` is a hard provider
//! 400 that fails the whole turn), and the steer must be positioned so the
//! next model call actually observes it.

use super::*;

/// A [`crate::ports::TurnSteering`] that withholds its queue until the Nth
/// drain, so the steer lands at a chosen step boundary rather than the first.
struct SteerAtDrain {
    queue: std::sync::Mutex<Vec<String>>,
    fire_on_drain: u32,
    drains: AtomicU32,
}

impl crate::ports::TurnSteering for SteerAtDrain {
    fn drain_steering(&self) -> Vec<String> {
        let n = self.drains.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.fire_on_drain {
            std::mem::take(&mut *self.queue.lock().unwrap())
        } else {
            Vec::new()
        }
    }
    fn soft_stop_requested(&self) -> bool {
        false
    }
}

#[tokio::test]
async fn a_steer_after_a_tool_round_keeps_every_call_paired_with_its_result() {
    // Step 0 calls a tool; step 1's boundary is where the steer lands, so the
    // transcript it appends to ends with the Tool message from step 0.
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(tool_call_result("c1", "bash")),
            Ok(text_result("done, and I read the tests too")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let steering = SteerAtDrain {
        queue: std::sync::Mutex::new(vec!["also check the tests".into()]),
        fire_on_drain: 2,
        drains: AtomicU32::new(0),
    };
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_steering(&steering);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("the task"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "a steer arriving mid-tool-round must not fail the turn: {outcome:?}"
    );

    // The invariant a provider enforces with a 400: no tool result may precede
    // the call it answers, and no call may be left unanswered.
    assert_tool_pairing(&messages);

    let steer_idx = messages
        .iter()
        .position(|m| m.role == MessageRole::User && m.content == "also check the tests")
        .expect("the steer must enter the conversation");
    let tool_idx = messages
        .iter()
        .position(|m| m.role == MessageRole::Tool)
        .expect("step 0's tool result");
    assert!(
        tool_idx < steer_idx,
        "the steer must land AFTER the tool result it interrupted, never between \
         the assistant tool_use and its answer — that orphans the call"
    );

    // And the model must actually have seen it: the reply that answers the
    // steer is committed after it.
    let last_assistant = messages
        .iter()
        .rposition(|m| m.role == MessageRole::Assistant)
        .expect("final assistant reply");
    assert!(
        steer_idx < last_assistant,
        "the steer must precede the model call that answers it"
    );
}

/// The shape the steer actually produces on the wire: two consecutive
/// user-role turns, because Stella renders a `Tool` message as a user turn
/// carrying `tool_result` blocks. This is legal on the Anthropic Messages API
/// (consecutive same-role turns are combined), and this witness pins the
/// transcript shape so a provider adapter that cannot tolerate it is caught
/// here rather than at runtime.
#[tokio::test]
async fn a_mid_tool_round_steer_leaves_a_tool_message_immediately_before_it() {
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
    let steering = SteerAtDrain {
        queue: std::sync::Mutex::new(vec!["actually, stop after this".into()]),
        fire_on_drain: 2,
        drains: AtomicU32::new(0),
    };
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper)
        .with_steering(&steering);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("the task"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, _rx) = mpsc::unbounded_channel();

    let _ = engine.run_turn(&mut messages, &mut budget, &tx).await;

    let steer_idx = messages
        .iter()
        .position(|m| m.role == MessageRole::User && m.content == "actually, stop after this")
        .expect("the steer must enter the conversation");
    assert_eq!(
        messages[steer_idx - 1].role,
        MessageRole::Tool,
        "the steer lands directly on the tool answer, so the rendered request \
         carries two consecutive user turns: {:?}",
        messages.iter().map(|m| m.role).collect::<Vec<_>>()
    );
}
