//! The wordmark — SPEC 3.3.
//!
//! `stella*`: the word in [`token::TEXT`] white, immediately followed by an
//! asterisk in [`token::GOLD`], no space between them. It sits in the upper
//! right of the tab bar on every screen, and the asterisk is the only brand
//! ornament the design has.
//!
//! Two things this module exists to make impossible. The wordmark must never
//! render all-gold — gold is the *acting* metal, and a mark that is always on
//! screen would spend it on nothing. And the retired `✦ stella` form must not
//! come back; it used the skill glyph ([`crate::glyph::SKILL`]) as a brand
//! mark, so the two meanings collided on every skill row. Building the mark
//! from one function rather than from spans at each call site is what keeps
//! both from happening by accident.

use ratatui::style::Style;
use ratatui::text::Span;

use crate::token;

/// The word, without its ornament.
pub const WORD: &str = "stella";

/// The ornament. The whole brand, in one cell.
pub const ORNAMENT: &str = "*";

/// The wordmark as two styled spans: white word, gold asterisk, no separator.
///
/// Returned as spans rather than a `Line` so a caller can pad or place it
/// inside a row it is already building — the tab bar right-aligns it against
/// the tab list, which is a decision the tab bar owns.
#[must_use]
pub fn spans() -> [Span<'static>; 2] {
    [
        Span::styled(WORD, Style::new().fg(token::TEXT)),
        Span::styled(ORNAMENT, Style::new().fg(token::GOLD)),
    ]
}
