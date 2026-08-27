// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The GRAPH tab's query bar and its session tags, pinned frame by frame.
//!
//! A submodule of `deck_render_snapshots` rather than more lines in it: the
//! parent is the whole deck's golden surface and every tab wants rows there,
//! so it is the file that reaches the 1500-line ceiling first. The tab's
//! three bar states -- a measured query time, a neighborhood that answers a
//! free-form query, and the query box mid-type -- belong together anyway, and
//! the session tag is read off the same frames.
//!
//! Every helper comes from the parent, so the goldens live in one directory
//! and are blessed by one command:
//! `BLESS=1 cargo test -p stella-tui --test deck_render_snapshots`.

use super::*;

/// The demo session, plus one edit to a file the demo neighborhood contains.
///
/// `demo_inbound` touches the scripted app's own files, and the demo graph is
/// a neighborhood of `stella-core` -- the two never intersect, which is what
/// keeps `tab_graph` free of `● hot` marks. Editing `router.rs` puts one node
/// of that neighborhood in the ledger so the tag has something to name.
fn model_that_edited_router() -> WorkspaceModel {
    let mut model = fixture_model();
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::FileChange {
            path: "stella-core/src/router.rs".into(),
            kind: FileChangeKind::Modified,
            added: 12,
            removed: 3,
            diff: None,
            minimal: true,
            task_id: None,
        },
    });
    model
}

/// **The witness (#5045).** An edge whose file this session edited names the
/// turn that did it, on the node card, in the deck's own frame.
///
/// SPEC 9.1: "Reverse edges may carry session tags (`edited turn 14`)." Before
/// this, `GraphNode` had no turn to carry and `render_card` drew every edge as
/// a bare relation, so the tag was structurally unreachable rather than merely
/// switched off.
#[test]
fn deck_render_snapshots_pin_the_graph_session_touch_tag() {
    let model = model_that_edited_router();
    let mut ui = ui_for(DeckTab::Graph);
    if let Some(graph) = ui.graph.as_mut() {
        graph.query_ms = Some(12);
    }
    let frame = render_frame(&model, &mut ui, W, H);
    assert!(
        frame.contains("uses → Router · edited turn 1"),
        "the edge citing the edited file names the turn:\n{frame}"
    );
    assert!(
        frame.contains("writes → Ledger") && !frame.contains("Ledger · edited"),
        "a neighbor the session never touched carries no tag:\n{frame}"
    );
    assert!(
        frame.contains("● hot"),
        "and the same ledger row marks the node hot:\n{frame}"
    );
    assert_golden(
        "tab_graph_touched",
        "the GRAPH tab with a session touch tag on an edge to an edited file",
        W,
        H,
        &frame,
    );
}

/// **The witness (#5220).** The hot mark's ladder, in the deck's own frames,
/// at the two widths where it changes rung.
///
/// The wide frame is `tab_graph_touched` above, which carries the full
/// `● hot · turn 1`. These are the other two, and they exist because the
/// ladder is a *rendering* decision: `hot_mark`'s unit pins fix the arithmetic,
/// and only a frame shows what the row looks like once the mark has given
/// ground — that the label kept its columns, and that the list still reads as a
/// list.
///
/// 80 columns is the width the issue names: the left pane is 36% of it, which
/// is where the full tag would eat most of the label.
#[test]
fn deck_render_snapshots_pin_the_hot_mark_as_the_pane_narrows() {
    let model = model_that_edited_router();

    let mut ui = ui_for(DeckTab::Graph);
    let narrow = render_frame(&model, &mut ui, 80, H);
    assert!(
        narrow.contains("● hot 1") && !narrow.contains("● hot · turn 1"),
        "at 80 columns the separator goes and the turn stays:\n{narrow}"
    );
    assert_golden(
        "tab_graph_touched_80",
        "the GRAPH tab at 80 columns: the hot mark drops its separator, not the turn",
        80,
        H,
        &narrow,
    );

    let mut ui = ui_for(DeckTab::Graph);
    let narrower = render_frame(&model, &mut ui, 64, H);
    assert!(
        narrower.contains("● hot") && !narrower.contains("● hot 1"),
        "and below that the turn goes too, leaving the mark #5220 started from:\n{narrower}"
    );
    assert_golden(
        "tab_graph_touched_64",
        "the GRAPH tab at 64 columns: the mark keeps the label's columns and drops the turn",
        64,
        H,
        &narrower,
    );
}

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
