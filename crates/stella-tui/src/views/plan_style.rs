// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One visual vocabulary for a plan step's lifecycle, shared by every surface
//! that draws steps — the rail ([`crate::views::plan_rail`]), the `/plan` card
//! ([`crate::views::plan_card`]), and the plan-review dialog
//! ([`crate::views::scope_dialog`]). The rail and the card used to carry two
//! hand-copied versions of the same style match, which is exactly how the two
//! surfaces drift apart one tweak at a time.
//!
//! The mapping is the product spec:
//!
//! - **Planned** — the whole row in the muted grey, always. Un-started work is
//!   background, whatever the plan's own state; the moment of approval must
//!   not light up nine rows nobody is working on.
//! - **Started** — the ring, the step number, and the title all *pulse*
//!   between the primary text tone and the muted steps. The pulse is a pure
//!   function of the deck clock (`model.now_ms`), like every other motion in
//!   this crate — no timer state, and `no_anim` pins it to the bright frame.
//!   It steps through palette tokens rather than interpolated RGB because
//!   [`crate::theme::apply_theme`] is a value-keyed remap: only named tokens
//!   follow the light theme, so an interpolated white would stay white on
//!   paper and vanish. `TEXT_PRIMARY` is white on `stella-dark` and ink on
//!   `stella-light` by construction, which is the whole contract.
//! - **Complete** — a `✓` sitting on the green indicator cell, and the title
//!   keeps the primary tone with a strike through it: done, but still legible
//!   as what was done.
//! - **Error** (a failed or cancelled step) — the entire row painted red, with
//!   the ground-colour `✗` on the red indicator cell. The ground colour is the
//!   theme's canvas (near-black on `stella-dark`, paper on `stella-light`), so
//!   the mark reads as cut *out of* the red in both themes.

use ratatui::style::{Modifier, Style};

use crate::plan::PlanStepState;
use crate::theme;

/// One full pulse (bright → dim → bright), in deck-clock milliseconds.
const PULSE_PERIOD_MS: u64 = 1_200;

/// The pulse's tone ladder: down through the text ramp and back up. All four
/// slots are palette tokens so the light-theme remap and the 256/16-colour
/// fallbacks apply to every frame of the animation.
const PULSE_PHASES: [ratatui::style::Color; 4] = [
    theme::TEXT_PRIMARY,
    theme::TEXT_SECONDARY,
    theme::TEXT_TERTIARY,
    theme::TEXT_SECONDARY,
];

/// How one plan-step row is drawn: the status indicator, the step number, the
/// title, and the trailing `(note)` / `(owner)` metadata.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StepVisual {
    /// The status indicator's glyph (`○` planned, `●` working, `✓` done,
    /// `✗` failed/cancelled).
    pub glyph: &'static str,
    /// The indicator cell's style.
    pub ring: Style,
    /// The step number's style — it follows the title through every state so
    /// the row reads as one unit, exactly as the spec asks.
    pub num: Style,
    /// The title's style.
    pub text: Style,
    /// The `(note)` / `(owner)` suffix — dim everywhere except an error row,
    /// which paints the whole row including the reason it is red.
    pub meta: Style,
    /// The spacer cell between the indicator and the number: styleless
    /// everywhere except an error row, where "the entire row" includes the
    /// gaps.
    pub gap: Style,
}

/// The pulse tone for `now_ms` — a palette token, never an interpolated RGB
/// (see the module doc for why).
fn pulse_tone(now_ms: u64) -> ratatui::style::Color {
    let slot = now_ms / (PULSE_PERIOD_MS / PULSE_PHASES.len() as u64);
    PULSE_PHASES[(slot % PULSE_PHASES.len() as u64) as usize]
}

