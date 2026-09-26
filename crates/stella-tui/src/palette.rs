//! Raw palette values -- the single normative colour source for the terminal.
//! Nothing here is semantic: for role names (accent, ink, rule, status) see
//! [`crate::theme`], which is the only module that should reference these
//! directly.
//!
//! ## What is a hex here, and what is not
//!
//! Most of these constants are **not values at all** -- they are the
//! generated tokens under another name:
//!
//! ```text
//! GROUND SURFACE RAISED HAIRLINE  ->  token::BG PANEL HL BORDER
//! HAIRLINE_STRONG                 ->  token::RULE
//! BRAND BRAND_LIVE GOLD GOLD_LIVE ->  token::GOLD GOLD_BRIGHT
//! TEXT_PRIMARY TEXT_EMPHASIS      ->  token::TEXT SILVER_TYPE
//! TEXT_SECONDARY TEXT_TERTIARY    ->  token::SILVER MUTED
//! TEXT_DIM SUCCESS DANGER INK     ->  token::DIM GREEN RED BG
//! WARNING                         ->  token::WARNING
//! PAPER SNOW PAPER_RAISED         ->  token::PAPER_GROUND PAPER PAPER_ROW
//! PAPER_HAIRLINE INK_MUTED        ->  token::PAPER_SEAM INK_MUTED
//! ```
//!
//! They were hand-typed copies of `design/tokens/stella-tokens.json`,
//! byte-identical to it and held there by nothing. That is the shape this
//! repository loses limits to -- a number in two places, with a comment
//! asking the next author to copy carefully. They are re-exports now, so
//! editing the JSON moves the deck and there is no second value to forget.
//! `WARNING` is the newest one. `amber` always held the same `verdict`
//! clamp `SUCCESS`/`DANGER` hold. It just had no `rust` name, so the
//! generator never made it a constant.
//!
//! The rest still carry their own hex, and each is a colour the token system
//! has no home for yet, not an oversight (#4058). The list below is a
//! promise: `spec_palette.rs`'s test
//! `every_literal_survivor_is_named_and_justified` checks it. Every literal
//! `pub const` is named here. A name drops off the list once it moves to a
//! `token::` re-export.
//!
//! <!-- BEGIN literal-survivors -->
//! - **`BRAND_INK`, `BRAND_INK_DEEP`, `GOLD_INK`, `SUCCESS_INK`,
//!   `WARNING_INK`, `DANGER_INK`, `INK_DIM`, `INK_EMPHASIS`.** The paper
//!   theme's own ink tier. The JSON already declares four of these eight
//!   values under other names -- `gold-ink`, `green-ink`, `amber-ink` and
//!   `red-ink` match `BRAND_INK`, `SUCCESS_INK`, `WARNING_INK` and
//!   `DANGER_INK` byte for byte -- but gives none of them a `rust` name:
//!   doing that would claim the deck renders a value it does not. The
//!   other four values have no JSON counterpart at all. Whether the paper
//!   theme should take the JSON's names, and what the rest should become,
//!   is a design call this file cannot make alone.
//! - **`VOID`.** A derived step past the end of the declared dark ramp.
//!   The JSON declares its own `void` at a different hex, on the
//!   `web-dark` surface alone, with no `rust` name either. Whether the two
//!   should become one token, and how a token says "one step past this
//!   ramp" at all, is the same design call.
//! - **The categorical data marks.** `DATA_1`, `DATA_2`, `DATA_3`, `DATA_4`,
//!   `DATA_5`. Each one must sit 30 degrees of OKLCH hue away from every
//!   other mark, and from gold. Every existing `Clamp` option checks one
//!   token alone, so none of them can check a group like this.
//! <!-- END literal-survivors -->
//!
//! Those are values the token system has no home for yet, and they are fine
//! where they are: `scripts/check-tokens.py` only requires a hex to *be* a
//! live token inside its `TOKEN_ONLY` paths, and this crate is not one. What
//! it does require everywhere is that no **retired** hex appears, and this
//! crate now meets that — so it has left the `MIGRATING` ledger, and a v4.0
//! bronze reappearing here fails the gate like anywhere else. #4058 is still
//! open for the rest of the alignment; leaving the ledger is not the same as
//! finishing it.
//!
//! The identity is **Gold on a cool near-black**: one colour, owned. Gold
//! `#D4AF37` is the signal -- the mark, the prompt, active/selected, focus --
//! and never the surface; the ground is a four-step neutral ramp from
//! `#09090B`, and text is a cool neutral ramp above it.
//!
//! **Every ground is neutral with blue one or two points above red.** That is
//! the whole reason the ramp is specified rather than derived: the house
//! system is warm on one axis end to end, which is what lets a single gold
//! read as metal against both the ink and the paper. Measured, the ramp holds
//! OKLCH hue 84.6-106.7 deg at chroma 0.002-0.009 -- a cast small enough to
//! read as neutral and consistent enough that the four steps look like one
//! material. Nothing on the dark side may be cool-dominant.
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
//! **This is the product palette, not the marketing brand kit** — but the two
//! agree by value now, which they did not when this paragraph was first
//! written. It used to say the kit and `website/src/app/tokens.css` were "still
//! on kit v4.0's Bronze Gold on Obsidian", and that was true right up until
//! v5.0 moved them: `docs/brand/css/tokens.css`, the site, the Observatory and
//! this file are one gold, all downstream of
//! `design/tokens/stella-tokens.json`. The sentence survived being false
//! because `crates/stella-tui/` sat in `check-tokens.py`'s `MIGRATING` ledger,
//! so the hex ban skipped this file entirely; it does not any more. The kit
//! still moves as its own pass — its ramp is byte-parity-checked against ~60
//! generated binaries (logo PNGs, `favicon.ico`, PWA icons, wallpapers,
//! spinner GIFs) needing `rsvg-convert` and `ffmpeg` — which is a statement
//! about *cadence*, not about the values diverging.
//!
//! The closest sibling this file *does* track is the
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
use stella_tui_theme::token;

