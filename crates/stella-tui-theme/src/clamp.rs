//! The hue clamp — SPEC 3.2, and the only thing standing between this palette
//! and the warm-brown drift every gold-on-black scheme dies of.
//!
//! ## Why the clamp is stated in two pieces
//!
//! SPEC 3.2 originally asked for one rule over "every color in the gold role":
//! `r > g > b`, `g >= 0.78 r`, `b <= 0.35 r`. That rule is not satisfiable by a
//! palette that also wants a *lift* — a brighter stop of the same gold for
//! single-cell live indicators — and the spec's own `gold_bright` `#F7D96B`
//! was the proof, measuring `b/r = 0.433`.
//!
//! It is not a bad colour. It is a geometry problem. Lightening a hue moves
//! every channel toward white, and the channel furthest from it — blue, in a
//! gold — moves proportionally the most. Take `GOLD` `#EFC53F` and lift it to
//! `gold_bright`'s lightness holding hue and saturation exactly: you land on
//! `#F3D36F`, `b/r = 0.46`. **No** hue-preserving lift to that lightness
//! satisfies a `0.35` blue ceiling. A single blue bound over both a resting
//! hue and its lift cannot be met by any palette, which is why an earlier pass
//! here recorded an exception instead — a second ceiling reverse-engineered
//! from the one value it had to admit. A bound derived from the value it must
//! admit is not an invariant; it is a rationalization, and it can say no to
//! nothing in principle.
//!
//! The fix is to state each clause where it is coherent, because the two
//! clauses were never doing the same job:
//!
//! - **`g >= 0.78 r` is the hue rule.** It is what separates gold from orange,
//!   and orange on a near-black ground reads brown on a cheap panel. That is
//!   the argument SPEC 3.2 actually makes, and it holds at *every* lightness.
//!   So it applies to every gold, resting or lifted — [`is_gold_role`].
//! - **`b <= 0.35 r` is the saturation rule.** A resting hue is not lightened,
//!   so it must hold full saturation — [`is_resting_gold`].
//! - **A lift is pinned to the gold it lifts** — [`is_lift_of`]: the same hue
//!   within [`LIFT_HUE_TOLERANCE_DEG`], strictly lighter. This is *stricter*
//!   than any blue ceiling, not looser. A ceiling admits every colour beneath
//!   it; an anchor admits only colours that are the authored gold, brighter.
//!   Recolour [`crate::token::GOLD`] and the test tells you immediately whether
//!   the lift still tracks it.
//!
//! The result is one authored gold in the palette and a lift whose relationship
//! to it is proven rather than asserted. SPEC 3.1's values both stand
//! byte-exact.
//!
//! ## Arithmetic
//!
//! The two ratio clauses are integer: `g >= 0.78 * r` is asserted as
//! `100 g >= 78 r`, the same predicate with no float rounding to argue about at
//! a boundary — and a boundary is exactly where a palette drifts. Hue and
//! lightness need real division and are `f64`; they are compared with an
//! explicit tolerance rather than for equality.

use ratatui::style::Color;

use crate::token;

/// The green ratio every gold must clear, as a percentage of red (SPEC 3.2).
/// Below this the colour is orange. Applies at every lightness.
///
/// Declared in `design/tokens/stella-tokens.json` and re-exported here so the
/// bound and the predicate that reads it cannot drift apart — there is one
/// number, generated, and this module is the only thing that consumes it.
pub const GOLD_GREEN_PCT: u32 = token::GOLD_GREEN_PCT;

/// The blue ceiling a *resting* gold must stay under, as a percentage of red
/// (SPEC 3.2). See the module doc for why a lift is not held to it.
pub const GOLD_BLUE_PCT: u32 = token::GOLD_BLUE_PCT;

/// How far a lift's hue may sit from the gold it lifts, in degrees.
///
/// Derived, not chosen. `stella-tui`'s v1 theme records that its warning amber
/// "sits 4.0° from gold in hue, so an outcome may never be told from chrome by
/// hue alone" — 4° is the distance this repository already treats as
/// *indistinguishable*. A lift must be the **same** hue, so its tolerance has
/// to sit strictly inside that: anything at or beyond 4° is a colour a reader
/// could not tell from gold anyway, which is the wrong end of the argument.
///
/// It discriminates in practice. `GOLD_BRIGHT` sits 1.46° from `GOLD` and
/// passes; the v1 gold `#FFB81A` sits 4.3° away and fails; the orange
/// `#EF8A1F` sits 14.8° away and fails.
pub const LIFT_HUE_TOLERANCE_DEG: f64 = token::GOLD_LIFT_HUE_TOLERANCE_DEG;

/// The green floor warm paper must clear, as a percentage of red.
pub const PAPER_GREEN_PCT: u32 = token::PAPER_GREEN_PCT;

/// The blue floor warm paper must clear, as a percentage of red.
pub const PAPER_BLUE_PCT: u32 = token::PAPER_BLUE_PCT;

