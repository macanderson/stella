//! Unit tests for [`crate::theme`].
//!
//! Split out of `theme.rs` so the module stays under the 1500-line ratchet
//! (#629). Pure relocation: no test was changed, added, or removed.

use super::*;
use stella_tui_theme::oklch;

#[test]
fn agent_color_is_stable_across_calls() {
    assert_eq!(agent_color("lead"), agent_color("lead"));
    assert_eq!(agent_color("sub:auth"), agent_color("sub:auth"));
}

#[test]
fn agent_color_never_panics_on_empty_or_unicode_ids() {
    let _ = agent_color("");
    let _ = agent_color("agent-🚀-42");
}

#[test]
fn trace_kind_color_covers_every_variant_without_panic() {
    for kind in [
        TraceKind::Stage,
        TraceKind::Text,
        TraceKind::Reasoning,
        TraceKind::Tool,
        TraceKind::File,
        TraceKind::Budget,
        TraceKind::Context,
        TraceKind::Verdict,
        TraceKind::Media,
        TraceKind::Vcs,
        TraceKind::Error,
        TraceKind::Complete,
        TraceKind::Other,
    ] {
        let _ = trace_kind_color(kind);
    }
}

/// Every distinct `Color::Rgb` value in the palette — kept explicit (not
/// derived) so this and [`every_named_token_has_a_fallback`] fail loudly the
/// moment a new truecolor token lands without a [`FALLBACKS`] entry. Role
/// aliases (`INK`, `OK`, `DANGER`, …) share a value with a palette token, so
/// they are intentionally not re-listed — that includes `ACCENT_FILL` and
/// `GOLD` (one gold with `ACCENT`), `GOLD_LIVE` (one value with
/// `ACCENT_LIVE`), `SYNTAX_COMMENT` (the caption tier, `TEXT_TERTIARY`) and
/// `SYNTAX_KEYWORD` (the bright neutral, `palette::TEXT_EMPHASIS`).
const ALL_RGB_TOKENS: &[Color] = &[
    VOID,
    GROUND,
    SURFACE,
    RAISED,
    HAIRLINE,
    HAIRLINE_STRONG,
    TEXT_PRIMARY,
    palette::TEXT_EMPHASIS,
    TEXT_SECONDARY,
    TEXT_TERTIARY,
    TEXT_DIM,
    ACCENT,
    ACCENT_LIVE,
    SUCCESS,
    WARNING,
    DANGER,
    VIOLET,
    TEAL,
    MAGENTA,
    CITRON,
    ORCHID,
    CODE,
    DIFF_ADD_BG,
    DIFF_DEL_BG,
    DIFF_ADD_BG_EMPH,
    DIFF_DEL_BG_EMPH,
    MATCH_BG,
    SYNTAX_STRING,
];

/// Palette values that only ever reach a cell through [`apply_theme`]'s
/// paper remap. They are never rendered on the canonical dark canvas, and
/// `stella-light` is truecolor-only, so they intentionally have no
/// [`FALLBACKS`] entry.
const LIGHT_ONLY_PALETTE_TOKENS: &[&str] = &[
    "brand-ink",
    "brand-ink-deep",
    "gold-ink",
    "success-ink",
    "warning-ink",
    "danger-ink",
    "paper",
    "snow",
    "paper-raised",
    "paper-hairline",
    "ink",
    "muted",
    "ink-dim",
    "ink-emphasis",
];

/// `palette::ALL` is the brand kit's token list in Rust form; this is the
/// check that makes keeping it complete worth anything. Every dark-side
/// palette value must be renderable on a 256- and a 16-colour terminal, so
/// every one of them needs a [`FALLBACKS`] entry — adding a token to
/// `palette` without wiring its degradation fails here rather than in a
/// user's `xterm`.
#[test]
fn every_dark_palette_value_has_a_fallback() {
    for (name, color) in palette::ALL {
        if LIGHT_ONLY_PALETTE_TOKENS.contains(&name) {
            continue;
        }
        assert!(
            FALLBACKS.iter().any(|(rgb, ..)| *rgb == color),
            "palette token `{name}` ({color:?}) has no FALLBACKS entry"
        );
    }
    // The skip list may not name a token that no longer exists.
    for name in LIGHT_ONLY_PALETTE_TOKENS {
        assert!(
            palette::ALL.iter().any(|(n, _)| n == name),
            "LIGHT_ONLY_PALETTE_TOKENS names `{name}`, which is not in palette::ALL"
        );
    }
    // Token names are unique, so `ALL` can be read as a map.
    for (i, (name, _)) in palette::ALL.iter().enumerate() {
        assert!(
            !palette::ALL[..i].iter().any(|(other, _)| other == name),
            "duplicate palette token name `{name}`"
        );
    }
}

#[test]
fn every_named_token_has_a_fallback() {
    for token in ALL_RGB_TOKENS {
        assert!(
            FALLBACKS.iter().any(|(rgb, ..)| rgb == token),
            "token {token:?} has no FALLBACKS entry (256 + 16 index)"
        );
    }
    // No duplicate values in the table (aliases share one entry by value).
    for (i, (rgb, ..)) in FALLBACKS.iter().enumerate() {
        assert!(
            !FALLBACKS[..i].iter().any(|(other, ..)| other == rgb),
            "duplicate FALLBACKS entry for {rgb:?}"
        );
    }
    assert_eq!(
        FALLBACKS.len(),
        ALL_RGB_TOKENS.len(),
        "one FALLBACKS entry per distinct palette token"
    );
}

#[test]
fn role_aliases_track_their_palette_token() {
    // Brand roles resolve to the generated palette, not to a local literal.
    // The accent IS the gold: one value serves text, rules and fills
    // (11.99:1 on ground), so ACCENT, ACCENT_FILL and GOLD are one value on
    // purpose, and ACCENT_LIVE/GOLD_LIVE are the one other gold there is.
    assert_eq!(ACCENT, palette::BRAND);
    assert_eq!(ACCENT_FILL, ACCENT);
    assert_eq!(GOLD, ACCENT);
    assert_eq!(palette::BRAND, palette::GOLD);
    assert_eq!(palette::BRAND_LIVE, palette::GOLD_LIVE);
    assert_eq!(ACCENT_LIVE, palette::BRAND_LIVE);
    assert_eq!(GOLD_LIVE, ACCENT_LIVE);
    // Exactly two golds, and they are different: the resting mark and the
    // live one. A third gold is what this palette exists to refuse.
    assert_ne!(ACCENT, ACCENT_LIVE);
    assert_eq!(VOID, palette::VOID);
    assert_eq!(HAIRLINE_STRONG, palette::HAIRLINE_STRONG);
    assert_eq!(GROUND, palette::GROUND);
    assert_eq!(INK, TEXT_PRIMARY);
    assert_eq!(MUTED, TEXT_SECONDARY);
    assert_eq!(RULE, HAIRLINE);
    assert_eq!(OK, SUCCESS);
    assert_eq!(WARN, WARNING);
    assert_eq!(BAD, DANGER);
    assert_eq!(HELD, VIOLET);
    assert_eq!(RUN, VIOLET);
    assert_eq!(TEXT_DIM, palette::TEXT_DIM);
    // The dim tier is its own value, not an alias of the caption tier — that
    // aliasing is what the palette's fourth neutral replaced.
    assert_ne!(TEXT_DIM, TEXT_TERTIARY);
    // Syntax hues: the keyword/type class rides the bright neutral (the
    // palette's own instruction for a code body), literals stay categorical.
    assert_eq!(SYNTAX_NUMBER, VIOLET);
    assert_eq!(SYNTAX_KEYWORD, palette::TEXT_EMPHASIS);
    assert_eq!(SYNTAX_COMMENT, TEXT_TERTIARY);
    // The categorical roles resolve to the palette's data marks in order.
    assert_eq!(VIOLET, palette::DATA_1);
    assert_eq!(MAGENTA, palette::DATA_2);
    assert_eq!(TEAL, palette::DATA_3);
    assert_eq!(CITRON, palette::DATA_4);
    assert_eq!(ORCHID, palette::DATA_5);
}

