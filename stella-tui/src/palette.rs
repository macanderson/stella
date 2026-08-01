//! Raw palette values -- the single normative colour source for the terminal.
//! Cut from the since-retired BRAND.md; the brand kit now lives at
//! `docs/brand/BRAND.md`, and the terminal has not yet been repainted to it.
//! Nothing here is semantic: for role names
//! (accent, ink, rule, status) see [`crate::theme`], which is the only module
//! that should reference these directly.
//!
//! The identity is **electric blue and gold on deep space**. Blue is the
//! interactive signal (active/running, focus, links); gold is the mark (the
//! logo cursor block, splash rules, section markers) and never carries status.
//! The paper theme (`stella-light`) swaps both for their ink counterparts.
//!
//! Token names are hue-neutral on purpose (`BRAND`, not `SKY`): the brand hue
//! has been recoloured before (aurora → gold → sky → green → ember → blue) and
//! the name must outlive the value. Add a *value* here; name a *role* in
//! `theme`.
//!
//! Mirrored by `docs/brand/tokens/*`, `website/src/app/tokens.css` and the
//! observatory's inlined `:root` block; all of them must be edited together.

use ratatui::style::Color;

// ── Ground (dark) ───────────────────────────────────────────────
//
// Deep space: a near-black with a blue cast rather than true black. Pure black
// makes an accent scream; a tinted ground lets it speak, and it is what the
// marketing site already renders. `surface` and `raised` step up for cards and
// popovers; the two hairlines are the seam.

/// Deepest ground -- full-bleed backdrops, the splash, OG art.
pub const VOID: Color = Color::Rgb(0x05, 0x07, 0x0C);

/// App background. The default dark canvas.
pub const GROUND: Color = Color::Rgb(0x08, 0x0A, 0x0F);

/// Card / panel surface, one step above ground.
pub const SURFACE: Color = Color::Rgb(0x0B, 0x0F, 0x17);

/// Raised surface -- popovers, selected rows, hovered cells.
pub const RAISED: Color = Color::Rgb(0x10, 0x16, 0x23);

/// Seam / rule. Deliberately low-contrast on ground (1.2:1): decorative only,
/// never the sole carrier of structure.
pub const HAIRLINE: Color = Color::Rgb(0x18, 0x20, 0x31);

/// Seam where a boundary must actually read -- panel edges, focused borders.
/// Still below 3:1 on ground (1.4:1), so it is a *stronger* decoration, not a
/// substitute for a glyph or a gap.
pub const HAIRLINE_STRONG: Color = Color::Rgb(0x23, 0x2D, 0x42);

// ── Brand (dark: electric blue) ─────────────────────────────────
//
// The interactive signal: active/running, focus, links, primary action.
// Reserved -- never a general-purpose highlight. `BRAND` is 5.1:1 on ground,
// which clears AA for body text but leaves no headroom on `surface`/`raised`,
// so anything that paints *text* or a one-cell rule takes `BRAND_BRIGHT`
// (7.4:1) and only larger flat fills take `BRAND`.

/// The brand hue on dark ground -- electric blue. Fills, not small text.
pub const BRAND: Color = Color::Rgb(0x2E, 0x7B, 0xFF);

/// The text- and stroke-safe brand tone. 7.4:1 on ground; use this wherever
/// the brand hue lands on a glyph or a 1-cell rule.
pub const BRAND_BRIGHT: Color = Color::Rgb(0x5A, 0xA0, 0xFF);

/// Pressed / gradient-deep stop, and the leading stop of the progress fill.
pub const BRAND_DEEP: Color = Color::Rgb(0x1A, 0x5F, 0xE0);

// ── Brand (light: blue on paper) ────────────────────────────────
//
// The `stella-light` primary. Same hue family, darkened until it clears AA on
// white (7.0:1). Applied by the per-frame theme remap in [`crate::theme`],
// truecolor only.

/// The light-theme brand hue -- 7.0:1 on [`PAPER`].
///
/// This *was* [`BRAND_DEEP`]'s value too, on the theory that the dark theme's
/// pressed stop is the light theme's resting tone. It isn't: the same colour
/// that reads at 7.0:1 on paper measures 2.84:1 on ground, under the 3:1 floor
/// for a graphical element -- and `BRAND_DEEP` leads the progress fill, so the
/// left end of a running bar sat below the floor. The two are separate values
/// now; only the golds still hold that relationship.
pub const BRAND_INK: Color = Color::Rgb(0x15, 0x50, 0xC8);

/// Pressed stop / leading progress stop on paper.
pub const BRAND_INK_DEEP: Color = Color::Rgb(0x0F, 0x3A, 0x94);

// ── Gold ────────────────────────────────────────────────────────
//
// The identity accent: the logo's block cursor, splash rules, section markers.
//
// Gold NEVER carries status. It sits close enough to [`WARNING`] in hue that a
// reader must never have to tell the two apart in the same row -- status is
// amber and always glyph-paired; gold is identity and appears only on brand
// chrome. `theme::gold_is_never_a_status_colour` enforces this.

/// The mark's gold -- 11.9:1 on ground.
pub const GOLD: Color = Color::Rgb(0xF5, 0xC1, 0x45);

/// Headline gradient stop -- the bright end of the gold sweep.
pub const GOLD_BRIGHT: Color = Color::Rgb(0xFF, 0xD8, 0x73);

