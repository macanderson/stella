//! Raw palette values -- the single normative colour source for the terminal.
//! Cut from the brand kit v2.0 ("the nebula") at `docs/brand/` -- read
//! `css/tokens.css` and `brand-guidelines.html` before touching a value here.
//! Nothing here is semantic: for role names
//! (accent, ink, rule, status) see [`crate::theme`], which is the only module
//! that should reference these directly.
//!
//! The identity is **the Nebula on Void**: a two-stop sweep, violet `#8F72FF`
//! into cyan `#1FD8E6`, over the blue-cast near-black `#080B1C`. A terminal
//! paints flat cells and cannot render a gradient, so the sweep survives here
//! as its three stops -- [`BRAND_DEEP`] → [`BRAND`] → [`BRAND_BRIGHT`] -- and
//! the gradient itself appears only where a real paint server exists (the
//! kit's SVGs, the website, the observatory masthead).
//!
//! Gold is no longer the brand. Under the comet kit `BRAND` and `GOLD`
//! deliberately shared a value; under the nebula they deliberately do not.
//! Gold `#FFC857` is the **north point**: rare, load-bearing, roughly one gold
//! moment per view. That demotion is the rebrand, and
//! `theme::tests::palette_law_the_nebula_is_the_brand` pins it.
//!
//! Token names are hue-neutral on purpose (`BRAND`, not `GOLD_ACCENT`): the
//! brand hue has been recoloured before (aurora → gold → sky → green → ember →
//! blue → gold → nebula) and the name must outlive the value. Add a *value*
//! here; name a *role* in `theme`.
//!
//! Mirrored by `docs/brand/css/tokens.css`, `website/src/app/tokens.css` and
//! the observatory's inlined `:root` block; all of them must be edited
//! together. (The observatory's dark block is the closest sibling: same
//! ground ramp, same text ramp, same status trio, same data marks.)

use ratatui::style::Color;

// ── Ground (dark) ───────────────────────────────────────────────
//
// Void: the canvas, not pure black. The kit's dark ground is a deliberately
// blue-cast near-black -- the cast is the point, it is what makes the ground
// read as sky rather than as a dead terminal. Pure black makes an accent
// scream; void lets it glow, and it is what the marketing site and the
// observatory already render. `surface` and `raised` step up for cards and
// popovers; the two hairlines are the seam.

/// Deepest ground -- full-bleed backdrops, the splash, OG art. The
/// observatory's `--void`.
pub const VOID: Color = Color::Rgb(0x05, 0x07, 0x1A);

/// App background. Void `#080B1C` -- THE brand ground, painted as a real frame
/// fill by the deck, so every contrast figure below is measured against it.
pub const GROUND: Color = Color::Rgb(0x08, 0x0B, 0x1C);

/// Card / panel surface, one step above ground (the kit's dark `--surface`).
pub const SURFACE: Color = Color::Rgb(0x10, 0x14, 0x2E);

/// Raised surface -- popovers, selected rows, hovered cells.
pub const RAISED: Color = Color::Rgb(0x18, 0x1D, 0x3D);

/// Seam / rule (the kit's dark `--border`). Deliberately low-contrast on
/// ground (1.40:1): decorative only, never the sole carrier of structure.
pub const HAIRLINE: Color = Color::Rgb(0x23, 0x2A, 0x4D);

/// Seam where a boundary must actually read -- panel edges, focused borders.
/// Still below 3:1 on ground (2.09:1), so it is a *stronger* decoration, not
/// a substitute for a glyph or a gap.
pub const HAIRLINE_STRONG: Color = Color::Rgb(0x3A, 0x44, 0x72);

