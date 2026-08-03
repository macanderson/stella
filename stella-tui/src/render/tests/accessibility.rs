//! Accessibility: the screen-reader surface (#936, #935).
//!
//! Split out of `render/tests.rs` when that file crossed the 1500-line guard,
//! following the same one-topic-per-file rule as `thinking`. These pin the
//! things that make `stella --simple` readable by something that is not a pair
//! of eyes — the terminal cursor tracking the caret, the stacked (rather than
//! split) panel layout, and a scrollback contract that stops a reader
//! announcing the whole conversation twice. They are asserted on the rendered
//! buffer and the backend's cursor state — never on raw ANSI (L-T6).

use super::*;

/// Type `text` into `ui`'s composer one key at a time — the way a user
/// produces it. `paste` would fold a long string into a chip, which is a
/// different buffer shape than typing.
fn type_into(ui: &mut UiState, text: &str) {
    for c in text.chars() {
        ui.composer.insert_char(c);
    }
}

/// Draw and hand back the whole terminal, so a test can inspect the *cursor*
/// as well as the cells. [`draw`] returns only the text, and the cursor is
/// precisely the thing that has no cell.
fn draw_terminal(model: &SessionModel, ui: &mut UiState, w: u16, h: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| render(model, ui, f)).unwrap();
    terminal
}

/// #935: ratatui hides the hardware cursor on any frame that never positions
/// it. Until this surface had a caller the fix landed on the deck alone, so
/// the caret here was a reversed cell and nothing else — invisible to a
/// screen reader (no insertion point to follow) and to a CJK/IME candidate
/// window (which anchors to the terminal cursor, not to a styled cell).
#[test]
fn the_terminal_cursor_sits_on_the_composer_caret() {
    let model = SessionModel::new();
    let mut ui = UiState::default();
    type_into(&mut ui, "hello");

    let terminal = draw_terminal(&model, &mut ui, 80, 24);
    assert!(
        terminal.backend().cursor_visible(),
        "a frame that never positions the cursor leaves it hidden — the #935 bug"
    );
    let pos = terminal.backend().cursor_position();

    // The composer is the last band, bordered, with a two-column `› ` prefix:
    // the caret sits one row into the band and three columns in, past the
    // five characters typed.
    let rows = buffer_rows(terminal.backend().buffer());
    let composer_row = rows
        .iter()
        .position(|r| r.contains("hello"))
        .expect("the composer painted the typed text");
    assert_eq!(
        pos.y as usize, composer_row,
        "cursor is on the composer row"
    );
    assert_eq!(
        pos.x,
        8,
        "1 border + 2 prefix + 5 typed characters:\n{}",
        rows.join("\n")
    );
}

/// The caret must follow the text, not sit at a fixed column — an insertion
/// point that lies is worse than none, because a reader trusts it.
#[test]
fn the_cursor_follows_the_caret_through_the_buffer() {
    let model = SessionModel::new();
    let mut ui = UiState::default();
    type_into(&mut ui, "abcdef");
    let at_end = draw_terminal(&model, &mut ui, 80, 24)
        .backend()
        .cursor_position();

    // Walk the caret two characters left; the cursor must move with it.
    ui.composer.move_left();
    ui.composer.move_left();
    let moved = draw_terminal(&model, &mut ui, 80, 24)
        .backend()
        .cursor_position();

    assert_eq!(moved.y, at_end.y);
    assert_eq!(
        moved.x + 2,
        at_end.x,
        "two characters left is two columns left"
    );
}

/// While the slash popup owns the keyboard the caret must not sit in the
/// composer — mirroring the precedence `handle_key` already applies, and the
/// same rule the deck uses for its overlays.
#[test]
fn the_cursor_is_suppressed_while_the_slash_popup_owns_the_keyboard() {
    let model = SessionModel::new();
    let mut ui = UiState::default();
    ui.slash_commands = vec![SlashCommand::new("/help", "show help")];
    type_into(&mut ui, "/he");

    let terminal = draw_terminal(&model, &mut ui, 80, 24);
    assert!(
        !terminal.backend().cursor_visible(),
        "keys are steering a menu, so the insertion point is not in the composer"
    );
}

