//! What `spawn_renderer` returns once the turn's stream closes.
//!
//! `RendererOutcome::events` is the turn's journal. `run_turn` folds it into a
//! `TurnFriction`. Reflection reads those counts to decide if the turn is
//! worth a model call. A journal the renderer drops is not a smaller answer.
//! It is a wrong one: the turn reads as one that hit nothing.

use crate::agent::persistence::*;

/// One announced tool call and the error that closed it — the shape the
/// friction fold counts.
fn failed_call() -> Vec<AgentEvent> {
    vec![
        AgentEvent::ToolStart {
            call: stella_protocol::ToolCall {
                call_id: "c1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "missing.rs"}),
            },
            sub_agent_id: None,
            task_id: None,
        },
        AgentEvent::ToolResult {
            call_id: "c1".into(),
            output: stella_protocol::ToolOutput::error("no such file"),
            duration_ms: 7,
            speculated: false,
            sub_agent_id: None,
            task_id: None,
        },
    ]
}

/// Drive the renderer over `events` in `format` and hand back what it kept.
async fn collected(format: OutputFormat, events: Vec<AgentEvent>) -> Vec<AgentEvent> {
    let (tx, rx) = mpsc::unbounded_channel();
    let renderer = spawn_renderer(rx, format, None, "anthropic".into(), false, None);
    for event in events {
        tx.send(event).expect("the renderer is live");
    }
    drop(tx);
    renderer.await.expect("renderer").events
}

/// **The witness.** Every format keeps the turn's journal.
///
/// On base the `Text` and `StreamJson` arms kept none of it. Every fold over
/// the journal then counted zero.
#[tokio::test]
async fn every_format_keeps_the_turns_journal() {
    for format in [
        OutputFormat::Text,
        OutputFormat::Json,
        OutputFormat::StreamJson,
    ] {
        let kept = collected(format, failed_call()).await;
        let friction = crate::memory::TurnFriction::from_events(&kept);
        assert_eq!(
            friction.counts().tool_errors,
            1,
            "{format:?}: the fold must see the failed call the turn made, \
             because the reflection gate decides on nothing else"
        );
    }
}
