// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A verify turn's gate board, worded for the surfaces that have one line to
//! say it in (SPEC 8.1).
//!
//! A sibling file rather than more lines in `textline.rs`, which sits a few
//! lines under the 1500-line ratchet: that file gains a constructor per event
//! it words, so one with anything to explain brings its own file. Both
//! surfaces read it through the parent's re-export, so nothing outside this
//! crate learns the wording moved.

use super::{EventLine, Tone};

/// One line for a verify turn's gate board.
///
/// The plain surface gets the header and the names of the gates that failed,
/// and nothing else — the excerpt, the keys and the tinted row are the deck's
/// affordances, and a `println` surface has no `l` to press. Naming the failed
/// gates rather than only the count is what makes the line actionable on a door
/// that cannot be scrolled back through.
///
/// `Tone::Error` only on a determinate failure. A board carrying an undecided
/// gate is `Tone::Warn`: not a pass, and explicitly not a failure.
#[must_use]
pub fn gate_board(board: &stella_protocol::GateBoard) -> EventLine {
    let failed: Vec<&str> = board
        .gates
        .iter()
        .filter(|gate| gate.failed())
        .map(|gate| gate.name.as_str())
        .collect();
    let patch = board
        .patch
        .as_deref()
        .map(|patch| format!(" {patch}"))
        .unwrap_or_default();
    let all_green = board.total() > 0 && board.green() == board.total();
    EventLine {
        glyph: if failed.is_empty() { "◇" } else { "✗" },
        tone: match (failed.is_empty(), all_green) {
            (false, _) => Tone::Error,
            (true, true) => Tone::Success,
            (true, false) => Tone::Warn,
        },
        strong: false,
        body: format!("gate board{patch}:"),
        detail: Some(if failed.is_empty() {
            format!("{}/{} green", board.green(), board.total())
        } else {
            format!(
                "{}/{} green — {} failed",
                board.green(),
                board.total(),
                failed.join(", ")
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::{GateBoard, GateRow, GateState};

    fn board(states: Vec<GateState>) -> GateBoard {
        GateBoard {
            patch: Some("patch-7".into()),
            gates: states
                .into_iter()
                .enumerate()
                .map(|(i, state)| GateRow {
                    name: format!("gate-{i}"),
                    state,
                    deterministic: true,
                })
                .collect(),
        }
    }

    /// The line names which gate went red, because the door that reads it
    /// cannot scroll back to the board to find out.
    #[test]
    fn a_failed_board_names_the_gate_that_failed() {
        let line = gate_board(&board(vec![
            GateState::Green,
            GateState::Failed {
                case: "no fail→pass flip".into(),
                log: String::new(),
            },
        ]));
        assert_eq!(line.tone, Tone::Error);
        assert_eq!(line.glyph, "✗");
        assert_eq!(line.body, "gate board patch-7:");
        assert_eq!(line.detail.as_deref(), Some("1/2 green — gate-1 failed"));
    }

    /// An abstention is warned about, never reported as a failure and never as
    /// a pass — the three-answer rule, at the one-line granularity.
    #[test]
    fn an_undecided_board_warns_rather_than_failing() {
        let line = gate_board(&board(vec![
            GateState::Green,
            GateState::Undecided {
                reason: "no snapshot to compare".into(),
            },
        ]));
        assert_eq!(line.tone, Tone::Warn);
        assert_eq!(line.glyph, "◇");
        assert_eq!(line.detail.as_deref(), Some("1/2 green"));

        let clean = gate_board(&board(vec![GateState::Green]));
        assert_eq!(clean.tone, Tone::Success);
        assert_eq!(clean.detail.as_deref(), Some("1/1 green"));
    }
}
