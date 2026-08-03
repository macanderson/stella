//! The slash-command popup: what it lists, what it marks, and how it windows.
//!
//! Split out of `render/tests.rs` when that file crossed the 1500-line guard,
//! following `thinking`. One topic per file: these are the only tests that
//! drive a `Composer` through its `SlashCommand` list, and between them they
//! pin the popup's two separable jobs — which commands survive the filter (and
//! which glyph marks a custom one), and the window that holds still until the
//! selection actually leaves its edge.

use super::*;

#[test]
fn slash_menu_renders_filtered_commands() {
    let model = SessionModel::new();
    let mut composer = Composer::new();
    for c in "/di".chars() {
        composer.insert_char(c);
    }
    let mut ui = UiState::new(
        composer,
        vec![
            SlashCommand::new("/diff", "open the diff viewer"),
            SlashCommand::new("/files", "focus files"),
        ],
    );
    let text = draw(&model, &mut ui, 100, 30);
    assert!(text.contains("/diff"), "slash menu lists /diff:\n{text}");
    assert!(!text.contains("/files"), "filtered out /files:\n{text}");
}

#[test]
fn slash_popup_marks_the_selected_command() {
    let model = SessionModel::new();
    let mut composer = Composer::new();
    composer.insert_char('/');
    let mut ui = UiState::new(
        composer,
        vec![
            SlashCommand::new("/help", "show help"),
            SlashCommand::new("/diff", "open the diff viewer"),
        ],
    );
    ui.slash_selected = 1;
    let text = draw(&model, &mut ui, 100, 30);
    // The glyph is double-width: its trailing filler cell dumps as an
    // extra space before the explicit separator.
    assert!(
        text.contains("▸ 🔒  /diff"),
        "selection marker + builtin glyph on the second row:\n{text}"
    );
    assert!(
        text.contains("/ commands · 2"),
        "popup title with the match count:\n{text}"
    );
}

#[test]
fn slash_popup_glyphs_distinguish_builtin_from_custom_commands() {
    let model = SessionModel::new();
    let mut composer = Composer::new();
    composer.insert_char('/');
    let mut ui = UiState::new(
        composer,
        vec![
            SlashCommand::new("/help", "show help"),
            SlashCommand::custom("/fix-bug", "fix the named bug"),
        ],
    );
    let text = draw(&model, &mut ui, 100, 30);
    assert!(
        text.contains("🔒  /help"),
        "productized commands carry the lock glyph:\n{text}"
    );
    assert!(
        text.contains("⚡  /fix-bug"),
        "custom commands carry the lightning glyph:\n{text}"
    );
}

// ---- Slash-popup windowing ---------------------------------------------

#[test]
fn scroll_window_start_holds_still_until_the_selection_leaves_the_edge() {
    // Fits entirely: never scrolls.
    assert_eq!(scroll_window_start(5, 4, 8), 0);
    // Selection inside the first window: no movement.
    assert_eq!(scroll_window_start(20, 0, 8), 0);
    assert_eq!(scroll_window_start(20, 7, 8), 0);
    // One past the window's last row: scroll down by one.
    assert_eq!(scroll_window_start(20, 8, 8), 1);
    // The tail clamps so the final window is full, never blank-padded.
    assert_eq!(scroll_window_start(20, 19, 8), 12);
    // Selecting back at the top pulls the window all the way up.
    assert_eq!(scroll_window_start(20, 0, 8), 0);
    // A stale selection past the end (e.g. the filter just shrank the
    // match list) clamps to the last full window instead of panicking.
    assert_eq!(scroll_window_start(20, 999, 8), 12);
    // Degenerate inputs don't panic.
    assert_eq!(scroll_window_start(0, 0, 8), 0);
    assert_eq!(scroll_window_start(5, 0, 0), 0);
}

/// Rendering a slash popup taller than its window keeps the *selected*
/// row on screen and pushes the ones scrolled past off it — the concrete
/// symptom of the un-windowed version (selection navigable but invisible).
#[test]
fn slash_popup_windows_the_selection_into_view() {
    let cmds: Vec<SlashCommand> = (0..15)
        .map(|i| SlashCommand::new(format!("/cmd{i:02}"), "desc"))
        .collect();
    let menu = SlashMenu::filter(&cmds, "/");
    let area = Rect {
        x: 0,
        y: 0,
        width: 56,
        height: (SLASH_POPUP_MAX_ROWS as u16) + 3,
    };
    // Select the very last command: without windowing it renders off the
    // bottom of the popup box and never appears in the buffer.
    let mut buf = Buffer::empty(area);
    render_slash_popup(&menu, 14, area, &mut buf);
    let text = buffer_text(&buf);
    assert!(text.contains("/cmd14"), "selected row is visible:\n{text}");
    assert!(
        !text.contains("/cmd00"),
        "the top rows scrolled out of view:\n{text}"
    );
    // The legend advertises the hidden rows above.
    assert!(text.contains('▲'), "scroll affordance shown:\n{text}");

    // Selecting the top shows the head and hides the tail instead.
    let mut buf = Buffer::empty(area);
    render_slash_popup(&menu, 0, area, &mut buf);
    let text = buffer_text(&buf);
    assert!(text.contains("/cmd00"), "top row visible:\n{text}");
    assert!(!text.contains("/cmd14"), "tail hidden:\n{text}");
    assert!(text.contains('▼'), "hidden-below affordance shown:\n{text}");
}

/// A stale-high selection (the match list shrank under the cursor before
/// the upstream clamp caught up) must still render a sane, in-bounds
/// window rather than panic on the slice.
#[test]
fn slash_popup_survives_a_selection_past_the_filtered_end() {
    let cmds: Vec<SlashCommand> = (0..3)
        .map(|i| SlashCommand::new(format!("/cmd{i:02}"), "desc"))
        .collect();
    let menu = SlashMenu::filter(&cmds, "/");
    let area = Rect {
        x: 0,
        y: 0,
        width: 56,
        height: (SLASH_POPUP_MAX_ROWS as u16) + 3,
    };
    let mut buf = Buffer::empty(area);
    // selected far past the 3 matches — the render-side clamp keeps it in
    // view; all three rows fit so nothing scrolls.
    render_slash_popup(&menu, 99, area, &mut buf);
    let text = buffer_text(&buf);
    assert!(text.contains("/cmd02"), "last row still shown:\n{text}");
    assert!(
        !text.contains('▲') && !text.contains('▼'),
        "short list shows no scroll affordance:\n{text}"
    );
}
