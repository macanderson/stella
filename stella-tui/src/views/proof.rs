// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The PROOF rail — [`crate::proof::ProofState`] drawn as a card under the
//! transcript, **on the turns where it has something to say**.
//!
//! Deliberately *not* a scrolling region and *not* a tab. Verification is the
//! claim stella makes about its own output, and a claim the user has to go
//! looking for is a claim they will not check. The transcript says what the
//! agent did; the rail says what has been established about it.
//!
//! # Why the height is no longer constant
//!
//! It used to be: [`ROWS`] + 2, from the first proof step to the end of the
//! turn, so the transcript could never reflow as the proof filled in. That is
//! a real property and giving it up was a real cost — paid for one reason.
//!
//! The rail's gate was `is_empty()`, and [`crate::proof::ProofState::is_empty`]
//! goes false on [`stella_protocol::ProofStep::Assurance`] — the first step
//! triage emits, before any work exists. So on the single most common turn
//! shape (triage waives the witness) the deck spent seven of its thirteen
//! content rows, for the entire turn, rendering five dashes and a border that
//! together mean *nothing was owed here*. A panel that is always up is a panel
//! nobody reads, and it was crowding out the one surface that is always worth
//! reading.
//!
//! So the rail is now **relevance-gated**: up only when
//! [`ProofState::is_notable`] says the proof carries news, and then only as
//! tall as the rows that carry it ([`ProofState::notable_rows`]). On a quiet
//! turn it costs zero rows and the one-line state strip
//! ([`crate::views::work_rail`]) carries the summary instead. The reflow that
//! buys this is bounded — at most a few transitions per turn, each of them a
//! moment when the proof genuinely changed — and `⌃S` opens the full
//! five-row rail whenever the reader wants it regardless.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::proof::{ProofRow, ProofState};
use crate::textline::Tone;
use crate::theme;

/// Content rows the *full* rail draws (warrant, witness, oracle, tamper,
/// verdict). The promoted band shows a subset; the `⌃S` overlay shows all of
/// them.
pub const ROWS: u16 = 5;

/// Total band height including the border, or 0 when the rail has no news —
/// a greeting, a lookup, a turn triage waived, or one still proving cleanly.
pub fn band_height(state: &ProofState) -> u16 {
    if !state.is_notable() {
        return 0;
    }
    // `notable_rows` is never empty (it falls back to the whole rail), so this
    // can never produce a bordered card with no interior.
    state.notable_rows().len() as u16 + 2
}

/// Draw the promoted rail — the rows that carry news, and a hint at the rest.
pub fn render(state: &ProofState, area: Rect, buf: &mut Buffer) {
    let hidden = ROWS as usize - state.notable_rows().len();
    let hint = if hidden > 0 {
        format!(" ⌃S full rail · {hidden} quiet ")
    } else {
        " ⌃S detail ".to_string()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(state))
        // The title states the rail's job, not its name: a user who has never
        // read the witness-protocol docs still learns what these rows are for.
        .title(" PROOF — what has been established about this work ")
        .title_bottom(
            Line::from(Span::styled(hint, Style::new().fg(theme::TEXT_TERTIARY))).right_aligned(),
        );
    Paragraph::new(rail_lines(&state.notable_rows()))
        .block(block)
        .render(area, buf);
}

/// Labels dim and column-aligned; only the values carry tone, so the eye lands
/// on what changed rather than on the scaffolding. Shared with the `⌃S`
/// overlay so the promoted band and the full rail render identically.
pub fn rail_lines(rows: &[ProofRow]) -> Vec<Line<'static>> {
    rows.iter()
        .map(|row| {
            Line::from(vec![
                Span::styled(format!(" {:<9}", row.label), theme::muted()),
                Span::styled(row.value.clone(), style_for(row.tone)),
            ])
        })
        .collect()
}

