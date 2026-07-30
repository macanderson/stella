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
/// they are intentionally not re-listed.
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
    ACCENT_FILL,
    ACCENT_DEEP,
    GOLD,
    GOLD_BRIGHT,
    GOLD_DEEP,
    SUCCESS,
    WARNING,
    DANGER,
    VIOLET,
    AMBER,
    TEAL,
    MAGENTA,
    CODE,
    DIFF_ADD_BG,
    DIFF_DEL_BG,
    DIFF_ADD_BG_EMPH,
    DIFF_DEL_BG_EMPH,
    MATCH_BG,
    SYNTAX_STRING,
    SYNTAX_COMMENT,
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
    // ACCENT is the *bright* brand tone: it is what paints text and rules,
    // and plain `brand` does not clear AA body on surface/raised.
    assert_eq!(ACCENT, palette::BRAND_BRIGHT);
    assert_eq!(ACCENT_FILL, palette::BRAND);
    assert_eq!(ACCENT_DEEP, palette::BRAND_DEEP);
    assert_eq!(GOLD, palette::GOLD);
    assert_eq!(GOLD_BRIGHT, palette::GOLD_BRIGHT);
    assert_eq!(GOLD_DEEP, palette::GOLD_DEEP);
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
    // Syntax and process hues are categorical -- never the brand accent.
    assert_eq!(SYNTAX_NUMBER, VIOLET);
    assert_eq!(SYNTAX_KEYWORD, AMBER);
    // The categorical set is the observatory's data-mark palette verbatim,
    // so a series in a chart and a chip in the deck are the same colour.
    assert_eq!(AMBER, palette::DATA_1);
    assert_eq!(VIOLET, palette::DATA_2);
    assert_eq!(MAGENTA, palette::DATA_3);
    assert_eq!(TEAL, palette::DATA_4);
}

