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
//! ## Which frame is the one frame: SPEC 6, and [`grid`] is where it lands
//!
//! Settled 2026-08-22, owner's call, recorded here because #4271 asked for it
//! in this charter specifically — a decision that lives only in an issue is one
//! the next author re-litigates.
//!
//! Two frames were drawing the same information. [`grid`] draws a
//! `╭─ … ─╮` / `│` / `╰──╯` box with [`digest::Chip`] right-aligned;
//! `stella-tui`'s `v2::transcript` draws `design/tui-v2/SPEC.md` §6 — a
//! full-width labelled rule, a run of railed event rows, a closing rule and a
//! one-line receipt. Neither was wrong, and the deck's was written without
//! reference to this crate at all.
//!
//! **SPEC 6 wins, and [`grid`] is changed to draw it** rather than the deck
//! being bent onto the box. Three reasons, in order of weight:
//!
//! 1. SPEC 6 is the *authored product design*; the box frame is an artifact of
//!    this crate's extraction (#3764) and was never designed against it. When a
//!    spec and an implementation disagree about a product decision, the spec is
//!    the side that was decided on purpose.
//! 2. The box cannot carry SPEC 2's **two-metal rule**. [`grid::Color`]'s roles
//!    are `Ink`/`Dim`/`Faint`/`Cyan`/`Green`/`Red`/…, and `Cyan` for "tool
//!    identity" is exactly the per-class hue SPEC 3.2's clamp rejects and #4125
//!    is about removing. Adopting the box would mean relaxing the palette rule
//!    the whole v2 theme is built on.
//! 3. The blast radius is one surface. [`grid`] has exactly one shipping caller
//!    — the plain `stella run` transcript
//!    (`crates/stella-cli/src/plain/transcript.rs`). The Observatory renders
//!    through [`html`] and is untouched by the frame decision. So "one frame"
//!    costs a changed appearance on one terminal surface, not three.
//!
//! ### Where the theme sits, which is the other half of the question
//!
//! [`grid`] does **not** gain a dependency on `stella-tui-theme`, and the deck
//! does not stop using it. The existing seam already answers this: this crate
//! emits a *role* and encodes at the very end, which is what keeps it free of a
//! terminal dependency. So the rule is **[`grid`] owns the role, the surface
//! owns the pigment** — [`grid::Color`] gains the two metals as roles, and the
//! deck maps role → `stella_tui_theme::token` RGB at its own encode step. A
//! warm gray cannot be smuggled in that way, because the deck's hue clamp still
//! guards every token the mapping can produce.
//!
//! ### The gap this exposes, which is real and is not the frame
//!
//! `v2::transcript`'s file-event fields are all `Option` because a head row
//! renders before anything is measured, and a `+0 -0` beside a path is a
//! different and louder claim than "not measured yet". [`model`] has no
//! equivalent notion of an unmeasured row. That is a genuine hole in the shared
//! model — the same one #4181 describes from the accessibility side — and it
//! has to be closed *before* the deck can render through [`grid`], not after,
//! or the migration silently reintroduces the fabricated zero.
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
//! sets must match in both directions. That file is the current answer; this
//! paragraph deliberately does not repeat a count, because a number in two
//! places is how the last one died.
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
