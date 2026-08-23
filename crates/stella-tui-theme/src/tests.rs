//! The palette's own gate.
//!
//! These tests are the reason the crate exists. SPEC 3.2 asks for the hue
//! clamp to ship "as a unit test on the theme struct so the palette cannot
//! drift". They may never be weakened — read that as a standing instruction:
//! a future change that makes one of these pass by loosening a bound has
//! broken the design, not fixed a test.

use ratatui::style::Color;

use crate::clamp;
use crate::fallback;
use crate::glyph;
use crate::token::{self, Clamp};
use crate::wordmark;

/// Channels of a token, which is always 24-bit by construction.
fn rgb(name: &str, color: Color) -> (u8, u8, u8) {
    clamp::channels(color).unwrap_or_else(|| panic!("token `{name}` is not a 24-bit colour"))
}

// ── The clamp, on the shipped palette (SPEC 3.2) ────────────────────────────

#[test]
fn gold_is_gold_and_not_orange() {
    let (r, g, b) = rgb("gold", token::GOLD);
    assert!(
        clamp::is_resting_gold(r, g, b),
        "gold #{r:02X}{g:02X}{b:02X} fails SPEC 3.2: needs r > g > b, \
         g >= 0.78r (>= {}), b <= 0.35r (<= {})",
        clamp::GOLD_GREEN_PCT * u32::from(r) / 100,
        clamp::GOLD_BLUE_PCT * u32::from(r) / 100,
    );
}

#[test]
fn every_gray_token_is_neutral_or_blue_tipped() {
    for &(name, color, clamp) in token::ALL {
        if clamp != Clamp::NeutralGray {
            continue;
        }
        let (r, g, b) = rgb(name, color);
        assert!(
            clamp::is_neutral_gray(r, g, b),
            "gray token `{name}` #{r:02X}{g:02X}{b:02X} fails SPEC 3.2: \
             needs r == g and b >= g. A gray one point warm is what makes \
             black-and-gold read as sepia."
        );
    }
}

#[test]
fn both_silvers_stay_cool() {
    for &(name, color, clamp) in token::ALL {
        if clamp != Clamp::CoolSilver {
            continue;
        }
        let (r, g, b) = rgb(name, color);
        assert!(
            clamp::is_cool_silver(r, g, b),
            "silver token `{name}` #{r:02X}{g:02X}{b:02X} has drifted warm: \
             needs b > r and g >= r"
        );
    }
}

/// The clamp is only worth having if it rejects the thing it was written
/// against. These are the failures SPEC 3.2 names in prose, asserted as
/// behaviour so a future "simplification" of the predicate is caught.
#[test]
fn the_clamp_rejects_what_it_was_written_against() {
    // Orange: r > g > b holds and blue is low, but green sits under 0.78r.
    // This is the drift that reads brown on a cheap panel.
    assert!(
        !clamp::is_resting_gold(0xEF, 0x8A, 0x1F),
        "an orange must not pass as gold"
    );
    // Pale gold: green fine, blue over 0.35r — washed out toward cream.
    assert!(
        !clamp::is_resting_gold(0xEF, 0xC5, 0xA0),
        "a cream must not pass as gold"
    );
    // Warm gray: one point of red over green is the whole failure mode.
    assert!(
        !clamp::is_neutral_gray(0x78, 0x77, 0x82),
        "a warm gray must not pass as neutral"
    );
    // A warm gray that is *also* blue-tipped is still warm.
    assert!(
        !clamp::is_neutral_gray(0x79, 0x77, 0xFF),
        "red above green is warm however blue the colour is"
    );
    // The repo's own v1 gold (`stella-tui::palette::BRAND`, #FFB81A) is the
    // concrete warm hex this palette exists to replace: green clears the
    // ratio, but it is the value the v2 spec cut a new gold away from, and
    // the neutral clamp rejects the v1 warm text ramp it travelled with.
    assert!(
        !clamp::is_neutral_gray(0xF4, 0xF1, 0xEA),
        "the v1 warm-paper text tone must not pass as a v2 neutral"
    );
}