/// A screen reader walks the terminal grid row by row. The side-by-side
/// transcript|files split is the fastest layout to scan visually and the
/// worst to listen to: every row is half a sentence of transcript followed by
/// half a filename. Stacked, a row is one thought.
#[test]
fn screen_reader_mode_stacks_the_panels_instead_of_splitting_them() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::Text {
        delta: "the quick brown fox".into(),
    });
    model.apply(&AgentEvent::FileChange {
        path: "src/lib.rs".into(),
        kind: FileChangeKind::Modified,
        added: 1,
        removed: 0,
        diff: None,
    });

    let mut split = UiState::default();
    let side_by_side = buffer_rows(
        draw_terminal(&model, &mut split, 100, 30)
            .backend()
            .buffer(),
    );
    let mut stacked_ui = UiState::default();
    stacked_ui.screen_reader = true;
    let stacked = buffer_rows(
        draw_terminal(&model, &mut stacked_ui, 100, 30)
            .backend()
            .buffer(),
    );

    // The default layout puts both on some shared row; that is the property
    // being fixed, so assert it is really there before asserting it is gone.
    assert!(
        side_by_side
            .iter()
            .any(|r| r.contains("quick brown fox") && r.contains("src/lib.rs")),
        "the default layout interleaves the two panels on one row:\n{}",
        side_by_side.join("\n")
    );
    assert!(
        !stacked
            .iter()
            .any(|r| r.contains("quick brown fox") && r.contains("src/lib.rs")),
        "no stacked row may carry content from both panels:\n{}",
        stacked.join("\n")
    );
    // Both panels are still there — this is a re-axis, not a removal, so
    // Tab-to-files and the diff viewer keep working.
    let all = stacked.join("\n");
    assert!(
        all.contains("quick brown fox"),
        "transcript survives:\n{all}"
    );
    assert!(all.contains("src/lib.rs"), "files panel survives:\n{all}");
    // …and in reading order: the conversation before the file ledger.
    let transcript_at = stacked
        .iter()
        .position(|r| r.contains("quick brown fox"))
        .unwrap();
    let files_at = stacked
        .iter()
        .position(|r| r.contains("src/lib.rs"))
        .unwrap();
    assert!(
        transcript_at < files_at,
        "reading order is document order:\n{all}"
    );
}

/// The scrollback contract: entries the shell has already written into the
/// terminal's own scrollback must not be painted again in the live pane.
/// Drawing both would have a reader announce the whole conversation twice.
#[test]
fn entries_already_in_scrollback_are_not_painted_again() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::Text {
        delta: "first answer".into(),
    });
    model.apply(&AgentEvent::Stage {
        name: StageKind::Execute,
    });
    model.apply(&AgentEvent::Text {
        delta: "second answer".into(),
    });

    let mut ui = UiState::default();
    ui.screen_reader = true;
    let before = draw(&model, &mut ui, 100, 30);
    assert!(before.contains("first answer"), "baseline:\n{before}");

    // The shell flushed everything up to the trailing entry.
    ui.scrollback_entries = model.transcript.len() - 1;
    let after = draw(&model, &mut ui, 100, 30);
    assert!(
        !after.contains("first answer"),
        "a flushed entry must leave the live pane:\n{after}"
    );
    assert!(
        after.contains("second answer"),
        "the still-live tail stays on screen:\n{after}"
    );
}

/// The memoized transcript render is keyed on a fingerprint. A scrollback
/// flush makes the pane *shorter* while the transcript only ever grows, so
/// no other term of that fingerprint moves — without `scrollback` in the key
/// the pane would keep painting entries that are already in scrollback, which
/// is precisely the double-announcement this surface exists to avoid.
#[test]
fn the_transcript_cache_notices_a_scrollback_flush() {
    let mut model = SessionModel::new();
    model.apply(&AgentEvent::Text {
        delta: "already read aloud".into(),
    });
    model.apply(&AgentEvent::Stage {
        name: StageKind::Execute,
    });

    let mut ui = UiState::default();
    ui.ensure_transcript_lines(&model, false, 60);
    let before = ui.transcript_lines().len();
    assert!(before > 0);

    // Same model, same width, same expansion — only the flush count moves.
    ui.scrollback_entries = 1;
    ui.ensure_transcript_lines(&model, false, 60);
    assert!(
        ui.transcript_lines().len() < before,
        "the cache must refold when entries leave for scrollback"
    );
}
