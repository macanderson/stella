// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Witness for #2764: a turn that declares itself finished while a service it
//! started is still listening is told so, once, before the declaration
//! stands.
//!
//! `driver::live_services`' own unit tests pin the gate as a function over a
//! transcript. These drive real turns through `run_turn`, which is where the
//! contract actually lives: the executor is asked at the completing step, the
//! declaration and the question land on the caller's transcript in that
//! order, the model gets another step to answer, and a turn that stopped what
//! it started is left byte-identical.
//!
//! The failure this closes is the third bullet of #2666, and it is a failure
//! of *silence*: the two mechanical causes (children SIGKILLed on
//! `ProcessTable::drop`, `bash` hung on a backgrounded grandchild) were fixed
//! without anything ever asking the agent whether the service it declared
//! done was still reachable.

use super::*;
use crate::ports::LiveService;

/// A `ToolExecutor` reporting a fixed service roster, counting how many times
/// the engine asked. The count is the load-bearing half of the "peek, not
/// drain" contract: the port must be safe to ask, so the engine asking twice
/// (two completing steps in one turn) must not change the answer.
struct Serving {
    services: Vec<LiveService>,
    asks: Arc<AtomicU32>,
}

impl Serving {
    fn with(services: Vec<LiveService>) -> Self {
        Self {
            services,
            asks: Arc::new(AtomicU32::new(0)),
        }
    }
}

#[async_trait]
impl ToolExecutor for Serving {
    fn schemas(&self) -> Vec<ToolSchema> {
        vec![ToolSchema {
            name: "start_process".into(),
            description: "scripted".into(),
            input_schema: serde_json::json!({"type": "object"}),
            read_only: false,
            speculation_safe: false,
        }]
    }

    async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
        ToolOutput::Ok {
            content: "started proc-1".into(),
            data: None,
        }
    }

    fn live_services(&self) -> Vec<LiveService> {
        self.asks.fetch_add(1, Ordering::SeqCst);
        self.services.clone()
    }
}

fn service(handle: &str, name: Option<&str>) -> LiveService {
    LiveService {
        handle: handle.into(),
        name: name.map(str::to_string),
        display: "python3 -m http.server 8080".into(),
    }
}

fn start_call() -> CompletionResultAlias {
    CompletionResultAlias {
        upstream_provider: None,
        text: String::new(),
        tool_calls: vec![ToolCall {
            call_id: "s1".into(),
            name: "start_process".into(),
            input: serde_json::json!({ "argv": ["python3", "-m", "http.server", "8080"] }),
        }],
        usage: CompletionUsage::reported_zero(),
        model: "scripted".into(),
        cost_usd: 0.0001,
        finish_reason: None,
    }
}

/// One turn against `tools` with the given scripted completions.
async fn run(
    tools: &dyn ToolExecutor,
    script: Vec<CompletionResultAlias>,
) -> (TurnOutcome, Vec<CompletionMessage>, Vec<AgentEvent>) {
    let provider = ScriptedProvider {
        id: "scripted".into(),
        script: TokioMutex::new(script.into_iter().map(Ok).collect()),
        calls: Arc::new(AtomicU32::new(0)),
    };
    let sleeper = NoopSleeper;
    let engine = Engine::with_sleeper(&provider, tools, EngineConfig::default(), &sleeper);
    let mut messages = vec![
        CompletionMessage::system("sys"),
        CompletionMessage::user("serve the docs on 8080"),
    ];
    let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
    let (tx, mut rx) = mpsc::unbounded_channel();
    let outcome = engine.run_turn(&mut messages, &mut budget, &tx).await;
    drop(tx);
    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    (outcome, messages, events)
}

/// **The witness.** A turn starts a service, never stops it, and declares
/// done — and the engine sends it back once with the handle named. On `main`
/// the executor is never asked and the turn completes on the first
/// declaration, having said nothing about the process it left listening.
#[tokio::test]
async fn a_turn_declaring_done_with_a_service_up_is_asked_about_it() {
    let tools = Serving::with(vec![service("proc-1", Some("docs"))]);
    let (outcome, messages, events) = run(
        &tools,
        vec![
            start_call(),
            text_result("Done — the docs server is up on 8080."),
            text_result("Confirmed: GET / returns 200. Leaving it running."),
        ],
    )
    .await;

    assert!(
        matches!(outcome, TurnOutcome::Completed { ref text, .. } if text.contains("Confirmed")),
        "the turn continues past the question and completes on the answer: {outcome:?}"
    );
    let asked: Vec<&CompletionMessage> = messages
        .iter()
        .filter(|m| m.role == MessageRole::User && m.content.contains("still running"))
        .collect();
    assert_eq!(
        asked.len(),
        1,
        "exactly one assertion per turn: {:#?}",
        messages
    );
    assert!(
        asked[0].content.contains("proc-1") && asked[0].content.contains("docs"),
        "the handle and its label must both reach the model: {}",
        asked[0].content
    );
    // Claim then question: the model's own declaration has to be on the
    // transcript ahead of the challenge, or its answer attaches to nothing.
    let question = messages
        .iter()
        .position(|m| m.content.contains("still running"))
        .expect("the question is on the transcript");
    assert!(
        messages[question - 1].content.contains("docs server is up"),
        "the declaration must precede the question: {:#?}",
        messages
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::Text { text } if text.contains("still up at the end of this turn")
        )),
        "and the operator sees why the turn did not stop: {events:?}"
    );
}

/// The control: a turn that stopped what it started reports no live
/// services, so nothing is appended and nothing is emitted. The executor is
/// still asked — the engine cannot know the answer without asking — which is
/// why the port must be a peek.
#[tokio::test]
async fn a_turn_with_nothing_running_is_left_untouched() {
    let tools = Serving::with(Vec::new());
    let (outcome, messages, events) = run(
        &tools,
        vec![
            start_call(),
            text_result("Done — started it, checked it, stopped it."),
        ],
    )
    .await;

    assert!(
        matches!(outcome, TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert!(
        !messages.iter().any(|m| m.content.contains("still running")),
        "a clean turn's transcript must carry no assertion: {messages:#?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            AgentEvent::Text { text } if text.contains("still up at the end of this turn")
        )),
        "and no notice: {events:?}"
    );
    assert!(
        tools.asks.load(Ordering::SeqCst) >= 1,
        "the engine still has to ask — it cannot know without the port"
    );
}

/// The bound. The service stays up (the model confirmed it should), so the
/// executor reports it at the second declaration too — and the turn must
/// still complete rather than looping the question forever. This is the
/// `gate-ab` shape (#2663) the shared nudge window exists to prevent, on a
/// gate whose condition, unlike the prove-it gate's, the model may
/// legitimately choose not to clear.
#[tokio::test]
async fn a_confirmed_service_never_re_arms_the_question() {
    let tools = Serving::with(vec![service("proc-1", None)]);
    let (outcome, messages, _) = run(
        &tools,
        vec![
            start_call(),
            text_result("Done — the server is up."),
            text_result("Checked: it answers on 8080. It should stay up."),
        ],
    )
    .await;

    assert!(
        matches!(outcome, TurnOutcome::Completed { ref text, .. } if text.contains("stay up")),
        "confirming must END the turn, not re-arm the question: {outcome:?}"
    );
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.content.contains("still running"))
            .count(),
        1,
        "one assertion, whatever the answer: {messages:#?}"
    );
}
