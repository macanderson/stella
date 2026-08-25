// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One call, one rail: the head, the result and every body row under them
//! share a left edge.
//!
//! The head of a tool call renders through the SPEC 6.2 head renderer and
//! its result renders long-form. That seam had no test across it, and the two
//! sides disagreed about every column: the head opened `" │ ● run …"` with its
//! text at column 5, the result opened `"  ⎿ "` with its glyph at column 2 and
//! its body at column 4. On screen the rail appeared for exactly one row, the
//! `⎿` sat under nothing, and the output stood a column left of the command it
//! belonged to.

use super::*;
use crate::render::row::{RAIL, Rail};
use crate::views::transcript::rail_span;
use unicode_width::UnicodeWidthStr;

const WIDTH: usize = 100;

/// The rail every row of a block must open with, taken from the **head**
/// renderer rather than from `row::RAIL`.
///
/// Deliberately the far side of the seam: these are geometry tests, and reading
/// the expectation off the constant the geometry is built from would make them
/// tautologies that pass whatever column the block actually lands in. Read off
/// the head they state the real requirement — *the half of the block the body
/// renderer draws must match the half the head draws* — and they fail on the old code for that reason
/// rather than for want of a symbol.
fn expected_rail() -> String {
    rail_span(ratatui::style::Color::Reset).content.to_string()
}

fn start(name: &str, input: &str, path: Option<&str>) -> TranscriptEntry {
    TranscriptEntry::ToolStart {
        call_id: "c1".into(),
        name: name.into(),
        input: input.into(),
        raw: "{}".into(),
        path: path.map(str::to_owned),
        sub_agent_id: None,
    }
}

fn result(name: &str, ok: bool, body: &str) -> TranscriptEntry {
    TranscriptEntry::ToolResult {
        call_id: "c1".into(),
        name: name.into(),
        path: None,
        ok,
        summary: "ok".into(),
        full: body.into(),
        duration_ms: 17,
        speculated: false,
        diff: Vec::new(),
        read_size: None,
        sub_agent_id: None,
    }
}

