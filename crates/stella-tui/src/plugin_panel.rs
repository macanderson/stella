// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The plugin panel host — SPEC 12's half that runs inside the deck.
//!
//! A plugin is leased a rectangle and answers with a [`PanelFrame`]; this
//! module draws the chrome around that rectangle and blits the frame into it.
//! The wire types are `stella_plugin::panel`'s, so the shapes a signed manifest
//! commits to are the shapes drawn here rather than a second copy of them.
//!
//! # What the host keeps, and why each is the host's
//!
//! **The chrome.** The border and the `◳ panel · <plugin>` title are drawn by
//! [`chrome`] before the plugin's frame is asked for, and the lease it returns
//! is strictly inside them. A plugin cannot paint its own border, so it cannot
//! draw something that reads as Stella's own — the spoofing SPEC 12 forbids is
//! unavailable rather than discouraged.
//!
//! **The clip, in display cells rather than characters.** [`PanelPaint::fits`]
//! refuses a frame that overruns its lease, and [`blit`] clips anyway — `fits`
//! is a validation the caller may forget, and the clip is what makes the
//! forgetting harmless.
//!
//! Both must count the same thing, and the thing is **display width**. A
//! `char` is not a cell: `あ` occupies two columns, and ratatui's own renderer
//! advances by `Cell::cell_width`, skipping the column after a double-width
//! glyph (`BufferDiff::next`). A blit that advanced one column per `char`
//! would place a wide glyph in the last leased column, where the terminal
//! draws it across the host's border — and the skipped column is never
//! repainted, so the border stays overwritten. That is a plugin painting a
//! cell it was not leased, through a path a `chars()`-counting bounds check
//! calls in-bounds. So a glyph whose width would cross the lease's right edge
//! is refused rather than truncated, and a zero-width glyph is refused at the
//! left edge, where the terminal would attach it to the border instead.
//!
//! **The palette.** [`PanelInk`] names a token, never an RGB triple, and
//! [`ink`] resolves it against the shipped theme — so a panel degrades with
//! everything else on a sixteen-colour terminal (SPEC 3.5) and cannot author
//! the warm hue the clamp exists to refuse (SPEC 3.2).
//!
//! **The budget.** A panel that spends longer than its lease allows is drawn
//! from its last good frame and tagged (SPEC 12's throttle). The tag is host
//! ink on host chrome: a slow plugin cannot hide that it is slow.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders, Widget};
use stella_tui_theme::token;
use unicode_width::UnicodeWidthChar;

use stella_plugin::{
    PanelEmphasis, PanelFrame, PanelInk, PanelLine, PanelPaint, PanelPatch, PanelRect, PanelStyle,
};

/// The glyph the host stamps on a plugin's chrome (SPEC 12).
///
/// Not one of the deck's own event glyphs: a reader scanning a
/// transcript should be able to tell at a glance which rectangles Stella drew
/// and which a third party did.
pub const PANEL_GLYPH: char = '◳';

/// The deck colour a panel's ink name resolves to.
///
/// Total over [`PanelInk`], so a token added to the wire vocabulary is a
/// compile error here rather than a colour that silently falls back to
/// something plausible.
#[must_use]
pub fn ink(ink: PanelInk) -> Color {
    match ink {
        PanelInk::Bg => token::BG,
        PanelInk::Panel => token::PANEL,
        PanelInk::Hl => token::HL,
        PanelInk::Border => token::BORDER,
        PanelInk::Rule => token::RULE,
        PanelInk::Gold => token::GOLD,
        PanelInk::GoldBright => token::GOLD_BRIGHT,
        PanelInk::Silver => token::SILVER,
        PanelInk::SilverType => token::SILVER_TYPE,
        PanelInk::Text => token::TEXT,
        PanelInk::Muted => token::MUTED,
        PanelInk::Dim => token::DIM,
        PanelInk::Green => token::GREEN,
        PanelInk::Red => token::RED,
        PanelInk::DiffAddBg => token::DIFF_ADD_BG,
        PanelInk::DiffDelBg => token::DIFF_DEL_BG,
    }
}

/// One span's style, as the deck draws it.
#[must_use]
fn style_of(style: &PanelStyle) -> Style {
    let mut out = Style::new();
    if let Some(fg) = style.fg {
        out = out.fg(ink(fg));
    }
    if let Some(bg) = style.bg {
        out = out.bg(ink(bg));
    }
    for emphasis in &style.emphasis {
        out = out.add_modifier(match emphasis {
            PanelEmphasis::Bold => Modifier::BOLD,
            PanelEmphasis::Dim => Modifier::DIM,
            PanelEmphasis::Italic => Modifier::ITALIC,
            PanelEmphasis::Underline => Modifier::UNDERLINED,
        });
    }
    out
}

