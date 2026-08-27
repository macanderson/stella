// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The gate board a verify turn draws — SPEC 8.1's rendering `09-gate-failure`.
//!
//! ```text
//!  │ ◇ gate board · patch-7 · 4/5 green · $0.00
//!  │   ✓ fmt · green · $0.00 · det
//!  │   ✗ tests failed                                              ← red row
//!  │       stella_core::loop_detect::a_short_cycle_is_detected
//!  │       assertion `left == right` failed
//!  │         left: 3
//!  │       ^N jump · l full log · r rerun gate
//! ```
//!
//! ## Red is the alarm, and it is spent here or nowhere
//!
//! SPEC 2's scarcity rule is what makes a failed gate legible at a glance: red
//! appears in the failing rows and in no other cell of the frame, so the eye
//! needs no search. That is a property of the whole frame rather than of this
//! module, which is why it is asserted over buffer cells
//! (`a_five_gate_board_spends_red_on_the_failing_row_alone`) rather than over
//! the spans this module returns — a span assertion cannot see a second red
//! that some neighbouring surface painted.
//!
//! The header's `4/5 green` is therefore never red, however few gates held:
//! the count is a summary and the failing row is the alarm, so tinting the
//! summary too would spend the scarce colour twice for one fact. The failure
//! block under a red row is dim for the same reason — the excerpt is evidence a
//! reader stops to read, not a signal they scan for.
//!
//! ## What a row is allowed to claim
//!
//! Every row is one clause of a definition of done that a verification plugin
//! declared, decided by the host against the evidence that plugin reported
//! (AGENTS.md's opening). Stella re-ran nothing. So a row says what the rule
//! concluded, the `det` tag says the *decision* asked no model, and the board
//! never says a test passed — only that a gate held on the evidence given.
//!
//! ## Purity
//!
//! A projection of owned data onto `Line<'static>`, exactly as
//! [`super::transcript`] is, and for the same reason: it is what lets the
//! goldens be fixture data all the way down.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use stella_protocol::{GateBoard, GateRow, GateState};
use stella_tui_theme::{glyph, token};

use super::transcript::rail_span;

/// Excerpt lines a collapsed failure block shows (SPEC 8.1's "two-line
/// stderr or assertion excerpt"). The rest is behind `l`.
pub const EXCERPT_LINES: usize = 2;

/// Cells a gate row is indented under the board's header.
const ROW_INDENT: &str = "   ";

/// Cells the failure block is indented under its gate row.
const BLOCK_INDENT: &str = "       ";

/// The keys SPEC 8.1 puts under a failure block.
const FAILURE_KEYS_COLLAPSED: &str = "^N jump · l full log · r rerun gate";

/// The same row once `l` has been pressed — the way back out.
///
/// Every way in has a way out (the deck's overlay rule), and a block that
/// opened to two hundred lines with no key named for closing it is a reader
/// scrolling to find one.
const FAILURE_KEYS_EXPANDED: &str = "^N jump · l fold log · r rerun gate";

/// The red-tinted ground a failed row is painted on.
///
/// The palette's one red row tint, and it is reused rather than twinned. A
/// second dark red would have to sit within a few degrees of this one to read
/// as the same alarm, which is exactly the confusion the 30° hue-separation law
/// exists to prevent — so the tint is one value with two roles (a removed diff
/// row, a failed gate) rather than two values a reader could not tell apart.
pub const FAIL_ROW_BG: Color = token::DIFF_DEL_BG;

/// Every row the board owns: the header, one row per gate, and a failure block
/// under each gate that failed.
///
/// `expanded` opens every failure block on this board to its whole log — the
/// deck's per-entry expansion, which `l` toggles and `ctrl+o` toggles too. It
/// is per *entry* rather than per gate because that is the granularity the
/// transcript's selection has: the reader highlights the board, not a row
/// inside it.
#[must_use]
pub fn board_rows(board: &GateBoard, expanded: bool, width: usize) -> Vec<Line<'static>> {
    let mut rows = vec![header_row(board)];
    for gate in &board.gates {
        rows.push(gate_row(gate, width));
        if let GateState::Failed { case, log } = &gate.state {
            rows.extend(failure_block(case, log, expanded));
        }
    }
    rows
}

/// `◇ gate board · patch-7 · 4/5 green · $0.00`.
///
/// The price is the board's, not a gate's: deciding every row cost nothing
/// (see [`GateRow::deterministic`]), so the header states it once instead of
/// each row repeating a `$0.00` the reader has already read.
fn header_row(board: &GateBoard) -> Line<'static> {
    let dim = Style::new().fg(token::DIM);
    let text = Style::new().fg(token::TEXT);
    let mut spans = vec![
        rail_span(token::GOLD),
        Span::styled(format!(" {} ", glyph::GATE), Style::new().fg(token::GOLD)),
        Span::styled(
            "gate board",
            Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(patch) = &board.patch {
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled(patch.clone(), text));
    }
    let (green, total) = (board.green(), board.total());
    spans.push(Span::styled(" · ", dim));
    spans.push(Span::styled(
        format!("{green}/{total} green"),
        // Green only when every gate held, on `super::transcript::receipt`'s
        // rule: a partial pass is not a pass and must not borrow the metal that
        // says one. The unmet case takes plain text rather than red — the
        // failing row below is where the alarm is spent (module docs).
        Style::new().fg(if total > 0 && green == total {
            token::GREEN
        } else {
            token::TEXT
        }),
    ));
    // Every gate on the board was decided by the host's own rule evaluation,
    // which asks no model. A board that ever carries a gate someone paid for
    // states that gate's price on its own row instead.
    if board.gates.iter().all(|gate| gate.deterministic) {
        spans.push(Span::styled(" · ", dim));
        spans.push(Span::styled("$0.00", Style::new().fg(token::GOLD)));
    }
    Line::from(spans)
}

