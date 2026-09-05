// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The one shared ANSI escape-sequence module for `ratatui` surfaces —
//! emission (#2421), plus a re-export of the stripper so no call site in
//! this crate had to change when it moved.
//!
//! **Stripping** lives in [`stella_ansi`], a leaf crate with no
//! dependencies. `stella-observatory` needed the same strip function: a
//! child tool can color its own output, and that color reaches a
//! transcript as-is. This crate could not lend it the function, since this
//! crate carries `ratatui` and the observatory's own `Cargo.toml` says it
//! carries almost nothing. [`strip_ansi`] is re-exported here, so every
//! caller already in this crate keeps working. See `stella-ansi`'s own
//! docs for the strip rules, and the order rule: strip first, then clip.
//!
//! **Emission** is the inverse, and it is what lets the deck and the plain
//! surface share a *renderer* rather than merely a vocabulary. Modules like
//! [`crate::markdown`] and [`crate::diff`] do real work — parsing markdown,
//! laying out a diff — and express the result as `ratatui` [`Line`]s. The
//! plain surface in `stella-cli` cannot consume those: it writes bytes to a
//! scrollback, and that crate carries `ratatui` explicitly *not* for rendering
//! (see its `Cargo.toml`). [`lines_to_ansi`] closes the gap, so one parse and
//! one layout serve both surfaces and the second copy that would otherwise
//! grow in `stella-cli` never has to exist.
//!
//! This module exists so callers stop growing private copies (three strippers
//! grew in `stella-cli` alone before it was extracted).

use std::fmt::Write as _;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;

pub use stella_ansi::strip_ansi;

/// How a [`Line`] is dressed when a surface writes ANSI bytes directly.
///
/// Two knobs, both of which exist because the plain surface is not the deck:
/// it writes into a terminal it does not own.
#[derive(Debug, Clone, Copy)]
pub struct AnsiPalette {
    /// Emit escapes at all. `false` yields the visible text only — the caller
    /// passes its own colour decision (`colored`'s, which already folds
    /// `NO_COLOR`, `CLICOLOR_FORCE`, `TERM=dumb` and a non-tty stream), so
    /// this module never second-guesses it.
    pub color: bool,
    /// The foreground meaning "the surface's own default ink". A span in this
    /// colour emits no foreground escape, so prose inherits the reader's
    /// terminal profile.
    ///
    /// The deck paints its own black ground and is therefore entitled to state
    /// that prose is [`crate::theme::INK`]; a scrollback line is drawn on
    /// whatever background the user chose, so the same explicit white would be
    /// wrong about half the time. Rendering the *structure* (headings, code,
    /// bullets, diff signs) while leaving the *prose* alone is the difference.
    pub transparent_fg: Option<Color>,
}

impl AnsiPalette {
    /// Colour on, nothing transparent — every style rendered as written.
    #[must_use]
    pub const fn colored() -> Self {
        Self {
            color: true,
            transparent_fg: None,
        }
    }

    /// Colour off — visible text only.
    #[must_use]
    pub const fn monochrome() -> Self {
        Self {
            color: false,
            transparent_fg: None,
        }
    }

    /// Treat `fg` as the surface's default ink (see [`Self::transparent_fg`]).
    #[must_use]
    pub const fn with_transparent_fg(mut self, fg: Color) -> Self {
        self.transparent_fg = Some(fg);
        self
    }
}

/// Render one styled line as a string of visible text and SGR escapes.
///
/// Each span is opened with its own SGR introducer and closed with a reset, so
/// a line is self-contained: nothing leaks into the next line of scrollback if
/// output is interleaved with another writer's. A span whose style resolves to
/// nothing at all is written bare rather than wrapped in an empty escape pair.
#[must_use]
pub fn line_to_ansi(line: &Line<'_>, palette: &AnsiPalette) -> String {
    let mut out = String::new();
    for span in &line.spans {
        // The line's own style is the backdrop for every span in it —
        // `Line::styled` sets it there rather than on each span, and dropping
        // it would silently lose the style of any line built that way.
        let style = line.style.patch(span.style);
        match sgr_params(style, palette) {
            Some(params) => {
                let _ = write!(out, "\u{1b}[{params}m{}\u{1b}[0m", span.content);
            }
            None => out.push_str(&span.content),
        }
    }
    out
}

