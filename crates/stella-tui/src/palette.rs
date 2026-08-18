//! Raw palette values -- the single normative colour source for the terminal.
//! Cut from the brand kit v3.0 ("the comet") at `docs/brand/` -- read
//! `css/tokens.css` and `brand-guidelines.html` before touching a value here.
//! Nothing here is semantic: for role names
//! (accent, ink, rule, status) see [`crate::theme`], which is the only module
//! that should reference these directly.
//!
//! The identity is **Ion on Obsidian**: one colour, owned. Ion `#00D1F9` --
//! the comet's ion tail, an electric azure-cyan -- is the signal (the mark,
//! the prompt, active/selected, focus) and never the surface; Obsidian
//! `#070B10` is the ground, a cool blue-black, and the text ramp above it is
//! a cool graphite. The v2.0 bronze/warm-neutral system is gone entirely:
//! there is no warm hue left in the chrome, only in the functional statuses
//! and two categorical marks. Brand and identity are one family, so `BRAND`
//! and `IDENTITY` deliberately share values. The paper theme (`stella-light`)
//! swaps the ground for cool paper and the ion for its deep ramp stops.
//!
//! Every ramp is generated in OKLCH -- the ion family at hue 218 and the
//! neutrals at hue 250 -- so the lightness steps are perceptual rather than
//! eyeballed in sRGB, and every contrast figure quoted below is a computed
//! WCAG ratio against the ground named beside it.
//!
//! Token names are hue-neutral on purpose (`BRAND`, not `ION_ACCENT`): the
//! brand hue has been recoloured before (aurora → gold → sky → green → ember →
//! blue → gold → ion) and the name must outlive the value. That rule cost the
//! repository real work this time -- `GOLD*` was a hue in a token name and had
//! to be renamed to `IDENTITY*` across the crate when the hue moved. Add a
//! *value* here; name a *role* in `theme`.
//!
//! Mirrored by `docs/brand/css/tokens.css`, `website/src/app/tokens.css` and
//! the observatory's inlined `:root` block; all of them must be edited
//! together. (The observatory's dark block is the closest sibling: same
//! ground ramp, same text ramp, same status trio, same data marks.)

use ratatui::style::Color;

// ── Ground (dark) ───────────────────────────────────────────────
//
// Obsidian: the terminal, not pure black. The kit's dark ground is a cool
// blue-black -- no warm cast, no brown-greys anywhere on the dark side. Pure
// black makes an accent scream; obsidian lets it speak, and it is what the
// marketing site and the observatory already render. `surface` and `raised`
// step up for cards and popovers; the two hairlines are the seam.

/// Deepest ground -- full-bleed backdrops, the splash, OG art. The
/// observatory's `--void`. Very nearly black (relative luminance 0.0016) so
/// a full-bleed panel reads as a hole rather than as another surface.
pub const VOID: Color = Color::Rgb(0x01, 0x03, 0x06);

/// App background. Obsidian `#070B10` -- THE brand ground, painted as a real
/// frame fill by the deck, so every contrast figure below is measured against
/// it.
pub const GROUND: Color = Color::Rgb(0x07, 0x0B, 0x10);

/// Card / panel surface, one step above ground (the kit's dark `--surface`).
pub const SURFACE: Color = Color::Rgb(0x12, 0x18, 0x1D);

/// Raised surface -- popovers, selected rows, hovered cells.
pub const RAISED: Color = Color::Rgb(0x1F, 0x25, 0x2B);

/// Seam / rule (the kit's dark `--border`). Deliberately low-contrast on
/// ground (1.48:1): decorative only, never the sole carrier of structure.
pub const HAIRLINE: Color = Color::Rgb(0x2A, 0x30, 0x36);

/// Seam where a boundary must actually read -- panel edges, focused borders.
/// Still below 3:1 on ground (1.94:1), so it is a *stronger* decoration, not
/// a substitute for a glyph or a gap.
pub const HAIRLINE_STRONG: Color = Color::Rgb(0x3C, 0x42, 0x48);

