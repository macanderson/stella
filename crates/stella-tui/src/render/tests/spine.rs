// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The witness for `#5030`: every head SPEC 6.3 draws, from a wire event.
//!
//! `views/transcript.rs` holds the head words and glyphs, and its own tests
//! build an `EventKind` by hand. That proves the painter and nothing more. A
//! head no session sends still passes there, and passed there for months.
//!
//! Each row below starts at an `AgentEvent`. The fold turns it into a
//! `TranscriptEntry`, and the draw turns that into rows. The head has to carry
//! the glyph and the word its own `EventKind` states. That is what shows the
//! row went through `views::transcript::event_rows` and not a second painter.
//!
//! [`declared`] covers every kind, so a new one stops this file from building
//! until somebody says what sends it. A kind nothing sends is allowed and
//! names the issue that will settle it — SPEC 6.3's `gate` row was the one
//! standing example (`EventKind::Gate`), and `#5651` deleted the kind rather
//! than give it a sender, since the engine only ever emits a whole board.

use super::*;
use crate::tool_class::ToolClass;
use crate::views::transcript::{EventKind, Extent};
use stella_protocol::{MemoryClass, ModelCallRole, SkillTrigger, TaskId, ToolCall};

/// What draws one head kind.
///
/// `Gap(u32)` — nothing sends one, and the number of the issue that settles
/// it — is not one of the options below. It was `EventKind::Gate`'s,
/// removed with the kind in `#5651`. Reintroduce it the day a new kind
/// again ships with no wire event behind it; until then a second option
/// nothing ever constructs is dead code, not a declared gap.
enum Producer {
    /// A wire event a session sends.
    Live,
}

/// What draws `kind`.
///
/// The match covers every kind, so adding one is a build error here rather
/// than a head nobody notices is out of reach. It is the rule
/// `stella-protocol` applies to its own events: a signal with no reader is
/// allowed, and it has to say so.
fn declared(kind: &EventKind) -> Producer {
    match kind {
        EventKind::Read { .. }
        | EventKind::Edit { .. }
        | EventKind::Write { .. }
        | EventKind::Delete { .. }
        | EventKind::Run { .. }
        | EventKind::Skill { .. }
        | EventKind::MemoryLog { .. }
        | EventKind::MemoryPromote { .. }
        | EventKind::Model { .. }
        | EventKind::Compaction { .. }
        | EventKind::Other { .. } => Producer::Live,
    }
}

/// One kind, the wire event that draws it, and text its head must carry on
/// top of its own glyph and word.
struct Row {
    kind: EventKind,
    event: AgentEvent,
    marks: &'static [&'static str],
}

/// A dispatched call, as the wire sends it.
fn tool_start(name: &str, input: serde_json::Value) -> AgentEvent {
    AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".into(),
            name: name.into(),
            input,
        },
        sub_agent_id: None,
        task_id: None,
    }
}

/// One settled model call, as the driver meters it. 420 tokens over 5 seconds
/// is 84 a second, which is the rate the head has to state.
fn step_usage() -> AgentEvent {
    AgentEvent::StepUsage {
        step: 0,
        turn_instance: Some(0),
        call_seq: Some(0),
        role: ModelCallRole::Worker,
        provider: "openrouter".into(),
        upstream_provider: None,
        output_text: None,
        model: "glm-5.2".into(),
        input_tokens: 4_000,
        output_tokens: 420,
        cached_input_tokens: 0,
        cache_write_tokens: 0,
        reasoning_tokens: None,
        estimated_input_tokens: 0,
        cost_usd: 0.01,
        duration_ms: 5_000,
        retries: 0,
        tool_calls: 1,
        complete: true,
        finish_reason: None,
        effort: None,
        max_output_tokens: None,
        temperature: None,
        params: None,
        sub_agent_id: None,
        task_id: None,
    }
}

