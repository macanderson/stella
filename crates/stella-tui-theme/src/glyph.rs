//! The glyph vocabulary — SPEC 4, one set across every panel including
//! plugin-drawn UI.
//!
//! Glyphs exist so colour is never the only carrier of state (SPEC 2, SPEC
//! 13). They are the only carrier under `NO_COLOR`, on a 16-color
//! terminal where the metals collapse toward each other, and for red/green
//! colour blindness — which is why they live beside the palette rather than
//! inside whichever widget first needed one.
//!
//! ## Cell width
//!
//! Every glyph here is one terminal cell wide; [`width`] is the single
//! source of truth so a call site never has to measure one by hand. [`WRITE`]
//! used to be the one exception — U+FF0B FULLWIDTH PLUS SIGN, budgeted as
//! two cells — until #4482: on macOS it fell through to PingFang SC, a
//! proportional CJK face, into the one cell the layout budgeted as fullwidth,
//! which is a visible hazard rather than a design a font can honour. It is
//! now the native ASCII `+` (U+002B), one cell everywhere, and nothing here
//! is fullwidth any more.
//!
//! Nine glyphs still carry East Asian Width `A` (**ambiguous**) rather than
//! `N`, so a terminal configured for CJK double-width ambiguity draws them in
//! two cells: [`RUNNING`], [`QUEUED`], [`GATE`], [`MEMORY`], [`NODE_FILE`],
//! [`TOOL_EXECUTE`], [`EVENT`], [`COMPACTED`], and `BLOCK_EIGHTHS[8]`.
//! [`width`] answers one for all of them, because that is what every
//! non-CJK configuration draws and what the layout budgets.
//!
//! The hazard predates the tool-class rows below: six of the nine shipped
//! before them. It is not guarded, because nothing here knows the terminal's
//! ambiguous-width setting — a real fix means asking the terminal, not
//! asserting a number.

/// Done, pass. Green.
pub const DONE: char = '✓';

/// Running. Gold-bright when stella is acting, silver when it is observing
/// (SPEC 4) — the metal is the caller's to choose; the glyph is not.
pub const RUNNING: char = '◐';

/// The spinner cycle for [`RUNNING`].
pub const SPINNER: [char; 4] = ['◐', '◓', '◑', '◒'];

/// Queued, pending. Dim.
pub const QUEUED: char = '○';

/// A gate — deterministic, merge-blocking. Gold.
pub const GATE: char = '◇';

/// Failed, delete. Red, and only ever red.
pub const FAILED: char = '✗';

/// Drift, plan revision. Gold-bright.
///
/// U+2325 OPTION KEY — a line that leaves its course and rejoins it, which is
/// what a plan revision is. Native to JetBrains Mono (SPEC 3.4) and width `N`.
///
/// It replaces U+2442 OCR FORK, whose shape was the better literal fork and
/// whose coverage was the worst in the whole vocabulary: absent from the brand
/// font, resolving on macOS to the proportional Apple Symbols — the exact
/// bucket that got U+2317 rejected in #4125 — and to tofu on a bare Linux
/// terminal. SPEC 4 answered that with a stated substitute, `↯` U+21AF, and
/// **the brand font lacks that too**, so on the one font the design names the
/// remedy was as absent as the thing it remedied (#4318). A fallback that is
/// missing wherever its subject is missing is not a fallback, and nothing
/// here selects between two glyphs anyway — this crate reads no font. So the
/// pair collapses to one character the brand font ships, and `DRIFT_FALLBACK`
/// is gone rather than repointed.
pub const DRIFT: char = '⌥';

/// Collapsed, expandable. Takes the metal of the event it heads.
pub const COLLAPSED: char = '▸';

/// Write (new file). Green sign on a gold rail (SPEC 4).
///
/// One cell, like everything else here. Was U+FF0B FULLWIDTH PLUS SIGN,
/// budgeted as two; #4482 replaced it with the native ASCII `+` after the
/// fullwidth form was found resolving to a proportional CJK face on macOS.
/// See the module doc.
pub const WRITE: char = '+';

