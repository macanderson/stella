//! Degradation — SPEC 3.5.
//!
//! Truecolor is detected from `COLORTERM`; without it the palette narrows to
//! the sixteen ANSI colours. The design survives that because glyphs carry
//! state (SPEC 4, SPEC 13) — the 16-color frame loses the metals' *tone*, not
//! its meaning.
//!
//! Detection is split the way `stella-home` splits its resolvers: [`truecolor`]
//! is pure over the value a caller read, and [`detect_truecolor`] is the
//! one-line wrapper that reads it. That is what lets the mapping be tested
//! without a fake environment — a fallback
//! map nobody can exercise is a fallback map nobody knows is wrong.

use ratatui::style::Color;

use crate::token;

/// Does this `COLORTERM` value promise 24-bit colour?
///
/// The two values terminals actually set are `truecolor` and `24bit`; both
/// have been in the wild long enough that neither can be dropped, and nothing
/// else is a reliable promise. Case-insensitive, because the variable is set
/// by hand often enough to see `TrueColor`.
#[must_use]
pub fn truecolor(colorterm: Option<&str>) -> bool {
    colorterm.is_some_and(|value| {
        value.eq_ignore_ascii_case("truecolor") || value.eq_ignore_ascii_case("24bit")
    })
}

/// [`truecolor`] over the process's own `COLORTERM`.
#[must_use]
pub fn detect_truecolor() -> bool {
    truecolor(std::env::var("COLORTERM").ok().as_deref())
}

/// The 16-color stand-in for a palette token (SPEC 3.5).
///
/// SPEC 3.5 fixes four of these by name — gold to bright yellow, silver to
/// white, silver_type to bright white, muted and dim to bright black — and
/// the rest follow from the ramp they belong to:
///
/// - The grounds ([`token::BG`], [`token::PANEL`]) go to black, and the seams
///   ([`token::HL`], [`token::BORDER`], [`token::RULE`]) to bright black: a
///   seam that collapses into the ground stops being a seam.
/// - Gold takes bright yellow, not plain. A 16-color terminal has no second
///   yellow to lift a live accent to. So the brand's one identity colour
///   takes the brighter of the two stops it can reach. Without that, a reader
///   there cannot tell the mark from an ordinary warning.
/// - [`token::TEXT`] and [`token::SILVER_TYPE`] both take bright white, one
///   tier above plain [`token::SILVER`], so an emphasised secondary still
///   reads as emphasised rather than collapsing into the same stand-in as
///   the text beside it — the whole reason `SILVER_TYPE` is a separate token.
/// - The status colours ([`token::GREEN`], [`token::RED`]) take the bright
///   half, like every chromatic stop above. Both are pale on a dark ground.
///   The deck's own table (`stella-tui`'s `FALLBACKS`) has always shipped
///   bright green and bright red. The v1 deck and a v2 surface must not
///   disagree about a status colour.
/// - The diff tints go to black rather than to a green and a red background.
///   At 16 colours a tint is not available; only a wash is, and a wash on
///   every changed row would spend the palette's whole red budget on a healthy
///   diff. The mandatory sign column (SPEC 6.4) is what carries the diff here,
///   which is exactly the case the sign column exists for.
///
/// Total by construction: the match is exhaustive over [`token::ALL`], and
/// `every_token_has_a_fallback` proves it.
#[must_use]
pub fn ansi16(color: Color) -> Color {
    match color {
        token::BG | token::PANEL | token::DIFF_ADD_BG | token::DIFF_DEL_BG => Color::Black,
        token::HL | token::BORDER | token::RULE => Color::DarkGray,
        token::GOLD | token::GOLD_BRIGHT => Color::LightYellow,
        token::SILVER => Color::Gray,
        token::TEXT | token::SILVER_TYPE => Color::White,
        token::MUTED | token::DIM => Color::DarkGray,
        token::GREEN => Color::LightGreen,
        token::RED => Color::LightRed,
        // The light-theme stops. They reach the terminal only when a paper
        // theme is active, and at sixteen colours a paper ground *is* white —
        // there is no lighter tier to distinguish panel from canvas, so they
        // collapse together and `paper_border` takes the one gray that is
        // left. `ink` is a dark ground like `bg` and degrades the same way.
        token::INK => Color::Black,
        token::PAPER | token::PAPER_PANEL => Color::White,
        token::PAPER_BORDER => Color::Gray,
        // Not a palette token — a caller's own colour, or one this crate does
        // not own. Passing it through is the answer: this function
        // narrows the palette, it does not police what else reaches a cell.
        other => other,
    }
}