/// The kit's one hard prohibition, restated for a gold accent: **gold never
/// carries a verdict.** Gold and [`WARNING`] are 4.0° apart in hue — the
/// same brass at a glance — so a reader must never be asked to tell an
/// *outcome* (ok / warn / bad, done / failed) from chrome by hue. The one
/// status gold does carry is activity: active/running IS the accent, by kit
/// rule ("gold is the signal"), and that pairing is asserted here so it
/// cannot silently regress to a lesser hue either. This is the test that
/// would fail if someone reached for the prettier colour in a verdict
/// mapping.
#[test]
fn gold_never_carries_a_verdict() {
    // Activity is gold — the one permitted (required, even) status use.
    assert_eq!(status_color(AgentStatus::Running), ACCENT);
    for gold in [GOLD, GOLD_LIVE] {
        for (status, name) in [
            (OK, "OK"),
            (WARN, "WARN"),
            (BAD, "BAD"),
            (HELD, "HELD"),
            (RUN, "RUN"),
        ] {
            assert_ne!(gold, status, "{name} must not be a gold");
        }
        for status in [
            AgentStatus::Queued,
            AgentStatus::Paused,
            AgentStatus::WaitingInput,
            AgentStatus::Done,
            AgentStatus::Failed,
            AgentStatus::Killed,
        ] {
            assert_ne!(
                status_color(status),
                gold,
                "status_color({status:?}) returned a gold"
            );
        }
        for kind in [
            TraceKind::Stage,
            TraceKind::Text,
            TraceKind::Reasoning,
            TraceKind::Tool,
            TraceKind::File,
            TraceKind::Budget,
            TraceKind::Context,
            TraceKind::Verdict,
            TraceKind::Media,
            TraceKind::Vcs,
            TraceKind::Error,
            TraceKind::Complete,
            TraceKind::Other,
        ] {
            assert_ne!(
                trace_kind_color(kind),
                gold,
                "trace_kind_color({kind:?}) returned a gold"
            );
        }
        for i in 0..=20 {
            assert_ne!(gauge_color(f64::from(i) / 20.0), gold);
        }
        for id in ["lead", "sub:auth", "", "agent-42"] {
            assert_ne!(agent_color(id), gold, "agent_color({id:?}) returned a gold");
        }
        // And it is not a graph node kind either — a node is a category.
        for kind in [
            "function", "method", "struct", "enum", "trait", "file", "module", "?",
        ] {
            assert_ne!(graph_kind_color(kind), gold);
        }
    }
}

/// OKLCH hue angle for a palette token, via the crate every surface shares.
///
/// The conversion itself lived here as a private `hue_deg` and was transcribed
/// a second time into `stella-cli`'s `design_token_parity`, which then held one
/// law to a 20 degree floor while this file held it to 30 (#4071). Both copies
/// are gone: [`stella_tui_theme::oklch`] is the Rust answer, and its module doc
/// carries the metric's `RGB distance -> sRGB hue -> OKLCH` chain.
fn hue_deg(color: Color) -> f64 {
    let Color::Rgb(r, g, b) = color else {
        panic!("{color:?} must be a truecolor token");
    };
    oklch::hue_deg(r, g, b)
}

/// The shortest angular distance between two hues, in degrees.
fn hue_separation(a: Color, b: Color) -> f64 {
    oklch::separation(hue_deg(a), hue_deg(b))
}

/// Every hue angle `palette.rs` argues from is the angle the ruler computes.
///
/// The palette's doc comments are where the separation law is *reasoned*: the
/// warning's hue was derived by maximising the smaller of two gaps, the orchid
/// was moved because one of its gaps measured under the floor, and each of
/// those arguments is carried by a figure written in prose. A figure in a
/// comment is the thing that goes stale under a recolour, and one already had
/// — `BRAND_INK` claimed 0.2° from `BRAND` where it measures 0.1° (#4071).
///
/// Each row is held three ways at once, the shape
/// `scripts/check-hue-separation.py`'s `CLAIMS` table uses: the number must
/// equal the computation, the number must appear inside the phrase, and the
/// phrase must still be in `palette.rs` verbatim. Recolour and the computation
/// moves; reword and the phrase is gone; edit this table and it agrees with
/// neither.
///
/// Angles the palette states about *retired* values (the amber that sat 5.5°
/// from the previous gold) are deliberately absent: nothing here can compute a
/// colour the palette no longer carries, and a row that cannot fail is worse
/// than no row.
#[test]
fn every_stated_hue_angle_matches_the_computation() {
    use palette::{
        BRAND, BRAND_INK, BRAND_LIVE, DANGER, DATA_1, DATA_2, DATA_3, DATA_4, DATA_5, SUCCESS,
        WARNING,
    };

    let source = include_str!("../palette.rs");
    let sep = hue_separation;
    let hue = hue_deg;

    for (phrase, computed) in [
        ("OKLCH hue 90.8", hue(BRAND)),
        ("3.5 deg from [`BRAND`] in hue", sep(BRAND_LIVE, BRAND)),
        ("(0.1 deg from [`BRAND`])", sep(BRAND_INK, BRAND)),
        ("OKLCH hue 153.9", hue(SUCCESS)),
        ("(63.1 deg from", sep(SUCCESS, BRAND)),
        ("OKLCH hue 51.7", hue(WARNING)),
        ("lands 39.1 deg from gold", sep(WARNING, BRAND)),
        ("and 38.9 deg from danger", sep(WARNING, DANGER)),
        ("OKLCH hue 12.8", hue(DANGER)),
        ("(78.0 deg from", sep(DANGER, BRAND)),
        ("OKLCH hue 292.6", hue(DATA_1)),
        ("(158.2 deg from gold)", sep(DATA_1, BRAND)),
        ("hue 355.6", hue(DATA_2)),
        ("(95.2 deg from gold)", sep(DATA_2, BRAND)),
        ("hue 186.6", hue(DATA_3)),
        ("(95.9 deg from gold)", sep(DATA_3, BRAND)),
        ("hue 126.2", hue(DATA_4)),
        ("(35.4 deg from gold", sep(DATA_4, BRAND)),
        ("hue 324.4", hue(DATA_5)),
        ("126.3 deg from", sep(DATA_5, BRAND)),
        ("31.8 deg from the violet", sep(DATA_5, DATA_1)),
        ("31.1 deg from the rose", sep(DATA_5, DATA_2)),
    ] {
        let stated = format!("{computed:.1}");
        assert!(
            phrase.contains(&stated),
            "this table says palette.rs writes `{phrase}`, but the ruler makes \
             that angle {stated}° — the row disagrees with the computation, \
             which means the row is what is wrong"
        );
        assert!(
            source.contains(phrase),
            "palette.rs no longer says `{phrase}`. The angle it argues from \
             has to stay readable beside the value; move the phrase and move \
             this row with it"
        );
    }
}

