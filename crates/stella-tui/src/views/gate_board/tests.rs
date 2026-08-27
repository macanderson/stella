// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! SPEC 8.1 acceptance: a five-gate board with one failure renders its header,
//! its rows and its failure block; red is spent on the failing row and nowhere
//! else in the frame; a deterministic gate is priced `$0.00 · det`.

use super::*;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::widgets::{Paragraph, Widget};
use stella_protocol::{GateBoard, GateRow, GateState};

const W: usize = 88;

/// The board SPEC 8.1 describes: five gates, one of them red.
fn five_gates() -> GateBoard {
    let gate = |name: &str, state: GateState| GateRow {
        name: name.into(),
        state,
        deterministic: true,
    };
    GateBoard {
        patch: Some("patch-7".into()),
        gates: vec![
            gate("fmt", GateState::Green),
            gate("clippy", GateState::Green),
            gate(
                "tests",
                GateState::Failed {
                    case: "stella_core::loop_detect::a_short_cycle_is_detected".into(),
                    log: "assertion `left == right` failed\n  left: 3\n  right: 2\n\n".into(),
                },
            ),
            gate("doc-warnings", GateState::Green),
            gate("witness-flip", GateState::Green),
        ],
    }
}

/// Draw the board onto a fixed grid, so the assertions read what a terminal
/// would rather than what the spans claim.
fn frame(board: &GateBoard, expanded: bool) -> (Buffer, String) {
    let lines = board_rows(board, expanded, W);
    let h = lines.len() as u16;
    let mut term = Terminal::new(TestBackend::new(W as u16, h)).unwrap();
    term.draw(|f| {
        let area = f.area();
        Paragraph::new(lines.clone()).render(area, f.buffer_mut());
    })
    .unwrap();
    let buf = term.backend().buffer().clone();
    let text = (0..buf.area().height)
        .map(|y| {
            (0..buf.area().width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    (buf, text)
}

/// The whole board, as a reader sees it. A golden rather than a handful of
/// `contains` assertions: a substring match is blind to position, and this row
/// set is exactly the thing whose *shape* SPEC 8.1 specifies — the header, the
/// indent under it, the failure block under the one red row.
#[test]
fn a_five_gate_board_with_one_failure_renders_the_specced_shape() {
    let (_, text) = frame(&five_gates(), false);
    assert_eq!(
        text,
        " │ ◇ gate board · patch-7 · 4/5 green · $0.00\n\
         \x20│   ✓ fmt · green · $0.00 · det\n\
         \x20│   ✓ clippy · green · $0.00 · det\n\
         \x20│   ✗ tests failed\n\
         \x20│       stella_core::loop_detect::a_short_cycle_is_detected\n\
         \x20│       assertion `left == right` failed\n\
         \x20│         left: 3\n\
         \x20│       ^N jump · l full log · r rerun gate\n\
         \x20│   ✓ doc-warnings · green · $0.00 · det\n\
         \x20│   ✓ witness-flip · green · $0.00 · det",
        "the board's shape moved:\n{text}"
    );
}

/// SPEC 8.1's acceptance test, and the reason the board exists at all: red is
/// the alarm, and an alarm that fires twice in one frame is not one.
///
/// Asserted on buffer cells rather than on spans, the pattern
/// `views::transcript::tests::a_healthy_turn_has_no_red_cells` establishes: a
/// span assertion can only see the spans this module returned, and the claim is
/// about every cell of the frame.
#[test]
fn a_five_gate_board_spends_red_on_the_failing_row_alone() {
    let (buf, text) = frame(&five_gates(), false);
    let red_rows: std::collections::BTreeSet<u16> = (0..buf.area().height)
        .flat_map(|y| (0..buf.area().width).map(move |x| (x, y)))
        .filter(|&(x, y)| buf.cell((x, y)).and_then(|c| c.fg.into()) == Some(token::RED))
        .map(|(_, y)| y)
        .collect();
    assert_eq!(
        red_rows.len(),
        1,
        "red reached {} rows; SPEC 8.1 spends it on the failing row alone:\n{text}",
        red_rows.len()
    );
    let row = *red_rows.first().expect("one red row");
    let painted: String = (0..buf.area().width)
        .map(|x| buf.cell((x, row)).map(|c| c.symbol()).unwrap_or(" "))
        .collect();
    assert!(
        painted.contains("✗ tests failed"),
        "the red row is not the failing gate: {painted:?}"
    );
    // And the failure block under it keeps the board's own metal — SPEC 8.1's
    // "every other row keeps its normal metal". Checked as its own claim rather
    // than left to the count above, because the count would also pass if the
    // block had no rail at all.
    let block_rail: Vec<Option<ratatui::style::Color>> = (row + 1..row + 5)
        .map(|y| buf.cell((1, y)).and_then(|c| c.fg.into()))
        .collect();
    assert!(
        block_rail.iter().all(|fg| *fg == Some(token::GOLD)),
        "the failure block's rail is not the board's metal: {block_rail:?}"
    );
}

/// A board with nothing wrong holds no red cell at all — the other half of
/// scarcity, and the half a regression would reach first.
#[test]
fn an_all_green_board_has_no_red_cells() {
    let mut board = five_gates();
    board.gates[2].state = GateState::Green;
    let (buf, text) = frame(&board, false);
    let red = (0..buf.area().height)
        .flat_map(|y| (0..buf.area().width).map(move |x| (x, y)))
        .filter(|&(x, y)| buf.cell((x, y)).and_then(|c| c.fg.into()) == Some(token::RED))
        .count();
    assert_eq!(red, 0, "a green board painted red:\n{text}");
    assert!(
        text.contains("5/5 green"),
        "the tally did not settle:\n{text}"
    );
}

/// SPEC 8.1: the failing row takes a red-tinted ground, and the rail beside it
/// does not — SPEC 6.4's two-layer rule, which `Line.style` would break by
/// painting the whole row's area including the margin.
#[test]
fn the_failing_row_is_tinted_out_to_the_pane_edge_and_the_rail_is_not() {
    let (buf, _) = frame(&five_gates(), false);
    let row = (0..buf.area().height)
        .find(|&y| {
            (0..buf.area().width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
                .contains("✗ tests failed")
        })
        .expect("the failing row renders");

    // `Reset` rather than `None`: a `TestBackend` cell that nobody painted a
    // ground onto reports the terminal's own, which is what "the rail is not
    // part of the diff" means once it reaches a buffer.
    let rail_bg: Option<ratatui::style::Color> = buf.cell((1, row)).and_then(|c| c.bg.into());
    assert_eq!(
        rail_bg,
        Some(ratatui::style::Color::Reset),
        "the tint swallowed the rail"
    );

    for x in super::super::transcript::RAIL_W as u16..buf.area().width {
        let bg: Option<ratatui::style::Color> = buf.cell((x, row)).and_then(|c| c.bg.into());
        assert_eq!(
            bg,
            Some(FAIL_ROW_BG),
            "the tint stopped at column {x} instead of the pane edge"
        );
    }
}

/// SPEC 8.1's excerpt is two lines; `l` is what opens the rest. Without the cap
/// a failing suite's whole output lands in the transcript, which is the row a
/// reader is trying to find buried under the reason they are looking for it.
#[test]
fn a_collapsed_failure_shows_two_lines_and_expanding_shows_the_whole_log() {
    let (_, collapsed) = frame(&five_gates(), false);
    assert!(collapsed.contains("assertion `left == right` failed"));
    assert!(collapsed.contains("left: 3"));
    assert!(
        !collapsed.contains("right: 2"),
        "the third line escaped the excerpt:\n{collapsed}"
    );
    assert!(collapsed.contains("l full log"));

    let (_, expanded) = frame(&five_gates(), true);
    assert!(
        expanded.contains("right: 2"),
        "expanding did not open the log:\n{expanded}"
    );
    assert!(
        expanded.contains("l fold log"),
        "an opened block names no way back:\n{expanded}"
    );
    // The log's trailing blank line is dropped: an empty row inside the block
    // reads as the excerpt having run out early.
    assert!(!expanded.ends_with("\n\n"));
}

/// SPEC 6.3: verify work prices at `$0.00 · det`. A gate whose decision was not
/// free must not borrow that tag, and the header must not claim the board was
/// free either.
#[test]
fn only_a_deterministic_gate_is_priced_zero() {
    let mut board = five_gates();
    board.gates[0].deterministic = false;
    let (_, text) = frame(&board, false);
    let fmt_row = text
        .lines()
        .find(|line| line.contains("fmt"))
        .expect("the fmt row renders");
    assert!(
        !fmt_row.contains("$0.00"),
        "a gate that was not free was priced as free: {fmt_row}"
    );
    assert!(
        text.lines()
            .find(|line| line.contains("clippy"))
            .is_some_and(|line| line.contains("$0.00 · det")),
        "a deterministic gate lost its price:\n{text}"
    );
    assert!(
        !text.lines().next().expect("a header").contains("$0.00"),
        "the header priced a board holding a gate that was not free:\n{text}"
    );
}

/// An abstention is not a failure: it takes no red, no tint and no failure
/// block, and it does not count toward the green tally either.
#[test]
fn an_undecided_gate_is_neither_green_nor_red() {
    let mut board = five_gates();
    board.gates[2].state = GateState::Undecided {
        reason: "the oracle reported no value for \"p50\"".into(),
    };
    let (buf, text) = frame(&board, false);
    let red = (0..buf.area().height)
        .flat_map(|y| (0..buf.area().width).map(move |x| (x, y)))
        .filter(|&(x, y)| buf.cell((x, y)).and_then(|c| c.fg.into()) == Some(token::RED))
        .count();
    assert_eq!(red, 0, "an abstention was painted as a failure:\n{text}");
    assert!(text.contains("4/5 green"), "the tally is wrong:\n{text}");
    assert!(
        text.contains("○ tests · the oracle reported no value for \"p50\""),
        "an abstention did not state why:\n{text}"
    );
    assert!(
        !text.contains("^N jump"),
        "an abstention drew a failure block:\n{text}"
    );
}

/// A board with no patch to name states none, rather than inventing one — the
/// same rule the transcript's turn rule follows for a model it has not been
/// told.
#[test]
fn a_board_with_no_patch_renders_no_patch_cell() {
    let mut board = five_gates();
    board.patch = None;
    let (_, text) = frame(&board, false);
    let header = text.lines().next().expect("a header");
    assert_eq!(header, " │ ◇ gate board · 4/5 green · $0.00");
}