/// The visual for one step. `animate` is the deck's motion switch
/// (`!ui.no_anim`); off, the working row holds the bright frame.
pub(crate) fn step_visual(state: PlanStepState, now_ms: u64, animate: bool) -> StepVisual {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    match state {
        PlanStepState::Planned => StepVisual {
            glyph: "○",
            ring: dim,
            num: dim,
            text: dim,
            meta: dim,
            gap: Style::new(),
        },
        PlanStepState::Started => {
            let tone = if animate {
                pulse_tone(now_ms)
            } else {
                theme::TEXT_PRIMARY
            };
            let live = Style::new().fg(tone).add_modifier(Modifier::BOLD);
            StepVisual {
                glyph: "●",
                ring: live,
                num: live,
                text: live,
                meta: dim,
                gap: Style::new(),
            }
        }
        PlanStepState::Complete => StepVisual {
            glyph: "✓",
            // The checkmark ON the green indicator: ground-colour mark, green
            // cell — one terminal cell can hold one glyph, so the overlay is
            // fg-on-bg rather than two stacked marks.
            ring: Style::new().fg(theme::GROUND).bg(theme::OK),
            num: Style::new()
                .fg(theme::TEXT_PRIMARY)
                .add_modifier(Modifier::CROSSED_OUT),
            text: Style::new()
                .fg(theme::TEXT_PRIMARY)
                .add_modifier(Modifier::CROSSED_OUT),
            meta: dim,
            gap: Style::new(),
        },
        PlanStepState::Error => {
            // The whole row painted red, the ✗ cut out of the indicator in
            // the ground colour — a failed or cancelled step is the one row
            // allowed to shout.
            let row = Style::new().fg(theme::GROUND).bg(theme::DANGER);
            StepVisual {
                glyph: "✗",
                ring: row.add_modifier(Modifier::BOLD),
                num: row,
                text: row,
                meta: row,
                gap: row,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec's first line: un-started work is muted grey, full stop. The
    /// old mapping brightened every planned row to the primary tone the moment
    /// the plan was approved, which made "nine rows of queued work" compete
    /// with the one row actually moving.
    #[test]
    fn a_planned_step_is_muted_grey() {
        let v = step_visual(PlanStepState::Planned, 0, true);
        assert_eq!(v.glyph, "○");
        for style in [v.ring, v.num, v.text] {
            assert_eq!(style.fg, Some(theme::TEXT_TERTIARY));
            assert_eq!(style.bg, None);
        }
    }

    /// The working row pulses: ring, number and title move together through
    /// the text ramp, as a pure function of the deck clock.
    #[test]
    fn a_started_step_pulses_ring_number_and_title_together() {
        let bright = step_visual(PlanStepState::Started, 0, true);
        assert_eq!(bright.text.fg, Some(theme::TEXT_PRIMARY));
        // Half a period later the tone has moved down the ramp.
        let dimmed = step_visual(PlanStepState::Started, PULSE_PERIOD_MS / 2, true);
        assert_ne!(bright.text.fg, dimmed.text.fg, "the pulse must move");
        for v in [bright, dimmed] {
            assert_eq!(v.glyph, "●");
            assert_eq!(v.ring.fg, v.text.fg, "the indicator pulses with the text");
            assert_eq!(v.num.fg, v.text.fg, "the number pulses with the text");
            assert!(
                PULSE_PHASES.contains(&v.text.fg.unwrap()),
                "every pulse frame is a palette token, so the light theme and \
                 the 256/16-colour fallbacks can remap it"
            );
        }
    }

    /// `no_anim` pins the working row to its bright frame — recordings and
    /// goldens stay byte-stable.
    #[test]
    fn no_anim_freezes_the_pulse_on_the_bright_frame() {
        for now_ms in [0, 300, 599, 1_100] {
            let v = step_visual(PlanStepState::Started, now_ms, false);
            assert_eq!(v.text.fg, Some(theme::TEXT_PRIMARY), "at {now_ms}ms");
        }
    }

    /// Done: a checkmark on the green indicator, and the title stays in the
    /// primary tone with a strikethrough — not demoted to grey.
    #[test]
    fn a_complete_step_is_a_check_on_green_with_struck_primary_text() {
        let v = step_visual(PlanStepState::Complete, 0, true);
        assert_eq!(v.glyph, "✓");
        assert_eq!(v.ring.bg, Some(theme::OK), "the indicator cell is green");
        assert_eq!(v.text.fg, Some(theme::TEXT_PRIMARY), "the text stays bright");
        assert!(v.text.add_modifier.contains(Modifier::CROSSED_OUT));
        assert!(v.num.add_modifier.contains(Modifier::CROSSED_OUT));
    }

    /// Failed or cancelled: the whole row is painted red, with the ✗ cut out
    /// of the indicator in the ground colour.
    #[test]
    fn an_error_step_paints_the_entire_row_red_with_a_ground_cross() {
        let v = step_visual(PlanStepState::Error, 0, true);
        assert_eq!(v.glyph, "✗");
        for style in [v.ring, v.num, v.text, v.meta, v.gap] {
            assert_eq!(style.bg, Some(theme::DANGER), "the row is red end to end");
            assert_eq!(style.fg, Some(theme::GROUND));
        }
    }
}
