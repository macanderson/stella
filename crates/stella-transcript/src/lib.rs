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
//! | Raw JSON argument blob | Arguments are key/value rows behind a toggle, and only the ones not already displayed; a JSON *result* is read the same way by [`fields`], on every surface (#4340) |
//! | Metadata at the weight of the work | [`digest::Chip`] is the only carrier, always right-aligned and muted |
//! | Dead truncation text | [`digest::fold_output`] returns the hidden lines' count *and* the tail, so the control has something behind it |
//!
//! ## The two renderers
//!
//! [`html`] emits the jet-black web surface; [`grid`] emits a character grid in
//! the same shape a TUI needs, though nothing in the shipping TUI reads it yet.
//! They share [`model`], [`fold`], [`digest`], [`fields`], [`file_diff`] and
//! [`word`] — which is the point. The previous arrangement had the Observatory
//! re-implementing the TUI's renderer in JavaScript, and that copy had silently
//! drifted to the point of having no diff rendering at all.
//!
//! ## One frame: SPEC 6
//!
//! Two frames draw the same information. [`grid`] draws a `╭─ … ─╮` / `│` /
//! `╰──╯` box with [`digest::Chip`] right-aligned; `stella-tui`'s
//! `v2::transcript` draws `design/tui-v2/SPEC.md` §6 — a full-width labelled
//! rule, a run of railed event rows, a closing rule and a one-line receipt.
//!
//! **SPEC 6 is the frame this crate will draw** (settled 2026-08-22, owner's
//! call, recorded here because #4271 asked for it in this charter). [`grid`]
//! has not been changed yet and still draws the box; #4756 tracks the change.
//! SPEC 6 wins because it is the authored product design, because the box
//! cannot carry SPEC 2's two-metal rule ([`grid::Color`]'s `Cyan` for "tool
//! identity" is the per-class hue SPEC 3.2's clamp rejects), and because
//! [`grid`] has one shipping caller — the plain `stella run` transcript
//! (`crates/stella-cli/src/plain/transcript.rs`) — so the cost is one terminal
//! surface, not three.
//!
//! [`grid`] does **not** gain a dependency on `stella-tui-theme`. It emits a
//! *role* and encodes at the very end, which is what keeps it free of a
//! terminal dependency: **[`grid`] owns the role, the surface owns the
//! pigment**. [`grid::Color`] gains the two metals as roles, and the deck maps
//! role → `stella_tui_theme::token` RGB at its own encode step, where the hue
//! clamp still guards every token the mapping can produce.
//!
//! One hole has to close first. `v2::transcript`'s file-event fields are all
//! `Option` because a head row renders before anything is measured, and a
//! `+0 -0` beside a path is a louder claim than "not measured yet". [`model`]
//! has no notion of an unmeasured row (the hole #4181 describes from the
//! accessibility side). Close it *before* the deck renders through [`grid`],
//! or the migration reintroduces the fabricated zero.
//!
//! ## Which surfaces actually draw through this crate
//!
//! Extracting the crate proved nothing on its own, and the gap between "the
//! crate exists" and "every surface uses it" went unnoticed for long enough to
//! close #3578 as completed twice while the deck had never called
//! [`grid::render`]. The adoption ledger is
//! `scripts/check-transcript-surfaces.py` (`make transcript-surfaces`): one row
//! per surface, a row that claims to share must really reference the entry
//! point, a row that does not must cite the issue deciding it, and the caller
//! sets must match in both directions. That file is the current answer; no
//! count is repeated here, because a number in two places is how the last one
//! died.
//!
//! ## A turn closes with its outcome, on both renderers
//!
//! Neither a turn's status nor its accounting exists when the turn opens, so
//! both live at the *end* of a turn — [`grid`]'s bottom rail, and [`html`]'s
//! receipt. That is a constraint before it is a layout: it is the only thing
//! that lets an append-only surface write a frame a line at a time as the turn
//! runs, instead of showing a placeholder until it can draw the whole thing
//! (see [`grid::render_turn_lines`]). A collapsed `<details>` is the one
//! exception, and only because it renders nothing but its summary.
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
pub mod fields;
pub mod file_diff;
pub mod fold;
pub mod grid;
pub mod html;
pub mod model;
pub mod syntax;
pub mod tabs;
pub mod word;

#[cfg(test)]
mod tests;

pub use fold::{Command, Cursor, FoldState, Zoom};
pub use model::{
    Accounting, ArgRow, Call, FileChange, FileStatus, NodeId, Note, NoteKind, Output, Prose, Run,
    Status, Step, ToolKind, Turn,
};
