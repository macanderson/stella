// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The PROOF panel's presentation vocabulary: how tall it is, how its sentences
//! wrap, and what tone each standing renders in.
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
//! the same height, so a change of standing is a light changing colour in a
//! fixed place. What is left here is the part that was never the problem: the
//! shared geometry and the tone→style mapping.
//!
//! # Why the body is three rows
//!
//! It carried five before, one per pipeline stage. [`crate::proof`] explains at
//! length why those stage names left the surface; what remains is a headline in
//! the panel's title and a plain sentence under it, and three rows is what that
//! sentence needs at the rail's ~38 columns without ever reflowing. The two rows
//! given back go to PLAN, which is the panel a squeezed rail used to lose
//! entirely.

use ratatui::style::{Modifier, Style};

use crate::textline::Tone;
use crate::theme;

/// Rows the panel gives its explanation, under the title that carries the
/// standing itself. Fixed, not a function of how the turn is going — the old
/// band shrank as things improved, which is what made its absence unreadable.
pub const BODY_ROWS: u16 = 3;

pub fn style_for(tone: Tone) -> Style {
    match tone {
        Tone::Success => Style::new().fg(theme::SUCCESS).add_modifier(Modifier::BOLD),
        Tone::Warn => Style::new().fg(theme::WARN),
        Tone::Error => Style::new().fg(theme::DANGER).add_modifier(Modifier::BOLD),
        Tone::Muted => theme::muted(),
        Tone::Info => Style::new().fg(theme::TEXT_SECONDARY),
    }
}

/// Word-wrap one plain sentence to `width` columns.
///
/// Greedy, and on word boundaries only: a single word longer than the line — in
/// practice always a file path — is emitted whole on its own row rather than
/// broken, and the renderer's ellipsis truncation is what marks the cut.
/// Breaking a path across two rows would produce two strings that each look
/// like a real path and neither of which is one.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + 1 + word.chars().count() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The panel's height is a constant, not a function of how the turn is
    /// going. The old band shrank as things improved, which is what made its
    /// absence unreadable.
    #[test]
    fn the_body_is_always_three_rows() {
        assert_eq!(BODY_ROWS, 3);
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

    #[test]
    fn a_sentence_wraps_on_word_boundaries() {
        assert_eq!(
            wrap("a test fails without this change and passes with it", 20),
            vec!["a test fails without", "this change and", "passes with it"],
        );
    }

    /// A path is one word and longer than the rail. It must survive whole for
    /// the renderer to ellipsize — split, it would read as a path that exists.
    #[test]
    fn an_overlong_word_keeps_its_own_row_rather_than_breaking() {
        assert_eq!(
            wrap("apps/app/automations/store.ts", 12),
            vec!["apps/app/automations/store.ts"],
        );
    }

    #[test]
    fn wrapping_nothing_produces_no_rows() {
        assert!(wrap("", 20).is_empty());
        assert!(wrap("anything", 0).is_empty());
    }
}