/// The border tracks the rail's worst row: an unproven turn or a failed
/// verdict is legible from the frame alone, before any text is read.
fn border_style(state: &ProofState) -> Style {
    let tones: Vec<Tone> = state.rows().into_iter().map(|r| r.tone).collect();
    if tones.contains(&Tone::Error) {
        Style::new().fg(theme::DANGER)
    } else if tones.contains(&Tone::Warn) {
        Style::new().fg(theme::WARN)
    } else if state.flip.achieved() {
        Style::new().fg(theme::SUCCESS)
    } else {
        theme::muted()
    }
}

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
    use stella_protocol::{ProofStep, ProofTree};

    /// Draw the rail at exactly the height it asks for, which is the only
    /// height it is ever given in production.
    fn draw(state: &ProofState) -> Buffer {
        let area = Rect::new(0, 0, 64, band_height(state).max(3));
        let mut buf = Buffer::empty(area);
        render(state, area, &mut buf);
        buf
    }

    /// A turn whose witness was warranted and could not be produced — the
    /// canonical notable state, and the one the rail exists for.
    fn unproven() -> ProofState {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Warrant {
            required: true,
            reason: None,
            diff_lines: 41,
        });
        state.apply(&ProofStep::WitnessUnavailable {
            reason: "no author independent of the worker".into(),
        });
        state
    }

    /// Buffer-not-ANSI, like the rest of the deck's render tests: assert on
    /// the cells, never on escape sequences.
    fn text_of(buf: &Buffer) -> String {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn an_empty_rail_claims_no_height() {
        assert_eq!(band_height(&ProofState::default()), 0);
    }

    /// **The change this module exists for.** A turn triage waived reports a
    /// complete, correct, entirely uninteresting rail — and must not spend a
    /// single row of the frame on it.
    #[test]
    fn a_waived_turn_raises_no_band_at_all() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Assurance {
            witness: false,
            judge: false,
        });
        assert!(!state.is_empty(), "the rail has folded a step");
        assert_eq!(
            band_height(&state),
            0,
            "…and still claims no height, because it has no news"
        );
        // And that stays true once the turn is over, which is when the old
        // gate was at its worst: five resolved-but-empty rows, forever.
        state.finish();
        assert_eq!(band_height(&state), 0);
    }

    /// A turn proving itself cleanly is also not news. The band appears when
    /// something needs looking at, not when verification is merely happening.
    #[test]
    fn a_clean_proof_stays_collapsed() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Warrant {
            required: true,
            reason: None,
            diff_lines: 41,
        });
        state.apply(&ProofStep::WitnessAuthored {
            path: "tests/clear_reset.rs".into(),
            command: "cargo test clear_reset".into(),
            fingerprint: "sha256:9f3c1d2e4a5b6c7d".into(),
        });
        state.apply(&ProofStep::Oracle {
            command: "cargo test clear_reset".into(),
            passed: false,
            tree: ProofTree::Baseline,
        });
        assert_eq!(band_height(&state), 0, "nothing has gone wrong yet");
        state.apply(&ProofStep::Oracle {
            command: "cargo test clear_reset".into(),
            passed: true,
            tree: ProofTree::Candidate,
        });
        assert!(state.flip.achieved());
        assert_eq!(
            band_height(&state),
            0,
            "a clean flip is good news, not news"
        );
    }

    /// The band tracks its own contents: it is exactly as tall as the rows
    /// that have something to say, plus its border.
    #[test]
    fn the_band_is_as_tall_as_the_news_it_carries() {
        let state = unproven();
        assert_eq!(band_height(&state) as usize, state.notable_rows().len() + 2);
        assert!(
            band_height(&state) < ROWS + 2,
            "a filtered rail must be shorter than the full one"
        );
    }

    /// The whole promoted rail, exactly as a user sees it. A golden because
    /// the value of this surface is that it reads as one glance — a change
    /// that quietly makes it wordier or misaligns the label column is a
    /// regression no per-row assertion would catch.
    #[test]
    fn the_promoted_rail_reads_as_one_glance() {
        let expected = "\
┌ PROOF — what has been established about this work ───────────┐
│ warrant  required · 41 changed lines                         │
│ witness  unavailable · no author independent of the worker   │
│ verdict  pending                                             │
└────────────────────────────────────── ⌃S full rail · 2 quiet ┘";
        assert_eq!(text_of(&draw(&unproven())), expected);
    }

    /// The quiet rows are hidden, not lost — the band says how many and where
    /// to find them, so the filter can never read as "the rail only has three
    /// rows".
    #[test]
    fn the_band_names_the_rows_it_is_hiding() {
        let text = text_of(&draw(&unproven()));
        assert!(text.contains("⌃S full rail · 2 quiet"), "{text}");
    }

    /// An unproven turn must be legible from the frame, not only the text.
    #[test]
    fn an_unavailable_witness_warns_on_the_border() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Warrant {
            required: true,
            reason: None,
            diff_lines: 9,
        });
        state.apply(&ProofStep::WitnessUnavailable {
            reason: "no independent author".into(),
        });
        assert_eq!(border_style(&state).fg, Some(theme::WARN));
    }
}