/// The palette law, in the form a future edit would actually break.
///
/// The guard has outlived five recolours now (aurora → gold → sky → green →
/// blue → gold again), which is the point: each time, the *shape* of the law
/// survived and only the hue it names changed. Keep rewriting it rather than
/// deleting it.
///
/// This recolour changed the law in three real ways.
///
/// First, the temperature **inverted, and is now a hard rule in the other
/// direction**. The previous palette banned cool grays and asserted every
/// neutral was warm-dominant (`r >= g >= b`); this one requires the exact
/// opposite of every neutral it ships — blue one or two points above red —
/// because the accent is a yellow, and a warm neutral puts the ground and
/// the one owned colour in the same family, which on a cheap panel reads as
/// muddy brown. Clause 3 is therefore the inverse of the clause it replaces,
/// and clause 2 retires the whole warm ramp by value so it cannot come back
/// one token at a time.
///
/// Second, gold is **two values and no more**. The old ramp carried four
/// (`ACCENT`, `ACCENT_DEEP`, `GOLD_BRIGHT`, `GOLD_DEEP`) plus an amber data
/// mark 0.8° away from the accent, which is how a reserved colour stops
/// being one. Clause 6 pins the pair and clause 5 admits no exception for a
/// near-gold categorical hue, because there is no longer one.
///
/// Third, the reservation has **no named neighbours at all**. Warning used
/// to sit 4.0° from the accent and be excused on the grounds that a status
/// is always glyph-paired; it is derived now to sit 39.1° away (see
/// `palette::WARNING`), so the excuse is retired along with the collision.
/// The glyph pairing is still asserted — it is a good rule — but it is no
/// longer required for telling a warning from the mark.
///
/// What must hold now:
///   1. The accent is `#EFC53F`, exactly; the canvas is `#0A0A0C`, exactly.
///      Brand and gold are one family (the collapse IS the identity).
///   2. Every retired hue is gone — the electric-blue family, every gold
///      this palette replaced, and the whole warm neutral ramp with them.
///   3. Grounds and text are cool neutrals: near-neutral, never true black,
///      and never warm-dominant. Warm grays are banned on the dark side.
///   4. Warning ramps warm, success ramps green, danger ramps red.
///   5. Gold is reserved. **Every** chromatic role sits ≥ 30° of hue away —
///      no named neighbours, no carve-outs.
///   6. There are exactly two golds, and the second one is the live stop.
#[test]
fn palette_law_gold_is_the_brand() {
    const RETIRED_AURORA_CYAN: Color = Color::Rgb(0x3F, 0xE0, 0xFF);
    const RETIRED_EMBER_FLAME: Color = Color::Rgb(0xFF, 0x7E, 0x5F);
    const RETIRED_EMBER_CRIMSON: Color = Color::Rgb(0xC2, 0x18, 0x5B);
    const RETIRED_GOLD: Color = Color::Rgb(0xFF, 0xDD, 0x00);
    const RETIRED_GOLD_DEEP: Color = Color::Rgb(0xE0, 0xB8, 0x00);
    /// The old glacier blue, retired *because* it collided with the sky
    /// accent — and doubly banned now: the owner hates ice blue.
    const RETIRED_AGENT_ICE: Color = Color::Rgb(0xA8, 0xC7, 0xF0);
    /// The sky blue of three recolours ago.
    const RETIRED_SKY: Color = Color::Rgb(0x7D, 0xD3, 0xFC);
    const RETIRED_SKY_DEEP: Color = Color::Rgb(0x38, 0xBD, 0xF8);
    /// The warning orange the "get rid of the orange" pass removed; warning
    /// is amber-yellow now and must not slide back to it.
    const RETIRED_WARNING_ORANGE: Color = Color::Rgb(0xFF, 0x8A, 0x1F);
    /// The terminal green of two identities ago, and its deep stop.
    const RETIRED_PHOSPHOR_GREEN: Color = Color::Rgb(0x00, 0xE6, 0x76);
    const RETIRED_PHOSPHOR_GREEN_DEEP: Color = Color::Rgb(0x00, 0xB2, 0x5A);
    /// The vermilion light-theme brand ("ember") and its deep stop.
    const RETIRED_EMBER: Color = Color::Rgb(0xFF, 0x3D, 0x1F);
    const RETIRED_EMBER_DEEP: Color = Color::Rgb(0xD6, 0x2E, 0x0E);
    /// The electric blue this recolour retired: brand, bright, deep, and the
    /// two paper blues. The comet kit has no blue anywhere.
    const RETIRED_ELECTRIC_BLUE: Color = Color::Rgb(0x2E, 0x7B, 0xFF);
    const RETIRED_ELECTRIC_BLUE_BRIGHT: Color = Color::Rgb(0x5A, 0xA0, 0xFF);
    const RETIRED_ELECTRIC_BLUE_DEEP: Color = Color::Rgb(0x1A, 0x5F, 0xE0);
    const RETIRED_BLUE_INK: Color = Color::Rgb(0x15, 0x50, 0xC8);
    const RETIRED_BLUE_INK_DEEP: Color = Color::Rgb(0x0F, 0x3A, 0x94);
    /// The warm-tinted gold that shipped beside the blue (`#F5C145` family).
    /// The comet gold is `#FFB81A` exactly; the old tint may not resurface.
    const RETIRED_WARM_GOLD: Color = Color::Rgb(0xF5, 0xC1, 0x45);
    const RETIRED_WARM_GOLD_BRIGHT: Color = Color::Rgb(0xFF, 0xD8, 0x73);
    const RETIRED_WARM_GOLD_DEEP: Color = Color::Rgb(0xC9, 0x94, 0x20);
    /// The cool, blue-leaning gray text ramp — the "cool grays" the comet
    /// kit's warm neutral ramp replaces.
    const RETIRED_COOL_TEXT: Color = Color::Rgb(0xF2, 0xF5, 0xFA);
    const RETIRED_COOL_TEXT_2: Color = Color::Rgb(0x8E, 0x97, 0xA8);
    const RETIRED_COOL_TEXT_3: Color = Color::Rgb(0x73, 0x7D, 0x92);
    /// The blue-cast "deep space" ground an older ink ramp replaced. Kept
    /// listed even though this palette is cool again: `#080A0F` is a
    /// *blue-cast* near-black (b−r = 7 at hue 265°), not the neutral
    /// blue-above-red this ramp specifies, and re-adopting it would trade a
    /// warm cast for a blue one rather than removing the cast.
    const RETIRED_DEEP_SPACE: Color = Color::Rgb(0x08, 0x0A, 0x0F);
    /// The four golds this palette replaces with two. Phosphor Gold and its
    /// three ramp stops — a reserved colour that needs four values is not
    /// reserved, and `#FFB81A` is also the one this tree shipped longest, so
    /// it is the value most likely to be pasted back in from memory.
    const RETIRED_PHOSPHOR_GOLD: Color = Color::Rgb(0xFF, 0xB8, 0x1A);
    const RETIRED_PHOSPHOR_GOLD_BRIGHT: Color = Color::Rgb(0xFD, 0xC1, 0x54);
    const RETIRED_PHOSPHOR_GOLD_DEEP: Color = Color::Rgb(0xE5, 0xA0, 0x00);
    const RETIRED_PHOSPHOR_GOLD_TRAIL: Color = Color::Rgb(0xA3, 0x72, 0x00);
    /// The amber data mark, retired for measuring 5.5° from this gold — the
    /// same colour at a glance. It is the reason clause 5 has no carve-outs.
    const RETIRED_AMBER_MARK: Color = Color::Rgb(0xE3, 0xB3, 0x41);
    /// The **warm** neutral ramp this palette inverts: warm Paper text, its
    /// two grays, the warm ink grounds, and the warm papers of the light
    /// theme. Listed in full rather than by the ground alone, because the
    /// drift this catches is a half-applied recolour — one token pasted back
    /// from the old ramp is exactly what nobody notices.
    const RETIRED_WARM_INK: Color = Color::Rgb(0x0B, 0x0B, 0x0C);
    const RETIRED_WARM_VOID: Color = Color::Rgb(0x05, 0x05, 0x06);
    const RETIRED_WARM_SURFACE: Color = Color::Rgb(0x13, 0x13, 0x15);
    const RETIRED_WARM_RAISED: Color = Color::Rgb(0x1B, 0x1B, 0x1E);
    const RETIRED_WARM_HAIRLINE: Color = Color::Rgb(0x23, 0x23, 0x27);
    const RETIRED_WARM_TEXT: Color = Color::Rgb(0xF4, 0xF1, 0xEA);
    const RETIRED_WARM_TEXT_2: Color = Color::Rgb(0x9B, 0x98, 0x90);
    const RETIRED_WARM_TEXT_3: Color = Color::Rgb(0x8D, 0x8A, 0x82);
    const RETIRED_WARM_PAPER: Color = Color::Rgb(0xF6, 0xF2, 0xE9);
    const RETIRED_WARM_SNOW: Color = Color::Rgb(0xFC, 0xFA, 0xF4);
    /// The previous status trio, all three of which sat outside this
    /// palette's cool register.
    const RETIRED_HOT_SUCCESS: Color = Color::Rgb(0x4A, 0xDE, 0x80);
    const RETIRED_HOT_WARNING: Color = Color::Rgb(0xEA, 0xB3, 0x08);
    const RETIRED_HOT_DANGER: Color = Color::Rgb(0xFF, 0x5C, 0x7A);

    // 1. The accent is the gold and the ground is the canvas — pinned by
    //    hex so the identity cannot silently drift. This is the one clause
    //    that names numbers: `#EFC53F` on `#0A0A0C`.
    assert_eq!(
        ACCENT,
        Color::Rgb(0xEF, 0xC5, 0x3F),
        "the accent must be gold #EFC53F, exactly"
    );
    assert_eq!(
        GROUND,
        Color::Rgb(0x0A, 0x0A, 0x0C),
        "the ground must be the canvas #0A0A0C, exactly"
    );
    assert_eq!(ACCENT, palette::BRAND, "the accent comes from the palette");
    assert_eq!(GOLD, ACCENT, "brand and gold are one value");
    // The accent itself is the one warm thing on the dark side, and has to
    // be: it is a yellow, which is exactly why every neutral around it is
    // pulled cool in clause 3.
    for (gold, name) in [
        (ACCENT, "ACCENT"),
        (ACCENT_FILL, "ACCENT_FILL"),
        (ACCENT_LIVE, "ACCENT_LIVE"),
    ] {
        let Color::Rgb(r, g, b) = gold else {
            panic!("{name} must be a truecolor token");
        };
        assert!(r > g && g > b, "{name} must be warm-dominant (r > g > b)");
    }

    // 2. The retired hues are gone from every token and alias.
    let mut all: Vec<Color> = ALL_RGB_TOKENS.to_vec();
    all.extend([
        RUN,
        SYNTAX_NUMBER,
        SYNTAX_KEYWORD,
        SYNTAX_COMMENT,
        HELD,
        ACCENT,
        ACCENT_FILL,
        ACCENT_LIVE,
        OK,
        WARN,
        BAD,
        CODE,
        GOLD,
        GOLD_LIVE,
        TEXT_DIM,
        CITRON,
        ORCHID,
    ]);
    all.extend(LIGHT_REMAP.iter().map(|(_, to)| *to));
    for token in &all {
        for (retired, name) in [
            (RETIRED_AURORA_CYAN, "aurora cyan"),
            (RETIRED_EMBER_FLAME, "ember flame"),
            (RETIRED_EMBER_CRIMSON, "ember crimson"),
            (RETIRED_GOLD, "the retired gold"),
            (RETIRED_GOLD_DEEP, "the retired deep gold"),
            (RETIRED_AGENT_ICE, "agent ice"),
            (RETIRED_SKY, "sky blue"),
            (RETIRED_SKY_DEEP, "deep sky blue"),
            (RETIRED_WARNING_ORANGE, "warning orange"),
            (RETIRED_PHOSPHOR_GREEN, "phosphor green"),
            (RETIRED_PHOSPHOR_GREEN_DEEP, "deep phosphor green"),
            (RETIRED_EMBER, "ember vermilion"),
            (RETIRED_EMBER_DEEP, "deep ember vermilion"),
            (RETIRED_ELECTRIC_BLUE, "electric blue"),
            (RETIRED_ELECTRIC_BLUE_BRIGHT, "bright electric blue"),
            (RETIRED_ELECTRIC_BLUE_DEEP, "deep electric blue"),
            (RETIRED_BLUE_INK, "the paper blue"),
            (RETIRED_BLUE_INK_DEEP, "the deep paper blue"),
            (RETIRED_WARM_GOLD, "the warm-tinted gold"),
            (RETIRED_WARM_GOLD_BRIGHT, "the warm-tinted bright gold"),
            (RETIRED_WARM_GOLD_DEEP, "the warm-tinted deep gold"),
            (RETIRED_COOL_TEXT, "the cool primary gray"),
            (RETIRED_COOL_TEXT_2, "the cool secondary gray"),
            (RETIRED_COOL_TEXT_3, "the cool tertiary gray"),
            (RETIRED_DEEP_SPACE, "the deep-space ground"),
            (RETIRED_PHOSPHOR_GOLD, "phosphor gold"),
            (RETIRED_PHOSPHOR_GOLD_BRIGHT, "bright phosphor gold"),
            (RETIRED_PHOSPHOR_GOLD_DEEP, "deep phosphor gold"),
            (RETIRED_PHOSPHOR_GOLD_TRAIL, "the phosphor gold trail stop"),
            (RETIRED_AMBER_MARK, "the amber data mark"),
            (RETIRED_WARM_INK, "the warm ink ground"),
            (RETIRED_WARM_VOID, "the warm void"),
            (RETIRED_WARM_SURFACE, "the warm surface"),
            (RETIRED_WARM_RAISED, "the warm raised ground"),
            (RETIRED_WARM_HAIRLINE, "the warm hairline"),
            (RETIRED_WARM_TEXT, "the warm primary text"),
            (RETIRED_WARM_TEXT_2, "the warm secondary text"),
            (RETIRED_WARM_TEXT_3, "the warm tertiary text"),
            (RETIRED_WARM_PAPER, "the warm paper ground"),
            (RETIRED_WARM_SNOW, "the warm snow surface"),
            (RETIRED_HOT_SUCCESS, "the uncooled success green"),
            (RETIRED_HOT_WARNING, "the uncooled warning amber"),
            (RETIRED_HOT_DANGER, "the uncooled danger red"),
        ] {
            assert_ne!(*token, retired, "a token still holds {name}");
        }
    }

    // 3. Every neutral — ground ramp and text ramp alike — is cool: blue at
    //    or above red, never below it, and never by enough to read as a
    //    blue *cast*. This is the inverse of the clause it replaces, and it
    //    is the whole reason these values are specified rather than derived:
    //    the accent is a yellow, so a warm neutral would sit in the accent's
    //    own family and the screen would read muddy on a cheap panel. Never
    //    true black either — pure black makes an accent scream.
    //
    //    The grounds are held to a tighter band (≤ 8) than the text tiers
    //    (≤ 14) because a ground is a large field where any cast is visible,
    //    while a text tier is a few thousand lit pixels.
    for (ground, name) in [
        (VOID, "VOID"),
        (GROUND, "GROUND"),
        (SURFACE, "SURFACE"),
        (RAISED, "RAISED"),
        (HAIRLINE, "HAIRLINE"),
        (HAIRLINE_STRONG, "HAIRLINE_STRONG"),
    ] {
        let Color::Rgb(r, g, b) = ground else {
            panic!("{name} must be a truecolor token");
        };
        assert!(
            b >= r,
            "{name} ({ground:?}) must not be warm — blue sits above red on \
             every ground in this palette"
        );
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        assert!(
            max - min <= 8,
            "{name} ({ground:?}) must be a near-neutral, not a blue cast"
        );
        assert_ne!(ground, Color::Rgb(0, 0, 0), "{name} must not be true black");
    }
    for (text, name) in [
        (TEXT_PRIMARY, "TEXT_PRIMARY"),
        (palette::TEXT_EMPHASIS, "TEXT_EMPHASIS"),
        (TEXT_SECONDARY, "TEXT_SECONDARY"),
        (TEXT_TERTIARY, "TEXT_TERTIARY"),
        (TEXT_DIM, "TEXT_DIM"),
    ] {
        let Color::Rgb(r, g, b) = text else {
            panic!("{name} must be a truecolor token");
        };
        assert!(
            b >= r && b >= g,
            "{name} ({text:?}) must be a cool neutral (b ≥ r, b ≥ g) — warm \
             grays are banned on the dark side"
        );
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        assert!(
            max - min <= 14,
            "{name} ({text:?}) must be a near-neutral, not a blue cast"
        );
    }
    // The paper ramp is cooled to match, so a theme switch changes the
    // lightness and not the temperature.
    for (paper, name) in [
        (palette::PAPER, "PAPER"),
        (palette::SNOW, "SNOW"),
        (palette::PAPER_RAISED, "PAPER_RAISED"),
        (palette::PAPER_HAIRLINE, "PAPER_HAIRLINE"),
        (palette::INK, "INK"),
        (palette::MUTED, "MUTED"),
        (palette::INK_DIM, "INK_DIM"),
        (palette::INK_EMPHASIS, "INK_EMPHASIS"),
    ] {
        let Color::Rgb(r, _, b) = paper else {
            panic!("{name} must be a truecolor token");
        };
        assert!(b >= r, "{name} ({paper:?}) must not be warm");
    }

    // 4. Warning ramps warm (r > g > b — amber, no longer the orange that
    //    dominated the transcript), success ramps green, danger ramps red.
    let Color::Rgb(wr, wg, wb) = WARNING else {
        panic!("WARNING must be a truecolor token");
    };
    assert!(wr > wg && wg > wb, "warning must ramp red > green > blue");
    let Color::Rgb(sr, sg, sb) = SUCCESS else {
        panic!("SUCCESS must be a truecolor token");
    };
    assert!(sg > sr && sg > sb, "success must be green-dominant");
    let Color::Rgb(dr, dg, db) = DANGER else {
        panic!("DANGER must be a truecolor token");
    };
    assert!(dr > dg && dr > db, "danger must be red-dominant");

    // 5. Gold is reserved for brand / active / focus / selection / progress.
    //    Every chromatic role sits ≥ 30° of hue away — which is how
    //    `AGENT_ICE` died, and now how the amber data mark died too. There
    //    are no named neighbours any more: WARN is in this list rather than
    //    excused from it.
    for (role, name) in [
        (BAD, "BAD"),
        (OK, "OK"),
        (WARN, "WARN"),
        (RUN, "RUN"),
        (HELD, "HELD"),
        (VIOLET, "VIOLET"),
        (TEAL, "TEAL"),
        (MAGENTA, "MAGENTA"),
        (CODE, "CODE"),
        (SYNTAX_STRING, "SYNTAX_STRING"),
        (SYNTAX_NUMBER, "SYNTAX_NUMBER"),
        (CITRON, "CITRON"),
        (ORCHID, "ORCHID"),
    ] {
        assert_ne!(role, ACCENT, "{name} must not be the reserved gold");
        let sep = hue_separation(role, ACCENT);
        assert!(
            sep >= oklch::SEPARATION_FLOOR_DEG,
            "{name} ({role:?}) is {sep:.1}° from the gold accent; {:.0}° is \
             the floor for two hues to be told apart in a cell",
            oklch::SEPARATION_FLOOR_DEG
        );
    }
    // The warning is the clause's own regression test. It sat 4.0° from the
    // previous gold and was excused as "always glyph-paired"; the derivation
    // in `palette::WARNING` puts it 39.1° away instead, and if it ever slid
    // back under the floor the loop above would now catch it rather than a
    // carve-out waving it through. The glyph pairing still holds — it is a
    // good rule — it is simply no longer the only thing keeping a warning
    // and the mark apart.
    assert!(
        hue_separation(WARN, ACCENT) >= oklch::SEPARATION_FLOOR_DEG,
        "warning must clear the same {:.0}° floor as every other chromatic role",
        oklch::SEPARATION_FLOOR_DEG
    );

    // 6. There are exactly two golds, they are one hue, and nothing else is
    //    allowed near them. The pair is what the palette permits: a resting
    //    mark and a live one.
    assert_eq!(ACCENT_FILL, ACCENT);
    assert_eq!(GOLD_LIVE, ACCENT_LIVE);
    assert_ne!(ACCENT_LIVE, ACCENT, "the live stop must be visibly lifted");
    assert!(
        hue_separation(ACCENT_LIVE, ACCENT) < 10.0,
        "the live stop is the same gold lit up, not a second hue"
    );
    // Two, and only two. Any *other* palette value landing inside gold's
    // 30° exclusion zone is a third gold arriving by the back door, which is
    // how the last one grew to four plus an amber.
    for (name, value) in palette::ALL {
        if value == ACCENT || value == ACCENT_LIVE {
            continue;
        }
        // The light-ground golds are the same hue by construction; they are
        // the paper theme's counterparts of the pair above, never a third
        // stop on the dark canvas.
        if matches!(name, "brand-ink" | "brand-ink-deep" | "gold-ink") {
            continue;
        }
        let Color::Rgb(r, g, b) = value else { continue };
        // Neutrals have no meaningful hue to separate; skip anything whose
        // channels are within the ramp's own spread.
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        if max - min <= 14 {
            continue;
        }
        let sep = hue_separation(value, ACCENT);
        assert!(
            sep >= oklch::SEPARATION_FLOOR_DEG,
            "palette token `{name}` ({value:?}) sits {sep:.1}° from the gold \
             accent — this palette ships two golds and no third"
        );
    }
}