// ── Brand (dark: the Nebula sweep) ──────────────────────────────
//
// The owned colour, and the one place this palette is a *pair* rather than a
// single hue: the identity is the sweep from violet to cyan, so the terminal
// keeps both ends. Brand marks, the prompt, active/running, focus, selection,
// primary action -- and nothing else: the nebula is the signal, never the
// surface. A brand fill (a pill, a selected tab) always carries INK text --
// paper on violet is 3.10:1 and fails.
//
// Unlike the gold it replaces, the violet needs its bright partner to do real
// work: `BRAND` measures 5.58:1 and `BRAND_BRIGHT` 11.20:1, so anything that
// must survive at one cell (a spinner, a one-cell rule, a dense sparkline)
// reaches for the cyan end rather than the violet one. That is a genuine
// regression against the retired gold's 11.36:1 and is recorded here rather
// than smoothed over.

/// Nebula Violet `#8F72FF` -- the sweep's near stop and the resting accent.
/// 5.58:1 on ground.
///
/// The kit's `#7C5CFF` lifted ~10% in lightness, exactly as the comet kit
/// lifted its gold: the web ground is lit by a page, the terminal ground is
/// not. It may not drift.
pub const BRAND: Color = Color::Rgb(0x8F, 0x72, 0xFF);

/// The bright stop of the brand sweep -- Nebula Cyan `#1FD8E6`, the kit's
/// `cyan-500` and the gradient's far end. 11.20:1 on ground: this is the stop
/// that carries small glyphs, gradients and hover lifts.
pub const BRAND_BRIGHT: Color = Color::Rgb(0x1F, 0xD8, 0xE6);

/// Pressed / gradient-deep stop, and the leading stop of the progress fill
/// (kit `violet-600`). 3.29:1 on ground -- it clears the 3:1 graphical floor
/// and nothing more, so it paints fills and gradient tails, never text.
pub const BRAND_DEEP: Color = Color::Rgb(0x65, 0x44, 0xE6);

// ── Brand (light: violet on paper) ──────────────────────────────
//
// The `stella-light` primary. Violet cannot hold a text edge on paper at full
// strength (3.93:1), so the light accent walks down the kit's own violet ramp
// until it clears AA on [`PAPER`]. Applied by the per-frame theme remap in
// [`crate::theme`], truecolor only.

/// The light-theme brand hue (kit `violet-700`) -- 7.59:1 on [`PAPER`].
pub const BRAND_INK: Color = Color::Rgb(0x51, 0x33, 0xBE);

/// Pressed stop / leading progress stop on paper (kit `violet-800`,
/// 10.26:1).
pub const BRAND_INK_DEEP: Color = Color::Rgb(0x3D, 0x26, 0x94);

// ── Gold: the north point ───────────────────────────────────────
//
// Identity chrome, rationed. Under the comet kit gold WAS the brand and
// [`GOLD`] aliased [`BRAND`]; under the nebula it is the star you steer by --
// the single warm accent in a cool system, and the kit's rule is roughly one
// gold moment per view. It marks the wordmark's asterisk, the effect
// annotation in a code body, and the one thing on a screen that matters most.
//
// Gold NEVER carries a verdict. It sits 0.0° from [`WARNING`] in hue -- they
// are the same family by design -- so a reader must never be asked to tell an
// outcome from chrome by hue alone; status is always glyph-paired
// (`theme::gold_never_carries_a_verdict` enforces this). Unlike under the
// comet kit, gold no longer carries "active" either: that is the nebula's job
// now, which is what makes the ration affordable.

/// The north point `#FFC857` -- the kit's one gold. 12.80:1 on ground.
pub const GOLD: Color = Color::Rgb(0xFF, 0xC8, 0x57);

/// The gold gradient's top (kit `gold-hi`). 15.51:1 on ground.
pub const GOLD_BRIGHT: Color = Color::Rgb(0xFF, 0xE3, 0xA3);

/// Trailing stop of the gold sweep. 7.20:1 on ground -- clears AA body text
/// and the 3:1 graphical floor.
pub const GOLD_DEEP: Color = Color::Rgb(0xC9, 0x93, 0x2F);

/// Gold chrome on a light ground (the kit's `gold-deep`, its one light-ground
/// gold). 5.53:1 on [`PAPER`], so unlike the comet kit's `gold-ink` this tone
/// clears AA for text as well as chrome.
pub const GOLD_INK: Color = Color::Rgb(0x8A, 0x5A, 0x12);

