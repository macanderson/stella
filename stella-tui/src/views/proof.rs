// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The PROOF rail — [`crate::proof::ProofState`] drawn as a pinned card under
//! the transcript.
//!
//! Deliberately *not* a scrolling region and *not* a tab. Verification is the
//! claim stella makes about its own output, and a claim the user has to go
//! looking for is a claim they will not check. The card sits below the work,
//! at a fixed height, for the whole turn: the transcript says what the agent
//! did, the rail says what has been established about it, and the two are read
//! together.
//!
//! Height is constant at [`ROWS`] + 2 whenever the rail is up, so the
//! transcript never reflows as the proof fills in — the one thing that would
//! make a live panel worse than a summary at the end.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use crate::proof::ProofState;
use crate::textline::Tone;
use crate::theme;

/// Content rows the rail always draws (warrant, witness, oracle, tamper,
/// verdict). Fixed so the band's height never depends on progress.
pub const ROWS: u16 = 5;

/// Total band height including the border, or 0 when there is no proof to
/// show — a greeting, a lookup, or a turn that never reached verification.
pub fn band_height(state: &ProofState) -> u16 {
    if state.is_empty() { 0 } else { ROWS + 2 }
}

/// Draw the rail. Labels are dim and column-aligned; only the values carry
/// tone, so the eye lands on what changed rather than on the scaffolding.
pub fn render(state: &ProofState, area: Rect, buf: &mut Buffer) {
    let rows: Vec<Line<'static>> = state
        .rows()
        .into_iter()
        .map(|row| {
            Line::from(vec![
                Span::styled(format!(" {:<9}", row.label), theme::muted()),
                Span::styled(row.value, style_for(row.tone)),
            ])
        })
        .collect();

    // The title states the rail's job, not its name: a user who has never read
    // the witness-protocol docs still learns what these five rows are for.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(state))
        .title(" PROOF — what has been established about this work ");
    Paragraph::new(rows).block(block).render(area, buf);
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

fn style_for(tone: Tone) -> Style {
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

    fn draw(state: &ProofState) -> Buffer {
        let area = Rect::new(0, 0, 64, ROWS + 2);
        let mut buf = Buffer::empty(area);
        render(state, area, &mut buf);
        buf
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

    #[test]
    fn the_rail_keeps_one_height_as_the_proof_fills_in() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Warrant {
            required: true,
            reason: None,
            diff_lines: 41,
        });
        let early = band_height(&state);
        state.apply(&ProofStep::WitnessAuthored {
            path: "tests/w.rs".into(),
            command: "cargo test w".into(),
            fingerprint: "sha256:abc".into(),
        });
        state.apply(&ProofStep::Oracle {
            command: "cargo test w".into(),
            passed: false,
            tree: ProofTree::Baseline,
        });
        assert_eq!(
            early,
            band_height(&state),
            "the transcript must not reflow as proof accumulates"
        );
    }

    /// The witness for the whole surface: the five rows reach the buffer, so a
    /// user watching a turn can see the proof without opening anything.
    #[test]
    fn every_row_reaches_the_buffer() {
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
        let text = text_of(&draw(&state));
        for expected in [
            "PROOF",
            "warrant",
            "41 changed lines",
            "witness",
            "clear_reset.rs",
            "oracle",
            "tamper",
            "verdict",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }

    /// The whole rail, exactly as a user sees it mid-flight. A golden because
    /// the value of this surface is that it reads as one glance — a change
    /// that quietly makes it wordier or misaligns the label column is a
    /// regression no per-row assertion would catch.
    #[test]
    fn the_rail_reads_as_one_glance() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Warrant {
            required: true,
            reason: None,
            diff_lines: 41,
        });
        state.apply(&ProofStep::WitnessAuthored {
            path: "tests/clear_reset.rs".into(),
            command: "cargo test clear_reset".into(),
            fingerprint: "sha256:9f3c1d2e4a5b6c7d8e9f".into(),
        });
        state.apply(&ProofStep::Oracle {
            command: "cargo test clear_reset".into(),
            passed: false,
            tree: ProofTree::Baseline,
        });
        let expected = "\
┌ PROOF — what has been established about this work ───────────┐
│ warrant  required · 41 changed lines                         │
│ witness  authored  tests/clear_reset.rs                      │
│ oracle   ✗ fails on base → running on new                    │
│ tamper   pinned  sha256:9f3c1d2e4…                           │
│ verdict  pending                                             │
└──────────────────────────────────────────────────────────────┘";
        assert_eq!(text_of(&draw(&state)), expected);
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
