//! Raw palette values -- the single normative colour source for the terminal.
//! Cut from the brand kit v1.0 ("the comet") at `docs/brand/` -- read
//! `css/tokens.css` and `brand-guidelines.html` before touching a value here.
//! Nothing here is semantic: for role names
//! (accent, ink, rule, status) see [`crate::theme`], which is the only module
//! that should reference these directly.
//!
//! The identity is **Phosphor Gold on Ink**: one colour, owned. Gold
//! `#FFB81A` is the signal -- the mark, the prompt, active/selected, focus --
//! and never the surface; Ink `#0B0B0C` is the ground; text is warm Paper
//! with the kit's warm neutral ramp beneath it. The old electric-blue/gold
//! split is gone: brand and gold are one family now, so `BRAND` and `GOLD`
//! deliberately share values. The paper theme (`stella-light`) swaps the
//! ground for warm paper and the gold for its deep ramp stops.
//!
//! Token names are hue-neutral on purpose (`BRAND`, not `GOLD_ACCENT`): the
//! brand hue has been recoloured before (aurora → gold → sky → green → ember →
//! blue → gold) and the name must outlive the value. Add a *value* here; name
//! a *role* in `theme`.
//!
//! Mirrored by `docs/brand/css/tokens.css`, `website/src/app/tokens.css` and
//! the observatory's inlined `:root` block; all of them must be edited
//! together. (The observatory's dark block is the closest sibling: same
//! ground ramp, same text ramp, same status trio, same data marks.)

use ratatui::style::Color;

// ── Ground (dark) ───────────────────────────────────────────────
//
// Ink: the terminal, not pure black. The kit's dark ground is a warm-neutral
// near-black -- no blue cast, no cool grays anywhere on the dark side. Pure
// black makes an accent scream; ink lets it speak, and it is what the
// marketing site and the observatory already render. `surface` and `raised`
// step up for cards and popovers; the two hairlines are the seam.

/// Deepest ground -- full-bleed backdrops, the splash, OG art. The
/// observatory's `--void`.
pub const VOID: Color = Color::Rgb(0x05, 0x05, 0x06);

/// App background. Ink `#0B0B0C` -- THE brand ground, painted as a real frame
/// fill by the deck, so every contrast figure below is measured against it.
pub const GROUND: Color = Color::Rgb(0x0B, 0x0B, 0x0C);

/// Card / panel surface, one step above ground (the kit's dark `--surface`).
pub const SURFACE: Color = Color::Rgb(0x13, 0x13, 0x15);

/// Raised surface -- popovers, selected rows, hovered cells.
pub const RAISED: Color = Color::Rgb(0x1B, 0x1B, 0x1E);

/// Seam / rule (the kit's dark `--border`). Deliberately low-contrast on
/// ground (1.26:1): decorative only, never the sole carrier of structure.
pub const HAIRLINE: Color = Color::Rgb(0x23, 0x23, 0x27);

/// Seam where a boundary must actually read -- panel edges, focused borders.
/// Still below 3:1 on ground (1.57:1), so it is a *stronger* decoration, not
/// a substitute for a glyph or a gap.
pub const HAIRLINE_STRONG: Color = Color::Rgb(0x33, 0x33, 0x38);

// ── Brand (dark: Phosphor Gold) ─────────────────────────────────
//
// The one owned colour. Brand marks, the prompt, active/running, focus,
// selection, primary action -- and nothing else: gold is the signal, never
// the surface. Reserved exactly as the blue before it was, but unlike the
// blue it needs no separate text tone: `#FFB81A` measures 11.36:1 on ground,
// so the same value is safe on a glyph, a one-cell rule, and a fill. A gold
// fill (a pill, a selected tab) always carries INK text -- white on gold is
// 1.83:1 and illegible.

/// Phosphor Gold `#FFB81A` -- CRT amber, the gold star. 11.36:1 on ground.
/// The kit's `#FFB000` lifted 10% in lightness; it may not drift.
pub const BRAND: Color = Color::Rgb(0xFF, 0xB8, 0x1A);

