// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The task zoom's golden frames — SPEC 7.5, rendering `03-task-zoom` (#5041).
//!
//! A submodule of `deck_render_snapshots` rather than more lines in it: the
//! parent sits at the 1500-line ceiling, so a new golden goes here.
//!
//! Every helper comes from the parent, so both frames are blessed by one
//! command: `BLESS=1 cargo test -p stella-tui --test deck_render_snapshots`.

use super::*;

/// The task zoom (SPEC 7.5, rendering `03-task-zoom`) in both of its forms —
/// #5041.
///
/// Two goldens, because the surface has two truthful shapes and only one of
/// them exists in this workspace today. `zoom_scripted` is the whole view over
/// [`stella_tui::scenario::demo_task_zoom`]: a contract with a passed check, a
/// failed one, a pending one and a model-judged one, an evidence ledger, both
/// plan-graph lanes with a divergence, and a spend strip. `zoom_elided` is
/// what a live session actually renders until #5037 and #5039 land — every
/// block that has no source says so by name, and none of them invents a row.
///
/// A golden is the only thing that can hold the second frame to what it says:
/// the elision copy is a *sentence*, and a sentence quietly becoming a blank
/// line is invisible to every other kind of test.
/// **The witness.** On a short terminal the zoom's body scrolls. The first
/// frame folds its tail and admits the cut. `⇟` moves the window, and the
/// moved frame admits what scrolled past at the top. Before this the zoom
/// had no scroll at all. The first rows were all a short terminal showed.
#[test]
fn the_task_zoom_scrolls_on_a_short_terminal() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use stella_tui::deck_ui::cards::Card;

    let (board, lanes, ledger) = stella_tui::scenario::demo_task_zoom();
    let mut model = fixture_model();
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::TaskUpdate { tasks: board },
    });
    let lead = &mut model.agents[0].model.plan;
    lead.lanes = Some(lanes);
    lead.ledger.insert("5".to_string(), ledger);

    let mut ui = ui_for(DeckTab::Session);
    ui.cards.raise(Card::TaskZoom);
    ui.cards.plan_sel = 4;
    let (w, h) = (80u16, 20u16);
    let before = render_frame(&model, &mut ui, w, h);
    assert!(
        before.contains("more below"),
        "the fold admits the rows it cut:\n{before}"
    );

    stella_tui::deck_ui::cards::handle_card_key(
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
        &model,
        &mut ui,
    );
    let frame = render_frame(&model, &mut ui, w, h);
    assert_ne!(before, frame, "⇟ moved the body");
    assert!(
        frame.contains("more above"),
        "the moved window admits what scrolled past:\n{frame}"
    );
    assert_golden(
        "zoom_scrolled",
        "the task zoom paged down on a short terminal — the window moved, admission at the top",
        w,
        h,
        &frame,
    );
}

#[test]
fn deck_render_snapshots_pin_the_task_zoom() {
    use stella_tui::deck_ui::cards::Card;

    let (board, lanes, ledger) = stella_tui::scenario::demo_task_zoom();
    let mut model = fixture_model();
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::TaskUpdate { tasks: board },
    });
    // Assigned rather than folded: no event carries a plan graph or a task id
    // yet — that is the whole of #5037 and #5039, and the reason the second
    // frame below exists.
    let lead = &mut model.agents[0].model.plan;
    lead.lanes = Some(lanes);
    lead.ledger.insert("5".to_string(), ledger);

    let mut ui = ui_for(DeckTab::Session);
    ui.cards.raise(Card::TaskZoom);
    // Task 5 — the board's in-progress row, and the one the fixture contracted.
    ui.cards.plan_sel = 4;
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "zoom_scripted",
        "the task zoom over a scripted task: contract, evidence, lanes, spend",
        W,
        H,
        &frame,
    );

    let model = fixture_model();
    let mut ui = ui_for(DeckTab::Session);
    ui.cards.raise(Card::TaskZoom);
    ui.cards.plan_sel = 4;
    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "zoom_elided",
        "the task zoom with no contract, ledger or plan graph — every block names why",
        W,
        H,
        &frame,
    );
}