#[test]
fn truecolor_supported_reads_colorterm_first() {
    assert!(truecolor_supported(Some("truecolor"), None));
    assert!(truecolor_supported(Some("24bit"), Some("xterm")));
    assert!(truecolor_supported(Some("TrueColor"), None)); // case-insensitive
}

#[test]
fn truecolor_supported_falls_back_to_term_direct_suffix() {
    assert!(truecolor_supported(None, Some("xterm-direct")));
    assert!(truecolor_supported(None, Some("st-direct")));
}

#[test]
fn truecolor_supported_is_false_for_known_limited_terms() {
    assert!(!truecolor_supported(None, Some("xterm")));
    assert!(!truecolor_supported(None, Some("xterm-256color")));
    assert!(!truecolor_supported(None, Some("screen")));
    assert!(!truecolor_supported(None, Some("linux")));
    assert!(!truecolor_supported(None, Some("tmux-256color")));
    assert!(!truecolor_supported(None, None));
}

#[test]
fn color_mode_no_color_beats_every_color_signal() {
    // NO_COLOR wins even on a truecolor terminal.
    assert_eq!(color_mode(true, Some("truecolor"), None), ColorMode::None);
    assert_eq!(
        color_mode(true, None, Some("xterm-256color")),
        ColorMode::None
    );
}

