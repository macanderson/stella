//! The palette's own gate.
//!
//! These tests are the reason the crate exists. SPEC 3.2 asks for the hue
//! clamp to ship "as a unit test on the theme struct so the palette cannot
//! drift", and `prompt.md` rule 3 adds that they may never be weakened. Read
//! that as a standing instruction: a future change that makes one of these
//! pass by loosening a bound has broken the design, not fixed a test.

use ratatui::style::Color;

use crate::clamp;
use crate::fallback;
use crate::glyph;
use crate::token::{self, Role};
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
    for (name, color, role) in token::ALL {
        if role != Role::Gray {
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
    for (name, color, role) in token::ALL {
        if role != Role::Silver {
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

#[test]
fn gold_bright_is_a_recorded_lift_not_an_unclamped_colour() {
    let (r, g, b) = rgb("gold_bright", token::GOLD_BRIGHT);
    // The exception is deliberate and documented on `GOLD_LIFT_BLUE_PCT`:
    // SPEC 3.1's own value does not satisfy SPEC 3.2's blue ceiling, and
    // `prompt.md` rule 4 forbids inventing a replacement colour. What must
    // stay true is that it is a *lift* and not an escape hatch.
    assert!(
        !clamp::is_resting_gold(r, g, b),
        "if gold_bright now clears the resting clamp, delete \
         GOLD_LIFT_BLUE_PCT and hold it to SPEC 3.2 like every other gold"
    );
    assert!(
        clamp::is_lifted_gold(r, g, b),
        "gold_bright #{r:02X}{g:02X}{b:02X} exceeds even the lift ceiling"
    );
    // The lift ceiling is tight against the one token it was measured from:
    // one percent lower and the shipped value fails. That tightness is what
    // stops it becoming a place to park any warm colour.
    assert_eq!(
        clamp::GOLD_LIFT_BLUE_PCT,
        44,
        "the lift ceiling was derived from gold_bright's own 107/247 = 0.433 \
         and is the tightest whole percent admitting it; changing it needs a \
         new derivation, not a nudge"
    );
    assert!(
        100 * u32::from(b) > (clamp::GOLD_LIFT_BLUE_PCT - 1) * u32::from(r),
        "the lift ceiling is no longer tight against gold_bright"
    );
}

/// Both clamps applied through the role table, so a token added tomorrow is
/// held to whatever it claims to be — the anti-drift mechanism, rather than
/// the individual assertions above which only cover today's set.
#[test]
fn token_roles_are_honoured() {
    for (name, color, role) in token::ALL {
        let (r, g, b) = rgb(name, color);
        let ok = match role {
            Role::Gray => clamp::is_neutral_gray(r, g, b),
            Role::Gold => clamp::is_resting_gold(r, g, b),
            // A lift must clear the loose bound and *fail* the tight one —
            // a token that satisfies SPEC 3.2 outright has no business
            // claiming the exception.
            Role::GoldLift => clamp::is_lifted_gold(r, g, b) && !clamp::is_resting_gold(r, g, b),
            Role::Silver => clamp::is_cool_silver(r, g, b),
            // Verdicts carry no hue clamp — their job is to be unmistakable.
            // What they may not be is gold: a pass or a fail that reads as
            // brand chrome is the one confusion the two-metal rule forbids.
            Role::Verdict => !clamp::is_lifted_gold(r, g, b),
            // A tint is a background. It has to stay under the panel it sits
            // on, or it is a wash.
            Role::Tint => {
                let (pr, pg, pb) = rgb("panel", token::PANEL);
                u32::from(r) + u32::from(g) + u32::from(b)
                    <= 2 * (u32::from(pr) + u32::from(pg) + u32::from(pb))
            }
        };
        assert!(
            ok,
            "token `{name}` #{r:02X}{g:02X}{b:02X} fails its declared role {role:?}"
        );
    }
}

// ── The palette itself (SPEC 3.1) ───────────────────────────────────────────

/// The spec's table, byte for byte. The clamps above prove the palette is
/// *well formed*; this proves it is *the specified one* — a recolour that
/// still clears every clamp is still a recolour, and has to be a deliberate
/// diff on this list.
#[test]
fn the_palette_is_the_specified_one() {
    let expected: [(&str, u32); 17] = [
        ("bg", 0x0A0A0C),
        ("panel", 0x0F0F12),
        ("hl", 0x17171B),
        ("border", 0x26262C),
        ("rule", 0x2C2C33),
        ("gold", 0xEFC53F),
        ("gold_bright", 0xF7D96B),
        ("silver", 0xA9AAB5),
        ("silver_type", 0xBFC1CC),
        ("text", 0xE8E8EC),
        ("muted", 0x777782),
        ("dim", 0x4B4B56),
        ("comment", 0x565660),
        ("green", 0x74C991),
        ("red", 0xE0687A),
        ("diff_add_bg", 0x10201A),
        ("diff_del_bg", 0x241019),
    ];
    assert_eq!(
        token::ALL.len(),
        expected.len(),
        "SPEC 3.1 has 17 tokens; the table has {}",
        token::ALL.len()
    );
    for ((name, color, _), (want_name, want_hex)) in token::ALL.iter().zip(expected) {
        assert_eq!(*name, want_name, "token order diverged from SPEC 3.1");
        let (r, g, b) = rgb(name, *color);
        let got = (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b);
        assert_eq!(
            got, want_hex,
            "token `{name}` is #{got:06X}, SPEC 3.1 says #{want_hex:06X}"
        );
    }
}

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
    for (name, color, _) in token::ALL {
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

#[test]
fn the_write_glyph_is_the_only_fullwidth_one() {
    for (name, ch) in glyph::ALL {
        let want = if name == "write" { 2 } else { 1 };
        assert_eq!(
            glyph::width(ch),
            want,
            "glyph `{name}` ({ch:?}) claims a width the layout does not budget for"
        );
    }
    assert_eq!(glyph::WRITE, '\u{FF0B}', "SPEC 4 names FULLWIDTH PLUS SIGN");
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
/// collided on every skill row. `prompt.md` rule 7 says remove every
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
