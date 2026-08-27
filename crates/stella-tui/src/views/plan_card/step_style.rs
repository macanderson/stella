// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! One visual vocabulary for a plan step's lifecycle.
//!
//! It was shared by three surfaces once — the `/plan` card, an always-on PLAN
//! rail, and a plan-review dialog — because a hand-copied style match is how
//! two surfaces drift apart one tweak at a time. The dialog went in #3861 and
//! the rail folded into the tab row's breadcrumb (SPEC 5, [`crate::views::frame`]),
//! so the card is the surface that draws steps and this is its vocabulary. It
//! stays a module of its own because the mapping below is a product decision
//! with six states and a clock in it, and it is testable without a buffer.
//!
//! Every glyph comes from [`stella_tui_theme::glyph`], the SPEC 4 table the
//! whole product draws from, rather than being typed in again here.
//!
//! The mapping is the product spec:
//!
//! - **Planned** (`○`) — the whole row in the muted grey, always. Un-started
//!   work is background, whatever the plan's own state; the moment of approval
//!   must not light up nine rows nobody is working on.
//! - **Started** (`◐`) — the ring, the step number, and the title all *pulse*
//!   between the primary text tone and the muted steps. The pulse is a pure
//!   function of the deck clock (`model.now_ms`), like every other motion in
//!   this crate — no timer state, and `no_anim` pins it to the bright frame.
//!   It steps through palette tokens rather than interpolated RGB because
//!   [`crate::theme::apply_theme`] is a value-keyed remap: only the tokens in
//!   [`stella_tui_theme::token`] follow the light theme, so an interpolated
//!   white would stay white on paper and vanish. [`token::TEXT`] is white on
//!   `stella-dark` and ink on `stella-light` by construction, which is the
//!   whole contract.
//! - **Verify** (`◇`) — the gate diamond in gold on the indicator cell, and
//!   the row in the primary tone. Gold rather than green: the checks have not
//!   answered yet, and a row that already reads as pass is the self-report
//!   SPEC 7.1 refuses.
//! - **Complete** (`✓`) — a check sitting on the green indicator cell, and the
//!   title keeps the primary tone with a strike through it: done, but still
//!   legible as what was done.
//! - **Blocked** (`✗`) — the entire row painted red, with the ground-colour
//!   cross on the red indicator cell. The ground colour is the theme's canvas
//!   (near-black on `stella-dark`, paper on `stella-light`), so the mark reads
//!   as cut *out of* the red in both themes. A cancelled step draws the same
//!   row and says `(cancelled)` in its metadata.
//! - **DriftInserted** (`⌥`) — the option glyph in gold-bright, with the
//!   `(inserted)` tag beside it in the same metal (SPEC 7.3). The row itself
//!   stays in the primary tone: the step is ordinary work, and what is worth
//!   marking is that the approved plan did not contain it.

use ratatui::style::{Modifier, Style};
use stella_tui_theme::{glyph, token};

use crate::plan::PlanStepState;

/// One full pulse (bright → dim → bright), in deck-clock milliseconds.
const PULSE_PERIOD_MS: u64 = 1_200;

/// The pulse's tone ladder: down through the text ramp and back up. All four
/// slots are palette tokens so the light-theme remap and the 256/16-colour
/// fallbacks apply to every frame of the animation.
const PULSE_PHASES: [ratatui::style::Color; 4] =
    [token::TEXT, token::SILVER, token::MUTED, token::SILVER];