// -- Ground (dark) -----------------------------------------------
//
// Four blacks and nothing else: canvas, code block, highlight row, border.
// Every one is a neutral zinc, carrying no hue. Pure black is deliberately
// absent -- it makes an accent scream, where a near-black lets it speak.

/// Deepest ground -- full-bleed backdrops, the splash, OG art. One step below
/// the canvas on the same neutral ramp, and not pure black.
/// The four specified blacks start at [`GROUND`]; this is the derived fifth,
/// and exists because a full-bleed backdrop behind a canvas needs somewhere
/// to be.
pub const VOID: Color = Color::Rgb(0x04, 0x04, 0x05);

/// App background -- the canvas `#09090B`, painted as a real frame fill by
/// the deck, so every contrast figure below is measured against it.
pub const GROUND: Color = token::BG;

/// Card / panel surface, and the ground a **code block** sits on -- one step
/// above the canvas (1.03:1, a value step rather than a shadow).
pub const SURFACE: Color = token::PANEL;

/// Raised surface -- **highlight rows**, popovers, selected rows, hovered
/// cells. 1.11:1 on the canvas: visible as a band, invisible as a colour.
pub const RAISED: Color = token::HL;

/// Seam / rule -- the **border** value. Deliberately low-contrast on ground
/// (1.31:1): decorative only, never the sole carrier of structure.
pub const HAIRLINE: Color = token::BORDER;

/// Seam where a boundary must actually read -- panel edges, focused borders.
/// The derived fifth step of the ground ramp (1.63:1 on ground), still below
/// the 3:1 graphical floor, so it is a *stronger* decoration, not a
/// substitute for a glyph or a gap.
pub const HAIRLINE_STRONG: Color = token::RULE;

// -- Brand (dark: gold) ------------------------------------------
//
// The one owned colour, in exactly two stops. Brand marks, the prompt,
// active/running, focus, selection, primary action -- and nothing else: gold
// is the signal, never the surface. A gold fill always carries GROUND-dark
// text; white on this gold is 1.35:1 and illegible.

/// Gold `#D4AF37` -- the mark. 9.46:1 on ground, 8.43:1 on surface, 7.81:1 on
/// raised, so the same value is safe on a glyph, a one-cell rule and a fill on
/// every dark ground. OKLCH hue 91.1.
pub const BRAND: Color = token::GOLD;

/// The live stop `#F1CE65` -- **reserved for small things that are moving**:
/// the running spinner, the progress fill's leading edge. 14.22:1 on ground,
/// 0.0 deg from [`BRAND`] in hue, so it reads as the same gold lit up rather
/// than as a second colour. Never a resting mark: chrome that is not moving
/// takes [`BRAND`].
pub const BRAND_LIVE: Color = token::GOLD_BRIGHT;