/// Every rendered row of `entries`, as plain text.
fn rows(entries: &[TranscriptEntry]) -> Vec<String> {
    let mut out = Vec::new();
    for entry in entries {
        entry_lines(
            entry,
            EntryView::default(),
            false,
            false,
            false,
            WIDTH,
            &mut out,
        );
    }
    out.iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

/// The rows of a block that carry content — the trailing gap `push_gap` emits
/// is not one of them.
fn content_rows(entries: &[TranscriptEntry]) -> Vec<String> {
    rows(entries)
        .into_iter()
        .filter(|r| !r.trim().is_empty())
        .collect()
}

/// The witness. A `bash` call with multi-line output: every row of the block
/// opens with the rail, and every one of them puts its content in the same
/// column.
///
/// Both halves fail on the old code — the result row opened with two spaces,
/// and its body sat at column 4 against the head's 5.
#[test]
fn a_tool_block_rides_one_unbroken_rail() {
    let block = [
        start(
            "bash",
            "sed -n '930,1080p' crates/stella-tools/src/search.rs",
            None,
        ),
        result(
            "bash",
            true,
            "assert_eq!(shown, granted.len());\nassert_eq!(omitted, hits.len());\nassert!(facets.len() >= 3);\n}",
        ),
    ];
    let rendered = content_rows(&block);
    assert!(
        rendered.len() >= 4,
        "expected a head and a body, got:\n{rendered:#?}"
    );
    let rail = expected_rail();
    for row in &rendered {
        assert!(
            row.starts_with(&rail),
            "a row of the block broke the rail: {row:?}\nwhole block:\n{rendered:#?}"
        );
    }
    // And every row spends the same margin before its content: the rail, a
    // space, one glyph cell that may or may not carry a glyph, a space. Only
    // cell 3 is allowed to vary, which is what makes cell 5 one column for the
    // whole block instead of three.
    for row in &rendered {
        let cells: Vec<char> = row.chars().take(5).collect();
        assert_eq!(
            [cells[0], cells[1], cells[2], cells[4]],
            [' ', '│', ' ', ' '],
            "the block's margin is not the same five cells on every row: {row:?}\n{rendered:#?}"
        );
    }
}

/// A failed call's block rides its own rail just as unbrokenly — in danger
/// rather than muted, but never half of one and half of the other.
#[test]
fn a_failed_block_rides_its_own_rail_end_to_end() {
    let block = [
        start("bash", "cargo test -p stella-core", None),
        result(
            "bash",
            false,
            "error[E0432]: unresolved import\n  --> src/lib.rs:3:5\n   |\n 3 | use crate::gone;\n   |     ^^^^^^^^^^^\nerror: could not compile",
        ),
    ];
    let mut lines = Vec::new();
    for entry in &block {
        entry_lines(
            entry,
            EntryView::default(),
            false,
            false,
            false,
            WIDTH,
            &mut lines,
        );
    }
    let rail = expected_rail();
    let railed: Vec<_> = lines
        .iter()
        .filter(|l| {
            l.spans
                .first()
                .is_some_and(|s| s.content.starts_with(&rail))
        })
        .collect();
    assert!(railed.len() >= 4, "the failed block lost its rail rows");
    // Every rail cell below the head is the result's own metal — one block, one
    // colour, rather than a rail that changes hue partway down.
    let metals: std::collections::BTreeSet<_> = railed
        .iter()
        .skip(1)
        .map(|l| format!("{:?}", l.spans[0].style.fg))
        .collect();
    assert_eq!(
        metals.len(),
        1,
        "the failed block's rail changes colour partway down: {metals:?}"
    );
}

/// A row too wide for the pane keeps the rail on the lines it wraps onto.
///
/// The hole this closes is specific to the surface that produces the widest
/// rows there are: a `bash` result. Wrapping used to indent continuations with
/// blanks, so the margin lost the rail on exactly the output it exists to
/// index.
#[test]
fn a_wrapped_body_row_keeps_the_rail() {
    // The *second* line is the long one: the first is promoted onto the result
    // row, which truncates to keep its metric column and so never wraps. Only a
    // body row reaches [`wrap_one_lead`], which is the code under test.
    let body = format!("head line\n{}", "word ".repeat(WIDTH));
    let block = [start("bash", "echo", None), result("bash", true, &body)];
    let rendered = content_rows(&block);
    assert!(
        rendered.len() > 2,
        "the body did not wrap, so the test proves nothing: {rendered:#?}"
    );
    let rail = expected_rail();
    for row in &rendered {
        assert!(
            row.starts_with(&rail),
            "a wrapped continuation dropped the rail: {row:?}"
        );
    }
}

/// The body renderer's rail glyph and the head's are the same two cells.
///
/// Named separately from the geometry tests because it is the *seam* that
/// matters: the two renderers each own one half of a block, and a block only
/// looks like one thing while both spell its margin identically. A literal
/// copied into `row.rs` would pass every alignment test above on the day it was
/// written and drift the first time SPEC 6.2 changes the glyph.
#[test]
fn the_v1_block_rail_is_the_v2_event_rail() {
    assert_eq!(
        RAIL,
        expected_rail(),
        "the transcript now draws two different rails"
    );
    // The metal a result wears is now its call's (SPEC 6.2, #4127), so this is
    // constructed with one — the margin's *shape* is the same whichever it is.
    for rail in [Rail::Result(stella_tui_theme::token::GOLD), Rail::Fail] {
        assert!(
            rail.prefix().starts_with(RAIL),
            "{rail:?} opens a block without the rail"
        );
        let margin = rail
            .continuation()
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert_eq!(
            UnicodeWidthStr::width(margin.as_str()),
            rail.indent(),
            "{rail:?}'s continuation margin is not its own content column"
        );
        assert!(
            margin.starts_with(RAIL),
            "{rail:?}'s continuation drops the rail: {margin:?}"
        );
    }
}

// ── ctrl+o on a call reveals its arguments again (#4157) ────────────────────

/// A call carrying a `raw` argument object.
fn call_with_args() -> TranscriptEntry {
    TranscriptEntry::ToolStart {
        call_id: "c1".into(),
        name: "edit_file".into(),
        input: "src/lib.rs".into(),
        raw: r#"{"path":"src/lib.rs","old_string":"alpha","new_string":"bravo"}"#.into(),
        path: Some("src/lib.rs".into()),
        sub_agent_id: None,
    }
}

fn render(entry: &TranscriptEntry, expanded: bool) -> Vec<String> {
    let mut out = Vec::new();
    entry_lines(
        entry,
        EntryView::default(),
        false,
        expanded,
        false,
        WIDTH,
        &mut out,
    );
    out.iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

/// The witness. `ctrl+o` on a **call** row reveals the argument object it was
/// dispatched with.
///
/// The regression it pins: the SPEC 6 head router intercepted every `ToolStart` and
/// returned before `entry_body` ran, so the `expanded` flag was never consulted
/// for a call and the row rendered identically either way. Nothing caught it —
/// a dead *match arm* is invisible to `dead-code-allows` and to
/// `module-reachability`, both of which see items (#4157).
#[test]
fn ctrl_o_on_a_call_reveals_its_arguments() {
    let call = call_with_args();
    let collapsed = render(&call, false);
    let expanded = render(&call, true);

    assert!(
        !collapsed.iter().any(|r| r.contains("old_string")),
        "the collapsed row already shows the argument object, so the test \
         proves nothing:\n{collapsed:#?}"
    );
    assert!(
        expanded.iter().any(|r| r.contains("old_string alpha")),
        "ctrl+o revealed nothing — the argument object is unreachable:\n{expanded:#?}"
    );
    // One field per row (`views::fields`), not the compact one-liner it arrived
    // as — three fields, three rows under the head.
    assert!(
        expanded.len() >= collapsed.len() + 3,
        "the arguments were shown, but not one per row:\n{expanded:#?}"
    );
    assert!(
        !expanded.iter().any(|r| r.contains('{') || r.contains('"')),
        "JSON punctuation reached the pane:\n{expanded:#?}"
    );
}

/// Those revealed rows ride the block rail like every other row, and in the
/// **head's own metal** — a `read_file`'s block is silver-dim end to end, a
/// mutation's gold end to end, never one above the other.
#[test]
fn revealed_arguments_ride_the_heads_own_rail() {
    let rail = expected_rail();
    for name in ["read_file", "edit_file", "bash"] {
        let call = TranscriptEntry::ToolStart {
            call_id: "c1".into(),
            name: name.into(),
            input: "src/lib.rs".into(),
            raw: r#"{"path":"src/lib.rs","limit":40}"#.into(),
            path: Some("src/lib.rs".into()),
            sub_agent_id: None,
        };
        let mut lines = Vec::new();
        entry_lines(
            &call,
            EntryView::default(),
            false,
            true,
            false,
            WIDTH,
            &mut lines,
        );
        let railed: Vec<_> = lines
            .iter()
            .filter(|l| {
                l.spans
                    .first()
                    .is_some_and(|s| s.content.starts_with(&rail))
            })
            .collect();
        assert!(
            railed.len() > 1,
            "{name}: the revealed arguments are not on the rail"
        );
        let metals: std::collections::BTreeSet<_> = railed
            .iter()
            .map(|l| format!("{:?}", l.spans[0].style.fg))
            .collect();
        assert_eq!(
            metals.len(),
            1,
            "{name}: the block's rail changes metal between the head and the \
             arguments under it: {metals:?}"
        );
    }
}

/// An argument object big enough to have been cut by the fold's char cap no
/// longer parses. It is still shown — lexed as the capped JSON it is — rather
/// than swallowed.
#[test]
fn an_unparseable_argument_object_is_still_shown() {
    let call = TranscriptEntry::ToolStart {
        call_id: "c1".into(),
        name: "bash".into(),
        input: "echo".into(),
        raw: r#"{"command":"echo hello","cwd":"/tm"#.into(),
        path: None,
        sub_agent_id: None,
    };
    let rendered = render(&call, true);
    assert!(
        rendered.iter().any(|r| r.contains("echo hello")),
        "a capped argument object was dropped instead of shown:\n{rendered:#?}"
    );
}
