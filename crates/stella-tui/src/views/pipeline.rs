// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! SPEC 5's pipeline line — the first row of the prompt block (#5050).
//!
//! ```text
//! ✓ plan ▸ execute ██████░░░░░░ 50% · verify
//! ```
//!
//! Where the turn has got to, in the host's own three phases: the ones it has
//! passed marked `✓`, the one it is in lit gold behind the meter, the ones
//! ahead dim. The stage word is on the status bar and on each turn's opening
//! rule; neither says how far through a turn that stage is, which is the one
//! fact this row adds.
//!
//! ## Three phases, and where they come from
//!
//! [`StageKind::ALL`] is the twelve boundaries this host emits, in turn order,
//! and its doc names itself the list a renderer takes a reading order from.
//! Twelve is too many to draw and most of them never announce themselves on a
//! given run, so [`Phase`] groups them into three contiguous runs of that same
//! list — deciding, doing, proving. Contiguous is the property that matters:
//! the phase is monotonic in `ALL`'s order, so the meter can only ever move
//! forwards, and `the_phase_never_goes_backwards_along_the_host_order` holds
//! it there.
//!
//! The meter reads `50%` in `execute` because a turn in the middle phase of
//! three is halfway through the host's shape — position, not a prediction of
//! work remaining, which nothing here can know.
//!
//! ## Why it can be blank, and why that is the point
//!
//! The row draws only once the turn has announced a host stage. A plain
//! `stella run` is the raw step-loop and emits no stage boundaries at all
//! (AGENTS.md's opening), and the staged pipeline that used to emit them is
//! deleted from this workspace (#3865) — so on most runs there is no pipeline
//! to draw and this row stays air.
//!
//! That is the same refusal [`crate::views::status_source`]'s stage cell
//! already makes when it renders `—` rather than the word `idle`: a stepper
//! that drew `plan ▸ execute ▸ verify` over a run emitting none of them would
//! be describing a pipeline the binary cannot run. The phases ahead are drawn
//! dim rather than absent because a turn may settle before it reaches them —
//! the row says where the turn has got, never where it will get to.
//!
//! ## Which row
//!
//! [`crate::views::pulse`] and this share the band above the composer, and
//! they never contend for it: pulse holds the row off SESSION and at an
//! opened lane, and returns `None` on SESSION at the lead — the state SPEC 5's
//! prompt block is drawn in — where this row draws instead. Nothing moved and
//! nothing is displaced; the pipeline line took air that was already there.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};
use stella_protocol::StageKind;
use stella_tui_theme::{glyph, token};

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;

/// The width of the pipeline row's meter, in cells.
///
/// Narrower than the status bar's twelve: this meter divides three phases, so
/// its whole range is three readings and a wider bar would spend columns
/// implying a precision the phases do not carry.
const METER_CELLS: usize = 8;

/// One of the host's three turn phases — a contiguous run of [`StageKind::ALL`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Phase {
    /// Deciding what to do: triage, context recall, research, plan, scope review.
    Plan,
    /// Doing it: the witness that will prove it, then the work itself.
    Execute,
    /// Proving and recording it: verify, verdict, reflect, context write, complete.
    Verify,
}

impl Phase {
    /// The three, in turn order.
    pub const ALL: [Phase; 3] = [Phase::Plan, Phase::Execute, Phase::Verify];

    /// The word the row prints, which is SPEC 5's own.
    pub const fn label(self) -> &'static str {
        match self {
            Phase::Plan => "plan",
            Phase::Execute => "execute",
            Phase::Verify => "verify",
        }
    }

    /// The phase a host boundary belongs to.
    ///
    /// Exhaustive over [`StageKind`] on purpose: a thirteenth boundary must
    /// not compile until somebody decides which phase it is in, because the
    /// alternative is a stage that silently reads as `plan` and makes the
    /// meter claim the turn went backwards.
    pub const fn of(kind: StageKind) -> Phase {
        match kind {
            StageKind::Triage
            | StageKind::ContextRecall
            | StageKind::Research
            | StageKind::Plan
            | StageKind::ScopeReview => Phase::Plan,
            StageKind::Witness | StageKind::Execute => Phase::Execute,
            StageKind::Verify
            | StageKind::Verdict
            | StageKind::Reflect
            | StageKind::ContextWrite
            | StageKind::Complete => Phase::Verify,
        }
    }

    /// How far through the host's shape a turn in this phase is: `0.0` in the
    /// first, `1.0` in the last.
    ///
    /// Position among the phases, which is why `execute` reads the `50%` SPEC
    /// 5 writes. Work remaining is not knowable — a turn can settle in any
    /// phase — so the meter does not pretend to measure it.
    fn ratio(self) -> f64 {
        let last = Phase::ALL.len() - 1;
        Phase::ALL.iter().position(|p| *p == self).unwrap_or(0) as f64 / last as f64
    }
}

