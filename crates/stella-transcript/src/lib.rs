//! One transcript model, two renderers.
//!
//! The view this crate replaces was a flat dark dump: every tool call printed
//! its header, a raw JSON argument blob, and its result as three loosely
//! related rows; a command could appear three times; token and cost metadata
//! sat inline at the same visual weight as the work it paid for; and the
//! truncation marker (`… 24 more lines`) was dead text with nothing behind it.
//!
//! Each of those is a *structural* defect rather than a styling one, so each is
//! fixed structurally here:
//!
//! | Defect | Structural fix |
//! |---|---|
//! | Command echoed up to three times | [`model::Call`] owns its output; the invocation lives in exactly one field, and [`model::Call::extra_args`] drops any argument the header already said |
//! | Call and result as siblings | They are one node; there is no way to render one without the other |
//! | Raw JSON argument blob | Arguments are key/value rows behind a toggle, and only the ones not already displayed |
//! | Metadata at the weight of the work | [`digest::Chip`] is the only carrier, always right-aligned and muted |
//! | Dead truncation text | [`digest::fold_output`] returns the hidden lines' count *and* the tail, so the control has something behind it |
//!
//! ## The two renderers
//!
//! [`html`] emits the jet-black web surface; [`grid`] emits a character grid for
//! the TUI. They share [`model`], [`fold`], [`digest`], [`file_diff`] and
//! [`word`] — which is the point. The previous arrangement had the Observatory
//! re-implementing the TUI's renderer in JavaScript, and that copy had silently
//! drifted to the point of having no diff rendering at all.
//!
//! ## Purity
//!
//! Nothing here reads a file, spawns a process, formats a timestamp or touches
//! the network (invariant #2). A caller hands over owned data and gets a string
//! or a grid back, which is what makes every acceptance check in this crate a
//! plain unit test.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod digest;
pub mod file_diff;
pub mod fold;
pub mod grid;
pub mod html;
pub mod model;
pub mod word;

#[cfg(test)]
mod tests;

pub use fold::{Command, Cursor, FoldState, Zoom};
pub use model::{
    Accounting, ArgRow, Call, FileChange, FileStatus, NodeId, Output, Prose, Run, Status, Step,
    ToolKind, Turn,
};
