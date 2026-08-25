//! The GRAPH tab through the deck's frozen `render` signature: the empty
//! state, the cursor clamp, and the relations a reader is meant to be able to
//! read off the panel.
//!
//! These pin the tab from the outside — a `DeckUi` and a `WorkspaceModel` in,
//! a drawn frame out — which is the only level at which the clamp and the
//! empty state are observable at all.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use super::*;
use crate::graph::{GraphEdge, GraphNode, GraphSnapshot};

/// Flatten a `TestBackend` buffer to plain text (styling stripped — L-T6
/// convention shared with `render.rs`'s tests: assert on content, not ANSI).
fn buffer_text(buf: &Buffer) -> String {
    let area = *buf.area();
    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn small_snapshot() -> GraphSnapshot {
    // driver.rs --calls--> run() --imports--> serde
    GraphSnapshot {
        focus: "run".into(),
        nodes: vec![
            GraphNode {
                label: "driver.rs".into(),
                kind: "file".into(),
                location: None,
            },
            GraphNode {
                label: "run".into(),
                kind: "function".into(),
                location: Some("src/lib.rs:42".into()),
            },
            GraphNode {
                label: "serde".into(),
                kind: "module".into(),
                location: None,
            },
        ],
        edges: vec![
            GraphEdge {
                from: 0,
                to: 1,
                kind: "calls".into(),
            },
            GraphEdge {
                from: 1,
                to: 2,
                kind: "imports".into(),
            },
        ],
        files: vec!["driver.rs".into(), "src/lib.rs".into()],
        query: None,
        query_ms: None,
    }
}

fn draw(ui: &mut DeckUi, w: u16, h: u16) -> String {
    let model = WorkspaceModel::new();
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|f| {
            let area = f.area();
            render(&model, ui, area, f.buffer_mut());
        })
        .unwrap();
    buffer_text(terminal.backend().buffer())
}

#[test]
fn empty_snapshot_shows_the_muted_hint() {
    let mut ui = DeckUi::default();
    let text = draw(&mut ui, 100, 24);
    assert!(
        text.contains("no neighborhood loaded"),
        "empty hint shown:\n{text}"
    );

    // Same for `Some(GraphSnapshot::default())` (present but empty).
    ui.graph = Some(GraphSnapshot::default());
    let text = draw(&mut ui, 100, 24);
    assert!(
        text.contains("no neighborhood loaded"),
        "empty-but-present snapshot also shows the hint:\n{text}"
    );
}

#[test]
fn cursor_node_shows_label_kind_location_and_edges_as_human_relations() {
    let mut ui = DeckUi {
        graph: Some(small_snapshot()),
        graph_cursor: 1, // the `run` function
        ..DeckUi::default()
    };

    let text = draw(&mut ui, 100, 24);

    // The node list cites nodes by human label — never a raw id/index.
    assert!(text.contains("driver.rs"), "list shows driver.rs:\n{text}");
    assert!(text.contains("run"), "list shows run:\n{text}");
    assert!(text.contains("serde"), "list shows serde:\n{text}");

    // Detail panel: focus title, label, kind, location.
    assert!(text.contains("src/lib.rs:42"), "shows location:\n{text}");
    assert!(text.contains("function"), "shows kind:\n{text}");

    // Incident edges as human relations, citing the *other* node's label.
    assert!(
        text.contains("imports → serde"),
        "outgoing relation:\n{text}"
    );
    assert!(
        text.contains("called by ← driver.rs"),
        "incoming relation in passive form:\n{text}"
    );
}

#[test]
fn cursor_clamps_to_the_node_range_instead_of_panicking() {
    let mut ui = DeckUi {
        graph: Some(small_snapshot()),
        graph_cursor: 999, // stale/out-of-range cursor
        ..DeckUi::default()
    };

    let text = draw(&mut ui, 100, 24);
    assert!(
        text.contains("serde"),
        "clamps to the last node (index 2) and renders it:\n{text}"
    );
    assert_eq!(
        ui.graph_cursor, 2,
        "render() writes the clamped cursor back"
    );
}

#[test]
fn a_node_with_no_edges_says_so_instead_of_an_empty_list() {
    let mut ui = DeckUi {
        graph: Some(GraphSnapshot {
            focus: "orphan".into(),
            nodes: vec![GraphNode {
                label: "orphan_fn".into(),
                kind: "function".into(),
                location: None,
            }],
            edges: vec![],
            files: vec![],
            query: None,
            query_ms: None,
        }),
        ..DeckUi::default()
    };
    let text = draw(&mut ui, 100, 24);
    assert!(
        text.contains("no known relations"),
        "zero-degree node says so explicitly:\n{text}"
    );
}

#[test]
fn node_list_windows_to_keep_the_cursor_visible() {
    // Far more nodes than a short terminal can list, cursor on the last
    // one: the window must slide so the selection stays on screen.
    let n = 40;
    let mut ui = DeckUi {
        graph: Some(GraphSnapshot {
            focus: "big".into(),
            nodes: (0..n)
                .map(|i| GraphNode {
                    label: format!("node_{i:02}"),
                    kind: "function".into(),
                    location: None,
                })
                .collect(),
            edges: vec![],
            files: vec![],
            query: None,
            query_ms: None,
        }),
        graph_cursor: n - 1,
        ..DeckUi::default()
    };

    let text = draw(&mut ui, 100, 12);
    assert!(
        text.contains("node_39"),
        "the cursor node scrolled into view:\n{text}"
    );
    assert!(
        !text.contains("node_00"),
        "the head of the list scrolled out of the window:\n{text}"
    );
}

#[test]
fn passive_form_covers_the_documented_edge_kinds() {
    assert_eq!(passive("imports"), "imported by");
    assert_eq!(passive("calls"), "called by");
    assert_eq!(passive("defines"), "defined by");
    assert_eq!(passive("references"), "referenced by");
}