/// Draw the host's chrome for `plugin` into `area` and return the rectangle
/// the plugin is leased — strictly inside the border, never overlapping it.
///
/// **The label is the plugin's name and nothing the plugin chose.** `PanelGrant`
/// carries no title for this reason, stated in its own doc: a plugin-authored
/// title could read `GATES` or `stella*`, and chrome a person trusts must not
/// be able to name itself. What goes in the label is the identity they
/// consented to at install.
///
/// An `area` too small to hold a border and one interior cell leases nothing,
/// which callers read as "do not ask this plugin for a frame".
#[must_use]
pub fn chrome(area: Rect, plugin: &str, buf: &mut Buffer) -> Rect {
    if area.width < 3 || area.height < 3 {
        return Rect::new(area.x, area.y, 0, 0);
    }
    let label = format!(" {PANEL_GLYPH} panel · {plugin} ");
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(token::BORDER))
        .title(Span::styled(label, Style::new().fg(token::MUTED)))
        .render(area, buf);
    Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    )
}

/// The lease `rect` expresses on the wire.
#[must_use]
pub fn lease_rect(rect: Rect) -> PanelRect {
    PanelRect {
        cols: rect.width,
        rows: rect.height,
    }
}

/// Blit one frame into the leased rectangle, clipping anything that runs past
/// its edges.
///
/// Clipping rather than refusing: a frame reaches this function only after the
/// caller has had its chance to validate, and a plugin whose row ran one cell
/// long should lose that cell, not the whole panel. Nothing outside `rect` is
/// written under any input — that is the guarantee the rest of the deck is
/// drawn against.
pub fn blit(frame: &PanelFrame, rect: Rect, buf: &mut Buffer) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    match &frame.paint {
        PanelPaint::Lines(lines) => blit_lines(lines, rect, buf),
        PanelPaint::Diff(patches) => blit_patches(patches, rect, buf),
    }
}

fn blit_lines(lines: &[PanelLine], rect: Rect, buf: &mut Buffer) {
    for (row, line) in lines.iter().enumerate() {
        let Ok(row) = u16::try_from(row) else {
            return;
        };
        if row >= rect.height {
            return;
        }
        let y = rect.y + row;
        let mut x = rect.x;
        for span in &line.spans {
            let style = style_of(&span.style);
            x = write_run(buf, x, y, rect, span.text.as_str(), style);
        }
    }
}

fn blit_patches(patches: &[PanelPatch], rect: Rect, buf: &mut Buffer) {
    for patch in patches {
        if patch.row >= rect.height || patch.col >= rect.width {
            continue;
        }
        let y = rect.y + patch.row;
        let x = rect.x + patch.col;
        let style = style_of(&patch.style);
        write_run(buf, x, y, rect, patch.text.as_str(), style);
    }
}

/// Write `text` rightwards from `x`, in display cells, stopping at the lease's
/// right edge. Returns the column after the last glyph written.
///
/// A glyph is placed only when **all** of its columns are inside the lease, so
/// the last leased column refuses a double-width glyph rather than letting the
/// terminal paint its second half over the host's border. A zero-width glyph —
/// a combining mark that arrived without the character it modifies — is
/// dropped at the left edge, where the terminal would otherwise attach it to
/// the border cell.
fn write_run(buf: &mut Buffer, mut x: u16, y: u16, rect: Rect, text: &str, style: Style) -> u16 {
    let right = rect.x.saturating_add(rect.width);
    for ch in text.chars() {
        let width = ch.width().unwrap_or(0);
        if width == 0 {
            // Nothing to advance past, and nowhere safe to put it: at the
            // lease's left edge it would modify the host's border.
            continue;
        }
        let Ok(width) = u16::try_from(width) else {
            continue;
        };
        if x >= right || x.saturating_add(width) > right {
            break;
        }
        write_cell(buf, x, y, ch, style);
        // The columns a wide glyph occupies beyond its first are the
        // terminal's to fill; the buffer must not leave a stale symbol in
        // them, or the diff emits a cell that overlaps the glyph.
        for trailing in 1..width {
            if let Some(cell) = buf.cell_mut((x + trailing, y)) {
                cell.reset();
                cell.set_style(style);
            }
        }
        x += width;
    }
    x
}

/// Write one glyph, if the buffer actually has that cell.
///
/// The bounds check above is against the lease; this one is against the
/// terminal. They are different questions — a lease is only ever as valid as
/// the `Rect` the caller passed — and a panel must never be the reason the deck
/// panics on a resize that shrank the frame between layout and draw.
fn write_cell(buf: &mut Buffer, x: u16, y: u16, ch: char, style: Style) {
    if let Some(cell) = buf.cell_mut((x, y)) {
        cell.set_char(ch);
        cell.set_style(style);
    }
}