/// BRAND.md's one hard prohibition: **gold never carries status.** Gold and
/// [`WARNING`] are the same brass at a glance (42.3° vs 45.4° hue), so a
/// reader must never be asked to tell them apart — gold is identity chrome
/// and status is glyph-paired amber. This is the test that would fail if
/// someone reached for the prettier colour in a status mapping.
#[test]
fn gold_is_never_a_status_colour() {
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
            AgentStatus::Running,
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
/// The guard has outlived four recolours now (aurora → gold → sky → green →
/// blue), which is the point: each time, the *shape* of the law survived
/// and only the hue it names changed. Keep rewriting it rather than deleting
/// it.
///
/// The blue recolour changed the law in two real ways. First, the ground is
/// no longer neutral: deep space is a blue-cast near-black on purpose, so
/// the old "the canvas may not carry a cast" clause inverts into "the canvas
/// must carry the *right* cast". Second, the green brand had one permitted
/// neighbour (success, also green, told apart by ▶ vs ✓). A blue brand has
/// none: success, warning and danger are all far from blue, so the
/// reservation is absolute again — which is the stronger law.
///
/// What must hold now:
///   1. The accent is the generated brand blue — not a local literal that
///      drifted, not one of the retired hues, and actually blue. It is the
///      *bright* tone, because almost every call site paints text with it.
///   2. The ground is deep space: blue-cast, and never true black, or the
///      accent screams instead of speaking.
///   3. Warning ramps warm (r > g > b — amber, not the retired orange),
///      success ramps green, danger ramps red.
///   4. The brand blue is reserved. Every chromatic role must sit at least
///      30° of hue away from it, with no exceptions.
///   5. Gold is identity, not status: it stays clear of the brand blue *and*
///      is never a status value (see `gold_is_never_a_status_colour`).
#[test]
fn palette_law_blue_is_the_brand() {
    const RETIRED_AURORA_CYAN: Color = Color::Rgb(0x3F, 0xE0, 0xFF);
    const RETIRED_EMBER_FLAME: Color = Color::Rgb(0xFF, 0x7E, 0x5F);
    const RETIRED_EMBER_CRIMSON: Color = Color::Rgb(0xC2, 0x18, 0x5B);
    const RETIRED_GOLD: Color = Color::Rgb(0xFF, 0xDD, 0x00);
    const RETIRED_GOLD_DEEP: Color = Color::Rgb(0xE0, 0xB8, 0x00);
    /// The old glacier blue, retired *because* it collided with the sky
    /// accent — the exact failure this test's clause 4 still prevents.
    const RETIRED_AGENT_ICE: Color = Color::Rgb(0xA8, 0xC7, 0xF0);
    /// The sky blue of two recolours ago. The brand is blue again but a
    /// *different* blue; no token may drift back to the old pair.
    const RETIRED_SKY: Color = Color::Rgb(0x7D, 0xD3, 0xFC);
    const RETIRED_SKY_DEEP: Color = Color::Rgb(0x38, 0xBD, 0xF8);
    /// The warning orange the "get rid of the orange" pass removed; warning
    /// is amber-yellow now and must not slide back to it.
    const RETIRED_WARNING_ORANGE: Color = Color::Rgb(0xFF, 0x8A, 0x1F);
    /// The terminal green this recolour retired, and its deep stop.
    const RETIRED_PHOSPHOR_GREEN: Color = Color::Rgb(0x00, 0xE6, 0x76);
    const RETIRED_PHOSPHOR_GREEN_DEEP: Color = Color::Rgb(0x00, 0xB2, 0x5A);
    /// The vermilion light-theme brand ("ember") and its deep stop.
    const RETIRED_EMBER: Color = Color::Rgb(0xFF, 0x3D, 0x1F);
    const RETIRED_EMBER_DEEP: Color = Color::Rgb(0xD6, 0x2E, 0x0E);

    // 1. The accent comes from the palette, not a drifted local literal, it
    //    is the text-safe bright tone, and it is blue (b dominant).
    assert_eq!(
        ACCENT,
        palette::BRAND_BRIGHT,
        "the accent must be the bright brand blue"
    );
    for (blue, name) in [(ACCENT, "ACCENT"), (ACCENT_FILL, "ACCENT_FILL")] {
        let Color::Rgb(r, g, b) = blue else {
            panic!("{name} must be a truecolor token");
        };
        assert!(b > r && b > g, "{name} must be blue-dominant");
    }

    // The retired hues are gone from every token and alias.
    let mut all: Vec<Color> = ALL_RGB_TOKENS.to_vec();
    all.extend([
        RUN,
        SYNTAX_NUMBER,
        SYNTAX_KEYWORD,
        HELD,
        ACCENT,
        OK,
        ACCENT_DEEP,
        CODE,
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
        ] {
            assert_ne!(*token, retired, "a token still holds {name}");
        }
    }

    // 2. The ground is deep space: a blue cast, and never true black. Pure
    //    black makes the accent scream; the cast is what lets it speak.
    for (ground, name) in [
        (VOID, "VOID"),
        (GROUND, "GROUND"),
        (SURFACE, "SURFACE"),
        (RAISED, "RAISED"),
    ] {
        let Color::Rgb(r, g, b) = ground else {
            panic!("{name} must be a truecolor token");
        };
        assert!(
            b > r && b >= g,
            "{name} ({ground:?}) must carry the deep-space blue cast"
        );
        assert_ne!(ground, Color::Rgb(0, 0, 0), "{name} must not be true black");
    }

    // 3. Warning ramps warm (r > g > b — amber, no longer the orange that
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

    // 4. The brand blue is reserved for brand / active / focus / progress.
    //    No chromatic role may sit within 30° of it — which is how
    //    `AGENT_ICE` died, and the clause has no exceptions now that success
    //    is no longer a neighbour of the brand hue.
    for (role, name) in [
        (WARN, "WARN"),
        (BAD, "BAD"),
        (OK, "OK"),
        (RUN, "RUN"),
        (HELD, "HELD"),
        (VIOLET, "VIOLET"),
        (AMBER, "AMBER"),
        (TEAL, "TEAL"),
        (MAGENTA, "MAGENTA"),
        (GOLD, "GOLD"),
        (GOLD_BRIGHT, "GOLD_BRIGHT"),
        (GOLD_DEEP, "GOLD_DEEP"),
        (CODE, "CODE"),
        (SYNTAX_KEYWORD, "SYNTAX_KEYWORD"),
        (SYNTAX_STRING, "SYNTAX_STRING"),
        (SYNTAX_NUMBER, "SYNTAX_NUMBER"),
    ] {
        assert_ne!(role, ACCENT, "{name} must not be the reserved brand blue");
        assert_ne!(role, ACCENT_FILL, "{name} must not be the brand fill");
        let sep = hue_separation(role, ACCENT);
        assert!(
            sep >= 30.0,
            "{name} ({role:?}) is {sep:.1}° from the brand accent; \
             30° is the floor for two hues to be told apart in a cell"
        );
    }

    // 5. The brand's own tones are the only things allowed near it, and the
    //    bright/fill pair really is one hue family.
    assert!(hue_separation(ACCENT_FILL, ACCENT) < 10.0);
    assert!(hue_separation(ACCENT_DEEP, ACCENT) < 10.0);
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
    // The named ANSI colors are colors too: the single-session REPL styles
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
    buf.content[0].fg = ACCENT; // → 75 (256) / 12 (16)
    buf.content[0].bg = VIOLET; // → 98 (256) / 13 (16)
    degrade_buffer(&mut buf, ColorMode::Ansi256);
    assert_eq!(buf.content[0].fg, Color::Indexed(75));
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
    assert_eq!(remap_theme(GOLD, LIGHT_REMAP), palette::GOLD_INK);
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
    // The default theme is `stella-dark`, so the stops are the blue accent
    // pair; `primary_stops` swaps them for the paper blues under
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
