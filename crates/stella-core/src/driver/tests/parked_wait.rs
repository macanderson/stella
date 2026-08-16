//! Witness for #1471: waiting on external state costs O(1) model steps.
//!
//! Before parked waits, a monitoring turn polled as model behavior — one
//! full model round-trip per poll on a growing transcript, and any long
//! in-tool sleep aged the provider prompt cache to nothing. These tests pin
//! the replacement contract end-to-end through `run_turn`: a condition that
//! changes on the Nth engine-side probe re-invokes the model exactly once,
//! the poll history never enters the transcript, and the wake rides the
//! volatile tail as a marked user message.

use super::*;
use crate::waiting::{WAKE_MARKER, WaitCall, WaitRequest};

/// A `ToolExecutor` whose `ci_status` deposits a parked-wait request on
/// `wait=true`, answers `pending` until the `change_on`th probe replay, and
/// serves a recognizable fresh status to the wake call — the tool half of
/// the `ci.rs` composition, scripted.
struct ParkingTools {
    deposit: std::sync::Mutex<Option<WaitRequest>>,
    probe_calls: Arc<AtomicU32>,
    change_on: u32,
    wake_calls: Arc<AtomicU32>,
}

impl ParkingTools {
    fn depositing(request: WaitRequest, change_on: u32) -> Self {
        Self {
            deposit: std::sync::Mutex::new(Some(request)),
            probe_calls: Arc::new(AtomicU32::new(0)),
            change_on,
            wake_calls: Arc::new(AtomicU32::new(0)),
        }
    }
}

#[async_trait]
impl ToolExecutor for ParkingTools {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "ci_status".into(),
            description: "scripted CI".into(),
            input_schema: serde_json::json!({"type": "object"}),
            read_only: true,
            speculation_safe: false,
        }]
    }

    async fn execute(&self, _name: &str, input: &Value) -> ToolOutput {
        if input.get("probe").and_then(Value::as_bool).unwrap_or(false) {
            let n = self.probe_calls.fetch_add(1, Ordering::SeqCst) + 1;
            return ToolOutput::Ok {
                content: if n >= self.change_on {
                    "settled".into()
                } else {
                    "pending".into()
                },
                data: None,
            };
        }
        if input.get("wait").and_then(Value::as_bool).unwrap_or(false) {
            return ToolOutput::Ok {
                content: "2 runs in progress — parking".into(),
                data: None,
            };
        }
        self.wake_calls.fetch_add(1, Ordering::SeqCst);
        ToolOutput::Ok {
            content: "fresh status: all green".into(),
            data: None,
        }
    }

    fn drain_wait_request(&self) -> Option<WaitRequest> {
        self.deposit.lock().unwrap().take()
    }
}

fn ci_wait_request(timeout_secs: u64) -> WaitRequest {
    WaitRequest {
        description: "CI for branch main settles".into(),
        probe: WaitCall {
            name: "ci_status".into(),
            input: serde_json::json!({ "probe": true, "branch": "main" }),
        },
        baseline: "pending".into(),
        on_wake: Some(WaitCall {
            name: "ci_status".into(),
            input: serde_json::json!({ "branch": "main" }),
        }),
        poll_interval_secs: 5,
        timeout_secs,
    }
}

fn ci_wait_call() -> CompletionResultAlias {
    CompletionResultAlias {
        upstream_provider: None,
        text: String::new(),
        tool_calls: vec![ToolCall {
            call_id: "w1".into(),
            name: "ci_status".into(),
            input: serde_json::json!({ "branch": "main", "wait": true }),
        }],
        usage: CompletionUsage::reported_zero(),
        model: "scripted".into(),
        cost_usd: 0.0001,
        finish_reason: None,
    }
}