// ── Text (dark ground) ──────────────────────────────────────────
//
// Cool Paper over the kit's cool neutral ramp. The comet kit banned cool
// grays outright; the nebula inverts that law, because a warm gray on a
// blue-cast void reads as a stain. Prose is paper-white: the accent earns its
// meaning by being rare, which only works if the default voice is uncoloured.

/// Primary text. Cool Paper -- the transcript's default voice. 17.32:1 on
/// [`GROUND`].
pub const TEXT_PRIMARY: Color = Color::Rgb(0xEE, 0xF1, 0xFA);

/// Secondary text (the observatory's `--text-2`, the kit's dark
/// `--muted-fg`, the reference's Mist). The safe small-text tone on every
/// dark ground (7.82:1).
pub const TEXT_SECONDARY: Color = Color::Rgb(0x9A, 0xA3, 0xC2);

/// Labels and captions (kit `neutral-500`, the observatory's `--text-3`).
/// AA body on void/ground/surface (5.58:1 / 5.30:1); on [`RAISED`] it drops
/// to 4.71:1 -- still AA, but treat it as the floor.
pub const TEXT_TERTIARY: Color = Color::Rgb(0x7E, 0x88, 0xAB);

// ── Status ──────────────────────────────────────────────────────
//
// Functional, not brand: always paired with a glyph. Hue alone never carries
// meaning -- which matters doubly now that warning shares the gold family
// outright rather than merely neighbouring it.

/// Success / done / added (11.22:1 on ground). Also the settled cost of a
/// finished turn -- money spent is a fact, and a fact reads green.
pub const SUCCESS: Color = Color::Rgb(0x4A, 0xDE, 0x80);

/// Warning / needs-input. Amber (9.58:1) -- the same hue family as the gold
/// north point, which is exactly why the glyph, never the hue, is the status
/// carrier.
pub const WARNING: Color = Color::Rgb(0xF5, 0xA5, 0x24);

/// Error / failed / removed -- the kit's rose (7.16:1 on ground).
pub const DANGER: Color = Color::Rgb(0xFF, 0x6B, 0x8B);

/// The oracle's pre-flip state — the witness surface's `red` token, the one
/// sanctioned red-as-meaning in the deck (D6). A true signal red, kept
/// distinct from [`DANGER`]'s pink-red so "the test is red before the patch"
/// (a healthy, expected state) never shares a value with "something failed".
/// 7.07:1 on [`GROUND`].
pub const ORACLE_RED: Color = Color::Rgb(0xF8, 0x71, 0x71);

// ── Status (light ground) ───────────────────────────────────────
//
// The same three meanings, darkened along their own hue until they clear AA
// on the painted paper ground. The dark-ground status tones are light
// colours -- `success` is well under 2:1 on paper -- so a light surface
// needs its own set for the same reason the brand hue does.

/// Success on a light ground -- 5.41:1 on [`PAPER`].
pub const SUCCESS_INK: Color = Color::Rgb(0x16, 0x74, 0x4F);

/// Warning on a light ground -- amber-brown, 5.86:1 on [`PAPER`].
pub const WARNING_INK: Color = Color::Rgb(0x8A, 0x54, 0x05);

/// Error on a light ground -- 5.25:1 on [`PAPER`].
pub const DANGER_INK: Color = Color::Rgb(0xC8, 0x1E, 0x3E);

/// The oracle's pre-flip red on a light ground — [`ORACLE_RED`] darkened
/// along its own hue until it clears AA on paper (6.06:1 on [`PAPER`]).
pub const ORACLE_RED_INK: Color = Color::Rgb(0xB9, 0x1C, 0x1C);

// ── Ground (light) ──────────────────────────────────────────────
//
// The paper mode: the kit's cool paper, not white. Accent here is
// [`BRAND_INK`], text is [`INK`].

/// Light background -- the kit's `paper-bg`, a cool off-white.
pub const PAPER: Color = Color::Rgb(0xF6, 0xF7, 0xFB);