/// `gold_bright` is not a second authored gold — it is [`token::GOLD`],
/// brighter, and that relationship is the thing under test.
///
/// This replaced an exception. An earlier pass recorded a second blue ceiling
/// reverse-engineered from this one token, because SPEC 3.2's `b <= 0.35 r`
/// does not admit it. The geometry test below is why that was the wrong shape:
/// no lift to this lightness can satisfy that ceiling, so the ceiling was
/// never the rule a lift could be held to.
#[test]
fn gold_bright_is_a_lift_of_gold() {
    let gold = rgb("gold", token::GOLD);
    let lift = rgb("gold_bright", token::GOLD_BRIGHT);
    assert!(
        clamp::is_lift_of(lift, gold),
        "gold_bright #{:02X}{:02X}{:02X} is no longer a lift of gold \
         #{:02X}{:02X}{:02X}: needs the same hue within {}° and greater \
         lightness",
        lift.0,
        lift.1,
        lift.2,
        gold.0,
        gold.1,
        gold.2,
        clamp::LIFT_HUE_TOLERANCE_DEG,
    );
    // The anchor is the constraint, so it has to be tight enough to reject a
    // near neighbour. The v1 palette's own gold is 4.3° away and is exactly
    // the colour this whole crate exists to not become.
    assert!(
        !clamp::is_lift_of((0xFF, 0xB8, 0x1A), gold),
        "the v1 gold #FFB81A must not pass as a lift of the v2 gold"
    );
    // Same hue, but not lighter, is not a lift — it is a second gold, and this
    // palette authors one.
    assert!(
        !clamp::is_lift_of(gold, gold),
        "a colour is not a lift of itself"
    );
}

/// The finding that settled the clamp's shape, kept as a test so nobody has to
/// re-derive it: **`b <= 0.35 r` is unsatisfiable above lightness 0.6745, by
/// any colour at all.**
///
/// In a gold, red is the brightest channel and blue the dimmest, so lightness
/// is `(r + b) / 510` and the ceiling bounds `b` at `0.35 r`. Push `r` to its
/// maximum of 255 and `b` follows to 89, which is lightness 0.6745 — and there
/// is no further to go, because `r` cannot exceed 255. `gold_bright` sits at
/// 0.6941.
///
/// That is why the earlier exception was the wrong shape. The blue ceiling was
/// not a strict rule that one token happened to break; above this lightness it
/// is a rule *nothing* can satisfy, so holding a lift to it was never
/// available. The fix is the two-clause split in [`crate::clamp`] — the hue
/// clause holds at every lightness, the saturation clause only where a colour
/// is not lightened.
///
/// Exhaustive rather than sampled: `g` does not enter lightness for `r > g >
/// b`, so sweeping all 65,536 `(r, b)` pairs covers every case.
#[test]
fn the_resting_blue_ceiling_is_unsatisfiable_above_this_lightness() {
    /// The most lightness `b <= 0.35 r` admits: `(255 + 89) / 510`.
    const CEILING: f64 = 344.0 / 510.0;

    let mut worst = 0.0f64;
    let mut witness = (0u8, 0u8);
    for r in 0u16..=255 {
        for b in 0u16..=255 {
            if b >= r || 100 * b > clamp::GOLD_BLUE_PCT as u16 * r {
                continue;
            }
            // Green sits between the two in a gold and so never sets either
            // extreme; passing `b` for it keeps red the max and blue the min,
            // which is the only thing lightness reads.
            let light = clamp::lightness(r as u8, b as u8, b as u8);
            if light > worst {
                worst = light;
                witness = (r as u8, b as u8);
            }
        }
    }
    assert!(
        (worst - CEILING).abs() < 1e-9,
        "the reachable ceiling moved to {worst:.6} at r={}, b={} — the \
         geometry argument in `clamp`'s module doc needs redoing",
        witness.0,
        witness.1,
    );

    let (r, g, b) = rgb("gold_bright", token::GOLD_BRIGHT);
    assert!(
        clamp::lightness(r, g, b) > CEILING,
        "gold_bright now sits at or below {CEILING:.4}, where the resting \
         ceiling IS satisfiable. If it has come down that far, hold it to \
         `is_resting_gold` like every other gold and delete the lift role."
    );
    // And the resting gold is comfortably below it, which is what makes the
    // saturation clause meaningful where it does apply.
    let (r, g, b) = rgb("gold", token::GOLD);
    assert!(clamp::lightness(r, g, b) < CEILING);
}