/// How one plan-step row is drawn: the status indicator, the step number, the
/// title, and the trailing `(note)` / `(owner)` metadata.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StepVisual {
    /// The status indicator's glyph, from the SPEC 4 table in
    /// [`stella_tui_theme::glyph`].
    pub glyph: char,
    /// The indicator cell's style.
    pub ring: Style,
    /// The step number's style — it follows the title through every state so
    /// the row reads as one unit, exactly as the spec asks.
    pub num: Style,
    /// The title's style.
    pub text: Style,
    /// The `(note)` / `(owner)` suffix — dim, except on a blocked row, which
    /// paints the whole row including the reason it is red, and on a
    /// drift-inserted one, whose `(inserted)` tag takes the drift metal.
    pub meta: Style,
    /// The spacer cell between the indicator and the number: styleless
    /// everywhere except a blocked row, where "the entire row" includes the
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
    let dim = Style::new().fg(token::MUTED);
    match state {
        PlanStepState::Planned => StepVisual {
            glyph: glyph::QUEUED,
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
                token::TEXT
            };
            let live = Style::new().fg(tone).add_modifier(Modifier::BOLD);
            StepVisual {
                glyph: glyph::RUNNING,
                ring: live,
                num: live,
                text: live,
                meta: dim,
                gap: Style::new(),
            }
        }
        PlanStepState::Verify => {
            let live = Style::new().fg(token::TEXT);
            StepVisual {
                glyph: glyph::GATE,
                ring: Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD),
                num: live,
                text: live,
                meta: dim,
                gap: Style::new(),
            }
        }
        PlanStepState::DriftInserted => {
            let live = Style::new().fg(token::TEXT);
            let drift = Style::new().fg(token::GOLD_BRIGHT);
            StepVisual {
                glyph: glyph::DRIFT,
                ring: drift.add_modifier(Modifier::BOLD),
                num: live,
                text: live,
                // The `(inserted)` tag is half the signal, so it carries the
                // drift metal rather than fading into the row's metadata.
                meta: drift,
                gap: Style::new(),
            }
        }
        PlanStepState::Complete => StepVisual {
            glyph: glyph::DONE,
            // The checkmark ON the green indicator: ground-colour mark, green
            // cell — one terminal cell can hold one glyph, so the overlay is
            // fg-on-bg rather than two stacked marks.
            ring: Style::new().fg(token::BG).bg(token::GREEN),
            num: Style::new()
                .fg(token::TEXT)
                .add_modifier(Modifier::CROSSED_OUT),
            text: Style::new()
                .fg(token::TEXT)
                .add_modifier(Modifier::CROSSED_OUT),
            meta: dim,
            gap: Style::new(),
        },
        PlanStepState::Blocked => {
            // The whole row painted red, the ✗ cut out of the indicator in
            // the ground colour — a blocked or cancelled step is the one row
            // allowed to shout.
            let row = Style::new().fg(token::BG).bg(token::RED);
            StepVisual {
                glyph: glyph::FAILED,
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
        assert_eq!(v.glyph, glyph::QUEUED);
        for style in [v.ring, v.num, v.text] {
            assert_eq!(style.fg, Some(token::MUTED));
            assert_eq!(style.bg, None);
        }
    }

    /// The working row pulses: ring, number and title move together through
    /// the text ramp, as a pure function of the deck clock.
    #[test]
    fn a_started_step_pulses_ring_number_and_title_together() {
        let bright = step_visual(PlanStepState::Started, 0, true);
        assert_eq!(bright.text.fg, Some(token::TEXT));
        // Half a period later the tone has moved down the ramp.
        let dimmed = step_visual(PlanStepState::Started, PULSE_PERIOD_MS / 2, true);
        assert_ne!(bright.text.fg, dimmed.text.fg, "the pulse must move");
        for v in [bright, dimmed] {
            assert_eq!(v.glyph, glyph::RUNNING);
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
            assert_eq!(v.text.fg, Some(token::TEXT), "at {now_ms}ms");
        }
    }

    /// Done: a checkmark on the green indicator, and the title stays in the
    /// primary tone with a strikethrough — not demoted to grey.
    #[test]
    fn a_complete_step_is_a_check_on_green_with_struck_primary_text() {
        let v = step_visual(PlanStepState::Complete, 0, true);
        assert_eq!(v.glyph, glyph::DONE);
        assert_eq!(v.ring.bg, Some(token::GREEN), "the indicator cell is green");
        assert_eq!(v.text.fg, Some(token::TEXT), "the text stays bright");
        assert!(v.text.add_modifier.contains(Modifier::CROSSED_OUT));
        assert!(v.num.add_modifier.contains(Modifier::CROSSED_OUT));
    }

    /// Blocked or cancelled: the whole row is painted red, with the ✗ cut out
    /// of the indicator in the ground colour.
    #[test]
    fn a_blocked_step_paints_the_entire_row_red_with_a_ground_cross() {
        let v = step_visual(PlanStepState::Blocked, 0, true);
        assert_eq!(v.glyph, glyph::FAILED);
        for style in [v.ring, v.num, v.text, v.meta, v.gap] {
            assert_eq!(style.bg, Some(token::RED), "the row is red end to end");
            assert_eq!(style.fg, Some(token::BG));
        }
    }

    /// A gate task waiting on its checks: SPEC 4's `◇` in gold, and a row that
    /// still reads as live work.
    ///
    /// Gold rather than green is the whole point — a verify row that already
    /// looked like a pass would be the surface answering a question only the
    /// gate can answer (SPEC 7.1).
    #[test]
    fn a_verify_step_is_a_gold_diamond_over_a_live_row() {
        let v = step_visual(PlanStepState::Verify, 0, true);
        assert_eq!(v.glyph, glyph::GATE);
        assert_eq!(v.ring.fg, Some(token::GOLD), "the gate diamond is gold");
        assert_ne!(
            v.ring.fg,
            Some(token::GREEN),
            "the checks have not answered"
        );
        assert_eq!(v.ring.bg, None, "no filled cell — nothing has passed yet");
        assert_eq!(v.text.fg, Some(token::TEXT));
        assert_eq!(v.num.fg, v.text.fg, "the number reads with the title");
    }

    /// A step the approved plan did not contain: SPEC 7.3's `⌥` in
    /// gold-bright, with the `inserted` tag in the same metal so the mark and
    /// its word read as one signal.
    #[test]
    fn a_drift_inserted_step_is_gold_bright_glyph_and_tag() {
        let v = step_visual(PlanStepState::DriftInserted, 0, true);
        assert_eq!(v.glyph, glyph::DRIFT);
        assert_eq!(v.ring.fg, Some(token::GOLD_BRIGHT));
        assert_eq!(
            v.meta.fg,
            Some(token::GOLD_BRIGHT),
            "the tag carries it too"
        );
        assert_eq!(v.text.fg, Some(token::TEXT), "the work itself is ordinary");
        assert_eq!(v.ring.bg, None);
    }

    /// The six states draw six different glyphs, every one of them from the
    /// SPEC 4 table. Nothing else in this module would notice two states
    /// collapsing onto one mark — which is exactly how `✗` came to mean both
    /// *blocked* and *cancelled*.
    #[test]
    fn every_state_draws_a_distinct_glyph_from_the_spec_table() {
        let states = [
            PlanStepState::Planned,
            PlanStepState::Started,
            PlanStepState::Verify,
            PlanStepState::Complete,
            PlanStepState::Blocked,
            PlanStepState::DriftInserted,
        ];
        let drawn: Vec<char> = states
            .iter()
            .map(|s| step_visual(*s, 0, false).glyph)
            .collect();
        let unique: std::collections::BTreeSet<char> = drawn.iter().copied().collect();
        assert_eq!(
            unique.len(),
            states.len(),
            "two states share a glyph: {drawn:?}"
        );
        for g in drawn {
            assert!(
                glyph::ALL.iter().any(|(_, c)| *c == g),
                "{g:?} was typed in here rather than taken from the SPEC 4 table"
            );
        }
    }
}