/// Light surface (the kit's light `--surface`). Lifts *lighter* than paper,
/// as the dark surface lifts lighter than void.
pub const SNOW: Color = Color::Rgb(0xFD, 0xFD, 0xFF);

/// Light raised surface -- popovers, selected rows on paper (kit
/// `neutral-100`).
pub const PAPER_RAISED: Color = Color::Rgb(0xE6, 0xE9, 0xF2);

/// Light seam / rule (the kit's light `--border`) -- the paper counterpart
/// of [`HAIRLINE`].
pub const PAPER_HAIRLINE: Color = Color::Rgb(0xDD, 0xE2, 0xF0);

/// Primary text on paper -- Ink itself, 17.99:1.
///
/// Under the comet kit this was the *same* value as [`GROUND`]: one black
/// served as both the dark ground and the light text. The nebula splits them
/// -- the ground is Void `#080B1C` and the text is Ink `#0B0E1A` -- because a
/// canvas wants more blue than a glyph does.
pub const INK: Color = Color::Rgb(0x0B, 0x0E, 0x1A);

/// Secondary text on paper (the kit's light `--muted-fg`, the reference's
/// Comment) -- 5.12:1.
pub const MUTED: Color = Color::Rgb(0x5C, 0x67, 0x8F);

/// Tertiary text on paper -- the paper counterpart of [`TEXT_TERTIARY`].
/// 4.68:1 on [`PAPER`], so it clears AA body with little to spare; on
/// [`PAPER_RAISED`] (4.28:1) it is a large-text / UI tone only, exactly as
/// [`TEXT_TERTIARY`] is treated on [`RAISED`].
pub const INK_DIM: Color = Color::Rgb(0x64, 0x6E, 0x97);

// ── Data marks ──────────────────────────────────────────────────
//
// The categorical series palette, shared verbatim with the observatory
// (its `--c1..--c4`). Deliberately *not* the brand hue -- a data mark must
// not read as "active" -- and deliberately not a status hue either.
//
// Five semantic hues are already spoken for (brand 252°, its cyan stop 184°,
// success 142°, warning 37°, danger 347°). Excluding ±30° around each leaves
// only three intervals for a four-mark set, so two collisions are arithmetic,
// not oversight, and both are named on the tokens below. Every mark clears
// the *brand* by ≥ 30°, which is the clause the palette law enforces.
//
// The comet kit's amber mark stood down from every plotted surface because it
// sat 0.8° from the gold accent. That stand-down is lifted: the accent is no
// longer gold, so no data mark neighbours it.

/// Categorical 1 -- lime, hue 93° (159° from brand). 12.04:1 on ground.
pub const DATA_1: Color = Color::Rgb(0x8F, 0xE0, 0x4B);
/// Categorical 2 -- orchid, hue 304° (52° from brand). 7.04:1 on ground.
pub const DATA_2: Color = Color::Rgb(0xE8, 0x6B, 0xE0);
/// Categorical 3 -- rose, hue 331° (79° from brand). 5.05:1 on ground;
/// 16° from [`DANGER`], so it never carries an error meaning and never
/// appears without a label or glyph.
pub const DATA_3: Color = Color::Rgb(0xE4, 0x40, 0x8F);
/// Categorical 4 -- azure, hue 208° (44° from brand). 6.60:1 on ground;
/// 24° from [`BRAND_BRIGHT`]'s cyan, so it is kept off any surface where the
/// bright brand stop also appears.
pub const DATA_4: Color = Color::Rgb(0x4B, 0x9B, 0xE2);

/// Every palette colour, paired with its token name.
///
/// Lets a test walk the whole palette -- see theme.rs's
/// `every_dark_palette_value_has_a_fallback` -- without a hand-maintained
/// second list. Names match the kit's token vocabulary at
/// `docs/brand/css/tokens.css`.
pub const ALL: [(&str, Color); 37] = [
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
    ("oracle-red", ORACLE_RED),
    ("oracle-red-ink", ORACLE_RED_INK),
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