/// Every token against the clamp its own row declares — the anti-drift
/// mechanism, rather than the individual assertions above which only cover the
/// tokens named there.
///
/// The pairing is generated from `design/tokens/stella-tokens.json`, so adding
/// a colour means declaring in that file what kind of colour it is, and a warm
/// hex has no honest declaration to pick. [`clamp::satisfies`] is the bridge:
/// the generator emits role *tags*, never predicates, because a lift is checked
/// against another token's value and no per-row template can express that.
#[test]
fn every_token_satisfies_the_clamp_it_declares() {
    for &(name, color, clamp) in token::ALL {
        let (r, g, b) = rgb(name, color);
        assert!(
            clamp::satisfies(color, clamp),
            "token `{name}` #{r:02X}{g:02X}{b:02X} fails its declared clamp {clamp:?}"
        );
    }
}

/// A verdict may not read as brand chrome. SPEC 2's two-metal rule forbids
/// exactly one confusion — a pass or a fail that looks like gold — and the
/// generated `Clamp::Verdict` asserts nothing on its own, by design, so the
/// prohibition is stated here instead of going unstated.
#[test]
fn no_verdict_colour_is_a_gold() {
    for &(name, color, clamp) in token::ALL {
        if clamp != Clamp::Verdict {
            continue;
        }
        let (r, g, b) = rgb(name, color);
        assert!(
            !clamp::is_gold_role(r, g, b),
            "verdict `{name}` #{r:02X}{g:02X}{b:02X} is in the gold role"
        );
    }
}

// ── The palette itself (SPEC 3.1) ───────────────────────────────────────────

#[test]
fn token_names_are_unique() {
    for (i, (name, _, _)) in token::ALL.iter().enumerate() {
        assert!(
            !token::ALL[..i].iter().any(|(seen, _, _)| seen == name),
            "token `{name}` is declared twice"
        );
    }
}

// ── Degradation (SPEC 3.5) ──────────────────────────────────────────────────

#[test]
fn every_token_has_a_fallback() {
    for &(name, color, _) in token::ALL {
        let got = fallback::ansi16(color);
        assert_ne!(
            got, color,
            "token `{name}` has no 16-color stand-in — `ansi16` passed its \
             24-bit value straight through to a terminal that cannot show it"
        );
        assert!(
            !matches!(got, Color::Rgb(..)),
            "token `{name}` degrades to another 24-bit colour, which is not a \
             degradation"
        );
    }
}

/// The five mappings SPEC 3.5 names outright.
#[test]
fn the_named_fallbacks_are_what_the_spec_says() {
    assert_eq!(
        fallback::ansi16(token::GOLD),
        Color::Yellow,
        "gold to yellow"
    );
    assert_eq!(
        fallback::ansi16(token::GOLD_BRIGHT),
        Color::Yellow,
        "gold to yellow"
    );
    assert_eq!(
        fallback::ansi16(token::SILVER),
        Color::Gray,
        "silver to white"
    );
    assert_eq!(
        fallback::ansi16(token::MUTED),
        Color::DarkGray,
        "muted to bright black"
    );
    assert_eq!(
        fallback::ansi16(token::DIM),
        Color::DarkGray,
        "dim to bright black"
    );
    assert_eq!(fallback::ansi16(token::RED), Color::Red, "red to ANSI red");
    assert_eq!(
        fallback::ansi16(token::GREEN),
        Color::Green,
        "green to ANSI green"
    );
}

