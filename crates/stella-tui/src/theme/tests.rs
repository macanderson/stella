//! Unit tests for [`crate::theme`].
//!
//! Split out of `theme.rs` so the module stays under the 1500-line ratchet
//! (#629). Pure relocation: no test was changed, added, or removed.

use super::*;

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
/// they are intentionally not re-listed — that includes `ACCENT_FILL` (one
/// value with `ACCENT`) and `SYNTAX_COMMENT` (the caption tier,
/// `TEXT_TERTIARY`).
///
/// `ACCENT_BRIGHT` and `GOLD` are listed: under the comet kit each shared a
/// value with another token (the bright stop with `GOLD_BRIGHT`, gold with
/// `ACCENT`), and the nebula splits both apart.
const ALL_RGB_TOKENS: &[Color] = &[
    VOID,
    GROUND,
    SURFACE,
    RAISED,
    HAIRLINE,
    HAIRLINE_STRONG,
    TEXT_PRIMARY,
    TEXT_SECONDARY,
    TEXT_TERTIARY,
    ACCENT,
    ACCENT_BRIGHT,
    ACCENT_DEEP,
    GOLD,
    GOLD_BRIGHT,
    GOLD_DEEP,
    SUCCESS,
    WARNING,
    DANGER,
    ORACLE_PRE_FLIP,
    ORCHID,
    LIME,
    AZURE,
    ROSE,
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
    "oracle-red-ink",
    "paper",
    "snow",
    "paper-raised",
    "paper-hairline",
    "ink",
    "muted",
    "ink-dim",
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
    // Under the nebula the accent is NOT the gold: the brand is a violet→cyan
    // sweep and gold is the rationed north point, so the two families are
    // separate values on purpose. (Under the comet kit these were `assert_eq!`
    // — the split is the rebrand.)
    assert_eq!(ACCENT, palette::BRAND);
    assert_eq!(ACCENT_FILL, ACCENT);
    assert_eq!(ACCENT_BRIGHT, palette::BRAND_BRIGHT);
    assert_ne!(GOLD, ACCENT);
    assert_ne!(palette::BRAND, palette::GOLD);
    assert_ne!(palette::BRAND_BRIGHT, palette::GOLD_BRIGHT);
    assert_eq!(GOLD, palette::GOLD);
    assert_eq!(ACCENT_DEEP, palette::BRAND_DEEP);
    assert_eq!(GOLD_BRIGHT, palette::GOLD_BRIGHT);
    assert_eq!(GOLD_DEEP, palette::GOLD_DEEP);
    // The gold sweep and the progress fill lead with different deep stops:
    // they are now different hue families entirely, not two tunings of one.
    assert_ne!(GOLD_DEEP, ACCENT_DEEP);
    assert_eq!(VOID, palette::VOID);
    assert_eq!(HAIRLINE_STRONG, palette::HAIRLINE_STRONG);
    assert_eq!(GROUND, palette::GROUND);
    assert_eq!(INK, TEXT_PRIMARY);
    assert_eq!(MUTED, TEXT_SECONDARY);
    assert_eq!(RULE, HAIRLINE);
    assert_eq!(OK, SUCCESS);
    assert_eq!(WARN, WARNING);
    assert_eq!(BAD, DANGER);
    assert_eq!(HELD, ORCHID);
    assert_eq!(RUN, ORCHID);
    // Syntax and process hues are categorical -- never the brand accent, and
    // notably not the kit reference's violet keyword / cyan string, which are
    // both brand-sweep values (see `theme`'s syntax note).
    assert_eq!(SYNTAX_KEYWORD, ORCHID);
    assert_eq!(SYNTAX_NUMBER, AZURE);
    assert_eq!(SYNTAX_COMMENT, TEXT_TERTIARY);
    // The categorical set is the observatory's data-mark palette verbatim,
    // so a series in a chart and a chip in the deck are the same colour.
    assert_eq!(LIME, palette::DATA_1);
    assert_eq!(ORCHID, palette::DATA_2);
    assert_eq!(ROSE, palette::DATA_3);
    assert_eq!(AZURE, palette::DATA_4);
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
    for gold in [GOLD, GOLD_BRIGHT, GOLD_DEEP] {
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

/// Hue angle in degrees `[0, 360)`, for the separation law below.
///
/// The reservation used to be measured as squared euclidean RGB distance,
/// which worked while the brand was green and every categorical hue was far
/// from it in all three channels. With a blue brand it stops working: a
/// violet and a blue can be 40° apart — plainly different colours in a
/// terminal cell — and still sit within 0x30 per channel, because blue is
/// pinned at 0xFF in both. Hue separation measures the thing a reader
/// actually uses, so the law is stated in the units it is really about.
fn hue_deg(color: Color) -> f64 {
    let Color::Rgb(r, g, b) = color else {
        panic!("{color:?} must be a truecolor token");
    };
    let (r, g, b) = (
        f64::from(r) / 255.0,
        f64::from(g) / 255.0,
        f64::from(b) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let c = max - min;
    if c == 0.0 {
        return 0.0;
    }
    let h = if max == r {
        ((g - b) / c).rem_euclid(6.0)
    } else if max == g {
        (b - r) / c + 2.0
    } else {
        (r - g) / c + 4.0
    };
    (h * 60.0).rem_euclid(360.0)
}

/// The shortest angular distance between two hues, in degrees.
fn hue_separation(a: Color, b: Color) -> f64 {
    let d = (hue_deg(a) - hue_deg(b)).abs();
    d.min(360.0 - d)
}

/// The palette law, in the form a future edit would actually break.
///
/// The guard has outlived five recolours now (aurora → gold → sky → green →
/// blue → gold again), which is the point: each time, the *shape* of the law
/// survived and only the hue it names changed. Keep rewriting it rather than
/// deleting it.
///
/// The comet recolour changed the law in three real ways. First, the exact
/// values are load-bearing: Phosphor Gold `#FFB81A` and Ink `#0B0B0C` are
/// the brand, so clause 1 pins the hex rather than merely the dominance.
/// Second, the ground lost its cast: ink is a warm-neutral near-black, so
/// the old "the canvas must carry the blue cast" clause inverts back into
/// "the canvas may carry (almost) no cast" — and the same warmth law now
/// covers the text ramp, because the owner banned cool grays outright.
/// Third, the reservation has *named neighbours* again: a gold accent lives
/// 4.0° from warning amber and 0.8° from the amber data mark, so instead of
/// "no exceptions" the law says exactly which two hues may sit close and
/// what confines each of them.
///
/// What must hold now:
///   1. The accent is the kit's Nebula Violet lifted for the terminal,
///      exactly; the ground is Void, exactly. Brand and gold are **not** one
///      family — the *split* IS this rebrand, exactly as the collapse was the
///      last one.
///   2. Every retired hue is gone — the whole electric-blue family, the comet
///      kit's Phosphor Gold and the warm neutral ramp under it, and the older
///      warm-tinted gold with them.
///   3. Grounds are blue-cast and never true black; text is cool.
///   4. Warning ramps warm, success ramps green, danger ramps red.
///   5. The brand is reserved. Every chromatic role sits ≥ 30° of hue away,
///      with no exceptions at all — the named-neighbour clause the gold
///      identity needed is *gone*, because moving the accent off the yellow
///      side moved it clear of warning and the amber mark in one step.
///   6. Gold is reserved separately and rationed: it is not the accent, and
///      WARN is its one permitted hue-neighbour.
#[test]
fn palette_law_the_nebula_is_the_brand() {
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
    /// The blue-cast "deep space" ground the ink ramp replaced. The nebula
    /// ground is blue-cast again, but it is `#080B1C` and not this value —
    /// the return of the cast is deliberate, the return of the *hex* would be
    /// a revert.
    const RETIRED_DEEP_SPACE: Color = Color::Rgb(0x08, 0x0A, 0x0F);
    /// The comet kit's Phosphor Gold, the identity this recolor retired: the
    /// terminal's lifted accent, its bright and deep stops, and the two paper
    /// golds. Gold survives in the nebula kit — but as `#FFC857`, the north
    /// point, and never again as the accent.
    const RETIRED_PHOSPHOR_GOLD: Color = Color::Rgb(0xFF, 0xB8, 0x1A);
    const RETIRED_PHOSPHOR_GOLD_KIT: Color = Color::Rgb(0xFF, 0xB0, 0x00);
    const RETIRED_PHOSPHOR_GOLD_BRIGHT: Color = Color::Rgb(0xFD, 0xC1, 0x54);
    const RETIRED_PHOSPHOR_GOLD_DEEP: Color = Color::Rgb(0xE5, 0xA0, 0x00);
    const RETIRED_GOLD_INK: Color = Color::Rgb(0x85, 0x5E, 0x00);
    const RETIRED_GOLD_INK_DEEP: Color = Color::Rgb(0x5A, 0x3F, 0x00);
    /// The warm neutral ramp the cool one replaces — the comet kit banned
    /// cool grays, and the nebula bans these for the mirror-image reason: a
    /// warm gray on a blue-cast void reads as a stain.
    const RETIRED_WARM_TEXT: Color = Color::Rgb(0xF4, 0xF1, 0xEA);
    const RETIRED_WARM_TEXT_2: Color = Color::Rgb(0x9B, 0x98, 0x90);
    const RETIRED_WARM_TEXT_3: Color = Color::Rgb(0x8D, 0x8A, 0x82);
    /// The warm-neutral Ink ground, and the warm paper that partnered it.
    const RETIRED_INK_GROUND: Color = Color::Rgb(0x0B, 0x0B, 0x0C);
    const RETIRED_WARM_PAPER: Color = Color::Rgb(0xF6, 0xF2, 0xE9);

    // 1. The accent is Nebula Violet and the ground is Void — the exact kit
    //    values, pinned by hex so the brand cannot silently drift. This is
    //    the one clause that names numbers: `#8F72FF` on `#080B1C` is the
    //    identity.
    assert_eq!(
        ACCENT,
        Color::Rgb(0x8F, 0x72, 0xFF),
        "the accent must be Nebula Violet #8F72FF, exactly"
    );
    assert_eq!(
        GROUND,
        Color::Rgb(0x08, 0x0B, 0x1C),
        "the ground must be Void #080B1C, exactly"
    );
    assert_eq!(ACCENT, palette::BRAND, "the accent comes from the palette");
    assert_eq!(
        ACCENT_BRIGHT,
        Color::Rgb(0x1F, 0xD8, 0xE6),
        "the sweep's bright stop must be Nebula Cyan #1FD8E6, exactly"
    );
    // The split is the rebrand. Under the comet kit this was `assert_eq!`.
    assert_ne!(
        GOLD, ACCENT,
        "gold is the north point, not the brand — the split IS the nebula"
    );
    for (violet, name) in [(ACCENT, "ACCENT"), (ACCENT_FILL, "ACCENT_FILL")] {
        let Color::Rgb(r, g, b) = violet else {
            panic!("{name} must be a truecolor token");
        };
        assert!(b > r && r > g, "{name} must be violet-dominant (b > r > g)");
    }
    // Gold stays warm — it is the one warm thing left in the palette, which
    // is exactly what makes a single gold moment carry.
    let Color::Rgb(gr, gg, gb) = GOLD else {
        panic!("GOLD must be a truecolor token");
    };
    assert!(gr > gg && gg > gb, "GOLD must stay warm-dominant (r > g > b)");

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
        OK,
        ACCENT_DEEP,
        CODE,
        GOLD,
    ]);
    all.extend(BRAND_STOPS);
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
            (RETIRED_PHOSPHOR_GOLD, "the comet accent gold"),
            (RETIRED_PHOSPHOR_GOLD_KIT, "the comet kit gold"),
            (RETIRED_PHOSPHOR_GOLD_BRIGHT, "the comet bright gold"),
            (RETIRED_PHOSPHOR_GOLD_DEEP, "the comet deep gold"),
            (RETIRED_GOLD_INK, "the comet paper gold"),
            (RETIRED_GOLD_INK_DEEP, "the comet deep paper gold"),
            (RETIRED_WARM_TEXT, "the warm primary neutral"),
            (RETIRED_WARM_TEXT_2, "the warm secondary neutral"),
            (RETIRED_WARM_TEXT_3, "the warm tertiary neutral"),
            (RETIRED_INK_GROUND, "the warm Ink ground"),
            (RETIRED_WARM_PAPER, "the warm paper ground"),
        ] {
            assert_ne!(*token, retired, "a token still holds {name}");
        }
    }

    // 3. The ground is void: deliberately blue-cast, and never true black —
    //    pure black makes the accent scream; void lets it glow, and the cast
    //    is what makes the ground read as sky rather than as a dead terminal.
    //    This inverts the comet law, which required near-neutrality
    //    (`max - min <= 5`). The text ramp inverts with it: warm grays are
    //    banned now, because a warm gray on a blue-cast void reads as a
    //    stain.
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
            b > r && b > g,
            "{name} ({ground:?}) must be blue-cast (b > r, b > g) — the cast \
             is the nebula's ground, not an accident"
        );
        assert_ne!(ground, Color::Rgb(0, 0, 0), "{name} must not be true black");
    }
    for (text, name) in [
        (TEXT_PRIMARY, "TEXT_PRIMARY"),
        (TEXT_SECONDARY, "TEXT_SECONDARY"),
        (TEXT_TERTIARY, "TEXT_TERTIARY"),
    ] {
        let Color::Rgb(r, g, b) = text else {
            panic!("{name} must be a truecolor token");
        };
        assert!(
            b >= g && g >= r,
            "{name} ({text:?}) must be a cool neutral (b ≥ g ≥ r) — warm \
             grays are banned"
        );
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

    // 5. The brand is reserved for brand / active / focus / selection /
    //    progress. Every chromatic role sits ≥ 30° of hue away — which is how
    //    `AGENT_ICE` died — and this time there are *no* exceptions.
    //
    //    WARN and LIME are in this list rather than in a named-neighbour
    //    escape hatch below it, which is the clearest single measure of what
    //    the recolor bought: under the gold identity they sat 4.0° and 0.8°
    //    from the accent and had to be argued for individually. Off the
    //    yellow side, they clear it by 145° and 159° for free.
    for (role, name) in [
        (BAD, "BAD"),
        (OK, "OK"),
        (WARN, "WARN"),
        (RUN, "RUN"),
        (HELD, "HELD"),
        (ORCHID, "ORCHID"),
        (LIME, "LIME"),
        (AZURE, "AZURE"),
        (ROSE, "ROSE"),
        (CODE, "CODE"),
        (SYNTAX_STRING, "SYNTAX_STRING"),
        (SYNTAX_NUMBER, "SYNTAX_NUMBER"),
    ] {
        assert_ne!(role, ACCENT, "{name} must not be the reserved brand hue");
        let sep = hue_separation(role, ACCENT);
        assert!(
            sep >= 30.0,
            "{name} ({role:?}) is {sep:.1}° from the brand accent; \
             30° is the floor for two hues to be told apart in a cell"
        );
    }
    // 6. Gold is reserved *separately*: it is chrome and identity, never the
    //    accent and never a verdict. WARN is its one permitted hue-neighbour,
    //    and permitted for the reason it always was — a status never appears
    //    without its glyph (`status_glyph`), so the hue is never the only
    //    carrier. Everything else keeps 30° from gold too.
    assert!(
        hue_separation(WARN, GOLD) < 30.0,
        "WARN is gold's one *named* hue-neighbour; if it has moved away, \
         promote it to the reserved list below"
    );
    for (role, name) in [
        (BAD, "BAD"),
        (OK, "OK"),
        (ORCHID, "ORCHID"),
        (AZURE, "AZURE"),
        (ROSE, "ROSE"),
        (CODE, "CODE"),
        (SYNTAX_STRING, "SYNTAX_STRING"),
    ] {
        assert_ne!(role, GOLD, "{name} must not be the reserved gold");
        let sep = hue_separation(role, GOLD);
        assert!(
            sep >= 30.0,
            "{name} ({role:?}) is {sep:.1}° from the north-point gold; \
             gold is rationed, so nothing may be mistaken for it"
        );
    }
    // The amber mark's stand-down is *lifted*. Under the comet kit `data-1`
    // measured 1.06:1 against the gold accent, so it was barred from every
    // chip, node and trace. The accent is no longer gold and `data-1` is no
    // longer amber, so the bar has no subject: the assertions that enforced it
    // are gone rather than rewritten, because a rule kept past its reason is
    // how a palette accumulates cargo.
    // Gold's own ration, restated as a property: no trace chip, graph node or
    // agent slot may wear it, because a chip that recurs per row is the
    // opposite of "one gold moment per view". This replaces the amber
    // stand-down above — same shape, correct subject.
    assert!(
        !AGENT_PALETTE.contains(&GOLD),
        "gold is rationed: no agent chip may wear it"
    );
    for kind in [
        "function", "method", "struct", "enum", "trait", "file", "module", "?",
    ] {
        assert_ne!(
            graph_kind_color(kind),
            GOLD,
            "gold is rationed: no graph node may wear it"
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
            GOLD,
            "gold is rationed: no trace chip may wear it"
        );
    }

    // 7. The sweep is a sweep: fill matches accent, the deep stop stays in the
    //    accent's own hue, and the bright stop deliberately does NOT — the
    //    nebula's whole point is that it travels violet→cyan, so a bright stop
    //    within 10° of the accent would mean the gradient had collapsed.
    assert_eq!(ACCENT_FILL, ACCENT);
    assert!(hue_separation(ACCENT_DEEP, ACCENT) < 10.0);
    assert!(
        hue_separation(ACCENT_BRIGHT, ACCENT) > 30.0,
        "the sweep must actually travel: if the bright stop has collapsed \
         onto the accent's hue, the nebula is a single colour again"
    );
    // The gold family stays internally consistent, and away from the accent.
    assert!(hue_separation(GOLD_DEEP, GOLD) < 10.0);
    assert!(hue_separation(GOLD_BRIGHT, GOLD) < 10.0);
    assert!(hue_separation(GOLD, ACCENT) >= 30.0);
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
    buf.content[0].fg = ACCENT; // → 99 (256) / 13 (16)
    buf.content[0].bg = ORCHID; // → 170 (256) / 13 (16)
    degrade_buffer(&mut buf, ColorMode::Ansi256);
    assert_eq!(buf.content[0].fg, Color::Indexed(99));
    assert_eq!(buf.content[0].bg, Color::Indexed(170));
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
        for gold in [GOLD, GOLD_BRIGHT, GOLD_DEEP] {
            assert_ne!(
                idx(gold, mode),
                idx(WARN, mode),
                "{mode:?}: {gold:?} collapsed onto warning"
            );
        }
        // The progress fill keeps a head-to-tail ramp.
        assert_ne!(
            idx(ACCENT_DEEP, mode),
            idx(ACCENT, mode),
            "{mode:?}: the progress fill lost its ramp"
        );
        // The ground ladder keeps at least ground/raised apart, so a
        // selected row still reads.
        assert_ne!(idx(GROUND, mode), idx(RAISED, mode), "{mode:?}: select bg");
    }
}