#[test]
fn color_mode_detects_each_depth() {
    assert_eq!(
        color_mode(false, Some("truecolor"), None),
        ColorMode::Truecolor
    );
    assert_eq!(
        color_mode(false, None, Some("xterm-256color")),
        ColorMode::Ansi256
    );
    // Bare legacy terminals, and no environment at all, floor at 16 colors.
    assert_eq!(color_mode(false, None, Some("xterm")), ColorMode::Ansi16);
    assert_eq!(color_mode(false, None, Some("linux")), ColorMode::Ansi16);
    assert_eq!(color_mode(false, None, None), ColorMode::Ansi16);
}

#[test]
fn resolve_passes_through_when_truecolor() {
    assert_eq!(resolve(ACCENT, ColorMode::Truecolor), ACCENT);
}

#[test]
fn resolve_maps_every_token_to_its_index_when_degraded() {
    for (rgb, i256, i16) in FALLBACKS {
        assert_eq!(resolve(*rgb, ColorMode::Ansi256), Color::Indexed(*i256));
        assert_eq!(resolve(*rgb, ColorMode::Ansi16), Color::Indexed(*i16));
    }
}

#[test]
fn resolve_strips_color_under_no_color() {
    assert_eq!(resolve(ACCENT, ColorMode::None), Color::Reset);
    assert_eq!(resolve(Color::Indexed(9), ColorMode::None), Color::Reset);
    // A non-color (Reset) stays put — nothing to strip.
    assert_eq!(resolve(Color::Reset, ColorMode::None), Color::Reset);
    // The named ANSI colors are colors too: the transcript styles
    // its HUD/composer/cards with them, so `NO_COLOR` must strip them as
    // surely as it strips a palette token.
    for named in [
        Color::Green,
        Color::Red,
        Color::Yellow,
        Color::Cyan,
        Color::DarkGray,
    ] {
        assert_eq!(
            resolve(named, ColorMode::None),
            Color::Reset,
            "NO_COLOR must strip the named ANSI colors too ({named:?})"
        );
    }
}

