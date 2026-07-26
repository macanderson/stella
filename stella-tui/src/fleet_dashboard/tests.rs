//! Buffer-not-ANSI witness tests for the fleet live dashboard (L-T6): assert on
//! the flattened cell content the grid draws, never raw styling.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use stella_protocol::{AgentEvent, FileChangeKind, ToolCall};

use super::*;

/// Flatten a `Buffer` to one `String` per row (styling stripped).
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

fn seed() -> Vec<(String, String)> {
    vec![
        ("t1".into(), "auth-refactor".into()),
        ("t2".into(), "fix-flaky-retry".into()),
        ("t3".into(), "docs-quickstart".into()),
    ]
}

fn tool_start(name: &str, arg_key: &str, arg: &str) -> AgentEvent {
    AgentEvent::ToolStart {
        call: ToolCall {
            call_id: "c1".into(),
            name: name.into(),
            input: serde_json::json!({ arg_key: arg }),
        },
    }
}

fn draw(board: &FleetBoard, view: &FleetView, w: u16, h: u16) -> String {
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);
    render(board, view, Instant::now(), area, &mut buf);
    buffer_text(&buf)
}

#[test]
fn grid_shows_tasks_statuses_last_action_and_header_clocks() {
    let now = Instant::now();
    let mut board = FleetBoard::new("stella", &seed(), now);
    let mut view = FleetView::new(&board.rows);

    // t1 dispatched, edits a file.
    board.apply(
        FleetMsg::Status {
            id: "t1".into(),
            status: FleetStatus::Running,
        },
        Instant::now(),
    );
    board.apply(
        FleetMsg::Event {
            id: "t1".into(),
            event: tool_start("edit_file", "path", "src/auth/session.rs"),
        },
        Instant::now(),
    );
    board.apply(
        FleetMsg::Event {
            id: "t1".into(),
            event: AgentEvent::FileChange {
                path: "src/auth/session.rs".into(),
                kind: FileChangeKind::Modified,
                diff: Some("@@\n+a\n+b\n-c\n".into()),
            },
        },
        Instant::now(),
    );
    // t3 finished.
    board.apply(
        FleetMsg::Status {
            id: "t3".into(),
            status: FleetStatus::Done,
        },
        Instant::now(),
    );

    let text = draw(&board, &view, 110, 24);

    // Header + both clock labels.
    assert!(text.contains("FLEET"), "fleet header:\n{text}");
    assert!(text.contains("SESSION"), "session clock:\n{text}");
    assert!(text.contains("FLEET-IDLE"), "idle clock:\n{text}");
    // Grid headers.
    assert!(text.contains("LAST ACTION"), "grid headers:\n{text}");
    assert!(text.contains("TOOL-AGO"), "grid headers:\n{text}");
    // Rows.
    assert!(text.contains("auth-refactor"), "t1 title:\n{text}");
    assert!(text.contains("fix-flaky-retry"), "t2 title:\n{text}");
    // Last action carries the tool + path + line delta.
    assert!(text.contains("Edit"), "edit verb:\n{text}");
    assert!(text.contains("+2-1"), "line delta in last action:\n{text}");
    // Statuses.
    assert!(text.contains("done"), "t3 done:\n{text}");
    assert!(text.contains("queued"), "t2 still queued:\n{text}");
    // A finished task's tool-ago reads as dashes.
    assert!(text.contains("----"), "terminal tool-ago dashes:\n{text}");

    // Keep `view` mutable-use honest for the borrow checker across draws.
    view.sort = view.sort.next();
    let _ = draw(&board, &view, 110, 24);
}