#[test]
fn apply_theme_is_identity_for_dark_and_remaps_for_light() {
    use std::sync::atomic::Ordering;
    // remap_theme is pure — exercise it directly rather than mutating the
    // process-global active theme (which other tests read).
    assert_eq!(remap_theme(ACCENT, LIGHT_REMAP), palette::BRAND_INK);
    assert_eq!(remap_theme(ACCENT_FILL, LIGHT_REMAP), palette::BRAND_INK);
    assert_eq!(
        remap_theme(ACCENT_DEEP, LIGHT_REMAP),
        palette::BRAND_INK_DEEP
    );
    // The bright stop is its own value now, and cyan cannot survive on paper
    // (1.72:1), so it walks down its own ramp rather than borrowing violet's.
    assert_eq!(
        remap_theme(ACCENT_BRIGHT, LIGHT_REMAP),
        Color::Rgb(0x0C, 0x66, 0x70)
    );
    // Gold is its own family under the nebula, so every gold stop lands on the
    // kit's one light-ground gold instead of collapsing onto the accent's
    // paper tone the way it did under the comet kit.
    assert_eq!(remap_theme(GOLD, LIGHT_REMAP), palette::GOLD_INK);
    assert_eq!(remap_theme(GOLD_BRIGHT, LIGHT_REMAP), palette::GOLD_INK);
    assert_eq!(remap_theme(GOLD_DEEP, LIGHT_REMAP), palette::GOLD_INK);
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
    // An unmapped colour (an interpolated gradient cell) passes through.
    let mid = lerp_rgb(palette::BRAND_DEEP, palette::BRAND_BRIGHT, 0.5);
    assert_eq!(remap_theme(mid, LIGHT_REMAP), mid);
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

#[test]
fn brand_gradient_spans_deep_to_bright_accent() {
    // The default theme is `stella-dark`, so the stops are the gold accent
    // pair; `primary_stops` swaps them for the deep paper golds under
    // `stella-light`.
    assert_eq!(brand_gradient(0.0), ACCENT_DEEP);
    assert_eq!(brand_gradient(1.0), ACCENT);
    // Monotonic, clamped, never panics across the range.
    for i in 0..=20 {
        let _ = brand_gradient(f64::from(i) / 20.0);
    }
    assert_eq!(brand_gradient(-1.0), ACCENT_DEEP);
    assert_eq!(brand_gradient(2.0), ACCENT);
    // The bright end is exactly what `crate::progress` falls back to when
    // the terminal cannot render the gradient — the fill must not change
    // colour with the terminal's depth.
    assert_eq!(brand_gradient(1.0), ACCENT);
}

#[test]
fn gold_gradient_spans_deep_to_bright_gold() {
    assert_eq!(gold_gradient(0.0), GOLD_DEEP);
    assert_eq!(gold_gradient(1.0), GOLD_BRIGHT);
    for i in 0..=20 {
        let _ = gold_gradient(f64::from(i) / 20.0);
    }
    assert_eq!(gold_gradient(-1.0), GOLD_DEEP);
    assert_eq!(gold_gradient(2.0), GOLD_BRIGHT);
}

#[test]
fn lighten_moves_toward_white() {
    assert_eq!(lighten(ACCENT, 0.0), ACCENT);
    assert_eq!(lighten(ACCENT, 1.0), Color::Rgb(255, 255, 255));
}
