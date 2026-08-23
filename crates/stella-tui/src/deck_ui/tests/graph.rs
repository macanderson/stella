//! The GRAPH tab's file picker: `/` and `⏎` open it, typing filters it, `⏎`
//! re-roots the neighborhood on the selected file.
//!
//! The installed-agents browser used to live here too; it is in `agents.rs`
//! now, named for the tab whose keys it presses (#4429).

use super::*;

/// A three-file graph rooted on the busiest (`src/b.rs`), on the Graph tab.
fn ui_with_graph() -> DeckUi {
    use crate::graph::{GraphNode, GraphSnapshot};
    let mut ui = ready_ui();
    ui.tab = DeckTab::Graph;
    ui.graph = Some(GraphSnapshot {
        focus: "src/b.rs".into(),
        nodes: vec![GraphNode {
            label: "src/b.rs".into(),
            kind: "file".into(),
            location: Some("src/b.rs".into()),
        }],
        edges: vec![],
        files: vec!["src/a.rs".into(), "src/b.rs".into(), "src/c.rs".into()],
    });
    ui
}

#[test]
fn slash_opens_the_picker_defaulting_to_the_current_focus() {
    let model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    let action = handle_deck_key(ch('/'), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled);
    assert!(ui.graph_picker_open, "/ opens the picker on the Graph tab");
    // Default selection is the busiest/focused file (index 1 = src/b.rs),
    // the sensible default — not forced there, just pre-selected.
    assert_eq!(ui.graph_picker_sel, 1);
    assert!(
        ui.composer.buffer().is_empty(),
        "/ did not leak into the prompt"
    );
}

#[test]
fn enter_also_opens_the_picker() {
    let model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert!(ui.graph_picker_open);
}

#[test]
fn typing_filters_and_re_anchors_the_selection() {
    let model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    open_graph_picker(&mut ui);
    // Filter to just "a.rs" — one match, selection re-anchors to 0.
    handle_deck_key(ch('a'), &model, &mut ui);
    assert_eq!(ui.graph_picker_query, "a");
    assert_eq!(ui.graph_picker_sel, 0);
    let matches = ui
        .graph
        .as_ref()
        .unwrap()
        .matching_files(&ui.graph_picker_query);
    assert_eq!(matches, vec!["src/a.rs"]);
}

#[test]
fn enter_in_the_picker_re_roots_on_the_selected_file() {
    let model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    open_graph_picker(&mut ui);
    // A multi-char needle (`c.rs`) narrows to exactly src/c.rs — a bare
    // `c` would also match the shared `src/` prefix of every file.
    for c in "c.rs".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::FocusGraphFile {
            file: "src/c.rs".into()
        }),
        "Enter sends the re-root request for the filtered selection"
    );
    assert!(!ui.graph_picker_open, "the picker closes on selection");
}

#[test]
fn down_arrow_walks_the_filtered_matches_and_clamps() {
    let model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    open_graph_picker(&mut ui);
    ui.graph_picker_sel = 0;
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    handle_deck_key(key(KeyCode::Down), &model, &mut ui); // past the end
    assert_eq!(ui.graph_picker_sel, 2, "clamps to the last of three files");
}

#[test]
fn esc_closes_the_picker_without_re_rooting() {
    let model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    open_graph_picker(&mut ui);
    let action = handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled);
    assert!(!ui.graph_picker_open);
}

#[test]
fn the_picker_is_modal_over_the_composer() {
    // A printable key while the picker is open filters — it must NOT type
    // into the global composer (the queue-editor modality contract).
    let model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    open_graph_picker(&mut ui);
    handle_deck_key(ch('b'), &model, &mut ui);
    assert_eq!(ui.graph_picker_query, "b");
    assert!(
        ui.composer.buffer().is_empty(),
        "filter keys never reach the composer"
    );
}

#[test]
fn a_re_rooted_snapshot_resets_the_node_cursor() {
    let mut model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    ui.graph_cursor = 5; // stale cursor from the previous neighborhood
    use crate::graph::{GraphNode, GraphSnapshot};
    let rerooted = GraphSnapshot {
        focus: "src/a.rs".into(),
        nodes: vec![GraphNode {
            label: "src/a.rs".into(),
            kind: "file".into(),
            location: Some("src/a.rs".into()),
        }],
        edges: vec![],
        files: vec!["src/a.rs".into(), "src/b.rs".into(), "src/c.rs".into()],
    };
    ingest_inbound(&Inbound::GraphSnapshot(rerooted), &mut model, &mut ui);
    assert_eq!(ui.graph_cursor, 0, "the cursor lands on the new focus");
}

/// **The witness (#4368).** `↑ ↓` walk the neighborhood list itself — the
/// keymap row that had no test: every arrow witness on this tab pressed keys
/// inside the *picker*, which is a different list behind a modal.
#[test]
fn arrows_walk_the_neighborhood_and_clamp_at_both_ends() {
    use crate::graph::GraphNode;
    let model = model_with(&["lead"]);
    let mut ui = ui_with_graph();
    let node = |label: &str| GraphNode {
        label: label.into(),
        kind: "file".into(),
        location: Some(label.into()),
    };
    if let Some(graph) = ui.graph.as_mut() {
        graph.nodes = vec![node("src/a.rs"), node("src/b.rs"), node("src/c.rs")];
    }

    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(ui.graph_cursor, 1);
    for _ in 0..5 {
        handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    }
    assert_eq!(ui.graph_cursor, 2, "clamps at the last neighbor");
    for _ in 0..5 {
        handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    }
    assert_eq!(ui.graph_cursor, 0, "and at the first");

    // `j`/`k` are the same step, and — unlike the arrows, which are never
    // gated (`deck_ui::list_nav`) — they build a prompt while one is being
    // typed rather than walking the list under it.
    handle_deck_key(ch('j'), &model, &mut ui);
    assert_eq!(ui.graph_cursor, 1, "j is ↓");
    handle_deck_key(ch('z'), &model, &mut ui);
    handle_deck_key(ch('j'), &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "zj");
    assert_eq!(ui.graph_cursor, 1, "the neighborhood stayed where it was");
}

#[test]
fn the_picker_does_not_open_without_a_loaded_graph() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.tab = DeckTab::Graph; // no snapshot loaded
    handle_deck_key(ch('/'), &model, &mut ui);
    assert!(!ui.graph_picker_open, "nothing to pick from — stays closed");
}