/// Memory. Silver.
pub const MEMORY: char = '◆';

/// Skill. Silver.
///
/// Note that this is the *skill* glyph and nothing else. The retired
/// `✦ stella` wordmark used the same character, which is why SPEC 3.3 is
/// explicit that the wordmark is now `stella*` — see [`crate::wordmark`].
pub const SKILL: char = '✦';

/// Graph node: file. Silver.
pub const NODE_FILE: char = '▤';

/// Graph node: type. Silver.
pub const NODE_TYPE: char = '▢';

/// Graph node: function. Silver.
pub const NODE_FN: char = 'ƒ';

// ── Tool class (SPEC 4) ─────────────────────────────────────────────────────
//
// Four glyphs for the four classes a tool call falls into, drawn on the head
// of a call this renderer has no verb of its own for. They take the metal of
// the row they head and add none of their own: SPEC 2 spends its two metals on
// *kind*, and re-adding *class* as a third hue would erode the rule the whole
// scheme rests on. Shape carries class instead, which is what SPEC 2's "never
// colour alone" already points at (#4125).
//
// All four are drawn from the circle and arrow families rather than invented,
// and each was checked against the shipped `cmap` of the brand font before it
// was chosen — see the per-glyph notes. That check is why `TOOL_EXECUTE` is
// U+2299 and not U+2317 VIEWDATA SQUARE, which the design first named.

/// Tool class: a look — nothing changed. Outline, unfilled.
///
/// U+25CC DOTTED CIRCLE, native to JetBrains Mono (SPEC 3.4).
pub const TOOL_INSPECT: char = '◌';

/// Tool class: written — filled and marked.
///
/// U+25C9 FISHEYE, native to JetBrains Mono (SPEC 3.4).
pub const TOOL_MUTATE: char = '◉';

/// Tool class: external, opaque — a call this table cannot vouch for.
///
/// U+2299 CIRCLED DOT OPERATOR. The design's first choice was U+2317 VIEWDATA
/// SQUARE, which reads better for "external" and is width `N` rather than this
/// character's ambiguous `A`. It was rejected on coverage: U+2317 is absent
/// from the `cmap` of *every* monospace face checked — JetBrains Mono, DejaVu
/// Sans Mono, Fira Code, Cascadia Mono, Hack, Ubuntu Mono and Menlo — so it
/// resolves through CoreText to the proportional Apple Symbols on macOS and
/// has nothing to resolve to on a bare Linux terminal. A glyph whose entire
/// job is to be recognised may not ship as a tofu box (SPEC 2, cell-grid
/// honest). This character is present in JetBrains Mono itself.
pub const TOOL_EXECUTE: char = '⊙';

/// Tool class: handed to another agent.
///
/// U+21B3 DOWNWARDS ARROW WITH TIP RIGHTWARDS — width `N`, and the one glyph
/// here whose shape states the relation rather than naming it. Absent from
/// JetBrains Mono, so it resolves to Menlo, which is exactly what the shipped
/// [`RUNNING`] spinner already does and therefore no new hazard. The
/// circle-family alternative (U+25D1) was declined because it is
/// [`SPINNER`]`[2]`: a delegation would be indistinguishable from a call
/// caught mid-spin.
pub const TOOL_DELEGATE: char = '↳';

/// One thing happened here — the transcript's default head, for a row whose
/// verb the renderer knows and which has no glyph of its own: `● edit <path>`,
/// `● run <cmd>`, and an expanded `read`.
///
/// U+25CF BLACK CIRCLE, present in JetBrains Mono. Width `A` — see the module
/// doc. The most-drawn glyph in the deck, and until #4320 the only one drawn
/// as a bare literal, outside this vocabulary and so outside every test over
/// it.
pub const EVENT: char = '●';

/// A compaction — history replaced by a summary. Dim, and the row carries no
/// rail (SPEC 6.2).
///
/// U+2193 DOWNWARDS ARROW, present in JetBrains Mono. Width `A` — see the
/// module doc.
pub const COMPACTED: char = '↓';