#[test]
fn resolve_leaves_unmapped_colors_unchanged_when_indexed() {
    assert_eq!(
        resolve(Color::Indexed(9), ColorMode::Ansi256),
        Color::Indexed(9)
    );
    assert_eq!(resolve(Color::Reset, ColorMode::Ansi16), Color::Reset);
}

#[test]
fn degrade_buffer_is_noop_when_truecolor() {
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 1, 1));
    buf.content[0].fg = ACCENT;
    degrade_buffer(&mut buf, ColorMode::Truecolor);
    assert_eq!(buf.content[0].fg, ACCENT);
}

#[test]
fn degrade_buffer_resolves_every_cell_when_degraded() {
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 1, 1));
    buf.content[0].fg = ACCENT; // → 214 (256) / 11 (16)
    buf.content[0].bg = VIOLET; // → 98 (256) / 13 (16)
    degrade_buffer(&mut buf, ColorMode::Ansi256);
    assert_eq!(buf.content[0].fg, Color::Indexed(221));
    assert_eq!(buf.content[0].bg, Color::Indexed(98));
}

/// The two degraded paths must still separate the things a reader has to
/// tell apart. Nearest-by-RGB alone does not guarantee that (it is what
/// would have put `gold` on an orange cube entry), so the pairs that carry
/// meaning are asserted rather than assumed.
#[test]
fn degraded_paths_keep_meaningful_pairs_distinguishable() {
    let idx = |c: Color, m: ColorMode| match resolve(c, m) {
        Color::Indexed(i) => i,
        other => panic!("{c:?} did not resolve to an index in {m:?}: {other:?}"),
    };
    for mode in [ColorMode::Ansi256, ColorMode::Ansi16] {
        // Status must stay three different colours.
        assert_ne!(idx(OK, mode), idx(WARN, mode), "{mode:?}: ok vs warn");
        assert_ne!(idx(OK, mode), idx(BAD, mode), "{mode:?}: ok vs bad");
        assert_ne!(idx(WARN, mode), idx(BAD, mode), "{mode:?}: warn vs bad");
        // The accent must not collapse onto a status or onto the canvas.
        for (other, name) in [
            (OK, "ok"),
            (WARN, "warn"),
            (BAD, "bad"),
            (GROUND, "ground"),
            (TEXT_PRIMARY, "text-primary"),
        ] {
            assert_ne!(
                idx(ACCENT, mode),
                idx(other, mode),
                "{mode:?}: accent collapsed onto {name}"
            );
        }
        // Gold is brand chrome and warning is a status; they may never
        // become the same cell, at any depth.
        for gold in [GOLD, GOLD_LIVE] {
            assert_ne!(
                idx(gold, mode),
                idx(WARN, mode),
                "{mode:?}: {gold:?} collapsed onto warning"
            );
        }
        // The ground ladder keeps at least ground/raised apart, so a
        // selected row still reads.
        assert_ne!(idx(GROUND, mode), idx(RAISED, mode), "{mode:?}: select bg");
    }
    // The two golds stay distinct at 256 colours, so the progress fill keeps
    // a head-to-tail ramp there. At 16 there is no second yellow to lift to
    // and they share index 11 — which costs nothing, because `crate::progress`
    // already paints a solid ACCENT whenever the terminal is not truecolor.
    assert_ne!(
        resolve(ACCENT, ColorMode::Ansi256),
        resolve(ACCENT_LIVE, ColorMode::Ansi256),
        "the live stop must stay distinct from the resting gold at 256 colours"
    );
}

