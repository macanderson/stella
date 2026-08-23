//! OKLCH hue — the one ruler the 30° separation law is measured with.
//!
//! The law is one sentence: **no chromatic role may sit within 30° of the
//! accent, and no two categorical hues within 30° of each other.** It is
//! asserted on the terminal palette (`stella-tui`'s `theme::tests`), on the
//! eight instrument surfaces (`stella-cli`'s `design_token_parity`), and on the
//! web schemes (`scripts/check-hue-separation.py`) — three suites that had two
//! Rust implementations and two different floors between them (#4071). This
//! module is the single Rust answer; the Python guard holds its own port to
//! this file rather than to a copy.
//!
//! ## Why OKLCH, and not the two metrics before it
//!
//! The metric has moved twice, each time because the previous one stopped
//! measuring the thing the law is about. It was squared euclidean RGB
//! distance, which worked while the brand was green and every categorical hue
//! was far from it in all three channels, and broke against a blue brand: a
//! violet and a blue can be 40° apart — plainly different colours in a terminal
//! cell — and still sit within 0x30 per channel, because blue is pinned at 0xFF
//! in both. It became sRGB (HSV) hue, which fixed that.
//!
//! sRGB hue in turn breaks against a *yellow* brand, because sRGB's hue circle
//! is badly non-uniform through the warm quadrant. Measured on this palette's
//! own values: the accent and the success green are 63.1° apart in OKLCH but
//! 94.8° in sRGB, while the accent and the danger rose are 78.0° in OKLCH but
//! 54.7° in sRGB — sRGB stretches yellow→green and compresses yellow→red by
//! roughly 30° each. The consequence is not academic. It makes the 54.7° sRGB
//! arc between the accent and danger too narrow to hold a warning at 30° from
//! both, so the metric would have rejected the one hue that is *actually* 39.1°
//! from each — an unbuildable law, not a strict one.
//!
//! Do not re-derive the metric a third time without stating why, here, in this
//! chain.
//!
//! ## Not [`crate::clamp::srgb_hue_degrees`]
//!
//! That function is sRGB hue and stays sRGB hue: it serves the gold-lift
//! anchor, whose 3° tolerance was cut in that space against the gold this
//! palette actually ships. Two hue functions in one crate is a hazard only
//! while either one is named for the job instead of the space, which is how
//! this module and that one are named.

/// The floor two chromatic roles must clear to be told apart in one cell.
///
/// Stated once, for every surface. It was 30° on the terminal side and 20° in
/// the instrument-surface parity test — one law with two numbers, which means
/// the stricter one was never the law and the looser one was never enforced
/// (#4071). Every shipped pair clears 30° on both web schemes; the tightest is
/// `--warn` to `--bad` at 38.9° dark and 37.6° light.
pub const SEPARATION_FLOOR_DEG: f64 = 30.0;

/// OKLCH hue in degrees `[0, 360)` for an 8-bit sRGB triple.
///
/// sRGB → linear → LMS → Oklab → the angle of the two chromatic axes,
/// following Björn Ottosson's published matrices transcribed rather than
/// approximated. Achromatic input answers `0.0`: a gray has no hue, and every
/// caller here filters neutrals out before asking rather than reading a
/// sentinel.
#[must_use]
pub fn hue_deg(r: u8, g: u8, b: u8) -> f64 {
    // sRGB → linear light.
    let lin = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (lin(r), lin(g), lin(b));
    // Linear sRGB → LMS, then the cube root that makes the space uniform.
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    // LMS → Oklab's two chromatic axes; the hue is their angle.
    let a_axis = 1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s;
    let b_axis = 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s;
    if a_axis.hypot(b_axis) < 1e-6 {
        return 0.0;
    }
    b_axis.atan2(a_axis).to_degrees().rem_euclid(360.0)
}

/// The shortest angular distance between two hues, in degrees.
///
/// The shorter way round the wheel: 350° and 10° are 20° apart, not 340°.
#[must_use]
pub fn separation(a: f64, b: f64) -> f64 {
    let d = (a - b).abs().rem_euclid(360.0);
    d.min(360.0 - d)
}

/// [`separation`] between two 8-bit sRGB triples.
#[must_use]
pub fn separation_deg(a: (u8, u8, u8), b: (u8, u8, u8)) -> f64 {
    separation(hue_deg(a.0, a.1, a.2), hue_deg(b.0, b.1, b.2))
}

/// Parse `#RRGGBB` (or `RRGGBB`) into channels, or `None` if it is not one.
///
/// Here rather than at each call site because the surfaces that need this
/// ruler read their colours out of a stylesheet as text, and a second hex
/// parser per suite is the shape this module exists to end.
#[must_use]
pub fn channels_of_hex(hex: &str) -> Option<(u8, u8, u8)> {
    let hex = hex.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some((channel(0)?, channel(2)?, channel(4)?))
}