/// One compaction pass. The row reads four of these fields and the rest are
/// the pass's own bookkeeping.
fn compaction() -> AgentEvent {
    AgentEvent::Compaction {
        before_tokens: 74_000,
        after_tokens: 69_000,
        evicted: 3,
        deduped: 1,
        superseded: 0,
        aged: 0,
        summarized: 0,
        evicted_blocks: Vec::new(),
        deduped_blocks: Vec::new(),
        superseded_blocks: Vec::new(),
        aged_blocks: Vec::new(),
        summarized_blocks: Vec::new(),
        rewrites: Vec::new(),
        effective_budget_tokens: 0,
        calibration_factor: 1.0,
    }
}

/// Every head kind a session can send, one row each.
///
/// The tool rows all arrive as one `AgentEvent::ToolStart`, because the head
/// is drawn when the call goes out and the tool's name is what picks the kind
/// (`views::transcript_source::kind_for`).
fn live() -> Vec<Row> {
    vec![
        Row {
            kind: EventKind::Read { lines: None },
            event: tool_start("read_file", serde_json::json!({ "path": "src/lib.rs" })),
            // A live read head folds, so it draws the open key beside it.
            marks: &["lib.rs", "↵ open"],
        },
        Row {
            kind: EventKind::Edit {
                extent: Extent::default(),
            },
            event: tool_start("edit_file", serde_json::json!({ "path": "src/lib.rs" })),
            marks: &["lib.rs"],
        },
        Row {
            kind: EventKind::Write {
                extent: Extent::default(),
            },
            event: tool_start("write_file", serde_json::json!({ "path": "src/new.rs" })),
            marks: &["new file"],
        },
        Row {
            kind: EventKind::Delete {
                extent: Extent::default(),
            },
            event: tool_start("delete_file", serde_json::json!({ "path": "src/old.rs" })),
            marks: &["git-backed"],
        },
        Row {
            kind: EventKind::Run { touched: None },
            event: tool_start("bash", serde_json::json!({ "command": "cargo test" })),
            marks: &["cargo test"],
        },
        Row {
            // A tool this host has no word for keeps its own name, and an MCP
            // tool's name is its last segment.
            kind: EventKind::Other {
                class: ToolClass::Execute,
                touched: None,
            },
            event: tool_start("mcp__github__create_pull_request", serde_json::json!({})),
            marks: &["create_pull_request"],
        },
        Row {
            kind: EventKind::Skill {
                trigger: "auto".into(),
                tokens: 1_200,
            },
            event: AgentEvent::SkillInjected {
                name: "reviewer".into(),
                summary: "database review".into(),
                tokens: 1_200,
                trigger: SkillTrigger::Auto,
            },
            marks: &["reviewer", "auto"],
        },
        Row {
            kind: EventKind::MemoryLog {
                memory_id: "nod_83b3f1d29a".into(),
            },
            event: AgentEvent::MemoryLogged {
                memory_id: "nod_83b3f1d29a".into(),
                text: "dedup keys must be stable across runs".into(),
                class: MemoryClass::Observation,
                confidence: 62,
                kind: "domain".into(),
                decays: true,
                promotes_at: 85,
                task_id: None,
            },
            marks: &["logged"],
        },
        Row {
            kind: EventKind::MemoryPromote {
                from: MemoryClass::Observation,
                to: MemoryClass::Rule,
                confidence: 87,
                audit_event_id: "prm_dedup_keys_a1b2".into(),
            },
            event: AgentEvent::MemoryPromoted {
                lineage_id: "prp_directive_dedup-keys".into(),
                from: MemoryClass::Observation,
                to: MemoryClass::Rule,
                confidence: 87,
                audit_event_id: "prm_dedup_keys_a1b2".into(),
                task_id: None,
            },
            marks: &["promoted"],
        },
        Row {
            kind: EventKind::Model {
                tokens_per_sec: Some(84),
            },
            event: step_usage(),
            // The rate, the call's own wall clock, and the footer — the three
            // head fields beyond the glyph and the word, all from the fold.
            marks: &["worker", "84 tok/s", "⚡5000ms", "irreducible generation"],
        },
        Row {
            kind: EventKind::Compaction {
                from_tokens: 74_000,
                to_tokens: 69_000,
                evicted: 3,
                deduped: 1,
            },
            event: compaction(),
            marks: &["3 evicted", "1 deduped"],
        },
    ]
}