/// Does `color` satisfy the clamp its row in [`token::ALL`] declares?
///
/// The bridge between the generated table and the predicates below, and the
/// reason the generator emits role *tags* rather than an algorithm: a lift is
/// checked against another token's value, which no per-row template can
/// express.
///
/// [`token::Clamp::Verdict`] and [`token::Clamp::Surface`] assert
/// nothing and hold for any 24-bit value — a verdict's job is to be
/// unmistakable rather than on-brand, and a diff tint is held by the diff
/// renderer's mandatory sign column, not by a channel test.
#[must_use]
pub fn satisfies(color: Color, clamp: token::Clamp) -> bool {
    let Some((r, g, b)) = channels(color) else {
        return false;
    };
    match clamp {
        token::Clamp::RestingGold => is_resting_gold(r, g, b),
        token::Clamp::GoldLift => match channels(token::GOLD) {
            Some(anchor) => is_lift_of((r, g, b), anchor),
            None => false,
        },
        token::Clamp::CoolSilver => is_cool_silver(r, g, b),
        token::Clamp::NeutralGray => is_neutral_gray(r, g, b),
        token::Clamp::WarmPaper => is_warm_paper(r, g, b),
        token::Clamp::Verdict | token::Clamp::Surface => true,
    }
}

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

/// Does this colour belong to the gold role at all — gold rather than orange?
///
/// `r > g > b` and `g >= 0.78 r`. The universal half of SPEC 3.2, true of a
/// resting gold and of every lift of one, because it is a statement about hue
/// and lifting does not change hue.
#[must_use]
pub const fn is_gold_role(r: u8, g: u8, b: u8) -> bool {
    r > g && g > b && 100 * (g as u32) >= GOLD_GREEN_PCT * (r as u32)
}

/// Does this colour belong to the resting gold role (SPEC 3.2)?
///
/// [`is_gold_role`] plus the saturation clause, `b <= 0.35 r`.
#[must_use]
pub const fn is_resting_gold(r: u8, g: u8, b: u8) -> bool {
    is_gold_role(r, g, b) && 100 * (b as u32) <= GOLD_BLUE_PCT * (r as u32)
}

/// sRGB hue in degrees `[0, 360)`, or `None` for an achromatic colour.
///
/// The standard sRGB (HSV) conversion, and named for that space on purpose:
/// [`crate::oklch`] carries a second hue function measuring a different one,
/// and the two answer different questions. This one serves the gold-lift
/// anchor below, whose [`LIFT_HUE_TOLERANCE_DEG`] was cut in sRGB against the
/// gold this palette ships; the separation law is OKLCH and lives there.
/// `None` when all three channels are equal, where hue is undefined — a gray,
/// which has no business claiming a metal.
#[must_use]
pub fn srgb_hue_degrees(r: u8, g: u8, b: u8) -> Option<f64> {
    let (rf, gf, bf) = (f64::from(r), f64::from(g), f64::from(b));
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let delta = max - min;
    if delta == 0.0 {
        return None;
    }
    let hue = if max == rf {
        60.0 * (((gf - bf) / delta).rem_euclid(6.0))
    } else if max == gf {
        60.0 * ((bf - rf) / delta + 2.0)
    } else {
        60.0 * ((rf - gf) / delta + 4.0)
    };
    Some(hue.rem_euclid(360.0))
}

/// Lightness in `0.0..=1.0` — the `L` of HSL, the midpoint of the extreme
/// channels.
#[must_use]
pub fn lightness(r: u8, g: u8, b: u8) -> f64 {
    let (rf, gf, bf) = (f64::from(r), f64::from(g), f64::from(b));
    (rf.max(gf).max(bf) + rf.min(gf).min(bf)) / (2.0 * 255.0)
}

/// The shortest angular distance between two hues, in degrees.
#[must_use]
pub fn hue_distance(a: f64, b: f64) -> f64 {
    let raw = (a - b).abs().rem_euclid(360.0);
    raw.min(360.0 - raw)
}

/// Is `lift` the same gold as `base`, brighter?
///
/// Three things, all required: both are in the gold role, their hues agree to
/// within [`LIFT_HUE_TOLERANCE_DEG`], and the lift is strictly lighter. That
/// last clause is what makes it a *lift* rather than merely a neighbour — two
/// tokens at the same lightness are two golds, and this palette authors one.
///
/// Deliberately anchored to `base` rather than expressed as a looser channel
/// ceiling. An anchor is the stronger constraint and the more durable one: a
/// recolour of the authored gold either carries its lift along or fails here,
/// where a ceiling would silently keep admitting a lift of a gold that no
/// longer exists.
#[must_use]
pub fn is_lift_of(lift: (u8, u8, u8), base: (u8, u8, u8)) -> bool {
    let (lr, lg, lb) = lift;
    let (br, bg, bb) = base;
    if !is_gold_role(lr, lg, lb) || !is_gold_role(br, bg, bb) {
        return false;
    }
    let (Some(lh), Some(bh)) = (srgb_hue_degrees(lr, lg, lb), srgb_hue_degrees(br, bg, bb)) else {
        return false;
    };
    hue_distance(lh, bh) <= LIFT_HUE_TOLERANCE_DEG && lightness(lr, lg, lb) > lightness(br, bg, bb)
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

/// Is this colour warm paper — the light ground's clamp, off the deck?
///
/// `r >= g >= b`, with `g >= 0.97 r` and `b >= 0.93 r`. The mirror of
/// [`is_neutral_gray`]: warm or neutral, never blue. The two floors keep paper
/// from becoming tan, and are taken from brand v1.0's light neutrals, which sit
/// at `g = 0.988 r` and `b = 0.960 r`.
#[must_use]
pub const fn is_warm_paper(r: u8, g: u8, b: u8) -> bool {
    r >= g
        && g >= b
        && (g as u32) * 100 >= (r as u32) * PAPER_GREEN_PCT
        && (b as u32) * 100 >= (r as u32) * PAPER_BLUE_PCT
}