// ── Brand (dark: Ion) ───────────────────────────────────────────
//
// The one owned colour. Brand marks, the prompt, active/running, focus,
// selection, primary action -- and nothing else: ion is the signal, never the
// surface. Reserved exactly as the gold before it was, and like that gold it
// needs no separate text tone: `#00D1F9` measures 10.79:1 on ground, so the
// same value is safe on a glyph, a one-cell rule, and a fill. An ion fill (a
// pill, a selected tab) always carries GROUND text -- white on ion is 1.83:1
// and illegible.

/// Ion `#00D1F9` -- the comet's tail, kit `brand-500`. 10.79:1 on ground.
///
/// Unlike the bronze it replaces, this value is **not** held apart from the
/// web kit: `docs/brand/css/tokens.css`'s `--stella-brand` is the same hex.
/// The gold era kept a brighter terminal stop because the kit's bronze could
/// not carry a glyph on ink, and #2592 tracked whether the two should ever
/// mean the same thing. At hue 218 the question answers itself -- one stop
/// clears 10:1 on obsidian *and* holds its edge in a browser -- so the two
/// surfaces now share one number and #2592's divergence is closed rather than
/// re-litigated at a new hue.
pub const BRAND: Color = Color::Rgb(0x00, 0xD1, 0xF9);

/// The bright stop of the brand sweep (kit `brand-300`, the observatory's
/// `--brand-bright`). 13.09:1 on ground. Gradients and hover lifts only --
/// resting chrome stays on [`BRAND`].
pub const BRAND_BRIGHT: Color = Color::Rgb(0x72, 0xE1, 0xFF);

/// Pressed / gradient-deep stop, and the leading stop of the progress fill
/// (kit `brand-600`). 7.65:1 on ground, so even the fill's tail clears AA.
pub const BRAND_DEEP: Color = Color::Rgb(0x00, 0xB0, 0xD2);

// ── Brand (light: ion on paper) ─────────────────────────────────
//
// The `stella-light` primary. Ion cannot hold a text edge on paper at full
// strength (1.61:1), so the light accent walks down the kit's own ion ramp
// until it clears AA on [`PAPER`]. Applied by the per-frame theme remap in
// [`crate::theme`], truecolor only.

/// The light-theme brand hue -- kit `brand-800`, 4.59:1 on [`PAPER`] and
/// 4.93:1 on [`SNOW`], so it carries terminal-cell text on both light
/// surfaces.
///
/// Deliberately one step *below* [`IDENTITY_INK`]: kit `brand-700` measures
/// 3.16:1 on the painted paper ground -- fine for chrome, under the 4.5:1
/// floor for text -- so the ramp splits the same way the bronze one did, one
/// stop for glyphs and one for graphics.
pub const BRAND_INK: Color = Color::Rgb(0x00, 0x77, 0x8F);

/// Pressed stop / leading progress stop on paper (7.77:1 on [`PAPER`]).
pub const BRAND_INK_DEEP: Color = Color::Rgb(0x00, 0x52, 0x63);

// ── Identity ────────────────────────────────────────────────────
//
// Identity chrome: the logo's block cursor, splash rules, section markers.
// Under the comet kit, the identity IS the brand hue, so [`IDENTITY`] and
// [`BRAND`] share a value on purpose -- the split only survives as *names* so
// the identity sweep ([`IDENTITY_DEEP`] → [`IDENTITY_BRIGHT`]) can run wider
// and quieter than the progress fill's.
//
// Identity NEVER carries a verdict. Under the gold system that rule existed
// because the accent sat 4.0° from [`WARNING`] in hue and an outcome could
// have been read off chrome; the nearest status hue is now 35° away
// ([`SUCCESS`]) and the warning amber 146°, so the *confusion* is gone but
// the *rule* is not -- chrome and verdict are
// different jobs, and `theme::identity_never_carries_a_verdict` still proves
// no outcome mapping can return an identity value. Activity is the one status
// identity does carry: active/running IS the accent, by kit rule.
//
// These constants were named `GOLD*` until the v3.0 recolour, which is
// precisely the hazard the module doc warns about: a hue in a token name
// falsifies itself the first time the hue moves.

