// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The verification vocabulary: how many rows the DONE VERIFICATION panel has,
//! and what tone each one renders in.
//!
//! # What used to be here
//!
//! A whole panel — a bordered PROOF band under the transcript, **gated on bad
//! news**. `band_height` returned 0 unless some row's tone was `Warn` or
//! `Error`, which meant the band got *shorter* as the turn went better, and a
//! reader could not distinguish "nothing to report" from "not reported". On the
//! most common turn shape it was simply absent, and the verification indicators
//! were reported as never updating — correctly, because there was nothing on
//! screen to update.
//!
//! The panel now lives in [`crate::views::plan_rail`], unconditional and always
//! five rows, so a row moving from `pending` to `authored` is a light changing
//! colour in a fixed place. What is left here is the part that was never the
//! problem: the row count and the tone→style mapping, shared so the rail and
//! anything else that renders a verification row agree.

use ratatui::style::{Modifier, Style};

use crate::textline::Tone;
use crate::theme;

/// Rows the verification panel draws: warrant, witness, oracle, tamper,
/// verdict. Always all five — see the module docs for what filtering them cost.
pub const ROWS: u16 = 5;

pub fn style_for(tone: Tone) -> Style {
    match tone {
        Tone::Success => Style::new().fg(theme::SUCCESS).add_modifier(Modifier::BOLD),
        Tone::Warn => Style::new().fg(theme::WARN),
        Tone::Error => Style::new().fg(theme::DANGER).add_modifier(Modifier::BOLD),
        Tone::Muted => theme::muted(),
        Tone::Info => Style::new().fg(theme::TEXT_SECONDARY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The panel's height is a constant, not a function of how the turn is
    /// going. The old band shrank as things improved, which is what made its
    /// absence unreadable.
    #[test]
    fn the_panel_is_always_five_rows() {
        assert_eq!(ROWS, 5);
    }

    /// The three tones that carry a *verdict* must be mutually distinguishable
    /// and distinguishable from the quiet ones — those are the readings a user
    /// acts on. `Muted` and `Info` are deliberately close (both are "nothing to
    /// act on here"), so they are not held apart.
    #[test]
    fn the_verdict_tones_are_distinguishable_from_each_other_and_from_quiet() {
        let loud = [Tone::Success, Tone::Warn, Tone::Error];
        let quiet = [Tone::Muted, Tone::Info];
        for (i, a) in loud.iter().enumerate() {
            for b in &loud[i + 1..] {
                assert_ne!(style_for(*a), style_for(*b), "{a:?} and {b:?} render alike");
            }
            for q in &quiet {
                assert_ne!(
                    style_for(*a),
                    style_for(*q),
                    "{a:?} is indistinguishable from the quiet {q:?}"
                );
            }
        }
    }
}
