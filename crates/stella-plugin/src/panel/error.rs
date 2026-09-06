//! Why a panel's text, lease, or frame was refused.
//!
//! Split out of `panel.rs` at the gate's line ceiling. These three error
//! enums were already a group on their own in that file, so the move costs
//! no rework.

use super::PanelSurface;

/// Why a host will not draw a frame it was handed.
///
/// Two routing cases and the geometry, because a frame can be wrong in ways
/// that have nothing to do with its size: a plugin drawing three surfaces can
/// answer the wrong lease, and one that fell behind can answer a tick the host
/// has already replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PanelRefusal {
    /// The frame answers a different panel than the one leased.
    #[error("a panel frame for the \"{answered}\" surface answers a \"{leased}\" lease")]
    Surface {
        /// The surface the lease was for.
        leased: PanelSurface,
        /// The surface the frame claims.
        answered: PanelSurface,
    },
    /// The frame answers a tick the host has moved on from.
    #[error("a panel frame for tick {answered} answers the lease for tick {leased}")]
    Tick {
        /// The tick the lease was for.
        leased: u64,
        /// The tick the frame echoed.
        answered: u64,
    },
    /// The frame addresses a cell the lease does not hold.
    #[error(transparent)]
    Overflow(
        /// Which edge it ran past.
        #[from]
        PanelOverflow,
    ),
}

/// Why a run of glyphs is not drawable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PanelTextError {
    /// The text carries a control character. A panel writes glyphs into cells;
    /// every escape byte the terminal sees is the host's.
    #[error(
        "panel text carries the control character U+{code:04X} at position {index}: a panel \
         writes glyphs into the cells it was leased, and Stella writes every escape sequence \
         the terminal sees"
    )]
    ControlCharacter {
        /// Which character of the text, counted in `char`s from zero.
        index: usize,
        /// The Unicode scalar value that was refused.
        code: u32,
    },

    /// The text carries a bidi `char`. Its own error, not a reused
    /// [`PanelTextError::ControlCharacter`]. The two refusals ask different
    /// things, and a host that printed the wrong one would send an author
    /// hunting an escape that is not there.
    #[error(
        "panel text carries the bidi formatting character U+{code:04X} at position {index}: it \
         reorders the glyphs after it, so the panel would read one way and mean another"
    )]
    BidiControl {
        /// Which character of the text, counted in `char`s from zero.
        index: usize,
        /// The Unicode scalar value that was refused.
        code: u32,
    },
}

/// Why a frame does not fit the rectangle it was leased.
///
/// Four cases because the two frame shapes fail in two ways each, and a host
/// refusing a frame should be able to print which row, which column and which
/// edge without re-deriving any of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PanelOverflow {
    /// A [`crate::panel::PanelPaint::Lines`] frame carries more rows than the
    /// lease has.
    #[error("a panel frame of {lines} line(s) does not fit a lease {rows} row(s) tall")]
    Rows {
        /// How many rows the frame carries.
        lines: usize,
        /// How many the lease holds.
        rows: u16,
    },
    /// A row of a [`crate::panel::PanelPaint::Lines`] frame runs past the
    /// lease's right edge.
    #[error("line {line} of a panel frame is {cells} cell(s) wide, past a {cols}-column lease")]
    Line {
        /// Which row, counted from the top of the frame.
        line: usize,
        /// How wide it is.
        cells: usize,
        /// How wide the lease is.
        cols: u16,
    },
    /// A [`crate::panel::PanelPaint::Diff`] patch addresses a row the lease
    /// does not have.
    #[error("a panel frame patches row {row}, past a lease {rows} row(s) tall")]
    Row {
        /// The row the patch addressed.
        row: u16,
        /// How many rows the lease holds.
        rows: u16,
    },
    /// A [`crate::panel::PanelPaint::Diff`] patch starts past the lease's
    /// right edge, or runs past it. Both are this one case because both are
    /// answered by the same edit — move the patch left or shorten it — and a
    /// host printing the refusal wants the column and the run length either
    /// way.
    #[error(
        "a panel frame patches {cells} cell(s) from column {col} of row {row}, past a \
         {cols}-column lease"
    )]
    Patch {
        /// The row the patch addressed.
        row: u16,
        /// The column it started at.
        col: u16,
        /// How many cells it writes.
        cells: usize,
        /// How wide the lease is.
        cols: u16,
    },
}