/// [`line_to_ansi`] over a slice — the shape every caller actually wants,
/// since the modules that produce lines produce them a block at a time.
#[must_use]
pub fn lines_to_ansi(lines: &[Line<'_>], palette: &AnsiPalette) -> Vec<String> {
    lines.iter().map(|l| line_to_ansi(l, palette)).collect()
}

/// The `;`-joined SGR parameters for a style, or `None` when it asks for
/// nothing visible (colour disabled, or an unstyled span).
fn sgr_params(style: Style, palette: &AnsiPalette) -> Option<String> {
    if !palette.color {
        return None;
    }
    let mut params: Vec<String> = Vec::new();

    for (modifier, code) in [
        (Modifier::BOLD, "1"),
        (Modifier::DIM, "2"),
        (Modifier::ITALIC, "3"),
        (Modifier::UNDERLINED, "4"),
        (Modifier::REVERSED, "7"),
        (Modifier::CROSSED_OUT, "9"),
    ] {
        if style.add_modifier.contains(modifier) {
            params.push(code.to_string());
        }
    }

    if let Some(fg) = style.fg
        && palette.transparent_fg != Some(fg)
        && let Some(code) = color_params(fg, false)
    {
        params.push(code);
    }
    if let Some(bg) = style.bg
        && let Some(code) = color_params(bg, true)
    {
        params.push(code);
    }

    (!params.is_empty()).then(|| params.join(";"))
}

/// SGR parameters for one colour, or `None` for `Reset` (which is the absence
/// of a colour, not a colour to select).
fn color_params(color: Color, background: bool) -> Option<String> {
    // Foreground codes; the background is the same table plus ten, which is
    // how SGR is defined and is why one table serves both.
    let base = match color {
        Color::Reset => return None,
        Color::Black => 30,
        Color::Red => 31,
        Color::Green => 32,
        Color::Yellow => 33,
        Color::Blue => 34,
        Color::Magenta => 35,
        Color::Cyan => 36,
        Color::Gray => 37,
        Color::DarkGray => 90,
        Color::LightRed => 91,
        Color::LightGreen => 92,
        Color::LightYellow => 93,
        Color::LightBlue => 94,
        Color::LightMagenta => 95,
        Color::LightCyan => 96,
        Color::White => 97,
        Color::Rgb(r, g, b) => {
            let lead = if background { 48 } else { 38 };
            return Some(format!("{lead};2;{r};{g};{b}"));
        }
        Color::Indexed(i) => {
            let lead = if background { 48 } else { 38 };
            return Some(format!("{lead};5;{i}"));
        }
    };
    Some((base + if background { 10 } else { 0 }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Stripping's own unit tests moved to `stella-ansi` (#2527); this
    // module's tests cover only the emission half plus the round-trip below,
    // which is what actually needs `ratatui` types and belongs here.

    use ratatui::text::Span;

    /// The two halves of this module must invert each other: whatever
    /// [`line_to_ansi`] adds, [`strip_ansi`] must take away and leave exactly
    /// the visible text. This is the property that keeps the emitter honest
    /// without pinning byte-exact escape sequences in every test.
    #[test]
    fn emitted_escapes_strip_back_to_the_visible_text() {
        let line = Line::from(vec![
            Span::styled("head", Style::new().fg(Color::Rgb(1, 2, 3))),
            Span::raw(" plain "),
            Span::styled(
                "tail",
                Style::new()
                    .fg(Color::Green)
                    .bg(Color::Indexed(240))
                    .add_modifier(Modifier::BOLD | Modifier::ITALIC),
            ),
        ]);
        let rendered = line_to_ansi(&line, &AnsiPalette::colored());
        assert!(rendered.contains('\u{1b}'), "nothing was styled at all");
        assert_eq!(strip_ansi(&rendered), "head plain tail");
    }

    #[test]
    fn monochrome_emits_no_escapes_at_all() {
        let line = Line::from(Span::styled(
            "loud",
            Style::new().fg(Color::Red).add_modifier(Modifier::BOLD),
        ));
        assert_eq!(line_to_ansi(&line, &AnsiPalette::monochrome()), "loud");
    }

    /// The reason [`AnsiPalette::transparent_fg`] exists: prose must be able to
    /// keep the reader's own foreground while the structure around it stays
    /// coloured. A palette that dropped *every* colour could not do this.
    #[test]
    fn transparent_fg_frees_prose_without_freeing_the_structure() {
        let palette = AnsiPalette::colored().with_transparent_fg(Color::White);
        let prose = Line::from(Span::styled("body", Style::new().fg(Color::White)));
        assert_eq!(
            line_to_ansi(&prose, &palette),
            "body",
            "the surface's own ink must emit no escape"
        );

        // Same colour, but the span also asks for bold: the modifier is not
        // the foreground and must survive.
        let heading = Line::from(Span::styled(
            "H1",
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        ));
        assert_eq!(line_to_ansi(&heading, &palette), "\u{1b}[1mH1\u{1b}[0m");

        // And a different colour is untouched by the transparency rule.
        let code = Line::from(Span::styled("fn", Style::new().fg(Color::Cyan)));
        assert_eq!(line_to_ansi(&code, &palette), "\u{1b}[36mfn\u{1b}[0m");
    }

    /// `Line::styled` puts the style on the line, not the spans. Patching it
    /// under each span's own style is what keeps such a line from rendering
    /// bare — and the deck builds lines both ways.
    #[test]
    fn a_line_level_style_reaches_its_spans() {
        let line = Line::styled("whole", Style::new().fg(Color::Red));
        assert_eq!(
            line_to_ansi(&line, &AnsiPalette::colored()),
            "\u{1b}[31mwhole\u{1b}[0m"
        );
    }

    #[test]
    fn background_codes_are_the_foreground_table_plus_ten() {
        assert_eq!(color_params(Color::Red, false).as_deref(), Some("31"));
        assert_eq!(color_params(Color::Red, true).as_deref(), Some("41"));
        assert_eq!(color_params(Color::White, false).as_deref(), Some("97"));
        assert_eq!(color_params(Color::White, true).as_deref(), Some("107"));
        // `Reset` is the absence of a colour, not a colour to select.
        assert_eq!(color_params(Color::Reset, false), None);
    }
}
