//! The hue clamp — SPEC 3.2, and the only thing standing between this palette
//! and the warm-brown drift every gold-on-black scheme dies of.
//!
//! Two predicates, both pure over `(r, g, b)` so they can be asserted on the
//! shipped tokens *and* reused by any later surface that wants to prove a
//! colour belongs to a role:
//!
//! - [`is_resting_gold`] — `r > g > b`, `g >= 0.78 r`, `b <= 0.35 r`. The green
//!   ratio is the load-bearing half: below it the hue is orange, and orange on
//!   a near-black ground reads brown on cheap panels.
//! - [`is_neutral_gray`] — `r == g` and `b >= g`. Neutral, or tipped toward
//!   blue; never toward red. A gray one point warm is what makes a "black and
//!   gold" scheme read as sepia.
//!
//! All arithmetic is integer. `g >= 0.78 * r` is asserted as
//! `100 g >= 78 r`, which is the same predicate with no float rounding to
//! argue about at a boundary — and a boundary is exactly where a palette
//! drifts.

use ratatui::style::Color;

/// The green ratio a resting gold must clear, as a percentage of red
/// (SPEC 3.2). Below this the colour is orange.
pub const GOLD_GREEN_PCT: u32 = 78;

/// The blue ceiling a resting gold must stay under, as a percentage of red
/// (SPEC 3.2).
pub const GOLD_BLUE_PCT: u32 = 35;

/// The blue ceiling for the *lift* — the one gold that sits above
/// [`GOLD_BLUE_PCT`], used only for single-cell live indicators
/// ([`crate::token::GOLD_BRIGHT`]).
///
/// SPEC 3.2 states one clamp for "every color in the gold role", and SPEC
/// 3.1's own `gold_bright` `#F7D96B` does not satisfy it: `b/r` measures
/// 107/247 = 0.433, against a stated ceiling of 0.35. The spec contradicts
/// itself, and the two ways out are not equal. `prompt.md` rule 4 forbids
/// inventing a colour, so the *value* stands exactly as specified and the
/// ceiling that admits it is recorded here instead — at 44, the tightest
/// whole percent that does (0.43 × 247 = 106.2, under the token's 107).
///
/// This is a recorded exception, not a relaxation: [`is_resting_gold`] is
/// unchanged and still governs [`crate::token::GOLD`], and this bound is tight
/// enough that any drift bluer than the one token it was measured from fails.
/// A maintainer who wants the spec's 0.35 to hold everywhere should re-cut
/// `gold_bright`; the number to beat is in this paragraph.
pub const GOLD_LIFT_BLUE_PCT: u32 = 44;

/// Split a colour into its channels, or `None` if it is not a 24-bit value.
///
/// Every token in this crate is [`Color::Rgb`] by construction, so `None`
/// means a caller handed us an ANSI or indexed colour — which no clamp can
/// speak about.
#[must_use]
pub const fn channels(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

/// Does this colour belong to the resting gold role (SPEC 3.2)?
///
/// `r > g > b`, `g >= 0.78 r`, `b <= 0.35 r`.
#[must_use]
pub const fn is_resting_gold(r: u8, g: u8, b: u8) -> bool {
    gold_shape(r, g, b) && 100 * (b as u32) <= GOLD_BLUE_PCT * (r as u32)
}

/// Does this colour belong to the lifted gold role — the resting clamp with
/// [`GOLD_LIFT_BLUE_PCT`] in place of the blue ceiling?
///
/// Only [`crate::token::GOLD_BRIGHT`] may take it; see that constant's doc.
#[must_use]
pub const fn is_lifted_gold(r: u8, g: u8, b: u8) -> bool {
    gold_shape(r, g, b) && 100 * (b as u32) <= GOLD_LIFT_BLUE_PCT * (r as u32)
}

/// The half of the gold clamp both bounds share: descending channels and the
/// green ratio that separates gold from orange.
#[must_use]
const fn gold_shape(r: u8, g: u8, b: u8) -> bool {
    r > g && g > b && 100 * (g as u32) >= GOLD_GREEN_PCT * (r as u32)
}

/// Is this colour a neutral or blue-tipped gray (SPEC 3.2)?
///
/// `r == g` and `b >= g`. A gray with `r > g` is warm and is what turns a
/// black-and-gold scheme sepia; a gray with `g > r` is green-tipped and is not
/// what the spec asks for either.
#[must_use]
pub const fn is_neutral_gray(r: u8, g: u8, b: u8) -> bool {
    r == g && b >= g
}

/// Is this colour a cool silver — the second metal, never warm?
///
/// `b > r` and `g >= r`. SPEC 3.2's gray clamp is stated for the neutral ramp
/// and the two silvers do not satisfy its `r == g` half by design (they sit
/// one to two points off neutral, which is the tilt SPEC 3.1's opening
/// sentence describes). Left unclamped they would be the palette's one
/// unguarded warm-drift surface — the metal that appears on every read, skill
/// and memory row — so they carry the weaker predicate that still forbids the
/// failure mode: blue strictly above red, green never below it.
#[must_use]
pub const fn is_cool_silver(r: u8, g: u8, b: u8) -> bool {
    b > r && g >= r
}
