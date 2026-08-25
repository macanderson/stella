// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The GRAPH tab's query bar, pinned frame by frame.
//!
//! A submodule of `deck_render_snapshots` rather than more lines in it: the
//! parent is the whole deck's golden surface and every tab wants rows there,
//! so it is the file that reaches the 1500-line ceiling first. The tab's
//! three bar states -- a measured query time, a neighborhood that answers a
//! free-form query, and the query box mid-type -- belong together anyway.
//!
//! Every helper comes from the parent, so the goldens live in one directory
//! and are blessed by one command:
//! `BLESS=1 cargo test -p stella-tui --test deck_render_snapshots`.

use super::*;

/// **The witness (#4335).** The GRAPH query bar reports what the query cost
/// when the driver measured one.
///
/// A second golden rather than a changed `tab_graph`: the demo snapshot is
/// synthesized and carries no timing, and that is the state the bar must keep
/// rendering — `0ms` on a query nobody ran would be worse than silence. So
/// both halves are pinned, the untimed one by `tab_graph` and the timed one
/// here.
#[test]
fn deck_render_snapshots_pin_the_graph_query_time() {
    let model = fixture_model();
    let mut ui = ui_for(DeckTab::Graph);
    if let Some(graph) = ui.graph.as_mut() {
        graph.query_ms = Some(12);
    }
    let frame = render_frame(&model, &mut ui, W, H);
    assert!(
        frame.contains("· 12ms ·"),
        "the query bar reports the timing:\n{frame}"
    );
    assert_golden(
        "tab_graph_timed",
        "the GRAPH tab with a measured query time in the query bar",
        W,
        H,
        &frame,
    );
}

/// **The witness (#4335).** The GRAPH query bar's second mode: a
/// neighborhood that answers a free-form query reads `q:<text>`, not
/// `file:<focus>`, so it names which of the two ways of re-rooting produced
/// what is on screen.
#[test]
fn deck_render_snapshots_pin_the_graph_free_form_query() {
    let model = fixture_model();
    let mut ui = ui_for(DeckTab::Graph);
    if let Some(graph) = ui.graph.as_mut() {
        graph.query = Some("run_turn".into());
        graph.query_ms = Some(12);
    }
    let frame = render_frame(&model, &mut ui, W, H);
    assert!(
        frame.contains("q:run_turn"),
        "the bar reads the query it answers:\n{frame}"
    );
    assert!(
        !frame.contains("file:"),
        "and not the file selector it is standing in for:\n{frame}"
    );
    assert_golden(
        "tab_graph_query",
        "the GRAPH tab rooted on a free-form query rather than a file",
        W,
        H,
        &frame,
    );
}

/// The query box while it is being typed: the bar echoes the buffer with a
/// caret, and the footer swaps the tab's keys for the box's, because those
/// are the only ones that do anything while it is up.
#[test]
fn deck_render_snapshots_pin_the_graph_query_box() {
    let model = fixture_model();
    let mut ui = ui_for(DeckTab::Graph);
    ui.graph_query = Some("run_tu".into());
    let frame = render_frame(&model, &mut ui, W, H);
    assert!(frame.contains("q:run_tu"), "{frame}");
    assert!(frame.contains("run query"), "{frame}");
    assert_golden(
        "tab_graph_query_box",
        "the GRAPH tab with the free-form query box open mid-type",
        W,
        H,
        &frame,
    );
}