/// The bright stop of the brand sweep (kit `gold-400`, the observatory's
/// `--gold-bright`). 12.12:1 on ground. Gradients and hover lifts only --
/// resting chrome stays on [`BRAND`].
pub const BRAND_BRIGHT: Color = Color::Rgb(0xFD, 0xC1, 0x54);

/// Pressed / gradient-deep stop, and the leading stop of the progress fill
/// (kit `gold-600` lifted 10% with the brand). 8.77:1 on ground, so even the
/// fill's tail clears AA.
pub const BRAND_DEEP: Color = Color::Rgb(0xE5, 0xA0, 0x00);

// ── Brand (light: gold on paper) ────────────────────────────────
//
// The `stella-light` primary. Gold cannot hold a text edge on paper at full
// strength, so the light accent walks down the kit's own gold ramp until it
// clears AA on [`PAPER`]. Applied by the per-frame theme remap in
// [`crate::theme`], truecolor only.

/// The light-theme brand hue (kit `gold-800` lifted 10% with the brand) --
/// 5.22:1 on [`PAPER`].
///
/// Deliberately one step *below* the kit's `gold-deep` `#A37200`: that tone
/// is the web kit's light-ground gold for UI-sized text and measures 3.79:1
/// on the painted paper ground -- fine for chrome, under the 4.5:1 floor for
/// terminal-cell text. `gold-deep` stays in the family as [`GOLD_INK`], the
/// light theme's *chrome* gold.
pub const BRAND_INK: Color = Color::Rgb(0x85, 0x5E, 0x00);

/// Pressed stop / leading progress stop on paper (kit `gold-900` lifted 10%,
/// 8.76:1).
pub const BRAND_INK_DEEP: Color = Color::Rgb(0x5A, 0x3F, 0x00);

// ── Gold ────────────────────────────────────────────────────────
//
// Identity chrome: the logo's block cursor, splash rules, section markers.
// Under the comet kit, gold IS the brand, so [`GOLD`] and [`BRAND`] share a
// value on purpose -- the split only survives as *names* so the identity
// sweep ([`GOLD_DEEP`] → [`GOLD_BRIGHT`]) can run wider and quieter than the
// progress fill's.
//
// Gold NEVER carries a verdict. It sits 4.0° from [`WARNING`] in hue, so a
// reader must never be asked to tell an outcome from chrome by hue alone --
// status is always glyph-paired (`theme::gold_never_carries_a_verdict`
// enforces this). Activity is the one status gold does carry: active/running
// IS the accent, by kit rule.

/// The mark's gold -- the same Phosphor Gold as [`BRAND`].
pub const GOLD: Color = Color::Rgb(0xFF, 0xB8, 0x1A);

/// Headline gradient stop -- the bright end of the gold sweep (== kit
/// `gold-400`, [`BRAND_BRIGHT`]).
pub const GOLD_BRIGHT: Color = Color::Rgb(0xFD, 0xC1, 0x54);

/// Trailing stop of the identity sweep (kit `gold-700`, the web kit's
/// `gold-deep`). 4.65:1 on ground -- clears AA body text and the 3:1
/// graphical floor, but the *progress* fill leads with the stronger
/// [`BRAND_DEEP`] instead.
pub const GOLD_DEEP: Color = Color::Rgb(0xA3, 0x72, 0x00);

/// Gold chrome on a light ground (kit `gold-700` again -- `gold-deep` is the
/// kit's one light-ground gold). 3.79:1 on [`PAPER`]: a *graphical* tone
/// (splash rules, the identity sweep), while light-ground gold TEXT takes
/// [`BRAND_INK`].
pub const GOLD_INK: Color = Color::Rgb(0xA3, 0x72, 0x00);

// ── Text (dark ground) ──────────────────────────────────────────
//
// Warm Paper over the kit's warm neutral ramp -- no cool grays. Prose is
// paper-white: the accent earns its meaning by being rare, which only works
// if the default voice is uncoloured.

