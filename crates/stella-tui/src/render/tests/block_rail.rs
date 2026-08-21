// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One call, one rail: the head, the result and every body row under them
//! share a left edge.
//!
//! The head of a tool call renders through the v2 event renderer (SPEC 6.2) and
//! its result renders through v1. That seam had no test across it, and the two
//! sides disagreed about every column: the head opened `" │ ● run …"` with its
//! text at column 5, the result opened `"  ⎿ "` with its glyph at column 2 and
//! its body at column 4. On screen the rail appeared for exactly one row, the
//! `⎿` sat under nothing, and the output stood a column left of the command it
//! belonged to.

use super::*;
use crate::render::row::{RAIL, Rail};
use crate::v2::transcript::rail_span;
use unicode_width::UnicodeWidthStr;

const WIDTH: usize = 100;

/// The rail every row of a block must open with, taken from the **v2** head
/// renderer rather than from `row::RAIL`.
///
/// Deliberately the far side of the seam: these are geometry tests, and reading
/// the expectation off the constant the geometry is built from would make them
/// tautologies that pass whatever column the block actually lands in. Read off
/// v2 they state the real requirement — *the half of the block v1 draws must
/// match the half v2 draws* — and they fail on the old code for that reason
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
        diff: None,
    }
}

/// Every rendered row of `entries`, as plain text.
fn rows(entries: &[TranscriptEntry]) -> Vec<String> {
    let mut out = Vec::new();
    for entry in entries {
        entry_lines(entry, &[], false, false, false, WIDTH, &mut out);
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
        entry_lines(entry, &[], false, false, false, WIDTH, &mut lines);
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

/// The v1 rail glyph and the v2 one are the same two cells.
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
    for rail in [Rail::Call, Rail::Result, Rail::Fail] {
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
