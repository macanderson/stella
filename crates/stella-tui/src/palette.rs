//! Raw palette values -- the single normative colour source for the terminal.
//! Nothing here is semantic: for role names (accent, ink, rule, status) see
//! [`crate::theme`], which is the only module that should reference these
//! directly.
//!
//! The identity is **Gold on a cool near-black**: one colour, owned. Gold
//! `#EFC53F` is the signal -- the mark, the prompt, active/selected, focus --
//! and never the surface; the ground is a four-step neutral ramp from
//! `#0A0A0C`, and text is a cool neutral ramp above it.
//!
//! **Every ground is neutral with blue one or two points above red.** That is
//! the whole reason the ramp is specified by hex rather than derived: a
//! neutral that leans even slightly warm reads as *muddy brown* on a cheap
//! panel, and the accent is a yellow, so a warm ground puts the one owned
//! colour and the surface it sits on in the same family. Measured, the ramp
//! holds OKLCH hue 285.6-285.9 deg at chroma 0.004-0.011 -- a cast small
//! enough to read as neutral and consistent enough that the four steps look
//! like one material. Nothing on the dark side may be warm-dominant.
//!
//! The gold is deliberately **two values and no more**: [`GOLD`] for every
//! resting mark, and [`GOLD_LIVE`] for the small things that are *moving
//! right now* -- the running spinner, the progress fill's leading edge. A
//! third gold is what turns an owned colour into a family nobody can read a
//! state from, which is the failure this ramp replaced (see the retired-hue
//! block in `theme::tests`: the previous palette carried four golds plus a
//! near-identical amber data mark 5.5 deg away from the accent).
//!
//! Token names are hue-neutral on purpose (`BRAND`, not `GOLD_ACCENT`): the
//! brand hue has been recoloured before (aurora -> gold -> sky -> green ->
//! ember -> blue -> gold) and the name must outlive the value. Add a *value*
//! here; name a *role* in `theme`.
//!
//! **This is the product palette, not the marketing brand kit.** It is held
//! apart from `docs/brand/css/tokens.css` and `website/src/app/tokens.css`,
//! which are still on kit v4.0's Bronze Gold `#C58A32` on Obsidian `#070B10`:
//! the kit's ramp is byte-parity-checked against ~60 generated binary assets
//! (logo PNGs, `favicon.ico`, PWA icons, wallpapers, spinner GIFs) that
//! cannot be regenerated without `rsvg-convert` and `ffmpeg`, so moving it is
//! a dedicated pass. The closest sibling this file *does* track is the
//! instrument palette shared by the Observatory
//! (`crates/stella-observatory/src/assets/index.html`), the `/export`
//! dashboard and the two arenabench UIs -- same grounds, same text ramp, same
//! semantic pair -- enforced by
//! `crates/stella-cli/tests/design_token_parity.rs`.
//!
//! Every contrast figure below is WCAG (a luminance ratio) against
//! [`GROUND`] unless it names another ground, and every hue figure is OKLCH.
//! Both are computed, not estimated.

use ratatui::style::Color;

// -- Ground (dark) -----------------------------------------------
//
// Four blacks and nothing else: canvas, code block, highlight row, border.
// Every one is neutral with blue one or two points above red (see the module
// doc). Pure black is deliberately absent -- it makes an accent scream, where
// a near-black lets it speak.

/// Deepest ground -- full-bleed backdrops, the splash, OG art. One step below
/// the canvas on the same ramp (OKLCH hue 285.4, blue two points above red).
/// The four specified blacks start at [`GROUND`]; this is the derived fifth,
/// and exists because a full-bleed backdrop behind a canvas needs somewhere
/// to be.
pub const VOID: Color = Color::Rgb(0x05, 0x05, 0x07);

/// App background -- the canvas `#0A0A0C`, painted as a real frame fill by
/// the deck, so every contrast figure below is measured against it.
pub const GROUND: Color = Color::Rgb(0x0A, 0x0A, 0x0C);

/// Card / panel surface, and the ground a **code block** sits on -- one step
/// above the canvas (1.03:1, a value step rather than a shadow).
pub const SURFACE: Color = Color::Rgb(0x0F, 0x0F, 0x12);

/// Raised surface -- **highlight rows**, popovers, selected rows, hovered
/// cells. 1.11:1 on the canvas: visible as a band, invisible as a colour.
pub const RAISED: Color = Color::Rgb(0x17, 0x17, 0x1B);

/// Seam / rule -- the **border** value. Deliberately low-contrast on ground
/// (1.31:1): decorative only, never the sole carrier of structure.
pub const HAIRLINE: Color = Color::Rgb(0x26, 0x26, 0x2C);

