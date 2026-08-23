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
//! Every glyph here is one terminal cell wide except [`WRITE`], which is
//! U+FF0B FULLWIDTH PLUS SIGN and occupies **two**. SPEC 4 names that
//! character specifically; a layout that budgets one cell for it will shear
//! the row to its right, so [`width`] states the width rather than leaving
//! each call site to measure. Nothing else here is fullwidth.
//!
//! [`WRITE`] is the only *unconditionally* wide one. Nine others carry East
//! Asian Width `A` (**ambiguous**) rather than `N`, so a terminal configured
//! for CJK double-width ambiguity draws them in two cells: [`RUNNING`],
//! [`QUEUED`], [`GATE`], [`MEMORY`], [`NODE_FILE`], [`TOOL_EXECUTE`],
//! [`EVENT`], [`COMPACTED`], and `BLOCK_EIGHTHS[8]`. [`width`] answers one for
//! all of them, because that is what every non-CJK configuration draws and
//! what the layout budgets.
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
/// U+2442 OCR FORK is missing from a good many terminal fonts, where it
/// renders as a replacement box — and a box is worse than a different glyph,
/// because it reads as a rendering bug rather than as drift. [`DRIFT_FALLBACK`]
/// is SPEC 4's stated substitute.
pub const DRIFT: char = '⑂';

/// The substitute for [`DRIFT`] on fonts lacking U+2442 (SPEC 4).
pub const DRIFT_FALLBACK: char = '↯';

/// Collapsed, expandable. Takes the metal of the event it heads.
pub const COLLAPSED: char = '▸';

/// Write (new file). Green sign on a gold rail (SPEC 4).
///
/// Fullwidth: **two cells**. See the module doc.
pub const WRITE: char = '＋';

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
/// stated width is caught rather than assumed.
pub const ALL: [(&str, char); 22] = [
    ("done", DONE),
    ("running", RUNNING),
    ("queued", QUEUED),
    ("gate", GATE),
    ("failed", FAILED),
    ("drift", DRIFT),
    ("drift_fallback", DRIFT_FALLBACK),
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
    ("block_full", BLOCK_EIGHTHS[8]),
    ("meter_track", METER_TRACK),
];

/// How many terminal cells `glyph` occupies.
///
/// One for everything in this vocabulary except [`WRITE`]. Stated here rather
/// than measured per call site so the fullwidth case cannot be forgotten in
/// the one place it would shear a row.
#[must_use]
pub const fn width(glyph: char) -> usize {
    if glyph == WRITE { 2 } else { 1 }
}