/// Primary text. Warm Paper -- the transcript's default voice. 17.44:1 on
/// [`GROUND`].
pub const TEXT_PRIMARY: Color = Color::Rgb(0xF4, 0xF1, 0xEA);

/// Secondary text (the observatory's `--text-2`, the kit's dark
/// `--muted-fg`). The safe small-text tone on every dark ground (6.83:1).
pub const TEXT_SECONDARY: Color = Color::Rgb(0x9B, 0x98, 0x90);

/// Labels and captions (kit `neutral-500`, the observatory's `--text-3`).
/// AA body on void/ground/surface (5.71:1 / 5.38:1); on [`RAISED`] it drops
/// to 4.98:1 -- still AA, but treat it as the floor.
pub const TEXT_TERTIARY: Color = Color::Rgb(0x8D, 0x8A, 0x82);

// ── Status ──────────────────────────────────────────────────────
//
// Functional, not brand: always paired with a glyph. Hue alone never carries
// meaning -- which matters doubly now that warning is a hue-neighbour of the
// gold accent (4.0° apart).

/// Success / done / added (11.29:1 on ground). Also the settled cost of a
/// finished turn -- money spent is a fact, and a fact reads green.
pub const SUCCESS: Color = Color::Rgb(0x4A, 0xDE, 0x80);

/// Warning / needs-input. Amber (10.26:1) -- 4.0° from the gold accent in
/// hue, which is exactly why the glyph, never the hue, is the status
/// carrier.
pub const WARNING: Color = Color::Rgb(0xEA, 0xB3, 0x08);

/// Error / failed / removed (6.62:1 on ground).
pub const DANGER: Color = Color::Rgb(0xFF, 0x5C, 0x7A);

// ── Status (light ground) ───────────────────────────────────────
//
// The same three meanings, darkened along their own hue until they clear AA
// on the painted paper ground. The dark-ground status tones are light
// colours -- `success` is well under 2:1 on paper -- so a light surface
// needs its own set for the same reason the brand hue does.

/// Success on a light ground -- 5.16:1 on [`PAPER`].
pub const SUCCESS_INK: Color = Color::Rgb(0x16, 0x74, 0x4F);

/// Warning on a light ground -- amber-brown, 5.61:1 on [`PAPER`].
///
/// One step darker than the web kit's `warning-ink` `#A16207`, which was
/// tuned for white and measures 4.41:1 on the warm paper the deck actually
/// paints -- under the 4.5:1 floor. Same hue, more ink.
pub const WARNING_INK: Color = Color::Rgb(0x8A, 0x54, 0x05);

/// Error on a light ground -- 5.06:1 on [`PAPER`].
pub const DANGER_INK: Color = Color::Rgb(0xC8, 0x1E, 0x3E);

// ── Ground (light) ──────────────────────────────────────────────
//
// The paper mode: the kit's warm paper, not white. Accent here is
// [`BRAND_INK`], text is [`INK`].

/// Light background -- the kit's `paper-bg`, a warm off-white.
pub const PAPER: Color = Color::Rgb(0xF6, 0xF2, 0xE9);

/// Light surface (the kit's light `--surface`). Lifts *lighter* than paper,
/// as the dark surface lifts lighter than ink.
pub const SNOW: Color = Color::Rgb(0xFC, 0xFA, 0xF4);

/// Light raised surface -- popovers, selected rows on paper (kit
/// `neutral-100`).
pub const PAPER_RAISED: Color = Color::Rgb(0xE4, 0xE2, 0xDC);

/// Light seam / rule (the kit's light `--border`) -- the paper counterpart
/// of [`HAIRLINE`].
pub const PAPER_HAIRLINE: Color = Color::Rgb(0xE4, 0xDE, 0xCF);

/// Primary text on paper -- Ink itself, 17.61:1. (The same value as
/// [`GROUND`]: the kit's one black serves as both the dark ground and the
/// light text, which is the point of an identity this small.)
pub const INK: Color = Color::Rgb(0x0B, 0x0B, 0x0C);

/// Secondary text on paper (the kit's light `--muted-fg`) -- 4.83:1.
pub const MUTED: Color = Color::Rgb(0x6E, 0x6A, 0x5F);