/// Seam where a boundary must actually read -- panel edges, focused borders.
/// The derived fifth step of the ground ramp (1.63:1 on ground), still below
/// the 3:1 graphical floor, so it is a *stronger* decoration, not a
/// substitute for a glyph or a gap.
pub const HAIRLINE_STRONG: Color = Color::Rgb(0x35, 0x35, 0x3D);

// -- Brand (dark: gold) ------------------------------------------
//
// The one owned colour, in exactly two stops. Brand marks, the prompt,
// active/running, focus, selection, primary action -- and nothing else: gold
// is the signal, never the surface. A gold fill always carries GROUND-dark
// text; white on this gold is 1.35:1 and illegible.

/// Gold `#EFC53F` -- the mark. 11.99:1 on ground, 11.60:1 on surface,
/// 10.83:1 on raised, so the same value is safe on a glyph, a one-cell rule
/// and a fill on every dark ground. OKLCH hue 90.8.
pub const BRAND: Color = Color::Rgb(0xEF, 0xC5, 0x3F);

/// The live stop `#F7D96B` -- **reserved for small things that are moving**:
/// the running spinner, the progress fill's leading edge. 14.22:1 on ground,
/// 3.5 deg from [`BRAND`] in hue, so it reads as the same gold lit up rather
/// than as a second colour. Never a resting mark: chrome that is not moving
/// takes [`BRAND`].
pub const BRAND_LIVE: Color = Color::Rgb(0xF7, 0xD9, 0x6B);

// -- Brand (light: gold on paper) --------------------------------
//
// The `stella-light` primary. Gold cannot hold a text edge on paper at full
// strength -- `#EFC53F` measures 1.32:1 on [`PAPER`] -- so the light accent
// walks the same hue down until it clears AA. Applied by the per-frame theme
// remap in [`crate::theme`], truecolor only.

/// The light-theme brand hue -- OKLCH hue 90.6 (0.2 deg from [`BRAND`]),
/// 6.02:1 on [`PAPER`], 5.31:1 on [`PAPER_RAISED`]. Gold *text* on paper.
pub const BRAND_INK: Color = Color::Rgb(0x72, 0x5A, 0x00);

/// Pressed stop / trailing progress stop on paper -- 9.60:1 on [`PAPER`],
/// so even the fill's tail clears AA.
pub const BRAND_INK_DEEP: Color = Color::Rgb(0x4E, 0x3D, 0x00);

// -- Gold --------------------------------------------------------
//
// Identity chrome: the logo's block cursor, splash rules, section markers.
// Chrome and accent are one colour, so [`GOLD`] and [`BRAND`] share a value
// on purpose -- the split survives only as *names*, so a call site can say
// which job it is doing.
//
// Gold NEVER carries a verdict. [`WARNING`] sits 39.1 deg away in hue, which
// is far enough to tell apart, and the rule holds anyway: status is always
// glyph-paired (`theme::gold_never_carries_a_verdict` enforces this).
// Activity is the one status gold does carry: active/running IS the accent.

/// The mark's gold -- the same value as [`BRAND`].
pub const GOLD: Color = Color::Rgb(0xEF, 0xC5, 0x3F);

/// The live stop of the identity sweep -- the same value as [`BRAND_LIVE`],
/// and under the same reservation: small, and moving.
pub const GOLD_LIVE: Color = Color::Rgb(0xF7, 0xD9, 0x6B);

/// Gold chrome on a light ground. 3.37:1 on [`PAPER`]: a *graphical* tone
/// (splash rules, the identity sweep) that clears the 3:1 floor, while
/// light-ground gold TEXT takes [`BRAND_INK`].
pub const GOLD_INK: Color = Color::Rgb(0xA2, 0x81, 0x00);

// -- Text (dark ground) ------------------------------------------
//
// Four cool neutrals, on the same 285-286 deg ramp as the grounds. Prose is
// the top tier and uncoloured: the accent earns its meaning by being rare,
// which only works if the default voice carries no hue at all.

/// Primary text -- the transcript's default voice. 16.19:1 on [`GROUND`].
pub const TEXT_PRIMARY: Color = Color::Rgb(0xE8, 0xE8, 0xEC);

/// The bright neutral, one step under primary -- emphasis *inside* a body of
/// secondary text, which in practice means the token classes in a code body
/// (`theme::SYNTAX_KEYWORD`). 11.04:1 on ground, 9.97:1 on [`RAISED`].
pub const TEXT_EMPHASIS: Color = Color::Rgb(0xBF, 0xC1, 0xCC);

/// Secondary text, and the tone context events take. 8.58:1 on ground,
/// 7.75:1 on [`RAISED`] -- the safe small-text tone on every dark ground.
pub const TEXT_SECONDARY: Color = Color::Rgb(0xA9, 0xAA, 0xB5);