/// The witness: the watched state changes on the THIRD engine-side probe,
/// and the model is re-invoked exactly once — not three times — with the
/// wake delta on the transcript tail and zero poll debris anywhere in it.
#[tokio::test]
async fn a_change_on_the_nth_probe_re_invokes_the_model_exactly_once() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(ci_wait_call()),
            Ok(text_result("CI settled — the fix is green, done")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = ParkingTools::depositing(ci_wait_request(600), 3);
    let probe_calls = tools.probe_calls.clone();
    let wake_calls = tools.wake_calls.clone();
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("monitor CI and report"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(
        matches!(outcome, TurnOutcome::Completed { ref text, .. } if text.contains("green")),
        "got {outcome:?}"
    );
    // THE contract (#1471): one model call requested the wait, one answered
    // the wake. Three probes happened, and none of them was a model step.
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        2,
        "a change on the 3rd probe must cost exactly one re-invocation"
    );
    assert_eq!(probe_calls.load(Ordering::SeqCst), 3);
    assert_eq!(wake_calls.load(Ordering::SeqCst), 1, "one wake replay");

    // The wake rides the tail as a marked user message carrying the fresh
    // status — and the probes' outputs never entered the transcript.
    let wake_idx = messages
        .iter()
        .position(|m| m.role == MessageRole::User && m.content.starts_with(WAKE_MARKER))
        .expect("the wake message must enter the conversation");
    assert!(
        messages[wake_idx]
            .content
            .contains("fresh status: all green"),
        "the wake carries the on_wake output: {}",
        messages[wake_idx].content
    );
    assert!(
        messages
            .iter()
            .rposition(|m| m.role == MessageRole::Assistant)
            .expect("final reply")
            > wake_idx,
        "the model call that answers the wake must observe it"
    );
    // Probe outputs are the verbatim words `pending`/`settled`; neither may
    // surface anywhere — not as a message, not as a tool result.
    let polls_in_transcript = messages
        .iter()
        .flat_map(|m| {
            std::iter::once(m.content.trim().to_string()).chain(m.tool_results.iter().map(|r| {
                match &r.output {
                    ToolOutput::Ok { content, .. } => content.trim().to_string(),
                    ToolOutput::Error { message, .. } => message.trim().to_string(),
                }
            }))
        })
        .filter(|content| content == "pending" || content == "settled")
        .count();
    assert_eq!(
        polls_in_transcript, 0,
        "poll observations must never reach the transcript"
    );
    assert_tool_pairing(&messages);

    // The stream carries the typed span (#1857): a `TurnParked` naming the
    // wait's parameters and a `TurnWoken` accounting for the probes — never
    // prose `Text` a consumer would have to distinguish from model narration.
    let events = drain_events(&mut rx);
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::TurnParked {
                description,
                poll_interval_secs: 5,
                deadline_secs: 600,
            } if description == "CI for branch main settles"
        )),
        "the park must be announced as a typed TurnParked: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::TurnWoken { reason, polls_used: 3 } if reason == "changed"
        )),
        "the wake must be announced as a typed TurnWoken: {events:?}"
    );
    // The park narrates through the typed pair alone — no synthetic prose.
    assert!(
        !events.iter().any(
            |e| matches!(e, AgentEvent::Text { text } if text.contains("Parked")
                || text.contains("Wait ended"))
        ),
        "the ⏳/▶ prose deltas must not accompany the typed events"
    );
}

/// A condition that never changes wakes the model once with the timeout
/// marked — never N poll-steps, and never a silent hang past the deadline.
#[tokio::test]
async fn an_unchanged_condition_wakes_once_at_the_deadline() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(ci_wait_call()),
            Ok(text_result("CI is stuck; escalating instead of waiting")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    // 20s deadline at the 5s interval floor: exactly 4 afforded probes.
    let tools = ParkingTools::depositing(ci_wait_request(20), u32::MAX);
    let probe_calls = tools.probe_calls.clone();
    let wake_calls = tools.wake_calls.clone();
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("monitor CI"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    assert_eq!(probe_calls.load(Ordering::SeqCst), 4, "deadline / interval");
    // The typed wake names the timeout, not a phantom change (#1857).
    assert!(
        drain_events(&mut rx).iter().any(|e| matches!(
            e,
            AgentEvent::TurnWoken { reason, polls_used: 4 } if reason == "deadline_expired"
        )),
        "the deadline wake must be a typed TurnWoken with the spent polls"
    );
    assert_eq!(
        wake_calls.load(Ordering::SeqCst),
        0,
        "no change, no wake replay — the model re-queries if it wants to"
    );
    let wake = messages
        .iter()
        .find(|m| m.role == MessageRole::User && m.content.starts_with(WAKE_MARKER))
        .expect("the timeout wake must enter the conversation");
    assert!(
        wake.content.contains("timed out"),
        "the model must see the deadline, not a phantom change: {}",
        wake.content
    );
}

/// A request whose replayed calls are not read-only is refused outright —
/// the engine must never mutate on a timer the model cannot see.
#[tokio::test]
async fn a_non_read_only_probe_is_refused_not_replayed() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Ok(ci_wait_call()),
            Ok(text_result("done without parking")),
        ]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let mut request = ci_wait_request(600);
    request.probe.name = "bash".into(); // not in the read-only schema set
    let tools = ParkingTools::depositing(request, 1);
    let probe_calls = tools.probe_calls.clone();
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("monitor CI"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(probe_calls.load(Ordering::SeqCst), 0, "never replayed");
    assert!(
        !messages.iter().any(|m| m.content.starts_with(WAKE_MARKER)),
        "a refused park must not fabricate a wake"
    );
    let refused = drain_events(&mut rx).into_iter().any(
        |e| matches!(&e, AgentEvent::Text { text: delta } if delta.contains("not a read-only tool")),
    );
    assert!(refused, "the refusal must be visible on the stream");
}