/// The mark's ion -- the same value as [`BRAND`].
pub const IDENTITY: Color = Color::Rgb(0x00, 0xD1, 0xF9);

/// Headline gradient stop -- the bright end of the identity sweep (== kit
/// `brand-300`, [`BRAND_BRIGHT`]).
pub const IDENTITY_BRIGHT: Color = Color::Rgb(0x72, 0xE1, 0xFF);

/// Trailing stop of the identity sweep (kit `brand-700`). 5.52:1 on ground --
/// clears AA body text and the 3:1 graphical floor, but the *progress* fill
/// leads with the stronger [`BRAND_DEEP`] instead.
pub const IDENTITY_DEEP: Color = Color::Rgb(0x00, 0x94, 0xB1);

/// Identity chrome on a light ground (kit `brand-700` again -- the kit's one
/// light-ground *graphical* ion). 3.16:1 on [`PAPER`]: a graphical tone
/// (splash rules, the identity sweep), while light-ground ion TEXT takes
/// [`BRAND_INK`].
pub const IDENTITY_INK: Color = Color::Rgb(0x00, 0x94, 0xB1);

// ── Text (dark ground) ──────────────────────────────────────────
//
// Cool paper over the kit's cool graphite ramp -- no warm greys. Prose is
// paper-white: the accent earns its meaning by being rare, which only works
// if the default voice is uncoloured.

/// Primary text. Cool Paper -- the transcript's default voice. 16.78:1 on
/// [`GROUND`].
pub const TEXT_PRIMARY: Color = Color::Rgb(0xE9, 0xED, 0xF2);

/// Secondary text (the observatory's `--text-2`, the kit's dark
/// `--muted-fg`). The safe small-text tone on every dark ground (6.85:1).
pub const TEXT_SECONDARY: Color = Color::Rgb(0x92, 0x99, 0xA1);

/// Labels and captions (the observatory's `--text-3`). AA body on
/// void/ground/surface (6.05:1 / 5.96:1 / 5.40:1); on [`RAISED`] it drops to
/// 4.67:1 -- still AA, but treat it as the floor.
pub const TEXT_TERTIARY: Color = Color::Rgb(0x87, 0x8E, 0x96);

// ── Status ──────────────────────────────────────────────────────
//
// Functional, not brand: always paired with a glyph. Hue alone never carries
// meaning. The three status hues are the only warm colours left in the
// system, which is what makes them read as *functional* against an entirely
// cool chrome -- the inverse of the gold era, where a warm accent had to be
// held 4.0° off a warm warning.

/// Success / done / added (12.00:1 on ground). Also the settled cost of a
/// finished turn -- money spent is a fact, and a fact reads green.
pub const SUCCESS: Color = Color::Rgb(0x00, 0xE7, 0x86);

/// Warning / needs-input. Amber (10.40:1) -- 146° from the ion accent, so
/// unlike the gold era nothing in the chrome can be mistaken for it. The
/// glyph is still the status carrier; the hue is now merely unambiguous
/// rather than load-bearing.
pub const WARNING: Color = Color::Rgb(0xF2, 0xB1, 0x00);

/// Error / failed / removed (7.28:1 on ground).
pub const DANGER: Color = Color::Rgb(0xFF, 0x6A, 0x95);

/// The oracle's pre-flip state — the witness surface's `red` token, the one
/// sanctioned red-as-meaning in the deck (D6). A true signal red, kept
/// distinct from [`DANGER`]'s pink-red so "the test is red before the patch"
/// (a healthy, expected state) never shares a value with "something failed".
/// 5.59:1 on [`GROUND`], and 1.30:1 against [`DANGER`] at 25° of hue -- the
/// pair is told apart by hue and by the words around it, never by weight.
pub const ORACLE_RED: Color = Color::Rgb(0xFF, 0x3D, 0x2A);