/// Labels and captions. 4.47:1 on ground, 4.32:1 on surface, 4.04:1 on
/// raised -- **just under the 4.5:1 AA body floor on all three**, which is
/// stated rather than rounded away: this is a UI/large-text tier and a
/// caption tier, and anything a reader must actually read at 13px takes
/// [`TEXT_SECONDARY`] instead.
pub const TEXT_TERTIARY: Color = Color::Rgb(0x77, 0x77, 0x82);

/// The dim tier -- 2.30:1 on ground, below every text floor and below the
/// 3:1 graphical floor too. **Chrome only, never words**: the unfilled
/// progress groove and nothing else. It is a real token rather than a fifth
/// ground because it has to stay legible-as-texture against [`HAIRLINE`].
pub const TEXT_DIM: Color = Color::Rgb(0x4B, 0x4B, 0x56);

// -- Status ------------------------------------------------------
//
// Functional, not brand: always paired with a glyph. Both semantic hues are
// pulled cool so they sit inside the scheme instead of fighting it -- success
// at chroma 0.116 and danger at 0.150, against the 0.152 of the gold, so no
// status ever out-saturates the one owned colour.

/// Success / done / added. 9.90:1 on ground, OKLCH hue 153.9 (63.1 deg from
/// the gold accent). Also the settled cost of a finished turn -- money spent
/// is a fact, and a fact reads green.
pub const SUCCESS: Color = Color::Rgb(0x74, 0xC9, 0x91);

/// Warning / needs-input. 7.85:1 on ground, OKLCH hue 51.7.
///
/// The one status the palette did not name, and it is derived rather than
/// picked: [`GOLD`] sits at hue 90.8 and [`DANGER`] at 12.8, and 51.8 is the
/// point that **maximises the smaller of the two gaps**. The shipped value
/// lands 39.1 deg from gold and 38.9 deg from danger -- so a reader can tell
/// a warning from the mark *and* from a failure by hue, not just by glyph.
/// It carries the same cool pull as its two neighbours (chroma 0.131).
pub const WARNING: Color = Color::Rgb(0xE7, 0x8D, 0x54);

/// Error / failed / removed. 6.06:1 on ground, OKLCH hue 12.8 (78.0 deg from
/// the gold accent).
pub const DANGER: Color = Color::Rgb(0xE0, 0x68, 0x7A);

// -- Status (light ground) ---------------------------------------
//
// The same three meanings, darkened along their own hue until they clear AA
// on paper. The dark-ground status tones are light colours -- success is
// 1.53:1 on paper -- so a light surface needs its own set for the same
// reason the brand hue does. Each holds its dark twin's hue to within 2 deg.

/// Success on a light ground -- 6.23:1 on [`PAPER`], 5.31:1 on
/// [`PAPER_RAISED`].
pub const SUCCESS_INK: Color = Color::Rgb(0x00, 0x69, 0x33);

/// Warning on a light ground -- 6.85:1 on [`PAPER`].
pub const WARNING_INK: Color = Color::Rgb(0x8A, 0x3F, 0x00);

/// Error on a light ground -- 7.41:1 on [`PAPER`].
pub const DANGER_INK: Color = Color::Rgb(0x96, 0x21, 0x3C);

// -- Ground (light) ----------------------------------------------
//
// The paper mode, cooled to match the dark side: the same 285-286 deg
// neutral ramp read from the other end, so switching themes changes the
// lightness and not the temperature. Accent here is [`BRAND_INK`], text is
// [`INK`].

/// Light background -- a cool off-white, not pure white.
pub const PAPER: Color = Color::Rgb(0xF4, 0xF4, 0xF6);

/// Light surface. Lifts *lighter* than paper, as the dark surface lifts
/// lighter than the canvas.
pub const SNOW: Color = Color::Rgb(0xFA, 0xFA, 0xFC);

/// Light raised surface -- popovers, selected rows on paper.
pub const PAPER_RAISED: Color = Color::Rgb(0xE6, 0xE6, 0xEA);

/// Light seam / rule -- the paper counterpart of [`HAIRLINE`].
pub const PAPER_HAIRLINE: Color = Color::Rgb(0xDD, 0xDD, 0xE3);

/// Primary text on paper -- 18.01:1. The same value as [`GROUND`]: the
/// canvas black serves as both the dark ground and the light text, which is
/// the point of an identity this small.
pub const INK: Color = Color::Rgb(0x0A, 0x0A, 0x0C);

/// Secondary text on paper -- 5.83:1, the paper counterpart of
/// [`TEXT_SECONDARY`].
pub const MUTED: Color = Color::Rgb(0x5E, 0x5E, 0x69);