/// The eighth-block ramp, empty through full — the only sub-cell precision
/// this design allows itself (SPEC 2: cell-grid honest).
///
/// Indexed by eighths, so `BLOCK_EIGHTHS[3]` is three eighths of a cell.
pub const BLOCK_EIGHTHS: [char; 9] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];

/// The unfilled track of a meter — a texture rather than a solid, so the fill
/// reads as fill even where the two colours are close (SPEC 5 fixes the
/// colours: gold on `border` gray; the renderings fix the glyph).
pub const METER_TRACK: char = '░';

/// Every glyph in the vocabulary, paired with its name.
///
/// The tests walk this instead of a second list, so a glyph added without a
/// stated width is caught rather than assumed. Includes every frame of
/// [`SPINNER`] and every fill of [`BLOCK_EIGHTHS`] except the empty one
/// (`' '` is a cell state, not a glyph) — #4482 folded both in after a walk
/// over `ALL` alone was found to inherit their blind spot silently.
pub const ALL: [(&str, char); 31] = [
    ("done", DONE),
    ("running", RUNNING),
    ("spinner_2", SPINNER[1]),
    ("spinner_3", SPINNER[2]),
    ("spinner_4", SPINNER[3]),
    ("queued", QUEUED),
    ("gate", GATE),
    ("failed", FAILED),
    ("drift", DRIFT),
    ("collapsed", COLLAPSED),
    ("write", WRITE),
    ("memory", MEMORY),
    ("skill", SKILL),
    ("node_file", NODE_FILE),
    ("node_type", NODE_TYPE),
    ("node_fn", NODE_FN),
    ("tool_inspect", TOOL_INSPECT),
    ("tool_mutate", TOOL_MUTATE),
    ("tool_execute", TOOL_EXECUTE),
    ("tool_delegate", TOOL_DELEGATE),
    ("event", EVENT),
    ("compacted", COMPACTED),
    ("eighth_1", BLOCK_EIGHTHS[1]),
    ("eighth_2", BLOCK_EIGHTHS[2]),
    ("eighth_3", BLOCK_EIGHTHS[3]),
    ("eighth_4", BLOCK_EIGHTHS[4]),
    ("eighth_5", BLOCK_EIGHTHS[5]),
    ("eighth_6", BLOCK_EIGHTHS[6]),
    ("eighth_7", BLOCK_EIGHTHS[7]),
    ("block_full", BLOCK_EIGHTHS[8]),
    ("meter_track", METER_TRACK),
];

// ── Brand-font coverage (SPEC 4.2) ──────────────────────────────────────────

/// Does the brand terminal font ship this glyph, and if not, what draws it?
///
/// Only the first half travels. `Native` and the *absence* it contrasts with
/// are facts about JetBrains Mono's `cmap` and hold on every machine; the face
/// named by [`Coverage::Substituted`] is a fact about one fallback chain,
/// CoreText's on macOS. A bare Linux terminal on DejaVu Sans Mono resolves the
/// same codepoints differently and some of them to nothing at all, which is the
/// hazard this table keeps visible rather than one it can answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coverage {
    /// In the brand font's own `cmap`.
    Native,
    /// Absent from it. The named face is what CoreText resolves it to on
    /// macOS, and it is recorded rather than repaired because the substitute
    /// is monospace and the cell grid survives.
    Substituted(&'static str),
}

