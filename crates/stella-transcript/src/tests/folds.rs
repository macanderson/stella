//! Fold state, zoom presets, the cursor, and the fold controls both
//! renderers draw over them.
//!
//! Split out of the parent module for the 1500-line ceiling.

use super::*;

#[test]
fn collapsing_a_parent_preserves_child_fold_state() {
    let run = run_with(vec![
        step(bash("a", &["x"], Status::Ok), 0),
        step(bash("b", &["y"], Status::Ok), 1),
    ]);
    let turn = NodeId::Turn(0);
    let second = NodeId::Step { turn: 0, step: 1 };

    let mut state = FoldState::new();
    state.open(second);
    state.close(turn);
    assert!(!state.is_open(&run, turn));

    state.open(turn);
    assert!(
        state.is_open(&run, second),
        "collapsing the turn discarded the step's fold state"
    );
}

/// A turn is open at every zoom except `Turns` — for every turn, not only the
/// last one. The `NodeId::Turn` arm in `default_open` matches before the
/// `Zoom::Everything` catch-all, so an earlier-turn condition there overrides
/// "Everything open"; this pins the earlier turns of a multi-turn run open,
/// which every single-turn fixture in this file is structurally unable to see.
#[test]
fn every_turn_is_open_at_steps_and_everything_zoom() {
    let mut run = run_with(vec![step(bash("a", &["x"], Status::Ok), 0)]);
    let more = run.turns[0].clone();
    run.turns.push(more.clone());
    run.turns.push(more);

    for zoom in [Zoom::Steps, Zoom::Everything] {
        let mut state = FoldState::new();
        state.set_zoom(zoom);
        for t in 0..run.turns.len() {
            assert!(
                state.is_open(&run, NodeId::Turn(t)),
                "turn {t} rendered collapsed at {}",
                zoom.label()
            );
        }
    }
}

#[test]
fn the_output_fold_control_has_something_behind_it() {
    // Nine lines: past `PREVIEW_LINES` but short of `TAIL_FOLD_THRESHOLD`, so
    // this is the head-only fold. It used to be six, which was past the old
    // three-line head — the same shape, restated against the shared preview
    // budget the deck also uses now (#3644).
    let out = output(&["1", "2", "3", "4", "5", "6", "7", "8", "9"]);
    let fold = digest::fold_output(&out, "cmd");
    assert_eq!(fold.head.len(), digest::PREVIEW_LINES);
    assert!(fold.tail.is_empty(), "a short fold keeps no tail");
    assert_eq!(fold.hidden, 3);
    assert_eq!(fold.more_label(), "▸ 3 more lines");
}

/// The rule `PREVIEW_LINES` exists to state: **however** an output folds,
/// a reader sees the same number of lines of it.
///
/// Before this, the head-only fold and the head…tail fold showed different
/// totals (three against five), and interactive mode showed a third number
/// again (six) from its own constant — so "how much of this tool's output do I
/// get" depended on both which surface you opened and how long the output
/// happened to be. `crates/stella-tui/src/render/tests/tool_output.rs` is the other half
/// of this: the same assertion made against the deck's renderer.
#[test]
fn every_fold_shows_the_same_number_of_lines_whatever_its_shape() {
    for total in 0..40usize {
        let lines: Vec<String> = (0..total).map(|i| format!("line {i}")).collect();
        let out = Output { lines, clipped: 0 };
        let fold = digest::fold_output(&out, "cmd");
        let shown = fold.head.len() + fold.tail.len();
        assert_eq!(
            shown,
            total.min(digest::PREVIEW_LINES),
            "a {total}-line output shows {shown} lines, not the shared preview budget"
        );
        assert_eq!(
            fold.hidden,
            total - shown,
            "a {total}-line output must account for every line it does not show"
        );
    }
}

#[test]
fn a_long_output_folds_head_and_tail_so_errors_at_the_end_stay_visible() {
    let mut lines: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
    lines.push("error: the thing failed".to_string());
    let out = Output { lines, clipped: 0 };
    let fold = digest::fold_output(&out, "cmd");
    assert_eq!(fold.head.len(), digest::HEAD_LINES);
    assert_eq!(fold.tail.len(), digest::TAIL_LINES);
    assert!(fold.tail.last().unwrap().contains("failed"));
}

#[test]
fn clipped_lines_are_counted_in_the_fold_control() {
    let out = Output {
        lines: vec!["a".to_string(), "b".to_string()],
        clipped: 24,
    };
    let fold = digest::fold_output(&out, "cmd");
    assert_eq!(fold.hidden, 24, "the transport's clip must be admitted");
}

