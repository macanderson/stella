//! The slash-command popup: what it lists, what it marks, how it windows,
//! which letters it lights, and where on the frame it sits.
//!
//! Split out of `render/tests.rs` when that file crossed the 1500-line guard,
//! following `thinking`. One topic per file: these are the only tests that
//! drive a `Composer` through its `SlashCommand` list, and between them they
//! pin the popup's separable jobs — which commands survive the filter (and
//! which glyph marks a custom one), the window that holds still until the
//! selection actually leaves its edge, the gold on the matched letters, and
//! the two places SPEC 10 is read back out of `design/tui-v2/SPEC.md` so the
//! document and the deck cannot drift apart unnoticed (#5048).

use super::*;
use stella_tui_theme::token;

/// SPEC 10, as prose, for the two guards that read it. `design/` is outside
/// ci.yml's prose paths (`scripts/ci-rust-scope.sh`), so a diff that edits
/// the spec alone still runs the gates below — the same arrangement
/// `keymap`'s SPEC 11 guard relies on (#4341).
const SPEC: &str = include_str!("../../../../../design/tui-v2/SPEC.md");

/// The `## 10.` section's body.
fn spec_10_section() -> &'static str {
    SPEC.split("## 10. Command palette")
        .nth(1)
        .expect("SPEC.md has no section 10")
        .split("\n## ")
        .next()
        .expect("split always yields one")
}

/// The bullet in section 10 that mentions `needle`, with any amendment note
/// stripped.
///
/// A bullet the deck deviated from carries its history in a trailing
/// *(Amended …)* clause, which necessarily quotes what the spec used to say
/// — so a guard reading the whole line would find the very word the
/// amendment exists to retire. The rule these tests enforce is about the
/// **normative** half: what a reader implementing the palette today is told.
/// The clause itself is prose for a reader asking *why*.
fn spec_10_bullet(needle: &str) -> &'static str {
    spec_10_section()
        .lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("section 10 has no `{needle}` bullet"))
        .split(" *(Amended")
        .next()
        .expect("split always yields one")
}