/// Every character this vocabulary declares, against the brand font (SPEC 3.4).
///
/// SPEC 4's glyphs had never been checked against the font the design names.
/// Doing it for the four tool classes in #4125 found six shipped glyphs
/// resolving through font fallback; doing it exhaustively here found **eight**
/// — the issue's own list missed U+25A2 and counted the spinner as one glyph
/// when three of its four frames are absent (#4318). That gap is the argument
/// for a walked table over a prose list.
///
/// Regenerate after adding a glyph, on a machine with the font installed:
///
/// ```text
/// uv run --with fonttools --with pyobjc-framework-CoreText \
///     python scripts/gen-glyph-coverage.py
/// ```
///
/// The output is this table. It is **committed** rather than computed, because
/// `make gate` may not assume a font is installed and a guard that skips when
/// one is missing reports green over an unrun check —
/// `every_glyph_declares_its_brand_font_coverage` walks [`ALL`] against this
/// with no font involved.
///
/// Menlo is monospace, so its substitutions cost nothing on macOS and cost a
/// tofu box on a bare Linux terminal. That is the trade SPEC 4.2 accepts and
/// states. It used to hold one row that was not that trade: U+FF0B
/// [`WRITE`] resolved to **PingFang SC**, a proportional CJK face, into the
/// one cell the layout budgeted as fullwidth — #4482 retired that character
/// for the native `+` (U+002B), which every row here confirms is `Native`.
pub const COVERAGE: [(char, Coverage); 31] = [
    ('\u{002B}', Coverage::Native),               // PLUS SIGN
    ('\u{0192}', Coverage::Native),               // LATIN SMALL LETTER F WITH HOOK
    ('\u{2193}', Coverage::Native),               // DOWNWARDS ARROW
    ('\u{21B3}', Coverage::Substituted("Menlo")), // DOWNWARDS ARROW WITH TIP RIGHTWARDS
    ('\u{2299}', Coverage::Native),               // CIRCLED DOT OPERATOR
    ('\u{2325}', Coverage::Native),               // OPTION KEY
    ('\u{2588}', Coverage::Native),               // FULL BLOCK
    ('\u{2589}', Coverage::Native),               // LEFT SEVEN EIGHTHS BLOCK
    ('\u{258A}', Coverage::Native),               // LEFT THREE QUARTERS BLOCK
    ('\u{258B}', Coverage::Native),               // LEFT FIVE EIGHTHS BLOCK
    ('\u{258C}', Coverage::Native),               // LEFT HALF BLOCK
    ('\u{258D}', Coverage::Native),               // LEFT THREE EIGHTHS BLOCK
    ('\u{258E}', Coverage::Native),               // LEFT ONE QUARTER BLOCK
    ('\u{258F}', Coverage::Native),               // LEFT ONE EIGHTH BLOCK
    ('\u{2591}', Coverage::Native),               // LIGHT SHADE
    ('\u{25A2}', Coverage::Substituted("Menlo")), // WHITE SQUARE WITH ROUNDED CORNERS
    ('\u{25A4}', Coverage::Substituted("Menlo")), // SQUARE WITH HORIZONTAL FILL
    ('\u{25B8}', Coverage::Native),               // BLACK RIGHT-POINTING SMALL TRIANGLE
    ('\u{25C6}', Coverage::Native),               // BLACK DIAMOND
    ('\u{25C7}', Coverage::Native),               // WHITE DIAMOND
    ('\u{25C9}', Coverage::Native),               // FISHEYE
    ('\u{25CB}', Coverage::Native),               // WHITE CIRCLE
    ('\u{25CC}', Coverage::Native),               // DOTTED CIRCLE
    ('\u{25CF}', Coverage::Native),               // BLACK CIRCLE
    ('\u{25D0}', Coverage::Substituted("Menlo")), // CIRCLE WITH LEFT HALF BLACK
    ('\u{25D1}', Coverage::Substituted("Menlo")), // CIRCLE WITH RIGHT HALF BLACK
    ('\u{25D2}', Coverage::Substituted("Menlo")), // CIRCLE WITH LOWER HALF BLACK
    ('\u{25D3}', Coverage::Substituted("Menlo")), // CIRCLE WITH UPPER HALF BLACK
    ('\u{2713}', Coverage::Native),               // CHECK MARK
    ('\u{2717}', Coverage::Native),               // BALLOT X
    ('\u{2726}', Coverage::Substituted("Menlo")), // BLACK FOUR POINTED STAR
];

/// How many terminal cells `glyph` occupies.
///
/// One, for every glyph in this vocabulary (#4482 retired the one fullwidth
/// exception, [`WRITE`]). Kept as a function rather than assumed at each call
/// site, so a future glyph that genuinely needs two cells has one place to
/// change.
#[must_use]
pub const fn width(_glyph: char) -> usize {
    1
}
