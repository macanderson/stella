//! The slash-command popup: what it lists, what it marks, and how it windows.
//!
//! Split out of `render/tests.rs` when that file crossed the 1500-line guard,
//! following `thinking`. One topic per file: these are the only tests that
//! drive a `Composer` through its `SlashCommand` list, and between them they
//! pin the popup's separable jobs — which commands survive the filter (and
//! which glyph marks a custom one), the window that holds still until the
//! selection actually leaves its edge, which letters light gold, and the
//! ground the whole overlay floats on.

use super::*;
use ratatui::style::Color;
use stella_tui_theme::token;

/// The row and column of the first cell of `needle`, or `None`.
fn find_cell(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
    let rows = buffer_rows(buf);
    rows.iter().enumerate().find_map(|(y, row)| {
        row.find(needle)
            .map(|byte| (row[..byte].chars().count() as u16, y as u16))
    })
}

/// The foreground colours of the `len` cells starting at `(x, y)`.
fn foregrounds(buf: &Buffer, x: u16, y: u16, len: u16) -> Vec<Option<Color>> {
    (x..x + len)
        .map(|at| buf.cell((at, y)).map(|c| c.fg))
        .collect()
}

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
    render_slash_popup(&menu, 14, &[], area, &mut buf);
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
    render_slash_popup(&menu, 0, &[], area, &mut buf);
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
    render_slash_popup(&menu, 99, &[], area, &mut buf);
    let text = buffer_text(&buf);
    assert!(text.contains("/cmd02"), "last row still shown:\n{text}");
    assert!(
        !text.contains('▲') && !text.contains('▼'),
        "short list shows no scroll affordance:\n{text}"
    );
}

/// **The witness (#4338).** The browse list draws its headings: `relevant
/// now` with the reason that put those rows on top, then one per domain
/// group. The headings are rows of their own, so the popup has to be sized
/// and windowed over them — a list sized on matches alone clipped its tail
/// behind its own captions.
#[test]
fn the_browse_popup_draws_the_relevance_heading_and_the_domain_groups() {
    use crate::composer::{PaletteState, SlashDomain};
    let cmds = vec![
        SlashCommand::new("/plan", "the plan").in_domain(SlashDomain::Plan),
        SlashCommand::new("/clear", "reset").in_domain(SlashDomain::Session),
        SlashCommand::new("/diff", "the diff").in_domain(SlashDomain::Code),
    ];
    let state = PaletteState {
        turn_running: true,
        ..PaletteState::default()
    };
    let menu = SlashMenu::filter_with(&cmds, "/", &state, &[]);
    let rows = crate::render::display_rows(&menu);
    let area = Rect {
        x: 0,
        y: 0,
        width: 72,
        height: (rows.len() as u16) + 3,
    };
    let mut buf = Buffer::empty(area);
    render_slash_popup(&menu, 0, &[], area, &mut buf);
    let text = buffer_text(&buf);
    assert!(
        text.contains("relevant now · a turn is running"),
        "the heading says why:\n{text}"
    );
    assert!(
        text.contains("session"),
        "the domain groups are named:\n{text}"
    );
    assert!(text.contains("workspace"), "{text}");
    // Every command still renders — the headings took rows, not commands.
    for name in ["/plan", "/clear", "/diff"] {
        assert!(text.contains(name), "{name} missing:\n{text}");
    }
    assert!(
        !text.contains('\u{25b2}') && !text.contains('\u{25bc}'),
        "everything fit, so no scroll affordance:\n{text}"
    );
}

/// **The witness (#5048).** The matched letters light gold *inside* the name,
/// wherever they landed. `ga` is neither a prefix nor a substring of
/// `graph query`, so the old prefix-only lighting painted the whole row one
/// colour and the palette's own `· fuzzy` title was a claim nothing on screen
/// backed.
#[test]
fn scattered_matched_letters_light_gold_inside_the_name() {
    let cmds = vec![SlashCommand::new("/graph query", "free-form graph query")];
    let menu = SlashMenu::filter(&cmds, "/ga");
    let area = Rect {
        x: 0,
        y: 0,
        width: 64,
        height: 5,
    };
    let mut buf = Buffer::empty(area);
    render_slash_popup(&menu, 0, &[], area, &mut buf);

    let (x, y) = find_cell(&buf, "/graph").expect("the row is drawn");
    // `/ g r a p h` — the `g` at +1 and the `a` at +3 are the two matched
    // letters; the slash and everything between and after them stay resting
    // gold.
    let fg = foregrounds(&buf, x, y, 6);
    assert_eq!(
        fg,
        vec![
            Some(token::GOLD),
            Some(token::GOLD_BRIGHT),
            Some(token::GOLD),
            Some(token::GOLD_BRIGHT),
            Some(token::GOLD),
            Some(token::GOLD),
        ],
        "the g and the a light, and only them:\n{}",
        buffer_text(&buf)
    );
}

