//! The stella TUI v2 palette, glyph vocabulary, wordmark, and degradation map.
//!
//! One crate, so there is one answer. Every v2 surface pulls its colour from
//! [`token`] and its state glyphs from [`glyph`]; nothing above this crate
//! writes a hex literal, which is what makes the palette a thing that can be
//! *checked* rather than a convention that erodes. `design/tui-v2/SPEC.md`
//! sections 3 and 4 carry the design argument; this crate is its enforcement.
//!
//! ## What is enforced here
//!
//! - **The hue clamp** ([`clamp`], SPEC 3.2). Gold must satisfy `r > g > b`,
//!   `g >= 0.78 r`, `b <= 0.35 r`; below the green ratio the hue is orange,
//!   and orange on a near-black ground reads brown. Grays must be neutral or
//!   blue-tipped (`r == g`, `b >= g`); one point warm is how a black-and-gold
//!   scheme becomes sepia. Both are unit tests on the shipped table, so the
//!   palette cannot drift without a red gate.
//! - **Role totality.** [`token::ALL`] pairs every token with a
//!   [`token::Role`], and the role decides its clamp. A new token has to
//!   declare what kind of colour it is before it can exist, and a warm hex has
//!   no honest declaration to pick.
//! - **Fallback totality.** [`fallback::ansi16`] answers for every token in
//!   the table, proven by walking it — so a token added without a 16-color
//!   stand-in fails rather than silently passing its 24-bit value to a
//!   terminal that cannot show it.
//!
//! ## The two metals
//!
//! Gold is stella acting on the world; silver is the world coming in
//! (SPEC 2). Red and green are verdicts and nothing else, and red is the
//! rarest colour on screen: it never appears in a healthy frame, which is what
//! makes a red gate an alarm without blinking or a bell. Healthy-frame
//! snapshots assert a red cell count of zero — that assertion is a feature,
//! not test hygiene.
//!
//! ## Boundary — does this change belong here?
//!
//! This crate owns *what a colour is* and *what a state looks like*. It does
//! not own where either one goes: a rule about which metal an event rail takes
//! belongs to the widget that draws rails, and a decision about how much of a
//! meter is filled belongs to whatever computes the fraction. The test is
//! whether the answer changes when the layout does. If it does, it is not a
//! palette fact.
//!
//! It has exactly one dependency (`ratatui`, for `Color`/`Style`/`Span`) and
//! must keep it that way — every v2 surface depends on this crate, so it may
//! depend on nothing that could ever paint a cell.

pub mod clamp;
pub mod fallback;
// The palette as `design/tokens/stella-tokens.json` states it, emitted by
// `scripts/gen-tokens.py`. It is NOT what this crate renders: [`token`] and
// [`clamp`] are hand-written and are what every consumer imports.
//
// It arrived undeclared. #4055 created this crate with the hand-written pair;
// #4066 added the generator and its output beside them and never wrote this
// line, so the file was invisible to rustc, rustfmt and clippy at once — and
// `make tokens`, which checks that it is in sync with the JSON, was proving a
// property of a file nothing compiled (#1750 is the guard that caught it).
//
// Declaring it makes the generator load-bearing rather than decorative: with
// the module reachable, `the_hand_written_palette_agrees_with_the_generated_one`
// holds the two tables to each other, so a value edited in the JSON without
// being carried into `token.rs` — or the reverse — is now a red test instead of
// a silent second palette. Consolidating the two into one file is the right end
// state and is tracked separately; it is a design change, not a build fix.
pub mod generated;
pub mod glyph;
pub mod token;
pub mod wordmark;

#[cfg(test)]
mod tests;