#[test]
fn last_action_holds_tool_while_reasoning_but_thinks_before_any_action() {
    let now = Instant::now();
    let mut board = FleetBoard::new("stella", &seed(), now);

    // t2 only reasoned → shows "thinking…".
    board.apply(
        FleetMsg::Event {
            id: "t2".into(),
            event: AgentEvent::Reasoning {
                delta: "hmm".into(),
            },
        },
        Instant::now(),
    );
    let t2 = board.rows.iter().find(|r| r.id == "t2").unwrap();
    assert_eq!(t2.action_text(), "thinking…");

    // t1 ran a tool, then reasoned → the tool line is held, not "thinking…".
    board.apply(
        FleetMsg::Event {
            id: "t1".into(),
            event: tool_start("bash", "command", "cargo test retry_loop"),
        },
        Instant::now(),
    );
    board.apply(
        FleetMsg::Event {
            id: "t1".into(),
            event: AgentEvent::Reasoning {
                delta: "now what".into(),
            },
        },
        Instant::now(),
    );
    let t1 = board.rows.iter().find(|r| r.id == "t1").unwrap();
    assert!(
        t1.action_text().contains("Bash") && t1.action_text().contains("cargo test"),
        "held tool action: {:?}",
        t1.action_text()
    );
    assert_eq!(t1.tool_calls, 1);
}

#[test]
fn default_sort_is_blocked_then_running_then_queued_then_finished() {
    let now = Instant::now();
    let mut board = FleetBoard::new(
        "stella",
        &[
            ("done".into(), "d".into()),
            ("run".into(), "r".into()),
            ("block".into(), "b".into()),
            ("queue".into(), "q".into()),
        ],
        now,
    );
    board.apply(
        FleetMsg::Status {
            id: "done".into(),
            status: FleetStatus::Done,
        },
        Instant::now(),
    );
    board.apply(
        FleetMsg::Status {
            id: "run".into(),
            status: FleetStatus::Running,
        },
        Instant::now(),
    );
    board.apply(
        FleetMsg::Event {
            id: "block".into(),
            event: AgentEvent::AskUser {
                id: "q1".into(),
                question: "approve write?".into(),
                options: vec![],
            },
        },
        Instant::now(),
    );

    let order = display_order(&board, SortKey::Default, Instant::now());
    let ids: Vec<&str> = order.iter().map(|&i| board.rows[i].id.as_str()).collect();
    assert_eq!(ids, vec!["block", "run", "queue", "done"], "default order");
}

#[test]
fn focus_survives_resort_by_id_pinning() {
    let now = Instant::now();
    let board = FleetBoard::new("stella", &seed(), now);
    let mut view = FleetView::new(&board.rows);
    view.focused_id = Some("t2".into());
    // Move down then back up — focus is chased by id, not by position.
    move_focus(&board, &mut view, Instant::now(), 1);
    move_focus(&board, &mut view, Instant::now(), -1);
    assert_eq!(view.focused_id.as_deref(), Some("t2"));
}

#[test]
fn clock_formats_match_the_spec() {
    assert_eq!(fmt_hms(Duration::from_secs(14 * 60 + 32)), "00:14:32");
    assert_eq!(fmt_hms(Duration::from_secs(3661)), "01:01:01");
    assert_eq!(fmt_ms(Duration::from_secs(4 * 60 + 21)), "04:21");
    assert_eq!(fmt_ms(Duration::from_secs(11 * 60 + 3)), "11:03");
    assert_eq!(fmt_clock(Duration::from_secs(3)), "0:03");
    assert_eq!(fmt_clock(Duration::from_secs(130)), "2:10");
}

#[test]
fn tool_names_and_primary_args_render_for_the_row() {
    assert_eq!(tool_display_name("edit_file"), "Edit");
    assert_eq!(tool_display_name("bash"), "Bash");
    assert_eq!(tool_display_name("read_file"), "Read");
    assert_eq!(tool_display_name("graph_query"), "Graph");

    assert_eq!(
        primary_arg(&serde_json::json!({ "path": "src/x.rs" })).as_deref(),
        Some("src/x.rs")
    );
    assert_eq!(
        primary_arg(&serde_json::json!({ "command": "cargo build" })).as_deref(),
        Some("cargo build")
    );
    // Falls back to the first string value when no well-known key is present.
    assert_eq!(
        primary_arg(&serde_json::json!({ "weird": "value" })).as_deref(),
        Some("value")
    );
    assert!(primary_arg(&serde_json::json!({ "n": 3 })).is_none());
}