/// Every cell of `buf` that carries `fg`, as the text it spells — how a
/// highlight assertion reads back as letters rather than as coordinates.
fn cells_with_fg(buf: &Buffer, fg: ratatui::style::Color) -> String {
    let area = *buf.area();
    (area.y..area.y + area.height)
        .flat_map(|y| (area.x..area.x + area.width).map(move |x| (x, y)))
        .filter_map(|(x, y)| buf.cell((x, y)))
        .filter(|cell| cell.fg == fg)
        .map(|cell| cell.symbol())
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
    let menu = SlashMenu::filter_with(&cmds, "/", &state);
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

/// **The witness (#5048).** A fuzzy query lights the letters it matched
/// **inside** the command name, wherever they sit — `ga` puts gold on the
/// `g` and the `a` of `/graph query`, and on nothing else.
///
/// Before this, only a typed *prefix* lit: the matcher produced no indices,
/// so the renderer re-derived a `starts_with` and a mid-name match rendered
/// in one flat gold with no visible reason for being in the list at all. And
/// `ga` did not reach `/graph query` in the first place — the old matcher was
/// prefix / substring / description, with no tier for scattered letters.
///
/// Asserted from both ends. The lit set is exactly `ga`, so a regression
/// that lights the whole name (or the whole matched *run*) fails; and the
/// unmatched letters are still the plain gold, so a regression that simply
/// stops lighting anything fails too.
#[test]
fn a_fuzzy_query_lights_the_matched_letters_inside_the_name() {
    use crate::composer::SlashDomain;
    let cmds = vec![
        SlashCommand::new("/graph query", "search the code graph").in_domain(SlashDomain::Code),
    ];
    let menu = SlashMenu::filter(&cmds, "/ga");
    assert_eq!(menu.matches.len(), 1, "`ga` reaches `/graph query`");

    let area = Rect {
        x: 0,
        y: 0,
        width: 72,
        height: (SLASH_POPUP_MAX_ROWS as u16) + 3,
    };
    let mut buf = Buffer::empty(area);
    render_slash_popup(&menu, 0, &[], area, &mut buf);

    assert_eq!(
        cells_with_fg(&buf, token::GOLD_BRIGHT),
        "ga",
        "exactly the matched letters are lit:\n{}",
        buffer_text(&buf)
    );
    // The rest of the name still renders, in the plain gold — `GOLD` also
    // paints the row marker and the title, so this asserts containment
    // rather than equality.
    let plain = cells_with_fg(&buf, token::GOLD);
    for unlit in ["r", "ph", "query"] {
        assert!(
            plain.contains(unlit),
            "`{unlit}` should be plain gold, not missing:\n{plain}"
        );
    }
    assert!(
        !plain.contains("ga"),
        "a lit letter must not also be plain:\n{plain}"
    );
}

/// The overlay is a **surface**: SPEC 10's `panel` ground under every cell it
/// covers, so it reads as lifted off the transcript rather than as a bordered
/// hole punched in it. It used to paint only `Clear`, which leaves the
/// terminal's own background showing through.
///
/// The selection highlight has to survive that ground — it is the one thing
/// on this overlay a reader navigates by — so both are asserted together.
#[test]
fn the_palette_paints_a_panel_ground_without_losing_the_selection() {
    let cmds: Vec<SlashCommand> = (0..3)
        .map(|i| SlashCommand::new(format!("/cmd{i:02}"), "desc"))
        .collect();
    let menu = SlashMenu::filter(&cmds, "/");
    let area = Rect {
        x: 0,
        y: 0,
        width: 40,
        height: (SLASH_POPUP_MAX_ROWS as u16) + 3,
    };
    let mut buf = Buffer::empty(area);
    render_slash_popup(&menu, 1, &[], area, &mut buf);

    // The border corner: chrome, and on the panel ground like everything else.
    assert_eq!(
        buf.cell((0, 0)).map(|c| c.bg),
        Some(token::PANEL),
        "the border sits on the panel ground:\n{}",
        buffer_text(&buf)
    );
    let selected_row = buffer_rows(&buf)
        .iter()
        .position(|row| row.contains("/cmd01"))
        .expect("the selected command renders") as u16;
    assert_eq!(
        buf.cell((2, selected_row)).map(|c| c.bg),
        Some(token::HL),
        "the selected row keeps its highlight over the panel ground:\n{}",
        buffer_text(&buf)
    );
    let other_row = buffer_rows(&buf)
        .iter()
        .position(|row| row.contains("/cmd02"))
        .expect("an unselected command renders") as u16;
    assert_eq!(
        buf.cell((2, other_row)).map(|c| c.bg),
        Some(token::PANEL),
        "an unselected row is the plain panel ground:\n{}",
        buffer_text(&buf)
    );
}

/// **The `recent` section (#5048).** The browse list ends with the commands
/// this workspace ran before, newest first, under their own heading — and
/// each of them appears exactly once, moved out of its domain group rather
/// than printed twice.
#[test]
fn the_browse_popup_ends_with_the_workspaces_recent_commands() {
    use crate::composer::{PaletteState, SlashDomain};
    let cmds = vec![
        SlashCommand::new("/help", "show help").in_domain(SlashDomain::Session),
        SlashCommand::new("/plan", "the plan").in_domain(SlashDomain::Plan),
        SlashCommand::new("/diff", "the diff").in_domain(SlashDomain::Code),
    ];
    let state = PaletteState {
        recent: vec!["/diff".to_string(), "/plan".to_string()],
        ..PaletteState::default()
    };
    let menu = SlashMenu::filter_with(&cmds, "/", &state);
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
    // Newest first, and the section is the tail of the list.
    let names: Vec<&str> = menu.matches.iter().map(|m| m.name()).collect();
    assert_eq!(
        names,
        vec!["/help", "/diff", "/plan"],
        "the un-run command keeps its group; the run ones move to the tail, \
         newest first: {names:?}"
    );
    assert_eq!(
        menu.sections,
        vec![(0, "session".to_string()), (1, "recent".to_string()),],
        "one domain group survives, then `recent`: {:?}",
        menu.sections
    );
    for name in ["/help", "/plan", "/diff"] {
        assert_eq!(
            text.matches(name).count(),
            1,
            "{name} is drawn once, not twice:\n{text}"
        );
    }
}

/// **The recorded deviation (#5048).** SPEC 10 says where the palette sits,
/// and the deck puts it there.
///
/// The spec asked for a *centered* `Rect`. The deck anchors the overlay to
/// the composer and opens upward, because the query is typed into the
/// composer — a centered box would put the letters being typed and the list
/// they filter at opposite ends of the frame. The deck won that call and the
/// spec bullet was amended in the same change; this is what stops the two
/// drifting apart again, the way `keymap`'s SPEC 11 guard does for the plan
/// chord (#4341).
#[test]
fn spec_10_anchors_the_palette_to_the_composer() {
    let bullet = spec_10_bullet("Overlay:");
    assert!(
        bullet.contains("anchored to the composer"),
        "SPEC 10 must state where the palette actually sits: {bullet}"
    );
    assert!(
        bullet.contains("`panel` bg"),
        "SPEC 10 still asks for the panel ground, and the deck paints it: {bullet}"
    );

    // And the deck does what the bullet says: the popup's left edge is the
    // composer's, and it opens upward from the composer's top row.
    let root = Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 40,
    };
    let composer = Rect {
        x: 3,
        y: 30,
        width: 90,
        height: 3,
    };
    let popup = slash_popup_area(root, composer, 4);
    assert_eq!(popup.x, composer.x, "anchored to the composer's left edge");
    assert_eq!(
        popup.y + popup.height,
        composer.y,
        "opening upward, ending where the composer begins"
    );
}

/// **The recorded deviation, second half (#5048).** SPEC 10 named `nucleo`
/// for fuzzy matching; the palette matches in-tree
/// ([`crate::composer::fuzzy`], whose module doc argues it). The bullet was
/// amended to name what is actually here, and this is what holds it there:
/// a future reader must not be sent looking for a dependency the workspace
/// does not have.
#[test]
fn spec_10_names_the_matcher_the_palette_uses() {
    let bullet = spec_10_bullet("Fuzzy matching");
    assert!(
        !bullet.contains("nucleo"),
        "the palette does not use `nucleo`; the spec must not say it does: {bullet}"
    );
    assert!(
        bullet.contains("matched letters render gold inside each command name"),
        "the spec still owes the reader the behaviour, whatever matches: {bullet}"
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
