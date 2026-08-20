//! The glyph vocabulary — SPEC 4, one set across every panel including
//! plugin-drawn UI.
//!
//! Glyphs exist so colour is never the only carrier of state (SPEC 2, SPEC
//! 13). That makes them load-bearing under `NO_COLOR`, on a 16-color
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
pub const ALL: [(&str, char); 16] = [
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