#[test]
fn apply_theme_is_identity_for_dark_and_remaps_for_light() {
    use std::sync::atomic::Ordering;
    // remap_theme is pure — exercise it directly rather than mutating the
    // process-global active theme (which other tests read).
    assert_eq!(remap_theme(ACCENT, LIGHT_REMAP), palette::BRAND_INK);
    assert_eq!(remap_theme(ACCENT_FILL, LIGHT_REMAP), palette::BRAND_INK);
    // Flat gold cells are accent cells, so they take the accent's paper text
    // tone; the light-ground *graphical* gold is where the live stop lands,
    // because a lifted gold cannot carry text on paper at all.
    assert_eq!(remap_theme(GOLD, LIGHT_REMAP), palette::BRAND_INK);
    assert_eq!(remap_theme(ACCENT_LIVE, LIGHT_REMAP), palette::GOLD_INK);
    assert_eq!(remap_theme(VOID, LIGHT_REMAP), palette::PAPER);
    assert_eq!(remap_theme(GROUND, LIGHT_REMAP), palette::PAPER);
    assert_eq!(remap_theme(TEXT_PRIMARY, LIGHT_REMAP), palette::INK);
    assert_eq!(remap_theme(OK, LIGHT_REMAP), palette::SUCCESS_INK);
    // Every dark token the deck can paint has a paper counterpart, or the
    // light theme would show a dark-ground colour on white.
    for token in ALL_RGB_TOKENS {
        assert!(
            LIGHT_REMAP.iter().any(|(from, _)| from == token),
            "{token:?} has no LIGHT_REMAP entry"
        );
    }
    // A colour that is not a token passes through untouched. Nothing paints
    // one any more (#4058 retired the gradient that used to), so this pins
    // the remap's total behavior rather than a live path.
    let unmapped = Color::Rgb(0x7B, 0x9A, 0x35);
    assert!(
        !ALL_RGB_TOKENS.contains(&unmapped),
        "pick a value no token holds, or this asserts nothing"
    );
    assert_eq!(remap_theme(unmapped, LIGHT_REMAP), unmapped);
    // Every LIGHT_REMAP key is a distinct value (aliases share one entry).
    for (i, (from, _)) in LIGHT_REMAP.iter().enumerate() {
        assert!(
            !LIGHT_REMAP[..i].iter().any(|(other, _)| other == from),
            "duplicate LIGHT_REMAP key {from:?}"
        );
    }
    // The global default is dark, so apply_theme leaves a buffer untouched.
    assert_eq!(ACTIVE_THEME.load(Ordering::Relaxed), 0);
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 1, 1));
    buf.content[0].bg = GROUND;
    apply_theme(&mut buf, ColorMode::Truecolor);
    assert_eq!(
        buf.content[0].bg, GROUND,
        "stella-dark must be the identity"
    );
}

#[test]
fn theme_name_parse_round_trips_and_rejects_junk() {
    for t in ThemeName::ALL {
        assert_eq!(ThemeName::parse(t.slug()), Some(t));
    }
    assert_eq!(
        ThemeName::parse("Stella_Light"),
        Some(ThemeName::StellaLight)
    );
    assert_eq!(ThemeName::parse("dark"), Some(ThemeName::StellaDark));
    assert_eq!(ThemeName::parse("solarized"), None);
}

#[test]
fn degrade_buffer_strips_color_under_no_color() {
    let mut buf = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 1, 1));
    buf.content[0].fg = ACCENT;
    buf.content[0].bg = GROUND;
    degrade_buffer(&mut buf, ColorMode::None);
    assert_eq!(buf.content[0].fg, Color::Reset);
    assert_eq!(buf.content[0].bg, Color::Reset);
}

/// **Witness (#4058, rule 5).** No gradient may come back to this crate.
///
/// The brand gradient (`BRAND_STOPS`, `primary_stops`, `brand_gradient`,
/// `gradient_at`, `lerp_rgb`, `lighten`) was `pub`, so nothing in the gate
/// could see that its last consumer had gone: `crate::progress`, the module
/// its own doc comments named as the fill it painted, does not exist. Rule 5
/// of the v5 colour alignment retires gradients on every surface and SPEC 2
/// (`cell-grid honest`) allows only per-cell colour steps, so a re-added
/// interpolator is a spec violation rather than an unused item — and a `pub`
/// one leaves no other trace.
///
/// This asserts against the crate's own source rather than against a symbol,
/// because a deleted symbol cannot be named in a test that must compile.
#[test]
fn no_colour_interpolator_lives_in_this_crate() {
    for (file, source) in [
        ("theme.rs", include_str!("../theme.rs")),
        ("palette.rs", include_str!("../palette.rs")),
    ] {
        for banned in [
            "fn lerp_rgb",
            "fn brand_gradient",
            "fn gradient_at",
            "fn lighten",
            "fn primary_stops",
            "BRAND_STOPS",
        ] {
            assert!(
                !source.contains(banned),
                "{file} declares `{banned}` — rule 5 retires gradients on \
                 every surface, and `apply_theme`'s value remap cannot see an \
                 interpolated cell"
            );
        }
    }
}