// ── Status (light ground) ───────────────────────────────────────
//
// The same meanings, darkened along their own hue until they clear AA on the
// painted paper ground. The dark-ground status tones are light colours --
// `success` is 1.45:1 on paper -- so a light surface needs its own set for
// the same reason the brand hue does.

/// Success on a light ground -- 5.12:1 on [`PAPER`].
pub const SUCCESS_INK: Color = Color::Rgb(0x00, 0x75, 0x41);

/// Warning on a light ground -- amber-brown, 5.65:1 on [`PAPER`].
pub const WARNING_INK: Color = Color::Rgb(0x86, 0x54, 0x00);

/// Error on a light ground -- 6.69:1 on [`PAPER`].
pub const DANGER_INK: Color = Color::Rgb(0xA8, 0x00, 0x4C);

/// The oracle's pre-flip red on a light ground — [`ORACLE_RED`] darkened
/// along its own hue until it clears AA on paper (7.06:1 on [`PAPER`]).
pub const ORACLE_RED_INK: Color = Color::Rgb(0xA5, 0x06, 0x00);

// ── Ground (light) ──────────────────────────────────────────────
//
// The paper mode: the kit's cool paper, not white. Accent here is
// [`BRAND_INK`], text is [`INK`].

/// Light background -- the kit's `paper-bg`, a cool off-white.
pub const PAPER: Color = Color::Rgb(0xEE, 0xF1, 0xF5);

/// Light surface (the kit's light `--surface`). Lifts *lighter* than paper,
/// as the dark surface lifts lighter than obsidian.
pub const SNOW: Color = Color::Rgb(0xF7, 0xF9, 0xFC);

/// Light raised surface -- popovers, selected rows on paper (kit
/// `neutral-100`-adjacent).
pub const PAPER_RAISED: Color = Color::Rgb(0xD8, 0xDD, 0xE3);

/// Light seam / rule (the kit's light `--border`) -- the paper counterpart
/// of [`HAIRLINE`].
pub const PAPER_HAIRLINE: Color = Color::Rgb(0xCF, 0xD6, 0xDD);

/// Primary text on paper -- Obsidian itself, 17.41:1. (The same value as
/// [`GROUND`]: the kit's one black serves as both the dark ground and the
/// light text, which is the point of an identity this small.)
pub const INK: Color = Color::Rgb(0x07, 0x0B, 0x10);

/// Secondary text on paper (the kit's light `--muted-fg`) -- 5.04:1.
pub const MUTED: Color = Color::Rgb(0x61, 0x67, 0x6F);

/// Tertiary text on paper -- the paper counterpart of [`TEXT_TERTIARY`].
/// 4.56:1 on [`PAPER`], so it clears AA body; on [`SNOW`] (4.90:1) it holds,
/// and on [`PAPER_RAISED`] (3.78:1) it is a large-text / UI tone only,
/// exactly as [`TEXT_TERTIARY`] is treated on [`RAISED`].
pub const INK_DIM: Color = Color::Rgb(0x67, 0x6E, 0x75);

// ── Data marks ──────────────────────────────────────────────────
//
// The categorical series palette, shared verbatim with the observatory
// (its `--c1..--c4`). Deliberately *not* the brand hue -- a data mark must
// not read as "active" -- and deliberately not a status hue either.
//
// Two laws govern the placement, and hue is measured the way
// `theme::hue_deg` measures it (sRGB/HSV), because that is the guard a future
// edit actually has to satisfy:
//
//   * **≥30° of hue from the accent.** The closest mark is the periwinkle at
//     50°. Under the bronze/gold system `data-1` sat *0.8°* from the accent
//     and measured 1.06:1 against it, which is why it had to be stood down
//     from every plotted surface.
//   * **≥30° between marks**, so two adjacent rows are told apart by hue
//     alone. The minimum in this set is 34° (orchid 278° / magenta 312°).
//
// The four status hues (oracle-red 5°, apricot-adjacent warning 44°, success
// 155°, danger 343°) are *not* excluded zones -- eleven meanings do not fit
// on a wheel with 30° between all of them -- so two marks are near-neighbours
// of a status and are named as such in their own docs. Every one of those is
// a chip or a node that carries a label; a verdict is always glyph-paired.
// That is the same contract the gold set relied on, held at wider distances
// everywhere except the one mark that is still confined to code bodies.