/// One gate's row. A failure takes the red rail and the tinted ground; every
/// other state keeps the board's gold and its ordinary ground.
fn gate_row(gate: &GateRow, width: usize) -> Line<'static> {
    match &gate.state {
        GateState::Failed { .. } => failed_row(gate, width),
        GateState::Green => settled_row(gate, glyph::DONE, token::GREEN, "green"),
        GateState::Undecided { reason } => {
            settled_row(gate, glyph::QUEUED, token::MUTED, reason.as_str())
        }
    }
}

/// `✓ fmt · green · $0.00 · det` — a gate that is not the alarm.
///
/// The glyph carries the state and the rail carries the metal, so the row still
/// reads on a terminal with no colour at all (SPEC 13). `det` rides every row
/// this producer draws; a row whose decision cost money would have to say so
/// here, which is why the tag is read off the value rather than printed
/// unconditionally.
fn settled_row(gate: &GateRow, mark: char, mark_color: Color, state: &str) -> Line<'static> {
    let dim = Style::new().fg(token::DIM);
    let mut spans = vec![
        rail_span(token::GOLD),
        Span::styled(format!("{ROW_INDENT}{mark} "), Style::new().fg(mark_color)),
        Span::styled(gate.name.clone(), Style::new().fg(token::TEXT)),
        Span::styled(format!(" · {state}"), dim),
    ];
    if gate.deterministic {
        spans.push(Span::styled(" · $0.00 · det", dim));
    }
    Line::from(spans)
}

/// `✗ tests failed` on a red rail over a red-tinted ground.
///
/// The tint reaches the pane edge and stops short of the rail, which is SPEC
/// 6.4's two-layer rule applied to a row that is not a diff: `Line.style` would
/// paint the rail too, and a ground that swallows the rail leaves the two
/// layers disagreeing about what the row is. So the tint rides a per-span `bg`
/// plus one trailing pad.
fn failed_row(gate: &GateRow, width: usize) -> Line<'static> {
    let ground = |fg: Color| Style::new().fg(fg).bg(FAIL_ROW_BG);
    let mut spans = vec![
        rail_span(token::RED),
        Span::styled(
            format!("{ROW_INDENT}{} ", glyph::FAILED),
            ground(token::RED),
        ),
        Span::styled(gate.name.clone(), ground(token::TEXT)),
        Span::styled(" failed", ground(token::RED).add_modifier(Modifier::BOLD)),
    ];
    let used: usize = spans.iter().map(Span::width).sum();
    if used < width {
        spans.push(Span::styled(
            " ".repeat(width - used),
            Style::new().bg(FAIL_ROW_BG),
        ));
    }
    Line::from(spans)
}

/// The failure block SPEC 8.1 puts under a failed gate: the failing case, the
/// excerpt, and the keys.
///
/// Dim throughout, on the board's own gold rail. SPEC 8.1 says it in one
/// sentence — "every other row keeps its normal metal, so the red row is the
/// only saturated non-gold element on screen" — and the block is other rows.
/// Railing it red would double the alarm's footprint to five rows for one
/// failure, which is what `a_five_gate_board_spends_red_on_the_failing_row_alone`
/// caught the first time this module tried it. What ties the block to the row
/// above is the indent, and that survives a terminal with no colour at all.
fn failure_block(case: &str, log: &str, expanded: bool) -> Vec<Line<'static>> {
    let dim = Style::new().fg(token::DIM);
    let mut rows = vec![Line::from(vec![
        rail_span(token::GOLD),
        Span::styled(
            format!("{BLOCK_INDENT}{case}"),
            Style::new().fg(token::MUTED),
        ),
    ])];
    for line in excerpt(log, expanded) {
        rows.push(Line::from(vec![
            rail_span(token::GOLD),
            Span::styled(format!("{BLOCK_INDENT}{line}"), dim),
        ]));
    }
    rows.push(Line::from(vec![
        rail_span(token::GOLD),
        Span::styled(
            format!(
                "{BLOCK_INDENT}{}",
                if expanded {
                    FAILURE_KEYS_EXPANDED
                } else {
                    FAILURE_KEYS_COLLAPSED
                }
            ),
            dim,
        ),
    ]));
    rows
}

/// The lines of `log` a block shows: all of them when expanded, the first
/// [`EXCERPT_LINES`] otherwise.
///
/// A trailing blank line is dropped rather than rendered, because a log
/// captured from a process almost always ends in one and an empty row inside
/// a block reads as the excerpt having run out early. Blank lines *within* the
/// log are kept — they are part of what the reader is being shown.
fn excerpt(log: &str, expanded: bool) -> Vec<String> {
    let mut lines: Vec<&str> = log.lines().collect();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    let take = if expanded { lines.len() } else { EXCERPT_LINES };
    lines.into_iter().take(take).map(str::to_owned).collect()
}

#[cfg(test)]
mod tests;
