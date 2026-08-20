//! The stella colour system — one palette, one clamp, generated once.
//!
//! Every colour any stella surface paints comes from
//! `design/tokens/stella-tokens.json`. This crate is the Rust face of that
//! file: [`generated`] is emitted by `scripts/gen-tokens.py` and re-exported
//! here, so a caller writes `stella_tui_theme::GOLD` and never a hex literal.
//!
//! **Why a generator rather than a hand-written table.** The palette is a
//! *shared cell* — the TUI, the marketing site, the brand kit and the
//! Observatory each need the same twenty-one values, and each previous attempt
//! to hold them together was a doc comment asking the next author to copy
//! carefully. Twice, that comment was the only thing left true: the website's
//! ramp sat on a retired kit for the whole life of a rebrand while still
//! claiming to mirror it, and a reader crossing between two stella surfaces
//! watched the brand change hue. A comment cannot hold a shared cell. A
//! generator with a `--check` mode in the gate can.
//!
//! **What the clamp is for.** Gold on near-black is the scheme that dies of
//! warm drift: each well-meaning tune moves the hue a few points toward orange,
//! and orange on black reads brown on an uncalibrated panel. [`generated`]'s
//! predicates make that unrepresentable rather than discouraged — every token
//! declares the role it belongs to, and
//! [`every_token_satisfies_its_clamp`](generated) walks the declaration. The
//! retired brand golds are asserted to *fail*, so the clamp cannot go slack
//! without a test going red.

#![forbid(unsafe_code)]

pub mod generated;

pub use generated::{
    ALL, BG, BORDER, COMMENT, Clamp, DIFF_ADD_BG, DIFF_DEL_BG, DIM, GOLD, GOLD_BLUE_PCT,
    GOLD_BRIGHT, GOLD_GREEN_PCT, GOLD_LIFT_BLUE_PCT, GREEN, HL, INK, MUTED, PANEL, PAPER,
    PAPER_BORDER, PAPER_PANEL, RED, RULE, SILVER, SILVER_TYPE, TEXT, channels, is_cool_silver,
    is_lifted_gold, is_neutral_gray, is_resting_gold, satisfies,
};
