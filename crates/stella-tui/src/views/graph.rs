//! Graph tab — the code-graph neighborhood inspector.
//!
//! Renders from [`DeckUi::graph`], the out-of-band [`crate::graph::GraphSnapshot`] (see
//! `crate::graph` module docs — it is not folded from the `AgentEvent` log),
//! plus the focused lane's file ledger for the `● hot` mark. The drawing is
//! [`crate::v2::graph_tab`] (SPEC 9.1); this module keeps the empty state and
//! the cursor clamp, and its tests pin the tab through the deck's frozen
//! `render` signature.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;
use crate::theme;

pub fn render(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    let Some(snapshot) = ui.graph.as_ref().filter(|g| !g.is_empty()) else {
        // An empty snapshot that carries a query is a query that matched
        // nothing, which is a different fact from "no index here" and gets
        // its own sentence — otherwise a search for a misspelt symbol reads
        // as advice to run `stella init` (#4335).
        let hint = match ui.graph.as_ref().and_then(|g| g.query.as_deref()) {
            Some(query) => format!("nothing in the index matches `{query}`"),
            None => "no neighborhood loaded — the code graph appears here".to_string(),
        };
        render_empty(&hint, area, buf);
        return;
    };

    // Defensive clamp: the deck's key handler (`deck_ui::graph`) already
    // keeps `graph_cursor` in range on every keypress, but this view must
    // never index out of bounds regardless of how the cursor got here (a
    // fresh `DeckUi`, a test, a snapshot swapped out from under a stale
    // cursor).
    let cursor = ui.graph_cursor.min(snapshot.nodes.len() - 1);
    ui.graph_cursor = cursor;

    // The files this session changed, for the `● hot` mark.
    let changed: Vec<String> = model
        .agents
        .get(ui.focused)
        .map(|a| a.model.files.iter().map(|f| f.path.clone()).collect())
        .unwrap_or_default();
    crate::v2::graph_tab::render(
        snapshot,
        cursor,
        &changed,
        ui.accessible,
        ui.graph_query.as_deref(),
        area,
        buf,
    );
}

/// The "nothing to draw" state: one centered muted `hint`, no border chrome
/// beyond the tab's own frame.
fn render_empty(hint: &str, area: Rect, buf: &mut Buffer) {
    let block = Block::default().borders(Borders::ALL).title(" Graph ");
    let inner = block.inner(area);
    block.render(area, buf);
    // A 1–2 row tab body leaves no interior at all, and `inner.y` is then one
    // past the block — drawing the hint there would target a row outside the
    // buffer. Same guard the Files tab's empty state carries.
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let line =
        Line::from(Span::styled(hint.to_string(), theme::muted())).alignment(Alignment::Center);

    // Vertically center the single line (mirrors the splash's centering idiom
    // — this crate doesn't carry a generic `centered_rect` helper).
    let mid = inner.height / 2;
    let row = Rect {
        x: inner.x,
        y: inner.y + mid,
        width: inner.width,
        height: inner.height.saturating_sub(mid).max(1),
    };
    Paragraph::new(line).render(row, buf);
}

pub use crate::v2::graph_tab::passive;

#[cfg(test)]
// The lint is wrong here: these fixtures build with `Type::default()` and
// then set the few fields the test cares about, which reads better than a
// full struct literal that lists every field.
#[allow(clippy::field_reassign_with_default)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::graph::{GraphEdge, GraphNode, GraphSnapshot};

    /// Flatten a `TestBackend` buffer to plain text (styling stripped — L-T6
    /// convention shared with `render.rs`'s tests: assert on content, not
    /// ANSI).
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
            query_ms: None,
            query: None,
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
        let mut ui = DeckUi::default();
        ui.graph = Some(small_snapshot());
        ui.graph_cursor = 1; // the `run` function

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
        let mut ui = DeckUi::default();
        ui.graph = Some(small_snapshot());
        ui.graph_cursor = 999; // stale/out-of-range cursor

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
        let mut ui = DeckUi::default();
        ui.graph = Some(small_snapshot());
        ui.graph_cursor = 0; // driver.rs has one outgoing edge; use a truly isolated node instead
        ui.graph = Some(GraphSnapshot {
            focus: "orphan".into(),
            nodes: vec![GraphNode {
                label: "orphan_fn".into(),
                kind: "function".into(),
                location: None,
            }],
            edges: vec![],
            files: vec![],
            query_ms: None,
            query: None,
        });
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
        let snapshot = GraphSnapshot {
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
            query_ms: None,
            query: None,
        };
        let mut ui = DeckUi::default();
        ui.graph = Some(snapshot);
        ui.graph_cursor = n - 1;

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
}