/// A prefix keeps lighting as one contiguous run, which is the behaviour the
/// scattered walk had to preserve rather than replace. The leading `/` is the
/// sigil that opened the palette and no longer lights with the letters after
/// it — a matched letter is one the query actually consumed, and the
/// scattered case cannot pretend otherwise.
#[test]
fn a_prefix_still_lights_as_one_run() {
    let cmds = vec![SlashCommand::new("/plan", "the plan")];
    let menu = SlashMenu::filter(&cmds, "/pl");
    let area = Rect {
        x: 0,
        y: 0,
        width: 48,
        height: 5,
    };
    let mut buf = Buffer::empty(area);
    render_slash_popup(&menu, 0, &[], area, &mut buf);

    let (x, y) = find_cell(&buf, "/plan").expect("the row is drawn");
    assert_eq!(
        foregrounds(&buf, x, y, 5),
        vec![
            Some(token::GOLD),
            Some(token::GOLD_BRIGHT),
            Some(token::GOLD_BRIGHT),
            Some(token::GOLD),
            Some(token::GOLD),
        ],
        "`pl` lights, the sigil and `an` do not:\n{}",
        buffer_text(&buf)
    );
}

/// **The witness (#5048).** SPEC 10 asks for a `panel` ground under the
/// overlay. `Clear` alone leaves the terminal's own background showing, so a
/// palette over a dark deck read as a hole punched in it.
#[test]
fn the_palette_floats_on_the_panel_ground() {
    let cmds = vec![SlashCommand::new("/plan", "the plan")];
    let menu = SlashMenu::filter(&cmds, "/pl");
    let area = Rect {
        x: 0,
        y: 0,
        width: 48,
        height: 5,
    };
    let mut buf = Buffer::empty(area);
    render_slash_popup(&menu, 0, &[], area, &mut buf);

    let (x, y) = find_cell(&buf, "/plan").expect("the row is drawn");
    // A row that is not selected: the selected one carries `hl` over the top.
    let unselected_row = buf.cell((x, y + 1)).map(|c| c.bg);
    assert_eq!(
        buf.cell((0, 0)).map(|c| c.bg),
        Some(token::PANEL),
        "the border corner sits on panel:\n{}",
        buffer_text(&buf)
    );
    assert!(
        unselected_row.is_none() || unselected_row == Some(token::PANEL),
        "and so does the interior: {unselected_row:?}\n{}",
        buffer_text(&buf)
    );
    assert_eq!(
        buf.cell((x, y)).map(|c| c.bg),
        Some(token::HL),
        "the selected row keeps its own ground over the panel:\n{}",
        buffer_text(&buf)
    );
}

/// **The witness (#5048).** The browse list closes with SPEC 10's `recent`
/// section, listing the commands this workspace ran last. A recent row is a
/// second appearance of a command a domain group already carries — that is
/// what a shortcut is — so both are drawn.
#[test]
fn the_browse_popup_closes_with_the_recent_section() {
    use crate::composer::{PaletteState, SlashDomain};
    let cmds = vec![
        SlashCommand::new("/clear", "reset").in_domain(SlashDomain::Session),
        SlashCommand::new("/diff", "the diff").in_domain(SlashDomain::Code),
    ];
    let recent = vec!["/diff".to_string()];
    let menu = SlashMenu::filter_with(&cmds, "/", &PaletteState::default(), &recent);
    let rows = crate::render::display_rows(&menu);
    let area = Rect {
        x: 0,
        y: 0,
        width: 72,
        height: (rows.len() as u16) + 3,
    };
    let mut buf = Buffer::empty(area);
    render_slash_popup(&menu, 0, &[], area, &mut buf);
    let text = buffer_text(&buf);
    assert!(text.contains("recent"), "the heading is drawn:\n{text}");
    assert_eq!(
        text.matches("/diff").count(),
        2,
        "the shortcut and the group row both appear:\n{text}"
    );
    let recent_at = text.find("recent").expect("heading");
    let workspace_at = text.find("workspace").expect("domain heading");
    assert!(
        workspace_at < recent_at,
        "`recent` closes the list rather than opening it:\n{text}"
    );
}

/// A typed query draws no headings at all: three rows under three captions
/// is a worse list than three rows.
#[test]
fn a_queried_popup_draws_no_headings() {
    use crate::composer::SlashDomain;
    let cmds = vec![
        SlashCommand::new("/plan", "the plan").in_domain(SlashDomain::Plan),
        SlashCommand::new("/clear", "reset").in_domain(SlashDomain::Session),
    ];
    let menu = SlashMenu::filter(&cmds, "/pl");
    let area = Rect {
        x: 0,
        y: 0,
        width: 72,
        height: (SLASH_POPUP_MAX_ROWS as u16) + 3,
    };
    let mut buf = Buffer::empty(area);
    render_slash_popup(&menu, 0, &[], area, &mut buf);
    let text = buffer_text(&buf);
    assert!(text.contains("/plan"), "{text}");
    assert!(
        !text.contains("turn"),
        "no group headings under a query:\n{text}"
    );
}
