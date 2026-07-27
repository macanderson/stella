//! Raw palette values -- the single normative colour source, shared with
//! the docs site and the observatory. Nothing here is semantic: for role
//! names (accent, ink, rule, status) see [`crate::theme`], which is the
//! only module that should be referencing these directly.
//!
//! The identity is electric blue on jet black: a live wire in the dark.
//!
//! Mirrored by `website/src/app/tokens.css` (`--stella-*`); the two must be
//! edited together.

use ratatui::style::Color;

// ── Ground (dark) ───────────────────────────────────────────────
//
// Jet black, not a tinted navy. The accent is a vivid, high-chroma blue, and
// a blue-tinted ground robs it of the contrast that makes it read as a signal
// rather than as decoration -- so the canvas is neutral and the only colour on
// screen is colour that means something. `surface` and `raised` step up for
// cards and popovers as neutral greys, never as navy.

/// Deepest ground -- full-bleed backdrops, the splash, OG art.
pub const NIGHT: Color = Color::Rgb(0x00, 0x00, 0x00);

/// App background. The default dark canvas.
pub const GROUND: Color = Color::Rgb(0x00, 0x00, 0x00);

/// Card / panel surface, one step above ground.
pub const SURFACE: Color = Color::Rgb(0x0A, 0x0A, 0x0B);

/// Raised surface -- popovers, selected rows, hovered cells.
pub const RAISED: Color = Color::Rgb(0x15, 0x15, 0x19);

/// Seam / rule. Deliberately low-contrast on ground: decorative only, never
/// the sole carrier of structure.
pub const HAIRLINE: Color = Color::Rgb(0x26, 0x26, 0x2E);

// ── Brand ───────────────────────────────────────────────────────
//
// Electric blue. Reserved for brand, active/running, and progress -- never a
// general-purpose highlight. In the transcript this means exactly one thing
// carries it: the name of the tool being called.

/// The brand hue. On dark ground only, or as a fill under an ink label.
pub const ELECTRIC: Color = Color::Rgb(0x00, 0xAA, 0xFF);

/// Pressed / gradient-deep stop, and the leading stop of the progress fill.
pub const ELECTRIC_DEEP: Color = Color::Rgb(0x00, 0x66, 0xFF);

/// The ONLY brand text tone permitted on a light ground -- 10.0:1 on paper.
pub const ELECTRIC_INK: Color = Color::Rgb(0x0A, 0x3D, 0x91);

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
/// spent is a fact, and a fact reads green.
pub const SUCCESS: Color = Color::Rgb(0x4A, 0xDE, 0x80);

/// Warning / needs-input. Orange.
pub const WARNING: Color = Color::Rgb(0xFF, 0x8A, 0x1F);

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

/// Warning on a light ground -- 5.49:1 on paper. Still orange.
pub const WARNING_INK: Color = Color::Rgb(0xB0, 0x4A, 0x00);

/// Error on a light ground -- 5.88:1 on paper.
pub const DANGER_INK: Color = Color::Rgb(0xC8, 0x10, 0x2E);

// ── Ground (light) ──────────────────────────────────────────────
//
// The paper mode. Accent text here is ink, never the bright electric.

/// Light background.
pub const PAPER: Color = Color::Rgb(0xFF, 0xFF, 0xFF);

/// Light surface, one step in from paper.
pub const SNOW: Color = Color::Rgb(0xF4, 0xF4, 0xF5);

/// Primary text on paper. Also the label laid over an electric fill.
pub const INK: Color = Color::Rgb(0x0A, 0x0A, 0x0A);

/// Secondary text on paper.
pub const MUTED: Color = Color::Rgb(0x5B, 0x62, 0x70);

/// Every palette colour, paired with its token name.
///
/// Lets a test walk the whole palette -- see theme.rs's colour-depth
/// fallback coverage check -- without a hand-maintained second list.
pub const ALL: [(&str, Color); 21] = [
    ("night", NIGHT),
    ("ground", GROUND),
    ("surface", SURFACE),
    ("raised", RAISED),
    ("hairline", HAIRLINE),
    ("electric", ELECTRIC),
    ("electric-deep", ELECTRIC_DEEP),
    ("electric-ink", ELECTRIC_INK),
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