/// Categorical 1 -- apricot, hue 20° (170° from ion). 11.27:1 on ground. The
/// syntax-keyword tone and the first chart series.
///
/// This mark stays **stood down** from chips, graph nodes and agent colours,
/// as it was under the bronze kit -- but for a different reason. It is no
/// longer confusable with the accent (0.8° and 1.06:1 then; 170° now); it is
/// 24° from [`WARNING`] and 1.08:1 against it, so the two are told apart by
/// saturation (pastel against saturated) rather than by weight. Inside a code
/// body, where no verdict is ever painted, that is enough. On a chip beside a
/// status glyph it would not be, so the stand-down is kept and
/// `theme::identity_never_carries_a_verdict`'s sibling assertions still
/// enforce it.
pub const DATA_1: Color = Color::Rgb(0xFF, 0xB2, 0x8C);
/// Categorical 2 -- periwinkle, hue 240° (50° from ion, the closest mark to
/// the accent in the set). 6.20:1 on ground, 4.86:1 on [`RAISED`], and 1.74:1
/// against [`BRAND`] -- so a process chip beside an active one differs in
/// both hue and weight.
pub const DATA_2: Color = Color::Rgb(0x84, 0x84, 0xF5);
/// Categorical 3 -- magenta, hue 312° (122° from ion). 6.24:1 on ground.
/// 31° from [`DANGER`] and 1.17:1 against it, so it never carries an error
/// meaning and never appears without a label or glyph.
pub const DATA_3: Color = Color::Rgb(0xF2, 0x49, 0xD0);
/// Categorical 4 -- jade, hue 118° (72° from ion). 9.97:1 on ground. 37° from
/// [`SUCCESS`]'s 155°, which matters more here than anywhere else in the set:
/// this is the subagent mark, and it sits in the same column as the lead's
/// accent and the run's verdicts.
///
/// It replaces a *teal* that sat at 165° under the bronze kit. Teal was 134°
/// from a gold accent and only 24° from an ion one -- the single value in the
/// old categorical set that a cyan brand made untenable.
pub const DATA_4: Color = Color::Rgb(0x54, 0xD1, 0x4F);
/// Categorical 5 -- citron, hue 78° (112° from ion, 34° from [`WARNING`] and
/// 77° from [`SUCCESS`]). 12.50:1 on ground. The transcript's repository/VCS
/// class.
pub const DATA_5: Color = Color::Rgb(0xA6, 0xE0, 0x1D);
/// Categorical 6 -- orchid, hue 278° (89° from ion, 38° from the periwinkle
/// and 34° from the magenta). 8.00:1 on ground. The transcript's
/// delegation/orchestration class.
pub const DATA_6: Color = Color::Rgb(0xCF, 0x88, 0xF7);

/// Every palette colour, paired with its token name.
///
/// Lets a test walk the whole palette -- see theme.rs's
/// `every_dark_palette_value_has_a_fallback` -- without a hand-maintained
/// second list. Names match the kit's token vocabulary at
/// `docs/brand/css/tokens.css`.
pub const ALL: [(&str, Color); 39] = [
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
    ("identity", IDENTITY),
    ("identity-bright", IDENTITY_BRIGHT),
    ("identity-deep", IDENTITY_DEEP),
    ("identity-ink", IDENTITY_INK),
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
    ("data-5", DATA_5),
    ("data-6", DATA_6),
];