/// Trailing stop of the gold fill.
pub const GOLD_DEEP: Color = Color::Rgb(0xC9, 0x94, 0x20);

/// Gold on a light ground -- 5.5:1 on [`PAPER`]. One gold cannot hold its edge
/// against both white and deep space.
pub const GOLD_INK: Color = Color::Rgb(0x8A, 0x61, 0x18);

// ── Text (dark ground) ──────────────────────────────────────────
//
// Cool neutrals for the dark canvas. Prose is white: the accent earns its
// meaning by being rare, which only works if the default voice is uncoloured.

/// Primary text. The transcript's default voice. 18.1:1 on [`GROUND`].
pub const TEXT_PRIMARY: Color = Color::Rgb(0xF2, 0xF5, 0xFA);

/// Secondary text. The safe small-text tone on every dark ground (6.7:1).
pub const TEXT_SECONDARY: Color = Color::Rgb(0x8E, 0x97, 0xA8);

/// Labels and captions. AA body on void/ground/surface (4.8:1 / 4.6:1); on
/// [`RAISED`] it drops to 4.4:1 and is a large-text / UI tone only.
pub const TEXT_TERTIARY: Color = Color::Rgb(0x73, 0x7D, 0x92);

// ── Status ──────────────────────────────────────────────────────
//
// Always paired with a glyph. Hue alone never carries meaning.

/// Success / done / added. Also the settled cost of a finished turn -- money
/// spent is a fact, and a fact reads green.
pub const SUCCESS: Color = Color::Rgb(0x4A, 0xDE, 0x80);

/// Warning / needs-input. Amber-yellow -- caution, deliberately clear of the
/// retired warning orange and never sharing a row with [`GOLD`].
pub const WARNING: Color = Color::Rgb(0xEA, 0xB3, 0x08);

/// Error / failed / removed.
pub const DANGER: Color = Color::Rgb(0xFF, 0x5C, 0x7A);

// ── Status (light ground) ───────────────────────────────────────
//
// The same three meanings, darkened along their own hue until they clear AA
// on paper. The dark-ground status tones are light colours -- `success` is
// 1.72:1 on white -- so a light surface needs its own set for the same
// reason the brand hue does.

/// Success on a light ground -- 5.76:1 on paper.
pub const SUCCESS_INK: Color = Color::Rgb(0x16, 0x74, 0x4F);

/// Warning on a light ground -- amber-brown, 4.92:1 on paper.
pub const WARNING_INK: Color = Color::Rgb(0xA1, 0x62, 0x07);

/// Error on a light ground -- 5.66:1 on paper.
pub const DANGER_INK: Color = Color::Rgb(0xC8, 0x1E, 0x3E);

// ── Ground (light) ──────────────────────────────────────────────
//
// The paper mode. Accent here is [`BRAND_INK`], text is [`INK`].

/// Light background.
pub const PAPER: Color = Color::Rgb(0xFF, 0xFF, 0xFF);

/// Light surface, one step in from paper.
pub const SNOW: Color = Color::Rgb(0xF5, 0xF7, 0xFA);

/// Light raised surface -- popovers, selected rows on paper.
pub const PAPER_RAISED: Color = Color::Rgb(0xE7, 0xEB, 0xF2);

/// Light seam / rule -- the paper counterpart of [`HAIRLINE`].
pub const PAPER_HAIRLINE: Color = Color::Rgb(0xD3, 0xDA, 0xE6);

/// Primary text on paper -- 19.2:1.
pub const INK: Color = Color::Rgb(0x0A, 0x0F, 0x1A);

/// Secondary text on paper -- 6.7:1.
pub const MUTED: Color = Color::Rgb(0x52, 0x5C, 0x6E);

/// Tertiary text on paper -- the paper counterpart of [`TEXT_TERTIARY`].
/// 4.69:1 on [`PAPER`], so it clears AA body; on [`SNOW`] (4.37:1) and
/// [`PAPER_RAISED`] (3.92:1) it is a large-text / UI tone only, exactly as
/// [`TEXT_TERTIARY`] is on [`RAISED`].
pub const INK_DIM: Color = Color::Rgb(0x6B, 0x74, 0x88);

// ── Data marks ──────────────────────────────────────────────────
//
// The categorical series palette, shared with the observatory. Deliberately
// *not* the brand hue -- a data mark must not read as "active" -- and
// deliberately not a status hue either. [`DATA_1`] and [`GOLD`] must never
// appear in the same chart; they are the same brass at a glance.

/// Categorical 1 -- amber.
pub const DATA_1: Color = Color::Rgb(0xE3, 0xB3, 0x41);
/// Categorical 2 -- violet.
pub const DATA_2: Color = Color::Rgb(0x8F, 0x70, 0xE8);
/// Categorical 3 -- magenta.
pub const DATA_3: Color = Color::Rgb(0xE4, 0x40, 0x8F);
/// Categorical 4 -- teal.
pub const DATA_4: Color = Color::Rgb(0x2F, 0xD3, 0xC6);

/// Every palette colour, paired with its token name.
///
/// Lets a test walk the whole palette -- see theme.rs's
/// `every_dark_palette_value_has_a_fallback` -- without a hand-maintained
/// second list. Names match the token column of the retired BRAND.md (the
/// current kit lives at `docs/brand/BRAND.md`).
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