/// Stamp the throttle tag on a panel's chrome (SPEC 12).
///
/// Drawn on the border's bottom edge in the deck's own ink, so a plugin can
/// neither remove it nor imitate it. `elapsed_ms` is what the frame actually
/// cost and `budget_ms` what it was leased, because "slow" with no numbers is
/// a complaint rather than a report.
pub fn throttle_tag(area: Rect, elapsed_ms: u64, budget_ms: u32, buf: &mut Buffer) {
    if area.width < 3 || area.height < 3 {
        return;
    }
    let tag = format!(" throttled · {elapsed_ms}ms of {budget_ms}ms ");
    let width = tag.chars().count();
    if width + 2 > usize::from(area.width) {
        return;
    }
    let y = area.y + area.height - 1;
    let style = Style::new().fg(token::RED);
    // The tag is the host's own ASCII, so one column per character holds here
    // where it does not for a plugin's glyphs (see `write_run`).
    for (offset, ch) in tag.chars().enumerate() {
        let Ok(offset) = u16::try_from(offset) else {
            return;
        };
        write_cell(buf, area.x + 1 + offset, y, ch, style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_plugin::{PanelSpan, PanelSurface, PanelText};

    fn text_of(buf: &Buffer) -> String {
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

    fn span(text: &str, style: PanelStyle) -> PanelSpan {
        PanelSpan::new(PanelText::new(text).expect("plain text"), style)
    }

    fn lines(rows: Vec<Vec<PanelSpan>>) -> PanelFrame {
        PanelFrame::new(
            PanelSurface::Overlay,
            1,
            PanelPaint::Lines(rows.into_iter().map(PanelLine::new).collect()),
        )
    }

    /// The host owns the border and the title, and the lease it hands back is
    /// strictly inside them — the guarantee that makes a plugin unable to draw
    /// something a reader would take for Stella's own chrome.
    #[test]
    fn the_chrome_is_the_hosts_and_the_lease_is_inside_it() {
        let area = Rect::new(0, 0, 40, 6);
        let mut buf = Buffer::empty(area);
        let inner = chrome(area, "hello", &mut buf);

        assert_eq!(inner, Rect::new(1, 1, 38, 4), "the lease is inside");
        let painted = text_of(&buf);
        assert!(painted.contains("◳ panel · hello"), "{painted}");
        assert!(painted.starts_with('╭'), "{painted}");
    }

    /// A frame never writes outside its lease, even when it tries. `fits`
    /// refuses such a frame at the seam; this proves the host clips anyway, so
    /// a caller that forgot to validate still cannot corrupt the deck.
    #[test]
    fn a_frame_that_overruns_its_lease_is_clipped_not_drawn() {
        let area = Rect::new(0, 0, 12, 4);
        let mut buf = Buffer::empty(area);
        // The lease is the middle: one row, four columns.
        let lease = Rect::new(2, 1, 4, 1);
        let frame = lines(vec![
            vec![span("XXXXXXXXXX", PanelStyle::plain())],
            vec![span("SECOND", PanelStyle::plain())],
        ]);
        blit(&frame, lease, &mut buf);

        let painted = text_of(&buf);
        let row0: String = (0..12)
            .map(|x| buf.cell((x, 0)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();
        let row1: String = (0..12)
            .map(|x| buf.cell((x, 1)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();
        let row2: String = (0..12)
            .map(|x| buf.cell((x, 2)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();
        assert_eq!(row0.trim(), "", "nothing above the lease: {painted}");
        assert_eq!(row1, "  XXXX      ", "clipped at the lease edge: {painted}");
        assert_eq!(
            row2.trim(),
            "",
            "a second row past a one-row lease draws nothing: {painted}"
        );
    }

    /// A patch anchored outside the lease writes nothing, and one that runs
    /// long is cut at the edge.
    #[test]
    fn a_patch_outside_the_lease_writes_nothing() {
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(area);
        let lease = Rect::new(1, 1, 4, 1);
        let frame = PanelFrame::new(
            PanelSurface::Overlay,
            1,
            PanelPaint::Diff(vec![
                PanelPatch::new(0, 0, PanelText::new("abcdef").unwrap(), PanelStyle::plain()),
                PanelPatch::new(9, 0, PanelText::new("zz").unwrap(), PanelStyle::plain()),
            ]),
        );
        blit(&frame, lease, &mut buf);

        let row1: String = (0..10)
            .map(|x| buf.cell((x, 1)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();
        assert_eq!(
            row1, " abcd     ",
            "clipped, and the off-lease patch is gone"
        );
    }

    /// **The breach witness.** A double-width glyph in the last leased column
    /// is refused, not truncated.
    ///
    /// Counting `char`s instead of display cells put `あ` in that column, where
    /// the terminal draws it across the column *after* it — the host's border —
    /// and ratatui's own diff then skips that column (`BufferDiff::next`
    /// advances by `cell_width`), so nothing ever repaints over it. A plugin
    /// had painted a cell it was not leased, through a path both `fits` and the
    /// clip called in-bounds because both counted characters.
    #[test]
    fn a_wide_glyph_cannot_reach_past_the_lease_into_the_border() {
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(area);
        // Chrome first, so the border cells carry the host's own glyphs.
        let lease = chrome(Rect::new(0, 0, 8, 3), "wide", &mut buf);
        assert_eq!(lease, Rect::new(1, 1, 6, 1));
        let border_x = lease.x + lease.width; // the right border's column
        let before = buf
            .cell((border_x, lease.y))
            .expect("a border cell")
            .symbol()
            .to_string();

        // Five ASCII cells fill all but the last leased column; the wide glyph
        // that follows needs two and has one.
        let frame = lines(vec![vec![
            span("aaaaa", PanelStyle::plain()),
            span("あ", PanelStyle::plain()),
        ]]);
        blit(&frame, lease, &mut buf);

        assert_eq!(
            buf.cell((border_x, lease.y))
                .expect("a border cell")
                .symbol(),
            before,
            "the host's border survived a plugin's wide glyph"
        );
        let row: String = (lease.x..lease.x + lease.width)
            .map(|x| buf.cell((x, lease.y)).map(|c| c.symbol()).unwrap_or(" "))
            .collect();
        assert_eq!(
            row, "aaaaa ",
            "the glyph that did not fit was dropped whole"
        );
    }

    /// A wide glyph that *does* fit occupies both its columns, and the second
    /// is left blank rather than stale — a leftover symbol there would make
    /// the terminal's diff emit a cell overlapping the glyph.
    #[test]
    fn a_wide_glyph_that_fits_claims_both_its_columns() {
        let area = Rect::new(0, 0, 6, 1);
        let mut buf = Buffer::empty(area);
        let frame = lines(vec![vec![span("あb", PanelStyle::plain())]]);
        blit(&frame, area, &mut buf);
        assert_eq!(buf.cell((0, 0)).expect("cell").symbol(), "あ");
        assert_eq!(buf.cell((1, 0)).expect("cell").symbol(), " ");
        assert_eq!(buf.cell((2, 0)).expect("cell").symbol(), "b");
    }

    /// A lone combining mark is dropped rather than placed: it has no column of
    /// its own, and at the lease's left edge the terminal would attach it to
    /// the host's border glyph.
    #[test]
    fn a_zero_width_glyph_is_never_placed() {
        let area = Rect::new(0, 0, 4, 1);
        let mut buf = Buffer::empty(area);
        let frame = lines(vec![vec![span("\u{301}x", PanelStyle::plain())]]);
        blit(&frame, area, &mut buf);
        assert_eq!(
            buf.cell((0, 0)).expect("cell").symbol(),
            "x",
            "the mark was dropped and the real glyph took the first column"
        );
    }

    /// A panel paints in the deck's own tokens, so its colours are the theme's
    /// and degrade with everything else.
    #[test]
    fn a_panels_ink_resolves_to_the_shipped_token() {
        let area = Rect::new(0, 0, 6, 1);
        let mut buf = Buffer::empty(area);
        let frame = lines(vec![vec![span("gold", PanelStyle::ink(PanelInk::Gold))]]);
        blit(&frame, area, &mut buf);
        assert_eq!(buf.cell((0, 0)).expect("cell").fg, token::GOLD);
    }

    /// An over-budget panel says so, in host ink on host chrome.
    #[test]
    fn an_over_budget_panel_is_tagged() {
        let area = Rect::new(0, 0, 40, 5);
        let mut buf = Buffer::empty(area);
        let _ = chrome(area, "slow", &mut buf);
        throttle_tag(area, 91, 33, &mut buf);
        let painted = text_of(&buf);
        assert!(painted.contains("throttled · 91ms of 33ms"), "{painted}");
        let y = area.height - 1;
        let x = 1;
        assert_eq!(buf.cell((x, y)).expect("cell").fg, token::RED);
    }

    /// A rectangle too small for chrome leases nothing, which is how a caller
    /// learns not to ask the plugin for a frame at all.
    #[test]
    fn a_rectangle_too_small_for_chrome_leases_nothing() {
        let area = Rect::new(0, 0, 2, 2);
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 10));
        let inner = chrome(area, "tiny", &mut buf);
        assert_eq!(inner.width, 0);
        assert_eq!(inner.height, 0);
    }
}
