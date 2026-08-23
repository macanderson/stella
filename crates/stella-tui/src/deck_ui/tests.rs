//! Key-handling and ingest witness tests for the Command Deck UI.
//!
//! Split out of `deck_ui.rs` (#458): the module had grown to 6,884 lines, of
//! which ~2,960 were these tests. Shared fixtures live here; each tab or
//! overlay with its own fixtures owns a submodule below.

#![allow(clippy::field_reassign_with_default)]

use super::*;
use crate::envelope::AgentMeta;
use stella_protocol::AgentEvent;

mod agents;
mod composer;
mod esc;
mod focus;
mod gates;
mod graph;
mod help;
mod issues;
mod list_vocabulary;
mod queue;
mod routing_card;
mod selection;
mod sessions;
mod skills;
mod splash;
mod tabs;
mod traces;
mod transcript_nav;

/// A model whose lead already has `prompts` queued, for the queue-editor and
/// dispatch tests. Shared: both this module and `queue` build on it.
fn model_with_queue(prompts: &[&str]) -> WorkspaceModel {
    let mut m = model_with(&["lead"]);
    for (i, p) in prompts.iter().enumerate() {
        m.queue.enqueue((*p).to_string(), i as u64);
    }
    m
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn ch(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn model_with(ids: &[&str]) -> WorkspaceModel {
    let mut m = WorkspaceModel::new();
    for id in ids {
        m.apply_inbound(&Inbound::Register(AgentMeta::new(*id, *id, 0)));
    }
    m
}

/// Push one tool call + multi-line result onto `agent`'s transcript.
fn with_tool_exchange(m: &mut WorkspaceModel, agent: &str) {
    use stella_protocol::{AgentEvent, ToolCall, ToolOutput};
    m.apply_inbound(&Inbound::Event {
        agent: agent.into(),
        event: AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "c1".into(),
                name: "read_file".into(),
                input: serde_json::json!({ "path": "src/main.rs" }),
            },
        },
    });
    m.apply_inbound(&Inbound::Event {
        agent: agent.into(),
        event: AgentEvent::ToolResult {
            call_id: "c1".into(),
            output: ToolOutput::Ok {
                content: "line one\nline two\nline three".into(),
                data: None,
            },
            duration_ms: 7,
            speculated: false,
        },
    });
}

fn ready_ui() -> DeckUi {
    let mut ui = DeckUi::default();
    ui.splash.skip(); // past the splash for interaction tests
    ui
}