#[test]
fn terminal_status_freezes_elapsed_and_wins_over_late_events() {
    let start = Instant::now();
    let mut board = FleetBoard::new("stella", &[("t1".into(), "x".into())], start);
    board.apply(
        FleetMsg::Status {
            id: "t1".into(),
            status: FleetStatus::Running,
        },
        start,
    );
    let mid = start + Duration::from_secs(5);
    board.apply(
        FleetMsg::Status {
            id: "t1".into(),
            status: FleetStatus::Done,
        },
        mid,
    );
    // A late event must not walk the verdict back to Running.
    board.apply(
        FleetMsg::Event {
            id: "t1".into(),
            event: tool_start("bash", "command", "echo late"),
        },
        start + Duration::from_secs(9),
    );
    let row = &board.rows[0];
    assert_eq!(row.status, FleetStatus::Done);
    // Elapsed is frozen at the 5s mark regardless of `now`.
    assert_eq!(row.elapsed(start + Duration::from_secs(30)).as_secs(), 5);
    // Terminal → tool-ago dashes.
    assert!(row.tool_ago(start + Duration::from_secs(30)).is_none());
}

// ── Supervisor verbs (#645) ─────────────────────────────────────────────────
//
// These drive the whole `[p]`/`[r]`/`[x]` surface as `KeyEvent`s against
// `on_key`, which is why the verbs could be wired without an interactive
// rendering harness: the key handling is a pure fold over the view.

fn key(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
}

/// A board whose `t1` is dispatched and running, focus pinned to it.
fn running_board() -> (FleetBoard, FleetView) {
    let now = Instant::now();
    let board = {
        let mut b = FleetBoard::new("stella", &seed(), now);
        b.apply(
            FleetMsg::Status {
                id: "t1".into(),
                status: FleetStatus::Running,
            },
            now,
        );
        b
    };
    let view = FleetView::new(&board.rows);
    (board, view)
}

#[test]
fn pause_and_resume_emit_verbs_for_the_focused_task() {
    let (board, mut view) = running_board();
    let now = Instant::now();
    assert_eq!(view.focused_id.as_deref(), Some("t1"));

    assert_eq!(
        on_key(key('p'), &board, &mut view, now),
        KeyAction::Control(FleetControl::Pause("t1".into()))
    );
    assert!(view.paused.contains("t1"), "pause is recorded locally");
    assert_eq!(view.marker(&board.rows[0]), Some("paused"));

    assert_eq!(
        on_key(key('r'), &board, &mut view, now),
        KeyAction::Control(FleetControl::Resume("t1".into()))
    );
    assert!(!view.paused.contains("t1"), "resume clears the pause");
    assert_eq!(view.marker(&board.rows[0]), None);
}

#[test]
fn stop_takes_two_keystrokes_and_any_other_key_disarms() {
    let (board, mut view) = running_board();
    let now = Instant::now();

    // First `x` only arms — no verb leaves the dashboard.
    assert_eq!(on_key(key('x'), &board, &mut view, now), KeyAction::None);
    assert_eq!(view.pending_stop.as_deref(), Some("t1"));

    // An unrelated keystroke disarms it.
    assert_eq!(on_key(key('s'), &board, &mut view, now), KeyAction::None);
    assert_eq!(view.pending_stop, None, "sort disarmed the pending stop");

    // Re-arm, then confirm.
    assert_eq!(on_key(key('x'), &board, &mut view, now), KeyAction::None);
    assert_eq!(
        on_key(key('x'), &board, &mut view, now),
        KeyAction::Control(FleetControl::Stop("t1".into()))
    );
    assert_eq!(view.pending_stop, None, "confirming consumes the arm");
    assert_eq!(view.marker(&board.rows[0]), Some("stopping"));
}

#[test]
fn moving_focus_between_the_two_stop_keystrokes_does_not_stop_the_new_task() {
    let (board, mut view) = running_board();
    let now = Instant::now();

    on_key(key('x'), &board, &mut view, now);
    assert_eq!(view.pending_stop.as_deref(), Some("t1"));
    // Focus moves to t2 — the arm must not travel with it.
    on_key(key('j'), &board, &mut view, now);
    assert_eq!(view.focused_id.as_deref(), Some("t2"));
    assert_eq!(view.pending_stop, None);
    // So this `x` arms t2 rather than stopping it.
    assert_eq!(on_key(key('x'), &board, &mut view, now), KeyAction::None);
    assert!(view.stopped.is_empty(), "nothing was stopped");
}

