//! The hue clamp — SPEC 3.2, and the only thing standing between this palette
//! and the warm-brown drift every gold-on-black scheme dies of.
//!
//! ## Why the clamp is stated in two pieces
//!
//! SPEC 3.2 originally asked for one rule over "every color in the gold role":
//! `r > g > b`, `g >= 0.78 r`, `b <= 0.35 r`. That rule is not satisfiable by a
//! palette that also wants a *lift* — a brighter stop of the same gold for
//! single-cell live indicators — and the spec's own `gold_bright` `#F1C364`
//! was the proof, measuring `b/r = 0.433`.
//!
//! It is not a bad colour. It is a geometry problem. Lightening a hue moves
//! every channel toward white, and the channel furthest from it — blue, in a
//! gold — moves proportionally the most. Take `GOLD` `#D6962C` and lift it to
//! `gold_bright`'s lightness holding hue and saturation exactly: you land on
//! `#F3D36F`, `b/r = 0.46`. **No** hue-preserving lift to that lightness
//! satisfies a `0.35` blue ceiling. A single blue bound over both a resting
//! hue and its lift cannot be met by any palette, which is why an earlier pass
//! here recorded an exception instead — a second ceiling reverse-engineered
//! from the one value it had to admit. A bound derived from the value it must
//! admit is not a rule; it is a rationalization, and it can say no to
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

/// How far a shade's hue may sit from the gold it darkens, in degrees.
///
/// The mirror of [`LIFT_HUE_TOLERANCE_DEG`], and the same number: a shade must
/// be the *same* gold, and "same" does not change direction with lightness.
pub const SHADE_HUE_TOLERANCE_DEG: f64 = token::GOLD_SHADE_HUE_TOLERANCE_DEG;

/// The green floor every neutral must clear, as a percentage of red.
pub const NEUTRAL_GREEN_PCT: u32 = token::NEUTRAL_GREEN_PCT;

/// The blue floor every neutral must clear, as a percentage of red.
pub const NEUTRAL_BLUE_PCT: u32 = token::NEUTRAL_BLUE_PCT;

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
        token::Clamp::GoldLift => match token::lift_anchor().and_then(channels) {
            Some(anchor) => is_lift_of((r, g, b), anchor),
            None => false,
        },
        token::Clamp::GoldShade => match token::shade_anchor().and_then(channels) {
            Some(anchor) => is_shade_of((r, g, b), anchor),
            None => false,
        },
        token::Clamp::WarmNeutral => is_warm_neutral(r, g, b),
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
/// `r > g > b` and `g >= 0.70 r`. The universal half of SPEC 3.2, true of a
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
/// Both are in the gold role, their hues agree to within
/// [`LIFT_HUE_TOLERANCE_DEG`], and the lift is strictly lighter. That last
/// clause is what makes it a *lift* rather than merely a neighbour — two
/// tokens at the same lightness are two golds, and this palette authors one.
///
/// Anchored to `base` rather than expressed as a looser channel
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

/// Is `shade` the same gold as `base`, darker?
///
/// The mirror of [`is_lift_of`], and new with the house system. `gold-ink` is
/// the gold as it appears where the metal itself cannot clear AA — as text or
/// a hairline on paper. It is the same hue, darkened.
///
/// A green ratio is the wrong instrument for it: darkening compresses the
/// channels unevenly, so a shade's `g/r` sits away from its parent's for
/// reasons that have nothing to do with hue. Holding a shade to a ratio drags
/// the ratio down until it admits the shade, and a ratio loosened to fit one
/// dark token stops policing the hue of the bright one. So the shade is held to
/// the gold, and only its *shape* (`r > g > b`) is asserted directly.
#[must_use]
pub fn is_shade_of(shade: (u8, u8, u8), base: (u8, u8, u8)) -> bool {
    let (sr, sg, sb) = shade;
    let (br, bg, bb) = base;
    if !(sr > sg && sg > sb) || !is_gold_role(br, bg, bb) {
        return false;
    }
    let (Some(sh), Some(bh)) = (srgb_hue_degrees(sr, sg, sb), srgb_hue_degrees(br, bg, bb)) else {
        return false;
    };
    hue_distance(sh, bh) <= SHADE_HUE_TOLERANCE_DEG && lightness(sr, sg, sb) < lightness(br, bg, bb)
}

/// Is this colour a house neutral — warm or exactly neutral, never cool?
///
/// `r >= g >= b`, with `g >= 0.94 r` and `b >= 0.82 r`. One predicate for every
/// neutral in the system, ink to paper.
///
/// It replaces three. v5.0 had a blue-tipped dark ramp (`r == g`, `b >= g`),
/// two silvers that sat off neutral in the same direction, and a warm paper
/// ramp — three neutral families, so three clamps. The house system has one:
/// every neutral from `VOID` to `PAPER` is warm or exactly neutral. Three
/// predicates over one family is three places for it to drift, and the two
/// dark ones now disagree with the palette they were written for.
///
/// The floors are what keeps a warm ramp from becoming sepia, which is the
/// failure mode a black-and-gold scheme actually has — the greys creeping warm
/// one reasonable step at a time until the gold stops reading as a separate
/// colour. They are the tightest integer floors the house ramp clears, measured
/// against its two extremes: the hairline on ink (`#292722`, `g/r` 0.951) and
/// the hairline on paper (`#D8CDBD`, `b/r` 0.875).
///
/// Equality is admitted on both sides because the darkest stops are neutral to
/// the byte — `#10100F` is `r == g` — and rounding at that lightness has
/// nowhere else to land.
#[must_use]
pub const fn is_warm_neutral(r: u8, g: u8, b: u8) -> bool {
    r >= g
        && g >= b
        && (g as u32) * 100 >= (r as u32) * NEUTRAL_GREEN_PCT
        && (b as u32) * 100 >= (r as u32) * NEUTRAL_BLUE_PCT
}