/// The metals must not collapse into each other, or the 16-color frame loses
/// the one distinction the whole design is built on.
#[test]
fn the_two_metals_stay_apart_at_16_colors() {
    assert_ne!(
        fallback::ansi16(token::GOLD),
        fallback::ansi16(token::SILVER),
        "gold and silver degrade to the same colour"
    );
    assert_ne!(
        fallback::ansi16(token::TEXT),
        fallback::ansi16(token::SILVER),
        "prose and the silver metal degrade to the same colour"
    );
    assert_ne!(
        fallback::ansi16(token::BG),
        fallback::ansi16(token::BORDER),
        "the ground and its seams degrade to the same colour"
    );
}

#[test]
fn truecolor_is_detected_from_the_values_terminals_actually_set() {
    assert!(fallback::truecolor(Some("truecolor")));
    assert!(fallback::truecolor(Some("24bit")));
    assert!(
        fallback::truecolor(Some("TrueColor")),
        "set by hand often enough"
    );
    assert!(!fallback::truecolor(Some("")));
    assert!(!fallback::truecolor(Some("256color")));
    assert!(!fallback::truecolor(None));
}

// ── Glyphs (SPEC 4) and the wordmark (SPEC 3.3) ─────────────────────────────

/// Every glyph in the vocabulary is one terminal cell wide.
///
/// [`glyph::WRITE`] used to be the one exception — U+FF0B FULLWIDTH PLUS
/// SIGN, budgeted as two cells — until #4482 found it resolving through font
/// fallback to PingFang SC, a proportional CJK face, on macOS. It is now the
/// native `+` (U+002B), which makes this the whole vocabulary rather than a
/// carved-out rule for one glyph.
#[test]
fn every_glyph_is_one_terminal_cell_wide() {
    for (name, ch) in glyph::ALL {
        assert_eq!(
            glyph::width(ch),
            1,
            "glyph `{name}` ({ch:?}) claims a width the layout does not budget for"
        );
    }
    assert_eq!(
        glyph::WRITE,
        '+',
        "#4482: the native PLUS SIGN, not the fullwidth form"
    );
}

#[test]
fn glyphs_are_distinct() {
    for (i, (name, ch)) in glyph::ALL.iter().enumerate() {
        assert!(
            !glyph::ALL[..i].iter().any(|(_, seen)| seen == ch),
            "glyph `{name}` ({ch:?}) is already spoken for — one glyph, one \
             meaning, or the vocabulary stops being one"
        );
    }
}

#[test]
fn the_spinner_starts_on_the_running_glyph() {
    assert_eq!(
        glyph::SPINNER[0],
        glyph::RUNNING,
        "a spinner whose first frame is not the resting glyph flickers on \
         every start"
    );
    assert_eq!(glyph::SPINNER.len(), 4, "SPEC 4 names four frames");
}

#[test]
fn the_eighth_ramp_is_a_ramp() {
    assert_eq!(glyph::BLOCK_EIGHTHS[0], ' ', "empty is empty");
    assert_eq!(glyph::BLOCK_EIGHTHS[8], '█', "full is full");
    for (i, ch) in glyph::BLOCK_EIGHTHS.iter().enumerate().skip(1) {
        assert_eq!(
            glyph::width(*ch),
            1,
            "eighth {i} ({ch:?}) is not one cell, which breaks every meter"
        );
    }
}

