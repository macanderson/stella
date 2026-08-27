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
                touch: None,
            },
            GraphNode {
                label: "run".into(),
                kind: "function".into(),
                location: Some("src/lib.rs:42".into()),
                touch: None,
            },
            GraphNode {
                label: "serde".into(),
                kind: "module".into(),
                location: None,
                touch: None,
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
    draw_over(&WorkspaceModel::new(), ui, w, h)
}

/// `draw`, over a session that has already done something. The tab reads the
/// focused lane's file ledger for its `● hot` marks and turn tags, so those
/// need a model with a lane in it.
fn draw_over(model: &WorkspaceModel, ui: &mut DeckUi, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|f| {
            let area = f.area();
            render(model, ui, area, f.buffer_mut());
        })
        .unwrap();
    buffer_text(terminal.backend().buffer())
}

/// A one-lane session that edited `path` in the turn after `completed_turns`
/// have finished.
fn session_that_edited(path: &str, completed_turns: u32) -> WorkspaceModel {
    use crate::envelope::{AgentMeta, Inbound};
    use stella_protocol::{AgentEvent, FileChangeKind};

    let mut model = WorkspaceModel::new();
    model.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
    for _ in 0..completed_turns {
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::TurnComplete {
                model: "test".into(),
                cost_usd: 0.0,
            },
        });
    }
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::FileChange {
            path: path.into(),
            kind: FileChangeKind::Modified,
            added: 4,
            removed: 1,
            diff: None,
            minimal: true,
            task_id: None,
        },
    });
    model
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
                touch: None,
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
                    touch: None,
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

/// **The witness (#5045).** An edge whose file this session edited says so on
/// the node card, in the shape SPEC 9.1 writes: `· edited turn 14`.
///
/// Fails before the session tag existed, when `GraphNode` had no turn to carry
/// and `render_card` drew every edge as a bare relation — the tag was
/// structurally unreachable, not merely switched off.
///
/// Thirteen turns are completed before the edit, so the number under test is
/// 14 rather than 1: a tag that hard-coded a turn, or counted completed turns
/// instead of the turn in flight, renders a different string here.
#[test]
fn an_edge_whose_file_was_edited_this_session_names_the_turn_that_did_it() {
    let model = session_that_edited("src/lib.rs", 13);
    let mut ui = DeckUi {
        graph: Some(small_snapshot()),
        graph_cursor: 0, // driver.rs, whose `calls` edge points at `run`
        ..DeckUi::default()
    };

    let text = draw_over(&model, &mut ui, 100, 24);

    // `run` is defined in the edited file, so the edge citing it carries the
    // turn — beside the relation, never instead of it.
    assert!(
        text.contains("calls → run · edited turn 14"),
        "the edge names the turn that touched its file:\n{text}"
    );
    // `serde` is not in the ledger, so its edge stays a bare relation: the tag
    // marks what moved, and marking everything would mark nothing.
    assert!(
        !text.contains("serde · edited"),
        "an untouched neighbor carries no tag:\n{text}"
    );
    // And the same ledger row still drives the `● hot` mark it always did.
    assert!(
        text.contains("● hot"),
        "the node is marked hot too:\n{text}"
    );
}

/// A path the ledger kept across `/clear` was touched by a turn numbering the
/// session no longer has, so the node stays hot and names no turn — rather than
/// naming a turn that means nothing.
#[test]
fn a_touch_from_before_a_conversation_reset_marks_hot_without_a_turn() {
    let mut model = session_that_edited("src/lib.rs", 13);
    for agent in &mut model.agents {
        agent.model.reset_conversation();
    }
    let mut ui = DeckUi {
        graph: Some(small_snapshot()),
        graph_cursor: 0,
        ..DeckUi::default()
    };

    let text = draw_over(&model, &mut ui, 100, 24);

    assert!(
        text.contains("calls → run"),
        "the relation is still drawn:\n{text}"
    );
    assert!(
        !text.contains("edited turn"),
        "with no turn to name, it names none:\n{text}"
    );
    assert!(
        text.contains("● hot"),
        "the touch itself survived the reset:\n{text}"
    );
}

/// SPEC 9.1's footer prices the view *and* states what the answer cost. The
/// timing is drawn only when the producer measured one, matching the query
/// bar's rule (#4335) — `0ms` on a query nobody ran is a fabricated number.
#[test]
fn the_footer_carries_the_query_time_only_when_the_producer_measured_one() {
    let mut ui = DeckUi {
        graph: Some(small_snapshot()),
        ..DeckUi::default()
    };
    let untimed = draw(&mut ui, 100, 24);
    assert!(
        untimed.contains("every answer here is deterministic · $0.00"),
        "the price line is there:\n{untimed}"
    );
    assert!(
        !untimed.contains("$0.00 · "),
        "a snapshot nobody timed reports no duration:\n{untimed}"
    );

    if let Some(graph) = ui.graph.as_mut() {
        graph.query_ms = Some(12);
    }
    let timed = draw(&mut ui, 100, 24);
    assert!(
        timed.contains("every answer here is deterministic · $0.00 · 12ms"),
        "a measured query is priced in both money and time:\n{timed}"
    );
}

#[test]
fn passive_form_covers_the_documented_edge_kinds() {
    assert_eq!(passive("imports"), "imported by");
    assert_eq!(passive("calls"), "called by");
    assert_eq!(passive("defines"), "defined by");
    assert_eq!(passive("references"), "referenced by");
}