/// What the row says, or `None` when the turn has announced no host stage and
/// the row stays air.
///
/// Reads `Hud::host_stage` rather than `Hud::stage` for the reason that field
/// exists: a contributed stage leaves it alone, so a plugin boundary arriving
/// mid-execute cannot make the meter snap back to `plan` and claim the turn
/// regressed.
pub fn pipeline(model: &WorkspaceModel, ui: &DeckUi) -> Option<Phase> {
    if ui.tab != crate::deck::DeckTab::Session {
        return None;
    }
    let entry = model.agents.get(ui.focused)?;
    // At an opened lane the row belongs to pulse, which carries the lead there
    // so walking into a lane never costs sight of the conversation.
    if entry.is_subagent() {
        return None;
    }
    entry.model.hud.host_stage.map(Phase::of)
}

/// Draw the row into the top row of `area`. Draws nothing when the row should
/// stay air.
pub fn render_row(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some(current) = pipeline(model, ui) else {
        return;
    };
    Paragraph::new(Line::from(spans(current))).render(Rect { height: 1, ..area }, buf);
}

/// The row's spans for a turn in `current`.
fn spans(current: Phase) -> Vec<Span<'static>> {
    let done = Style::new().fg(token::MUTED);
    let lit = Style::new().fg(token::GOLD).add_modifier(Modifier::BOLD);
    let ahead = Style::new().fg(token::DIM);
    let mut spans = vec![Span::raw(" ")];
    for phase in Phase::ALL {
        match phase.cmp(&current) {
            std::cmp::Ordering::Less => {
                spans.push(Span::styled(format!("{} ", glyph::DONE), done));
                spans.push(Span::styled(phase.label(), done));
                spans.push(Span::raw(" "));
            }
            std::cmp::Ordering::Equal => {
                spans.push(Span::styled(format!("{} ", glyph::COLLAPSED), lit));
                spans.push(Span::styled(phase.label(), lit));
                spans.push(Span::raw(" "));
                spans.extend(super::status_bar::meter(phase.ratio(), METER_CELLS));
                spans.push(Span::styled(
                    format!(" {}%", (phase.ratio() * 100.0).round() as u32),
                    done,
                ));
            }
            std::cmp::Ordering::Greater => {
                spans.push(Span::styled(" · ", ahead));
                spans.push(Span::styled(phase.label(), ahead));
            }
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(current: Phase) -> String {
        spans(current)
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    /// SPEC 5's own line: a turn mid-execute has plan behind it, execute lit
    /// with the meter at halfway, and verify ahead.
    #[test]
    fn a_turn_mid_execute_renders_the_line_spec_5_writes() {
        let row = text(Phase::Execute);
        assert!(row.starts_with(" ✓ plan ▸ execute "), "{row}");
        assert!(row.contains("50%"), "{row}");
        assert!(row.trim_end().ends_with("· verify"), "{row}");
    }

    /// Every host boundary lands in a phase, and the phase only ever moves
    /// forwards along [`StageKind::ALL`] — the property that lets the meter be
    /// a position rather than a guess, and the one a thirteenth stage in the
    /// wrong group would break.
    #[test]
    fn the_phase_never_goes_backwards_along_the_host_order() {
        let mut seen = Phase::Plan;
        for kind in StageKind::ALL {
            let phase = Phase::of(kind);
            assert!(
                phase >= seen,
                "{kind:?} maps to {phase:?} after {seen:?}, so the meter would go backwards"
            );
            seen = phase;
        }
        assert_eq!(Phase::of(StageKind::ALL[0]), Phase::Plan);
        assert_eq!(
            Phase::of(StageKind::ALL[StageKind::ALL.len() - 1]),
            Phase::Verify
        );
    }

    /// The meter is a position among three phases, so it reads 0, 50 and 100
    /// and nothing between.
    #[test]
    fn the_meter_reads_the_phases_position() {
        for (phase, pct) in [
            (Phase::Plan, "0%"),
            (Phase::Execute, "50%"),
            (Phase::Verify, "100%"),
        ] {
            assert!(text(phase).contains(pct), "{phase:?} should read {pct}");
        }
    }

    /// The first phase has nothing behind it and the last nothing ahead, so
    /// the row never draws a `✓` or a `·` with no word after it.
    #[test]
    fn the_ends_of_the_ladder_draw_no_empty_marks() {
        let first = text(Phase::Plan);
        assert!(!first.contains('✓'), "nothing is done yet: {first}");
        assert!(first.contains("· execute"), "{first}");
        assert!(first.contains("· verify"), "{first}");

        let last = text(Phase::Verify);
        assert!(last.starts_with(" ✓ plan ✓ execute ▸ verify "), "{last}");
        assert!(!last.contains(" · "), "nothing is ahead: {last}");
    }
}
