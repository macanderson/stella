//! Generated from docs/brand/tokens.json by docs/brand/build.py (edit those, not this file).
//!
//! Raw palette values -- the single normative colour source, shared with
//! the docs site and the observatory. Nothing here is semantic: for role
//! names (accent, ink, rule, status) see [`crate::theme`], which is the
//! only module that should be referencing these directly.
//!
//! The identity is gold on deep navy: a star against night sky.

use ratatui::style::Color;

// ── Ground (dark) ───────────────────────────────────────────────
//
// The night sky the whole identity sits on. `ground` is the app background;
// `surface` and `raised` step up for cards and popovers.

/// Deepest ground -- full-bleed backdrops, the splash, OG art.
pub const NIGHT: Color = Color::Rgb(0x05, 0x09, 0x12);

/// App background. The default dark canvas.
pub const GROUND: Color = Color::Rgb(0x08, 0x0D, 0x1A);

/// Card / panel surface, one step above ground.
pub const SURFACE: Color = Color::Rgb(0x0F, 0x17, 0x29);

/// Raised surface -- popovers, selected rows, hovered cells.
pub const RAISED: Color = Color::Rgb(0x17, 0x21, 0x37);

/// Seam / rule. Deliberately 1.47:1 on ground: decorative only, never the
/// sole carrier of structure.
pub const HAIRLINE: Color = Color::Rgb(0x22, 0x30, 0x4C);

// ── Brand ───────────────────────────────────────────────────────
//
// Gold. Reserved for brand, active/running, and progress -- never a
// general-purpose highlight.

/// The brand hue. On dark ground only, or as a fill under an ink label.
pub const GOLD: Color = Color::Rgb(0xFF, 0xDD, 0x00);

/// Pressed / gradient-deep stop, and the leading stop of the progress fill.
pub const GOLD_DEEP: Color = Color::Rgb(0xE0, 0xB8, 0x00);

/// The ONLY warm text tone permitted on a light ground -- 6.12:1 on paper.
pub const GOLD_INK: Color = Color::Rgb(0x7A, 0x5E, 0x00);

// ── Text (dark ground) ──────────────────────────────────────────
//
// Cool neutrals for the dark canvas. `text-tertiary` clears AA body text on
// night and ground only -- see contracts.

/// Primary text.
pub const TEXT_PRIMARY: Color = Color::Rgb(0xED, 0xF1, 0xF8);

/// Secondary text. The safe small-text tone on every dark ground.
pub const TEXT_SECONDARY: Color = Color::Rgb(0x94, 0xA3, 0xBC);

/// Labels and captions. AA body on night/ground; large-text or UI only on
/// surface/raised.
pub const TEXT_TERTIARY: Color = Color::Rgb(0x6B, 0x7C, 0x99);

// ── Status ──────────────────────────────────────────────────────
//
// Always paired with a glyph. Hue alone never carries meaning.

/// Success / done / added.
pub const SUCCESS: Color = Color::Rgb(0x34, 0xD3, 0x99);

/// Warning / needs-input. Orange, not yellow -- yellow is the brand hue.
pub const WARNING: Color = Color::Rgb(0xFF, 0x7A, 0x1A);

/// Error / failed / removed.
pub const DANGER: Color = Color::Rgb(0xFF, 0x5C, 0x7A);

// ── Status (light ground) ───────────────────────────────────────
//
// The same three meanings, darkened along their own hue until they clear AA
// on paper. The dark-ground status tones are light colours -- `success` is
// 1.72:1 on white -- so a light surface needs its own set for the same
// reason gold does.

/// Success on a light ground -- 5.76:1 on paper.
pub const SUCCESS_INK: Color = Color::Rgb(0x16, 0x74, 0x4F);

/// Warning on a light ground -- 5.49:1 on paper. Still orange, not yellow.
pub const WARNING_INK: Color = Color::Rgb(0xB0, 0x4A, 0x00);

/// Error on a light ground -- 5.88:1 on paper.
pub const DANGER_INK: Color = Color::Rgb(0xC8, 0x10, 0x2E);

// ── Ground (light) ──────────────────────────────────────────────
//
// The paper mode. Accent text here is ink, never gold.

/// Light background.
pub const PAPER: Color = Color::Rgb(0xFF, 0xFF, 0xFF);

/// Light surface, one step in from paper.
pub const SNOW: Color = Color::Rgb(0xF4, 0xF4, 0xF5);

/// Primary text on paper. Also the label laid over a gold fill.
pub const INK: Color = Color::Rgb(0x0A, 0x0A, 0x0A);

/// Secondary text on paper.
pub const MUTED: Color = Color::Rgb(0x5B, 0x62, 0x70);

/// Every palette colour, paired with its tokens.json name.
///
/// Lets a test walk the whole palette -- see theme.rs's colour-depth
/// fallback coverage check -- without a hand-maintained second list.
pub const ALL: [(&str, Color); 21] = [
    ("night", NIGHT),
    ("ground", GROUND),
    ("surface", SURFACE),
    ("raised", RAISED),
    ("hairline", HAIRLINE),
    ("gold", GOLD),
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
    ("ink", INK),
    ("muted", MUTED),
];
