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
use crate::generated;
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
            // A lift is checked against the authored gold, not against a
            // ceiling — same hue, brighter. Anchoring is what stops a second
            // gold entering the palette under this role.
            Role::GoldLift => clamp::is_lift_of((r, g, b), rgb("gold", token::GOLD)),
            Role::Silver => clamp::is_cool_silver(r, g, b),
            // Verdicts carry no hue clamp — their job is to be unmistakable.
            // What they may not be is gold: a pass or a fail that reads as
            // brand chrome is the one confusion the two-metal rule forbids.
            Role::Verdict => !clamp::is_gold_role(r, g, b),
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

// ── The generated palette, against the one this crate renders ───────────────

/// A `token::Role`, spelled the way `generated::Clamp` spells it.
///
/// A total `match` rather than a lookup table: a seventh role cannot be added
/// without answering which generated clamp it corresponds to.
fn as_clamp(role: Role) -> generated::Clamp {
    match role {
        Role::Gray => generated::Clamp::NeutralGray,
        Role::Gold => generated::Clamp::RestingGold,
        Role::GoldLift => generated::Clamp::LiftedGold,
        Role::Silver => generated::Clamp::CoolSilver,
        Role::Verdict => generated::Clamp::Verdict,
        Role::Tint => generated::Clamp::Surface,
    }
}

/// `design/tokens/stella-tokens.json` and this crate's hand-written palette
/// hold the same values under the same names.
///
/// Two tables describe one palette: `token::ALL`, which every v2 widget
/// renders, and `generated::ALL`, which `scripts/gen-tokens.py` emits from the
/// JSON that also feeds the website, the brand kit and the Observatory. Until
/// the two are consolidated, this is what stops them being two palettes: a
/// value edited in the JSON without being carried into `token.rs`, or the
/// reverse, fails here by name.
///
/// This could not fail before, and not because it passed — `generated.rs` had
/// no `mod` declaration, so nothing compiled it, and `make tokens` was checking
/// a file the crate did not contain.
///
/// The generated table is the superset: it carries four `web-light` stops
/// (`ink`, `paper`, `paper-panel`, `paper-border`) the terminal never draws, so
/// this asserts containment rather than equality. Every token the TUI *does*
/// render must appear, which is the direction that matters — a hand-written
/// token missing from the JSON is a value outside the system.
#[test]
fn the_hand_written_palette_agrees_with_the_generated_one() {
    for (name, color, role) in token::ALL {
        // The JSON spells names with hyphens; the Rust table uses underscores.
        let wanted = name.replace('_', "-");
        let (_, gen_color, gen_clamp) = generated::ALL
            .iter()
            .find(|(candidate, _, _)| *candidate == wanted)
            .unwrap_or_else(|| {
                panic!(
                    "token `{name}` is rendered by this crate but is not in \
                     design/tokens/stella-tokens.json — add it there, or it is \
                     a colour outside the system"
                )
            });
        assert_eq!(
            color, *gen_color,
            "token `{name}` differs between token.rs and the generated palette \
             — edit design/tokens/stella-tokens.json and run `make tokens-update`"
        );
        assert_eq!(
            as_clamp(role),
            *gen_clamp,
            "token `{name}` is held to a different clamp on each side"
        );
    }
}

/// The two copies of the hue clamp's bounds are the same numbers.
///
/// `clamp.rs` states them for the code that runs; `generated.rs` states them
/// from the JSON. Same failure mode as the palette above, on the constants the
/// predicates are built out of — and a gold clamp that disagrees with itself is
/// how a value passes one gate and fails the other.
#[test]
fn the_clamp_bounds_agree_with_the_generated_ones() {
    assert_eq!(
        clamp::GOLD_GREEN_PCT,
        generated::GOLD_GREEN_PCT,
        "the resting-gold green ratio differs between clamp.rs and the JSON"
    );
    assert_eq!(
        clamp::GOLD_BLUE_PCT,
        generated::GOLD_BLUE_PCT,
        "the resting-gold blue ceiling differs between clamp.rs and the JSON"
    );
}