/// The transcript's tool-class hues are a *set*, and a set only works if
/// every member is telling apart from every other one — not just from gold.
///
/// This is the law behind `crate::tool_class`: one categorical hue per
/// class, none of them the reserved brand accent, and at least 30° of hue
/// between every pair (the same floor `palette_law_gold_is_the_brand`'s
/// clause 5 applies against gold, and the floor that killed `AGENT_ICE`).
/// Another class cannot be added without either finding another gap or
/// admitting the reader can no longer see the difference — which is a
/// decision, and this test is where it gets made. The count is deliberately
/// not written here: `ToolClass::ALL` is the count, and a number in prose
/// beside it is one more cell to drift (it already had, at "six").
#[test]
fn every_tool_class_is_categorical_and_distinct() {
    use crate::tool_class::ToolClass;
    for class in ToolClass::ALL {
        let color = class.color();
        assert_ne!(
            color,
            ACCENT,
            "{} must not wear the reserved gold — a tool name is not the brand, \
             not active, and not focused",
            class.label()
        );
        let sep = hue_separation(color, ACCENT);
        assert!(
            sep >= oklch::SEPARATION_FLOOR_DEG,
            "{} ({color:?}) is {sep:.1}° from the gold accent",
            class.label()
        );
    }
    for (i, a) in ToolClass::ALL.iter().enumerate() {
        for b in &ToolClass::ALL[..i] {
            let sep = hue_separation(a.color(), b.color());
            assert!(
                sep >= oklch::SEPARATION_FLOOR_DEG,
                "the `{}` and `{}` classes are {sep:.1}° apart; {:.0}° is the \
                 floor for two hues to be told apart in a terminal cell",
                a.label(),
                b.label(),
                oklch::SEPARATION_FLOOR_DEG
            );
        }
    }
}

/// A stage rule is hued by phase now, and the mapping it uses is the
/// statline's — one answer to "which phase is this", rendered twice.
///
/// The rule that survives from when the label was neutral: no stage may wear
/// a colour that reads as a verdict on the work, because a boundary is not an
/// outcome. `Complete` is the one exception and it is the honest one — the
/// stage whose whole meaning is "this finished".
#[test]
fn a_stage_rule_is_hued_by_phase_and_never_by_verdict() {
    use stella_protocol::StageKind as S;
    for stage in [
        S::Triage,
        S::ContextRecall,
        S::Research,
        S::Plan,
        S::ScopeReview,
        S::Witness,
        S::Execute,
        S::Verify,
        S::Verdict,
        S::Reflect,
        S::ContextWrite,
    ] {
        let stage = stella_protocol::StageName::from(stage);
        let color = stage_rule_color(&stage);
        assert_ne!(color, BAD, "{stage:?} must not read as a failure");
        assert_ne!(color, OK, "{stage:?} must not read as a settled success");
        assert_ne!(
            color, TEXT_SECONDARY,
            "{stage:?} must not fall back to the neutral tier the rules used to \
             be drawn in — a phase boundary is structure, not bookkeeping"
        );
        assert_ne!(
            color, ACCENT,
            "{stage:?} is history by the time it is read; gold means active"
        );
        // The statline's live dot has the same job and answers separately —
        // but only `Execute` may differ, and only in that direction.
        if stage.kind() != Some(S::Execute) {
            assert_eq!(
                color,
                stage_color(&stage),
                "{stage:?} must read the same in the transcript and the statline"
            );
        }
    }
    assert_eq!(stage_rule_color(&S::Complete.into()), OK);
    assert_eq!(
        stage_color(&S::Execute.into()),
        ACCENT,
        "the statline's LIVE execute dot keeps the accent — that is the one \
         status gold carries"
    );
}

/// **The witness for a plugin-contributed stage's styling.**
///
/// A stage the host has never heard of has to render as a stage — visible, in
/// the kit, and never dressed as an outcome. Before the vocabulary opened it
/// could not reach a renderer at all, so there was nothing to colour.
///
/// The bar it must clear is the same one every host stage clears above: not a
/// verdict hue, not the brand, and not the neutral tier that made a boundary
/// invisible.
#[test]
fn a_contributed_stage_is_visible_in_the_kit_and_never_reads_as_a_verdict() {
    for word in [
        "triage-lite",
        "vera/witness",
        "review",
        "sast",
        "spec-check",
        "benchmark",
        "x",
    ] {
        let stage = stella_protocol::StageName::new(word);
        assert!(
            stage.kind().is_none(),
            "{word} must be a contributed stage for this test to mean anything"
        );
        let color = stage_color(&stage);
        assert_ne!(color, BAD, "{word} must not read as a failure");
        assert_ne!(color, OK, "{word} must not read as a settled success");
        assert_ne!(color, WARN, "{word} must not read as a warning");
        assert_ne!(
            color, ACCENT,
            "{word} must not take the brand accent — gold is 'active', and a \
             plugin's stage is not the brand"
        );
        assert_ne!(
            color, TEXT_SECONDARY,
            "{word} must not fall back to the neutral tier — that is the exact \
             invisibility the phase hues were introduced to end"
        );
        // The transcript rule and the statline dot agree for a contributed
        // stage: it never takes the accent, so there is no gold to move off a
        // settled thing (the one divergence `stage_rule_color` makes).
        assert_eq!(
            stage_rule_color(&stage),
            color,
            "{word} must read the same in the transcript and the statline"
        );
    }
}

/// The hash is an identity, not a decoration: the same stage is the same
/// colour on every frame, in every process, forever. A colour that moved
/// between renders would make the hue actively misleading — the reader would
/// take a recolour for a change of stage.
#[test]
fn a_contributed_stage_keeps_one_colour() {
    let once = contributed_stage_color("triage-lite");
    for _ in 0..64 {
        assert_eq!(contributed_stage_color("triage-lite"), once);
    }
    // And it is genuinely keyed on the name — two stages are allowed to
    // collide, but they must not collide by construction.
    let words = ["triage-lite", "review", "sast", "spec-check", "benchmark"];
    let distinct: std::collections::BTreeSet<String> = words
        .iter()
        .map(|w| format!("{:?}", contributed_stage_color(w)))
        .collect();
    assert!(
        distinct.len() > 1,
        "every contributed stage landed on one colour — the hash is not keyed \
         on the name at all"
    );
}
