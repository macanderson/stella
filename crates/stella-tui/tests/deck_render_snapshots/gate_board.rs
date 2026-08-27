// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A verify turn's gate board, five gates with one failure — SPEC 8.1.
//!
//! A submodule of `deck_render_snapshots` rather than more lines in it: the
//! parent had already reached the 1500-line ceiling, so a new golden goes here
//! instead of pushing it over.
//!
//! Every helper comes from the parent, so the golden lives in one directory and
//! is blessed by one command:
//! `BLESS=1 cargo test -p stella-tui --test deck_render_snapshots`.
//!
//! What this pins that the unit tests beside `views::gate_board` cannot: the
//! board's rows landing at the right place *inside a rendered deck* — under the
//! turn's rail, at the transcript's own width, wrapped or not wrapped by the
//! pane it sits in. Colour is stripped here by design (see the parent's doc on
//! what a golden does and does not capture), so the red-scarcity claim stays
//! where it can be asserted on cells:
//! `views::gate_board::tests::a_five_gate_board_spends_red_on_the_failing_row_alone`.

use super::*;

use stella_protocol::{GateBoard, GateRow, GateState};

/// SPEC 8.1's `09-gate-failure`: `4/5 green`, one `✗ tests failed` row, and the
/// failure block under it carrying the failing case, a two-line excerpt and the
/// keys.
#[test]
fn deck_render_snapshots_pin_a_five_gate_board_with_one_failure() {
    let mut model = fixture_model();
    let gate = |name: &str, state: GateState| GateRow {
        name: name.into(),
        state,
        deterministic: true,
    };
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::GateBoard {
            board: GateBoard {
                patch: Some("patch-7".into()),
                gates: vec![
                    gate("fmt", GateState::Green),
                    gate("clippy", GateState::Green),
                    gate(
                        "tests",
                        GateState::Failed {
                            case: "stella_core::loop_detect::a_short_cycle_is_detected".into(),
                            log: "assertion `left == right` failed\n  left: 3\n  right: 2".into(),
                        },
                    ),
                    gate("doc-warnings", GateState::Green),
                    gate("witness-flip", GateState::Green),
                ],
            },
        },
    });

    let mut ui = ui_for(DeckTab::Session);
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "session_gate_board_failure",
        "SPEC 8.1: a five-gate board, one gate red, its failure block below it",
        W,
        H,
        &frame,
    );
}