/// Every character the vocabulary draws has a row in the coverage table, and
/// every row is a character it draws.
///
/// This is the check SPEC 4 never had: its glyphs were chosen for meaning and
/// nobody asked the brand font whether it ships them. Asking, once, found eight
/// that it does not — including `NODE_TYPE` and three of the spinner's four
/// frames, which every prose account of the problem had missed, because a prose
/// list is written from the constants a reader happens to look at and this walks
/// the arrays.
///
/// Totality both ways is the point. A glyph added without a row is the failure
/// this exists to stop; a row left behind by a glyph that moved is how the
/// table would come to describe a vocabulary that no longer exists — which is
/// exactly what `DRIFT_FALLBACK` did to SPEC 4 for two releases.
///
/// No font is read. The table is committed, regenerated by the command its doc
/// comment names, and this walks `ALL` against it — so the gate machine needs
/// nothing installed and cannot pass by skipping.
///
/// `ALL` alone is enough since #4482 folded every [`glyph::SPINNER`] frame and
/// [`glyph::BLOCK_EIGHTHS`] fill into it — before that, this test chained them
/// in by hand because `ALL` did not carry them, which is exactly the blind
/// spot a future walk over `ALL` would otherwise have inherited silently.
#[test]
fn every_glyph_declares_its_brand_font_coverage() {
    let declared: Vec<char> = glyph::ALL.iter().map(|(_, ch)| *ch).collect();

    for ch in &declared {
        assert!(
            glyph::COVERAGE.iter().any(|(row, _)| row == ch),
            "glyph {ch:?} (U+{:04X}) has no row in `glyph::COVERAGE`. Run \
             `scripts/gen-glyph-coverage.py` on a machine with JetBrains Mono \
             installed and commit the table it prints — a glyph nobody asked \
             the brand font about is how six of them came to resolve through \
             font fallback unnoticed (#4318)",
            *ch as u32
        );
    }

    for (ch, _) in glyph::COVERAGE {
        assert!(
            declared.contains(&ch),
            "`glyph::COVERAGE` carries {ch:?} (U+{:04X}), which this vocabulary \
             no longer draws. Regenerate the table; a row for a retired glyph \
             is the shape SPEC 4 kept for `↯` after nothing selected it",
            ch as u32
        );
    }
}

/// The drift glyph is one the brand font actually ships.
///
/// Its predecessor was not, and neither was the substitute SPEC 4 named for
/// it — so on JetBrains Mono the remedy was exactly as absent as the thing it
/// remedied, and on a bare Linux terminal both were tofu (#4318). This is the
/// one row of the coverage table where a substitution was not acceptable,
/// because U+2442 resolves to a *proportional* face and SPEC 2's cell-grid
/// rule is not negotiable there — the same ground U+2317 was rejected on in
/// #4125.
#[test]
fn the_drift_glyph_is_native_to_the_brand_font() {
    let row = glyph::COVERAGE
        .iter()
        .find(|(ch, _)| *ch == glyph::DRIFT)
        .map(|(_, coverage)| *coverage);
    assert_eq!(
        row,
        Some(glyph::Coverage::Native),
        "the drift glyph {:?} is not native to the brand font. A drift glyph \
         that falls back is the defect #4318 names; pick one JetBrains Mono \
         ships",
        glyph::DRIFT
    );
}

#[test]
fn the_wordmark_is_white_word_gold_star_no_space() {
    let [word, star] = wordmark::spans();
    assert_eq!(word.content, "stella");
    assert_eq!(star.content, "*", "the asterisk is the only brand ornament");
    assert_eq!(
        word.style.fg,
        Some(token::TEXT),
        "SPEC 3.3: the word is text white, never gold"
    );
    assert_eq!(star.style.fg, Some(token::GOLD));
    assert_eq!(
        wordmark::WIDTH,
        word.content.chars().count() + star.content.chars().count(),
        "the wordmark's stated width must include no separator"
    );
}

/// The retired form used the skill glyph as a brand mark, so the two meanings
/// collided on every skill row. The redesign removes every
/// occurrence; this is the half that can be asserted here.
#[test]
fn the_wordmark_carries_no_skill_glyph() {
    let [word, star] = wordmark::spans();
    for span in [word, star] {
        assert!(
            !span.content.contains(glyph::SKILL),
            "the `✦ stella` wordmark is retired (SPEC 3.3)"
        );
    }
}
