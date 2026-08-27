// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! SPEC 8.1 items 3 and 4: the failing board, and the plan revision it puts up.
//!
//! Both entries in one frame on purpose. The proposal is a *response* to the
//! board — its cause is the gate's own words — and a golden of the block alone
//! could not show that the two read as one exchange, which is the thing SPEC
//! 8.1 is actually asking for.
//!
//! A submodule for the reason [`super::gate_board`] is one: the parent reached
//! its ceiling. Every helper comes from the parent, and one command blesses
//! them all:
//! `BLESS=1 cargo test -p stella-tui --test deck_render_snapshots`.
//!
//! Colour is stripped by `render_frame`, so the gold/`GOLD_BRIGHT` claims stay
//! where they can be asserted on spans:
//! `views::revision_proposal::tests::the_proposal_spends_gold_and_never_red`.

use super::*;

use stella_protocol::{
    DivergenceCause, GateBoard, GateRow, GateState, RevisionProposal, plan_graph::PlanRevision,
};
use stella_tui::deck_ui::ingest_inbound;

/// The `09-gate-failure` exchange: `✗ tests failed`, then
/// `⌥ propose r2: add task "…"` with its cause, its linked issue, the action
/// row and the merge banner.
#[test]
fn deck_render_snapshots_pin_a_gate_failure_and_the_revision_it_proposes() {
    let mut model = fixture_model();
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::GateBoard {
            board: GateBoard {
                patch: Some("patch-7".into()),
                gates: vec![
                    GateRow {
                        name: "fmt".into(),
                        state: GateState::Green,
                        deterministic: true,
                    },
                    GateRow {
                        name: "tests".into(),
                        state: GateState::Failed {
                            case: "stella_core::loop_detect::a_short_cycle_is_detected".into(),
                            log: "assertion `left == right` failed\n  left: 3\n  right: 2".into(),
                        },
                        deterministic: true,
                    },
                ],
            },
        },
    });

    let mut ui = ui_for(DeckTab::Session);
    ingest_inbound(
        &Inbound::RevisionProposed {
            agent: "lead".into(),
            proposal: Box::new(RevisionProposal {
                revision: PlanRevision::new(2).expect("r2"),
                subject: "repair stella_core::loop_detect::a_short_cycle_is_detected".into(),
                gate: "tests".into(),
                cause: DivergenceCause::new("assertion `left == right` failed").expect("a cause"),
                issue: Some("#151".into()),
            }),
        },
        &mut model,
        &mut ui,
    );

    let frame = render_frame(&model, &mut ui, W, H);
    // SPEC 8.1 writes `⌥`, `stella_tui_theme::glyph::DRIFT` is `⌥`, and
    // `glyph::ALL` is gate-enforced — so the `⑂` both issues wrote would be a
    // drift mark no other surface draws. An absence assertion, because a
    // blessed golden cannot be relied on to notice a glyph swap.
    assert!(frame.contains('⌥'), "{frame}");
    assert!(!frame.contains('⑂'), "{frame}");
    assert_golden(
        "session_revision_proposal",
        "SPEC 8.1: a failing gate board and the plan revision it proposes",
        W,
        H,
        &frame,
    );
}