// -- Brand (light: gold on paper) --------------------------------
//
// The `stella-light` primary. Gold cannot hold a text edge on paper at full
// strength -- `#D4AF37` measures 1.32:1 on [`PAPER`] -- so the light accent
// walks the same hue down until it clears AA. Applied by the per-frame theme
// remap in [`crate::theme`], truecolor only.

/// The light-theme brand hue -- OKLCH hue 91.5 (0.5 deg from [`BRAND`]),
/// 4.88:1 on [`PAPER`], 4.62:1 on [`PAPER_RAISED`]. Gold *text* on paper.
pub const BRAND_INK: Color = Color::Rgb(0x8A, 0x72, 0x23);

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
pub const GOLD: Color = token::GOLD;

/// The live stop of the identity sweep -- the same value as [`BRAND_LIVE`],
/// and under the same reservation: small, and moving.
pub const GOLD_LIVE: Color = token::GOLD_BRIGHT;

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
pub const TEXT_PRIMARY: Color = token::TEXT;

/// The bright neutral, one step under primary -- emphasis *inside* a body of
/// secondary text, which in practice means the token classes in a code body
/// (`theme::SYNTAX_KEYWORD`). 11.04:1 on ground, 9.97:1 on [`RAISED`].
pub const TEXT_EMPHASIS: Color = token::SILVER_TYPE;

/// Secondary text, and the tone context events take. 8.58:1 on ground,
/// 7.75:1 on [`RAISED`] -- the safe small-text tone on every dark ground.
pub const TEXT_SECONDARY: Color = token::SILVER;

/// Labels and captions. 4.79:1 on ground, 4.64:1 on surface, 4.33:1 on
/// raised -- clears the 4.5:1 AA body floor on the first two and sits
/// fractionally under it on the third, which the ratchet never held as a
/// pairing. This is still a UI/large-text tier and a caption tier by role,
/// and anything a reader must actually read at 13px on a raised row takes
/// [`TEXT_SECONDARY`] instead.
pub const TEXT_TERTIARY: Color = token::MUTED;

/// The dim tier -- 3.14:1 on ground, clearing the 3:1 graphical/large-text
/// floor and still under the 4.5:1 body floor. **Chrome only, never words**:
/// the unfilled progress groove and nothing else. It is a real token rather
/// than a fifth ground because it has to stay legible-as-texture against
/// [`HAIRLINE`].
pub const TEXT_DIM: Color = token::DIM;

// -- Status ------------------------------------------------------
//
// Functional, not brand: always paired with a glyph. Both semantic hues are
// pulled cool so they sit inside the scheme instead of fighting it -- success
// at chroma 0.116 and danger at 0.150, against the 0.152 of the gold, so no
// status ever out-saturates the one owned colour.

/// Success / done / added. 9.52:1 on ground, OKLCH hue 153.9 (62.8 deg from
/// the gold accent). Also the settled cost of a finished turn -- money spent
/// is a fact, and a fact reads green.
pub const SUCCESS: Color = token::GREEN;

/// Warning / needs-input. 7.51:1 on ground, OKLCH hue 43.0.
///
/// The one status the palette did not name, and it is derived rather than
/// picked, against [`GOLD`] at hue 91.1 and [`DANGER`] at 11.2. The shipped
/// value lands 48.0 deg from gold and 31.8 deg from danger -- so a reader can
/// tell a warning from the mark *and* from a failure by hue, not just by glyph.
/// The v7.0 gold sits 16 deg further from danger than the gold before it, so
/// the arc between them is 79.9 deg wide and the warning clears 30 deg from
/// both ends with room to spare.
/// It carries the same cool pull as its two neighbours (chroma 0.131).
pub const WARNING: Color = token::WARNING;

/// Error / failed / removed. 5.84:1 on ground, OKLCH hue 11.2 (79.9 deg from
/// the gold accent).
pub const DANGER: Color = token::RED;

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
pub const WARNING_INK: Color = Color::Rgb(0x8D, 0x3B, 0x19);

/// Error on a light ground -- 7.41:1 on [`PAPER`].
pub const DANGER_INK: Color = Color::Rgb(0x95, 0x21, 0x41);