/// Tertiary text on paper -- 4.72:1 on [`PAPER`], the counterpart of
/// [`TEXT_TERTIARY`]. On [`PAPER_RAISED`] it drops to 4.16:1 and is a
/// large-text / UI tone there, exactly as its dark twin is on [`RAISED`].
pub const INK_DIM: Color = Color::Rgb(0x6C, 0x6C, 0x78);

/// The bright-neutral tier on paper -- 9.92:1, the counterpart of
/// [`TEXT_EMPHASIS`], and like it a cool neutral (OKLCH hue 285.5).
pub const INK_EMPHASIS: Color = Color::Rgb(0x3C, 0x3C, 0x46);

// -- Data marks --------------------------------------------------
//
// The categorical series palette: the hues the deck needs when it must show
// that two things are *different kinds*, which a four-neutral ramp plus one
// accent cannot express. Deliberately not the brand hue -- a data mark must
// not read as "active" -- and deliberately not a status hue either.
//
// Every one clears **30 deg of OKLCH hue from [`GOLD`]** (the floor for two
// hues to be told apart in a single terminal cell) and AA body on
// [`GROUND`]. The amber mark that used to open this series was retired with
// the recolour: at OKLCH hue 85.2 it sat **5.5 deg** from this gold -- the
// same colour at a glance -- and a categorical mark that can be mistaken for
// "running" is worse than one fewer category. Its one surviving job, the
// syntax-keyword tone inside code bodies, went to [`TEXT_EMPHASIS`], which
// is where the palette puts code tokens anyway.

/// Categorical 1 -- muted violet, OKLCH hue 292.6 (158.2 deg from gold).
/// 5.32:1 on ground.
pub const DATA_1: Color = Color::Rgb(0x8F, 0x70, 0xE8);
/// Categorical 2 -- warm rose, hue 355.6 (95.2 deg from gold). 5.11:1 on
/// ground; 1.14:1 against [`DANGER`], so it never carries an error meaning
/// and never appears without a label or glyph.
pub const DATA_2: Color = Color::Rgb(0xE4, 0x40, 0x8F);
/// Categorical 3 -- deep teal, hue 186.6 (95.9 deg from gold). 10.60:1 on
/// ground.
pub const DATA_3: Color = Color::Rgb(0x2F, 0xD3, 0xC6);
/// Categorical 4 -- citron, hue 126.2 (35.4 deg from gold -- the tightest
/// clearance in the set, and it clears). 11.10:1 on ground. The transcript's
/// repository/VCS class.
pub const DATA_4: Color = Color::Rgb(0xA3, 0xD1, 0x4B);
/// Categorical 5 -- orchid, hue 324.4. 6.74:1 on ground, 126.3 deg from
/// gold, and the tightest pair in the whole set: 31.8 deg from the violet
/// and 31.1 deg from the rose. It sits at the point that maximises the
/// smaller of those two gaps, because it has to -- at its previous value it
/// was 28.2 deg from the violet, which is under the 30 deg floor the tool-class
/// law asserts. Nothing caught that until the hue metric moved to OKLCH; the
/// sRGB hue it was measured in read the same pair as 34.6 deg. The transcript's
/// delegation class.
pub const DATA_5: Color = Color::Rgb(0xD8, 0x6C, 0xE1);

/// Every palette colour, paired with its token name.
///
/// Lets a test walk the whole palette -- see theme.rs's
/// `every_dark_palette_value_has_a_fallback` -- without a hand-maintained
/// second list.
pub const ALL: [(&str, Color); 37] = [
    ("void", VOID),
    ("ground", GROUND),
    ("surface", SURFACE),
    ("raised", RAISED),
    ("hairline", HAIRLINE),
    ("hairline-strong", HAIRLINE_STRONG),
    ("brand", BRAND),
    ("brand-live", BRAND_LIVE),
    ("brand-ink", BRAND_INK),
    ("brand-ink-deep", BRAND_INK_DEEP),
    ("gold", GOLD),
    ("gold-live", GOLD_LIVE),
    ("gold-ink", GOLD_INK),
    ("text-primary", TEXT_PRIMARY),
    ("text-emphasis", TEXT_EMPHASIS),
    ("text-secondary", TEXT_SECONDARY),
    ("text-tertiary", TEXT_TERTIARY),
    ("text-dim", TEXT_DIM),
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
    ("ink-emphasis", INK_EMPHASIS),
    ("data-1", DATA_1),
    ("data-2", DATA_2),
    ("data-3", DATA_3),
    ("data-4", DATA_4),
    ("data-5", DATA_5),
];
