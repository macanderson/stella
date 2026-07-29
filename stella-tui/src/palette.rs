//! Raw palette values -- the single normative colour source, shared with
//! the docs site and the observatory. Nothing here is semantic: for role
//! names (accent, ink, rule, status) see [`crate::theme`], which is the
//! only module that should be referencing these directly.
//!
//! The default identity is **terminal green on black**: a phosphor signal in
//! the dark. The light theme (`stella-light`) inverts to **ember on paper** --
//! the red-orange of the wordmark on white. The token names here are
//! hue-neutral on purpose (`BRAND`, not `SKY`): the brand hue has been
//! recoloured before (aurora → gold → sky → green) and the name must outlive
//! the value. Add a *value* here; name a *role* in `theme`.
//!
//! Mirrored by `website/src/app/tokens.css` (`--stella-*`); the two must be
//! edited together.

use ratatui::style::Color;

// ── Ground (dark) ───────────────────────────────────────────────
//
// True black, not a tinted navy. The accent is a bright, high-chroma green,
// and a colour-tinted ground robs it of the contrast that makes it read as a
// signal rather than as decoration -- so the canvas is neutral and the only
// colour on screen is colour that means something. `surface` and `raised`
// step up for cards and popovers.

/// Deepest ground -- full-bleed backdrops, the splash, OG art.
pub const NIGHT: Color = Color::Rgb(0x00, 0x00, 0x00);

/// App background. The default dark canvas.
pub const GROUND: Color = Color::Rgb(0x00, 0x00, 0x00);

/// Card / panel surface, one step above ground.
pub const SURFACE: Color = Color::Rgb(0x0A, 0x0E, 0x14);

/// Raised surface -- popovers, selected rows, hovered cells.
pub const RAISED: Color = Color::Rgb(0x14, 0x1C, 0x26);

/// Seam / rule. Deliberately low-contrast on ground: decorative only, never
/// the sole carrier of structure.
pub const HAIRLINE: Color = Color::Rgb(0x24, 0x31, 0x3F);

// ── Brand (dark: terminal green) ────────────────────────────────
//
// Bright phosphor green. Reserved for brand, active/running, and progress --
// never a general-purpose highlight. In the transcript this means exactly one
// thing carries it: the name of the tool being called. Green is close to the
// success hue by nature; the two are kept tellable apart (a purer, brighter
// green here; a softer green for status) and always paired with a distinct
// glyph (▶ active vs ✓ done), so hue never carries the meaning alone.

/// The brand hue on dark ground -- terminal green.
pub const BRAND: Color = Color::Rgb(0x00, 0xE6, 0x76);

/// Pressed / gradient-deep stop, and the leading stop of the progress fill.
pub const BRAND_DEEP: Color = Color::Rgb(0x00, 0xB2, 0x5A);

// ── Brand (light: ember) ────────────────────────────────────────
//
// The `stella-light` primary: the red-orange of the wordmark square (#FF3D1F).
// On paper this is the accent, the active/running signal, and the progress
// fill -- the light-theme counterpart of the dark green. Applied by the
// per-frame theme remap in [`crate::theme`], truecolor only.

/// Ember -- the light-theme brand hue, sampled from the logo.
pub const EMBER: Color = Color::Rgb(0xFF, 0x3D, 0x1F);

/// Deep ember -- the leading stop of the light-theme progress fill.
pub const EMBER_DEEP: Color = Color::Rgb(0xD6, 0x2E, 0x0E);

// ── Text (dark ground) ──────────────────────────────────────────
//
// Cool neutrals for the dark canvas. Prose is white: the accent earns its
// meaning by being rare, which only works if the default voice is uncoloured.

/// Primary text. The transcript's default voice.
pub const TEXT_PRIMARY: Color = Color::Rgb(0xF3, 0xF6, 0xFA);

/// Secondary text. The safe small-text tone on every dark ground.
pub const TEXT_SECONDARY: Color = Color::Rgb(0x98, 0xA6, 0xBA);

/// Labels and captions. AA body on night/ground; large-text or UI only on
/// surface/raised.
pub const TEXT_TERTIARY: Color = Color::Rgb(0x6C, 0x7B, 0x90);

// ── Status ──────────────────────────────────────────────────────
//
// Always paired with a glyph. Hue alone never carries meaning.

/// Success / done / added. Also the settled cost of a finished turn -- money
/// spent is a fact, and a fact reads green. A softer green than the brand.
pub const SUCCESS: Color = Color::Rgb(0x4A, 0xDE, 0x80);

/// Warning / needs-input. Amber-yellow -- caution, deliberately clear of the
/// ember light-brand and the retired warning orange.
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

/// Warning on a light ground -- amber-brown, legible on paper and clear of the
/// ember primary.
pub const WARNING_INK: Color = Color::Rgb(0xA1, 0x62, 0x07);

/// Error on a light ground -- 5.88:1 on paper.
pub const DANGER_INK: Color = Color::Rgb(0xC8, 0x10, 0x2E);

// ── Ground (light) ──────────────────────────────────────────────
//
// The paper mode. Accent here is ember, text is ink.

/// Light background.
pub const PAPER: Color = Color::Rgb(0xFF, 0xFF, 0xFF);

/// Light surface, one step in from paper.
pub const SNOW: Color = Color::Rgb(0xF4, 0xF4, 0xF5);

/// Light raised surface -- popovers, selected rows on paper.
pub const PAPER_RAISED: Color = Color::Rgb(0xE8, 0xE9, 0xEB);

/// Light seam / rule -- the paper counterpart of [`HAIRLINE`].
pub const PAPER_HAIRLINE: Color = Color::Rgb(0xD4, 0xD4, 0xD8);

/// Primary text on paper.
pub const INK: Color = Color::Rgb(0x0A, 0x0A, 0x0A);

/// Secondary text on paper.
pub const MUTED: Color = Color::Rgb(0x5B, 0x62, 0x70);

/// Tertiary text on paper -- the paper counterpart of [`TEXT_TERTIARY`].
pub const INK_DIM: Color = Color::Rgb(0x8A, 0x8F, 0x98);

/// Every palette colour, paired with its token name.
///
/// Lets a test walk the whole palette -- see theme.rs's colour-depth
/// fallback coverage check -- without a hand-maintained second list.
pub const ALL: [(&str, Color); 25] = [
    ("night", NIGHT),
    ("ground", GROUND),
    ("surface", SURFACE),
    ("raised", RAISED),
    ("hairline", HAIRLINE),
    ("brand", BRAND),
    ("brand-deep", BRAND_DEEP),
    ("ember", EMBER),
    ("ember-deep", EMBER_DEEP),
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
];