// ---- rate-limit parks (#2677 / #2667): abort becomes recovery ------------

/// Errors shaped like a provider brownout: HTTP 429 with no `Retry-After`.
fn throttled() -> ProviderError {
    ProviderError::RateLimited {
        message: "throttled".into(),
        retry_after_ms: None,
    }
}

/// The witness for #2667's measured loss (7 attempts burned in 15s with 880s
/// of budget left): sustained hint-less rate limiting that exhausts the
/// inline ladder now parks — bounded by wall clock, narrated on the stream —
/// and the step completes once the provider recovers. On the pre-#2677
/// ladder this run dies at the seventh attempt with `RetriesExhausted`.
#[tokio::test]
async fn sustained_rate_limiting_parks_within_budget_and_recovers() {
    // Nine 429s: six absorbed by the inline ladder, three more that only a
    // park survives. The tenth call succeeds.
    let mut script: Vec<Result<CompletionResultAlias, ProviderError>> =
        (0..9).map(|_| Err(throttled())).collect();
    script.push(Ok(text_result("recovered — done")));
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(script),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("do the task"),
    ];
    // No task deadline: the park allowance is the absolute six-hour cap.
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(
        matches!(outcome, TurnOutcome::Completed { ref text, .. } if text.contains("recovered")),
        "sustained 429s with budget left must recover, not abort: {outcome:?}"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        10,
        "every scripted attempt must have been dispatched"
    );
    let events = drain_events(&mut rx);
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::TurnParked { description, .. } if description.contains("rate limited")
        )),
        "the park must open a typed span naming the rate limit"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::TurnWoken { reason, .. } if reason == "retry_window_elapsed"
        )),
        "the elapsed park must close its span"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, AgentEvent::RetriesExhausted { .. })),
        "a recovered brownout is not an exhaustion"
    );
}

/// The witness for #2677's headline defect: a `Retry-After` past the 120s
/// inline ceiling used to fail the call fast as `Terminal` even with hours
/// of wall clock left. With headroom it is now honored as a parked wait.
#[tokio::test]
async fn a_long_retry_after_hint_is_honored_by_parking_when_budget_allows() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![
            Err(ProviderError::RateLimited {
                message: "hard limit".into(),
                retry_after_ms: Some(300_000), // 5 minutes — past the 120s inline ceiling
            }),
            Ok(text_result("after the stated wait — done")),
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
        CompletionMessage::user("do the task"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(
        matches!(outcome, TurnOutcome::Completed { ref text, .. } if text.contains("done")),
        "a stated wait that fits the budget must be honored: {outcome:?}"
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    let events = drain_events(&mut rx);
    let parked = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::TurnParked { deadline_secs, .. } => Some(*deadline_secs),
            _ => None,
        })
        .expect("the honored hint must park, not sleep silently");
    // The server asked for 300s; the park is that plus at most the additive
    // hint jitter (an eighth), never less than asked.
    assert!(
        (300..=338).contains(&parked),
        "park length must derive from the server hint: {parked}s"
    );
}

/// The fail-fast half the issue keeps: a stated wait that cannot fit inside
/// the task deadline's remaining headroom still fails fast — waiting less
/// than the server asked guarantees a re-429, so the wait would spend the
/// budget and buy nothing. The park machinery must not engage.
#[tokio::test]
async fn a_hint_past_the_remaining_deadline_still_fails_fast_without_parking() {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(vec![Err(ProviderError::RateLimited {
            message: "hard limit".into(),
            retry_after_ms: Some(300_000),
        })]),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let tools = CountingTools {
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, &tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("do the task"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    // 60s of deadline left: after the 30s wake reserve, a 300s stated wait
    // can never fit.
    budget.set_task_deadline(Some(
        std::time::Instant::now() + std::time::Duration::from_secs(60),
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();

    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;

    assert!(
        matches!(outcome, TurnOutcome::Aborted { ref reason, .. } if reason.contains("failing fast")),
        "an unaffordable stated wait must abort cleanly: {outcome:?}"
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        1,
        "must fail on the first attempt, not retry into the stated window"
    );
    assert!(
        !drain_events(&mut rx)
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnParked { .. })),
        "no park may open for a wait that cannot fit"
    );
}