/// The grid's width contract holds only if no cell carries a control
/// character: `\n` splits a row when the encoders join lines, and `\t` is
/// zero columns to `unicode-width` and a real stop to the terminal. Both
/// arrive on legal wire data — a bash `header_object` is routinely
/// multi-line, and an `edit_file`'s `old_string` arg carries whatever the
/// file did — and used to reach `Cell::new` verbatim, splitting one logical
/// row into two terminal rows and walking the whole frame below it.
#[test]
fn control_characters_cannot_reach_a_grid_cell() {
    let mut call = bash("cargo build &&\ncargo test", &["ok"], Status::Ok);
    call.args.push(ArgRow {
        key: "old_string".to_string(),
        value: "left\n\tright".to_string(),
    });
    let run = run_with(vec![step(call, 0)]);
    let mut state = FoldState::new();
    state.set_zoom(Zoom::Everything);
    for line in &grid::render(&run, &state, 100) {
        for cell in line {
            assert!(
                !cell.text.chars().any(char::is_control),
                "a control character survived into a cell: {:?}",
                cell.text
            );
        }
    }
}

/// A real CSI sequence carries one control byte — `ESC` — ahead of printable
/// parameter and final bytes (`\x1b[31m` is `ESC`, `[`, `3`, `1`, `m`).
/// Mapping only that `ESC` to a space renders `"\x1b[31merror\x1b[0m"` as
/// `" [31merror [0m"`. The width contract holds — nothing left is a control
/// character — but the text still carries bracket-and-digit noise a reader
/// would mistake for real content. `Cell::new` strips the whole sequence
/// instead.
#[test]
fn a_csi_sequence_is_stripped_whole_not_left_as_bracket_noise() {
    let cell = grid::Cell::new("\x1b[31merror\x1b[0m", grid::Color::Ink);
    assert_eq!(cell.text, "error");
    assert!(
        !cell.text.contains("[31m"),
        "the escape's parameter bytes survived as literal text: {:?}",
        cell.text
    );
}

/// `Output::clipped` promises a clipped marker "so a reader never
/// mistakes a transport limit for the end of the output," on both
/// renderers. An expanded grid body once carried the clip count only on the
/// closed fold's `▸ N more lines` control, so opening the output presented
/// three surviving lines as the whole thing. The HTML renderer had a
/// matching gap on this exact case: three received lines fit entirely
/// inside `head`, so the hidden slice its `<details>` control promises is
/// empty, and nothing on the page said the other 24 were dropped in
/// transport. This test's earlier body threw `markup` away before it ever
/// looked, so it never saw that gap.
#[test]
fn an_expanded_output_still_admits_the_transport_clip() {
    let mut call = bash("cmd", &["a", "b", "c"], Status::Ok);
    call.output.clipped = 24;
    let (plain, markup) = rendered(call);
    assert!(
        plain.contains("clipped"),
        "the expanded grid presented a clipped body as complete:\n{plain}"
    );
    assert!(
        markup.contains("clipped"),
        "the expanded HTML page presented a clipped body as complete:\n{markup}"
    );
}

#[test]
fn zoom_cycles_through_the_three_presets() {
    let mut state = FoldState::new();
    assert_eq!(state.zoom(), Zoom::Steps);
    state.cycle_zoom();
    assert_eq!(state.zoom(), Zoom::Everything);
    state.cycle_zoom();
    assert_eq!(state.zoom(), Zoom::Turns);
    state.cycle_zoom();
    assert_eq!(state.zoom(), Zoom::Steps);
}

#[test]
fn the_cursor_walks_steps_and_saturates_at_the_ends() {
    let run = run_with(vec![
        step(bash("a", &[], Status::Ok), 0),
        step(bash("b", &[], Status::Ok), 1),
    ]);
    let cursor = Cursor::default();
    assert_eq!(cursor.next(&run).step, 1);
    assert_eq!(cursor.next(&run).next(&run).step, 1, "wrapped past the end");
    assert_eq!(cursor.prev(&run).step, 0, "wrapped past the start");
}

/// A cursor's fields are public and round-trip through a saved session, so a
/// restored cursor can point past the end of a shorter re-loaded run. `prev`
/// used to index `run.turns[turn]` on that path and panic; it must land on the
/// last real step instead, the way `next` already survives a stale turn.
#[test]
fn a_stale_cursor_walks_back_into_the_run_instead_of_panicking() {
    let run = run_with(vec![
        step(bash("a", &[], Status::Ok), 0),
        step(bash("b", &[], Status::Ok), 1),
    ]);
    let stale = Cursor { turn: 5, step: 0 };
    assert_eq!(stale.prev(&run), Cursor { turn: 0, step: 1 });
}

#[test]
fn copy_returns_the_invocation_rather_than_reaching_a_clipboard() {
    let run = run_with(vec![step(bash("cargo test", &[], Status::Ok), 0)]);
    let mut state = FoldState::new();
    let mut cursor = Cursor::default();
    let copied = apply(&run, &mut state, &mut cursor, Command::CopyInvocation);
    assert_eq!(copied.as_deref(), Some("cargo test"));
}

#[test]
fn keys_bind_to_the_documented_commands() {
    assert_eq!(Command::from_key("j"), Some(Command::NextStep));
    assert_eq!(Command::from_key("z"), Some(Command::CycleZoom));
    assert_eq!(Command::from_key("e"), Some(Command::ExpandOutputs));
    assert_eq!(Command::from_key("c"), Some(Command::CopyInvocation));
    assert_eq!(Command::from_key("Q"), None);
}