/// Fold one wire event and draw the whole transcript, joined.
fn rendered(event: &AgentEvent) -> String {
    let mut model = SessionModel::new();
    model.apply(event);
    transcript_lines(&model, false, 120)
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every head kind with a live sender draws from one, through the same
/// `event_rows` the fixture tests use.
#[test]
fn every_live_head_kind_is_drawn_from_the_wire() {
    for row in live() {
        assert!(
            matches!(declared(&row.kind), Producer::Live),
            "{:?} is listed here as live and declared a gap",
            row.kind
        );
        let drawn = rendered(&row.event);
        // The glyph and the word come from the kind, and the text comes from
        // the fold. A row where they agree is a row that reached the painter.
        let glyph = row.kind.head_glyph(row.kind.collapses_by_default());
        assert!(
            drawn.contains(glyph),
            "{:?} drew no {glyph}: {drawn}",
            row.kind
        );
        let word = row.kind.verb();
        assert!(
            word.is_empty() || drawn.contains(word),
            "{:?} drew no `{word}`: {drawn}",
            row.kind
        );
        for mark in row.marks {
            assert!(
                drawn.contains(mark),
                "{:?} lost `{mark}`: {drawn}",
                row.kind
            );
        }
    }
}

/// A live head carries the board tag the wire stamped on the call — SPEC
/// 6.2's `→ task 3`.
///
/// `AgentEvent::ToolStart` carries `task_id`, the fold reads it onto
/// `TranscriptEntry::ToolStart`, and the metric group draws it. The other
/// tests for the tag hand `head_rows` a number they made up, so this is the
/// one that starts where the engine does. An untagged call beside it, because
/// a tag that always draws is as wrong as one that never does.
#[test]
fn a_live_tool_head_carries_the_task_tag_the_wire_stamped() {
    let tagged = AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".into(),
            name: "edit_file".into(),
            input: serde_json::json!({ "path": "src/lib.rs" }),
        },
        sub_agent_id: None,
        task_id: Some(TaskId::new("3")),
    };
    let drawn = rendered(&tagged);
    assert!(drawn.contains("→ task 3"), "the tag was dropped: {drawn}");

    let plain = rendered(&tool_start(
        "edit_file",
        serde_json::json!({ "path": "src/lib.rs" }),
    ));
    assert!(
        !plain.contains("→ task"),
        "an untagged call drew a tag the board cannot jump to: {plain}"
    );
}

/// `#5651`: `EventKind::Gate` is gone, and with it the copy of the gate
/// board's price text that could never be reached (nothing built one).
/// Before that deletion `views/transcript.rs`'s `kind_detail` carried its
/// own `" · $0.00 · det"` literal beside `gate_board.rs`'s live one — two
/// implementations of one fact, only one of which a session could ever see
/// drift — which is what this test asserts against and would have failed on.
#[test]
fn the_deterministic_gate_price_is_built_in_exactly_one_place() {
    let transcript_src = include_str!("../../views/transcript.rs");
    let gate_board_src = include_str!("../../views/gate_board.rs");
    let needle = "\" · $0.00 · det\"";
    assert_eq!(
        transcript_src.matches(needle).count(),
        0,
        "views/transcript.rs builds the gate board's own price literal again"
    );
    assert_eq!(
        gate_board_src.matches(needle).count(),
        1,
        "the price literal should live in gate_board.rs's live renderer alone"
    );
}
