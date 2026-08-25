// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A delegate child's tool call, badged on both rows it draws (#4699).
//!
//! A submodule of `deck_render_snapshots` rather than more lines in it: the
//! parent had already reached the 1500-line ceiling, so a new golden goes
//! here instead of pushing it over.
//!
//! Every helper comes from the parent, so the golden lives in one directory
//! and is blessed by one command:
//! `BLESS=1 cargo test -p stella-tui --test deck_render_snapshots`.

use super::*;

/// Before this, `TranscriptEntry` carried no `sub_agent_id` at all, so a
/// delegate's call rendered identically to the lead's own — the deck was the
/// one surface where a reader could not tell who made a call, though the
/// Observatory's `n` column already could (PR #4670). This golden is what
/// closes that gap: the lead's own call carries no badge, and the delegate's
/// carries `↳ d:1` on its head row and again on its result row, so a reader
/// scrolled past the head still knows who the output belongs to.
#[test]
fn deck_render_snapshots_pin_a_delegates_tool_call() {
    let mut model = fixture_model();
    // Both calls fold into the *lead's own* transcript — a `delegate` call
    // spawns a child inline within the lead's turn, and the child's tool
    // activity is reported back on the lead's own event stream, distinguished
    // only by `sub_agent_id` (#4699). This is not the separate registered
    // lane `views::subagents` draws.
    let ev = |event: AgentEvent| Inbound::Event {
        agent: "lead".into(),
        event,
    };
    for inbound in [
        ev(AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "lead-call".into(),
                name: "get_state".into(),
                input: serde_json::json!({ "key": "verify" }),
            },
            sub_agent_id: None,
        }),
        ev(AgentEvent::ToolResult {
            call_id: "lead-call".into(),
            output: ToolOutput::Ok {
                content: "{\"verify\":true}".into(),
                data: None,
            },
            duration_ms: 4,
            speculated: false,
            sub_agent_id: None,
        }),
        ev(AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "delegate-call".into(),
                name: "search".into(),
                input: serde_json::json!({ "pattern": "sub_agent_id" }),
            },
            sub_agent_id: Some("d:1".into()),
        }),
        ev(AgentEvent::ToolResult {
            call_id: "delegate-call".into(),
            output: ToolOutput::Ok {
                content: "crates/stella-tui/src/model/entry.rs:105".into(),
                data: None,
            },
            duration_ms: 9,
            speculated: false,
            sub_agent_id: Some("d:1".into()),
        }),
    ] {
        model.apply_inbound(&inbound);
    }

    let mut ui = ui_for(DeckTab::Session);
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "session_delegate_tool_call",
        "a delegate's call badged ↳ d:1, the lead's own call unbadged",
        W,
        H,
        &frame,
    );
}