/// Tertiary text on paper (kit `neutral-600`) -- the paper counterpart of
/// [`TEXT_TERTIARY`]. 4.61:1 on [`PAPER`], so it clears AA body; on [`SNOW`]
/// (4.94:1) it holds, and on [`PAPER_RAISED`] (3.98:1) it is a large-text /
/// UI tone only, exactly as [`TEXT_TERTIARY`] is treated on [`RAISED`].
pub const INK_DIM: Color = Color::Rgb(0x73, 0x6C, 0x68);

// ── Data marks ──────────────────────────────────────────────────
//
// The categorical series palette, shared verbatim with the observatory
// (its `--c1..--c4`). Deliberately *not* the brand hue -- a data mark must
// not read as "active" -- and deliberately not a status hue either. All are
// complements of gold that stay warm/rich on ink: a muted violet, a warm
// rose, a deep teal.
//
// [`DATA_1`] and [`GOLD`] are hue 42° and 41° -- 1.06:1 against each other,
// nothing distinguishes them but size. The observatory stands the amber mark
// down from every plotted surface for exactly this reason; in the deck it
// survives ONLY as the syntax-keyword tone inside code bodies, where gold
// chrome never reaches (see `theme::SYNTAX_KEYWORD`). Never put it on a
// chip, a node, or an agent.

/// Categorical 1 -- amber. 10.11:1 on ground; see the stand-down note above.
pub const DATA_1: Color = Color::Rgb(0xE3, 0xB3, 0x41);
/// Categorical 2 -- muted violet, hue 256° (215° from gold). 5.29:1 on
/// ground.
pub const DATA_2: Color = Color::Rgb(0x8F, 0x70, 0xE8);
/// Categorical 3 -- warm rose, hue 331° (70° from gold). 5.09:1 on ground;
/// 1.30:1 against [`DANGER`], so it never carries an error meaning and never
/// appears without a label or glyph.
pub const DATA_3: Color = Color::Rgb(0xE4, 0x40, 0x8F);
/// Categorical 4 -- deep teal, hue 175° (134° from gold). 10.54:1 on ground.
pub const DATA_4: Color = Color::Rgb(0x2F, 0xD3, 0xC6);

/// Every palette colour, paired with its token name.
///
/// Lets a test walk the whole palette -- see theme.rs's
/// `every_dark_palette_value_has_a_fallback` -- without a hand-maintained
/// second list. Names match the kit's token vocabulary at
/// `docs/brand/css/tokens.css`.
pub const ALL: [(&str, Color); 35] = [
    ("void", VOID),
    ("ground", GROUND),
    ("surface", SURFACE),
    ("raised", RAISED),
    ("hairline", HAIRLINE),
    ("hairline-strong", HAIRLINE_STRONG),
    ("brand", BRAND),
    ("brand-bright", BRAND_BRIGHT),
    ("brand-deep", BRAND_DEEP),
    ("brand-ink", BRAND_INK),
    ("brand-ink-deep", BRAND_INK_DEEP),
    ("gold", GOLD),
    ("gold-bright", GOLD_BRIGHT),
    ("gold-deep", GOLD_DEEP),
    ("gold-ink", GOLD_INK),
    ("text-primary", TEXT_PRIMARY),
    ("text-secondary", TEXT_SECONDARY),
    ("text-tertiary", TEXT_TERTIARY),
    ("success", SUCCESS),
    ("warning", WARNING),
    ("danger", DANGER),
    ("success-ink", SUCCESS_INK),
    ("warning-ink", WARNING_INK),
    ("danger-ink", DANGER_INK),
    ("paper", PAPER),
    ("snow", SNOW),
    ("paper-raised", PAPER_RAISED),
    ("paper-hairline", PAPER_HAIRLINE),
    ("ink", INK),
    ("muted", MUTED),
    ("ink-dim", INK_DIM),
    ("data-1", DATA_1),
    ("data-2", DATA_2),
    ("data-3", DATA_3),
    ("data-4", DATA_4),
];