// -- Ground (light) ----------------------------------------------
//
// The paper mode, cooled to match the dark side: the same 285-286 deg
// neutral ramp read from the other end, so switching themes changes the
// lightness and not the temperature. Accent here is [`BRAND_INK`], text is
// [`INK`].

/// Light background -- a warm off-white, not pure white.
pub const PAPER: Color = token::PAPER_GROUND;

/// Light surface. Lifts *lighter* than paper, as the dark surface lifts
/// lighter than the canvas.
pub const SNOW: Color = token::PAPER;

/// Light raised surface -- popovers, selected rows on paper.
pub const PAPER_RAISED: Color = token::PAPER_ROW;

/// Light seam / rule -- the paper counterpart of [`HAIRLINE`].
pub const PAPER_HAIRLINE: Color = token::PAPER_SEAM;

/// Primary text on paper -- 18.01:1. The same value as [`GROUND`]: the
/// canvas black serves as both the dark ground and the light text, which is
/// the point of an identity this small.
pub const INK: Color = token::BG;

/// Secondary text on paper -- 5.83:1, the paper counterpart of
/// [`TEXT_SECONDARY`].
///
/// Named for its ground, like [`INK_DIM`] and [`INK_EMPHASIS`]. It was
/// `MUTED` until #5001, which put it one word away from
/// `stella_tui_theme::token::MUTED` -- the dark ramp's tier below silver, a
/// different colour on a different ground, with nothing at a call site to say
/// which one a line had reached. #4966 removed the third constant of that
/// name; this is the last of them.
pub const INK_MUTED: Color = token::INK_MUTED;

/// Tertiary text on paper -- 3.09:1 on [`PAPER`], the counterpart of
/// [`TEXT_TERTIARY`] and a large-text / UI tone, exactly as its dark twin is
/// on [`RAISED`]. The house kit's quietest ink on paper, written out because
/// this ramp's name for the tier and the dark ramp's name for that value are
/// different words.
pub const INK_DIM: Color = Color::Rgb(0xA1, 0xA1, 0xAA);

/// The bright-neutral tier on paper -- 12.72:1, the counterpart of
/// [`TEXT_EMPHASIS`], and like it a warm neutral (OKLCH hue 88.8). The house
/// kit's body ink on paper, one tier under [`INK`].
pub const INK_EMPHASIS: Color = Color::Rgb(0x27, 0x27, 0x2A);

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
// the recolour: at OKLCH hue 85.2 it sat **10.4 deg** from this gold -- the
// same colour at a glance -- and a categorical mark that can be mistaken for
// "running" is worse than one fewer category. Its one surviving job, the
// syntax-keyword tone inside code bodies, went to [`TEXT_EMPHASIS`], which
// is where the palette puts code tokens anyway.

/// Categorical 1 -- muted violet, OKLCH hue 292.6 (158.5 deg from gold).
/// 5.32:1 on ground.
pub const DATA_1: Color = Color::Rgb(0x8F, 0x70, 0xE8);
/// Categorical 2 -- warm rose, hue 355.6 (95.5 deg from gold). 5.11:1 on
/// ground; 1.14:1 against [`DANGER`], so it never carries an error meaning
/// and never appears without a label or glyph.
pub const DATA_2: Color = Color::Rgb(0xE4, 0x40, 0x8F);
/// Categorical 3 -- deep teal, hue 186.6 (95.6 deg from gold). 10.60:1 on
/// ground.
pub const DATA_3: Color = Color::Rgb(0x2F, 0xD3, 0xC6);
/// Categorical 4 -- citron, hue 126.2 (35.1 deg from gold -- the tightest
/// clearance in the set, and it clears). 11.10:1 on ground. The transcript's
/// repository/VCS class.
pub const DATA_4: Color = Color::Rgb(0xA3, 0xD1, 0x4B);
/// Categorical 5 -- orchid, hue 324.4. 6.74:1 on ground, 126.6 deg from
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
    ("ink-muted", INK_MUTED),
    ("ink-dim", INK_DIM),
    ("ink-emphasis", INK_EMPHASIS),
    ("data-1", DATA_1),
    ("data-2", DATA_2),
    ("data-3", DATA_3),
    ("data-4", DATA_4),
    ("data-5", DATA_5),
];