#[test]
fn a_terminal_task_accepts_no_supervisor_verb() {
    let now = Instant::now();
    let mut board = FleetBoard::new("stella", &seed(), now);
    board.apply(
        FleetMsg::Status {
            id: "t1".into(),
            status: FleetStatus::Done,
        },
        now,
    );
    let mut view = FleetView::new(&board.rows);
    assert_eq!(view.focused_id.as_deref(), Some("t1"));

    for c in ['p', 'r', 'x'] {
        assert_eq!(
            on_key(key(c), &board, &mut view, now),
            KeyAction::None,
            "`{c}` on a finished task is declined"
        );
    }
    assert!(view.paused.is_empty() && view.stopped.is_empty());
    assert_eq!(view.pending_stop, None);
}

#[test]
fn a_queued_task_accepts_no_supervisor_verb() {
    // A not-yet-dispatched task has no control lines registered, so `Fleet`
    // would no-op the verb. The dashboard must decline rather than record
    // local intent and paint a `paused`/`stopping` marker that outlives the
    // no-op once the wave dispatches and the worker runs at full speed.
    let now = Instant::now();
    let board = FleetBoard::new("stella", &seed(), now);
    let mut view = FleetView::new(&board.rows);
    assert_eq!(view.focused_id.as_deref(), Some("t1"));
    assert_eq!(board.rows[0].status, FleetStatus::Queued);

    // Two `x` keystrokes: even a fully "confirmed" stop is declined.
    for c in ['p', 'r', 'x', 'x'] {
        assert_eq!(
            on_key(key(c), &board, &mut view, now),
            KeyAction::None,
            "`{c}` on a queued task is declined"
        );
    }
    assert!(view.paused.is_empty() && view.stopped.is_empty());
    assert_eq!(view.pending_stop, None);
    assert_eq!(view.marker(&board.rows[0]), None);
}

#[test]
fn a_task_that_finishes_while_paused_renders_as_finished() {
    let (mut board, mut view) = running_board();
    let now = Instant::now();
    on_key(key('p'), &board, &mut view, now);
    assert_eq!(view.marker(&board.rows[0]), Some("paused"));

    board.apply(
        FleetMsg::Status {
            id: "t1".into(),
            status: FleetStatus::Done,
        },
        now,
    );
    // Fleet truth wins over local supervisor intent.
    assert_eq!(view.marker(&board.rows[0]), None);
    let text = draw(&board, &view, 110, 24);
    assert!(text.contains("done"), "terminal status rendered:\n{text}");
}

#[test]
fn the_grid_and_footer_advertise_the_supervisor_keys() {
    let (board, mut view) = running_board();
    let now = Instant::now();

    let text = draw(&board, &view, 110, 24);
    for hint in ["[p] pause", "[r] resume", "[x] stop"] {
        assert!(text.contains(hint), "footer advertises {hint}:\n{text}");
    }

    // A paused task says so in the grid.
    on_key(key('p'), &board, &mut view, now);
    let text = draw(&board, &view, 110, 24);
    assert!(text.contains("paused"), "grid marks the pause:\n{text}");

    // An armed stop replaces the legend with the confirmation prompt.
    on_key(key('x'), &board, &mut view, now);
    let text = draw(&board, &view, 110, 24);
    assert!(text.contains("stop t1?"), "confirm prompt:\n{text}");
    assert!(
        text.contains("[x] again to confirm"),
        "confirm hint:\n{text}"
    );
}

#[test]
fn detach_keys_are_unchanged_by_the_supervisor_surface() {
    let (board, mut view) = running_board();
    let now = Instant::now();
    assert_eq!(on_key(key('q'), &board, &mut view, now), KeyAction::Detach);
    assert_eq!(
        on_key(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &board,
            &mut view,
            now
        ),
        KeyAction::Detach
    );
    assert_eq!(
        on_key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &board,
            &mut view,
            now
        ),
        KeyAction::Detach
    );
}
