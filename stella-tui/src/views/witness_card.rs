//! The witness panel (`/witness`): the deterministic proof, as three records
//! in strict order — author → execute → result — exactly one active at a
//! time, folded from the same `AgentEvent::Proof` / `JudgeVerdict` stream the
//! proof rail reads.
//!
//! ```text
//! ✓  author   — test built before the patch                        0:38
//!    oracle: pnpm --filter app test:unit · confirmed red pre-patch
//! ▸  execute  — replaying oracle against patched worktree          1:12
//!    run 2 of 3 · deterministic seed 7741 · evidence → witness only, no self-report
//! ○  result   — awaiting flip
//!    red ──▸ green · 3/3 required
//! ```
//!
//! This panel is the staged pipeline's surface — the redesign of what the
//! statline's `PIPELINE ON/OFF` box used to gesture at. `/pipeline` remains
//! the toggle (a driver command); `/witness` opens this view of what the
//! armed pipeline is actually proving, and when the pipeline is off the
//! panel says so instead of implying a witness is coming.
//!
//! ## Copy law (D6)
//!
//! The framing is deterministic proof: author / execute / result, oracle,
//! evidence, flip. The internal `judge` role identifier never reaches a
//! string this panel renders. **Red appears in exactly one place in the
//! deck** — the oracle's pre-flip state: the `red` token in the author line
//! and the `red ──▸ green` result line, both in [`theme::ORACLE_PRE_FLIP`]
//! (its own role, deliberately not [`theme::DANGER`]). A refused flip
//! (tamper, or a pass that fixed a different failure) is a *distinct result
//! state*, worded and toned as such — never a generic failure.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::deck::{AgentEntry, PipelineRole, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::proof::{ProofState, WitnessStanding};
use crate::theme;
use crate::views::cards;

/// The three records' lifecycle — exactly one is `Active` at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WitnessPhase {
    Author,
    Execute,
    Result,
}

impl WitnessPhase {
    /// 1-based position, for the statline's `witness <phase n/m>` collapse.
    fn index(self) -> usize {
        match self {
            WitnessPhase::Author => 1,
            WitnessPhase::Execute => 2,
            WitnessPhase::Result => 3,
        }
    }
}

/// Which phase is live, derived purely from the proof fold: authoring until
/// the oracle has confirmed the witness red on the untouched tree, executing
/// until the flip resolves (every required replay in, a refusal, or the
/// verdict), then the result.
pub fn current_phase(proof: &ProofState) -> WitnessPhase {
    if proof.verdict.is_some() || proof.flip_refused || flip_settled(proof) {
        return WitnessPhase::Result;
    }
    if proof.flip.baseline_passed == Some(false) {
        return WitnessPhase::Execute;
    }
    WitnessPhase::Author
}

/// Whether the candidate replays have settled the flip either way.
fn flip_settled(proof: &ProofState) -> bool {
    let flip = &proof.flip;
    match flip.candidate_passed {
        // A failing candidate settles it immediately: no flip.
        Some(false) => flip.baseline_passed == Some(false),
        // A passing candidate settles it once every required replay is in.
        Some(true) => match flip.runs_required {
            Some(required) => flip.candidate_runs >= required,
            None => flip.baseline_passed == Some(false),
        },
        None => false,
    }
}

/// The statline's collapsed witness item: `witness author` / `witness
/// executing 2/3` / `witness result` — phase in words, run fraction when the
/// oracle stated one.
pub fn phase_label(proof: &ProofState) -> String {
    match current_phase(proof) {
        WitnessPhase::Author => "witness author".to_string(),
        WitnessPhase::Execute => match proof.flip.runs_required {
            Some(required) => format!(
                "witness executing {}/{required}",
                proof.flip.candidate_runs.min(required)
            ),
            None => "witness executing".to_string(),
        },
        WitnessPhase::Result => "witness result".to_string(),
    }
}

/// Body rows for the panel (marker glyph + copy per record, detail line
/// under each), styled; the accessible branch linearizes the same facts as
/// labeled records instead.
fn body_rows(
    agent: &AgentEntry,
    now_ms: u64,
    inner_w: usize,
    accessible: bool,
) -> Vec<Line<'static>> {
    let proof = &agent.model.proof;
    let phase = current_phase(proof);
    let stamps = agent.witness_phase_ms;
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let primary = Style::new().fg(theme::TEXT_PRIMARY);
    let red = Style::new().fg(theme::ORACLE_PRE_FLIP);
    let mut rows: Vec<Line<'static>> = Vec::new();

    let record_glyph = |this: WitnessPhase| -> (Span<'static>, Style) {
        use std::cmp::Ordering::*;
        match this.index().cmp(&phase.index()) {
            Less => (Span::styled("✓  ", Style::new().fg(theme::OK)), primary),
            Equal => (
                Span::styled("▸  ", theme::accent().add_modifier(Modifier::BOLD)),
                primary,
            ),
            Greater => (Span::styled("○  ", dim), dim),
        }
    };

    // ── author ─────────────────────────────────────────────────────────
    let author_desc = if phase == WitnessPhase::Author {
        match &proof.witness {
            Some(WitnessStanding::Unavailable { .. }) => "witness unavailable",
            _ => "authoring the failing test",
        }
    } else {
        "test built before the patch"
    };
    let author_elapsed = stamps
        .author_ms
        .map(|start| cards::fmt_mss(stamps.execute_ms.unwrap_or(now_ms).saturating_sub(start)));
    let oracle_cmd = proof
        .flip
        .command
        .clone()
        .unwrap_or_else(|| "not yet armed".to_string());
    let red_confirmed = proof.flip.baseline_passed == Some(false);
    if accessible {
        let mut row = format!("· phase author · {author_desc}");
        if red_confirmed {
            row.push_str(" · oracle red confirmed pre-patch");
        }
        rows.push(Line::from(Span::styled(row, primary)));
        rows.push(Line::from(Span::styled(
            format!("· oracle {oracle_cmd}"),
            dim,
        )));
    } else {
        let (glyph, style) = record_glyph(WitnessPhase::Author);
        let mut row = vec![
            glyph,
            Span::styled("author", style.add_modifier(Modifier::BOLD)),
            Span::styled(format!("   — {author_desc}"), style),
        ];
        if let Some(elapsed) = author_elapsed {
            row = cards::pad_right(row, Span::styled(elapsed, dim), inner_w);
        }
        rows.push(Line::from(row));
        let mut detail = vec![
            Span::raw("   "),
            Span::styled("oracle: ", dim),
            Span::styled(
                cards::truncate_cols(&oracle_cmd, inner_w.saturating_sub(30)),
                dim,
            ),
        ];
        match &proof.witness {
            Some(WitnessStanding::Unavailable { reason }) => {
                detail.push(Span::styled(
                    format!(" · unavailable — {}", cards::truncate_cols(reason, 24)),
                    Style::new().fg(theme::WARN),
                ));
            }
            _ if red_confirmed => {
                detail.push(Span::styled(" · confirmed ", dim));
                detail.push(Span::styled("red", red));
                detail.push(Span::styled(" pre-patch", dim));
            }
            _ => {}
        }
        rows.push(Line::from(detail));
    }

    // ── execute ────────────────────────────────────────────────────────
    let exec_desc = match phase {
        WitnessPhase::Author => "awaiting the authored witness",
        WitnessPhase::Execute => "replaying oracle against patched worktree",
        WitnessPhase::Result => "oracle replayed against patched worktree",
    };
    let exec_elapsed = stamps
        .execute_ms
        .map(|start| cards::fmt_mss(stamps.result_ms.unwrap_or(now_ms).saturating_sub(start)));
    let run_part = match (proof.flip.candidate_runs, proof.flip.runs_required) {
        (0, _) => None,
        (n, Some(required)) => Some(format!("run {n} of {required}")),
        (n, None) => Some(format!("run {n}")),
    };
    let seed_part = proof
        .flip
        .seed
        .map(|seed| format!("deterministic seed {seed}"));
    if accessible {
        let mut row = format!("· phase execute · {exec_desc}");
        if let Some(runs) = &run_part {
            row.push_str(&format!(" · {runs}"));
        }
        if let Some(seed) = &seed_part {
            row.push_str(&format!(" · {seed}"));
        }
        rows.push(Line::from(Span::styled(row, primary)));
        rows.push(Line::from(Span::styled(
            "· evidence to witness only · no self-report".to_string(),
            dim,
        )));
    } else {
        let (glyph, style) = record_glyph(WitnessPhase::Execute);
        let mut row = vec![
            glyph,
            Span::styled("execute", style.add_modifier(Modifier::BOLD)),
            Span::styled(format!("  — {exec_desc}"), style),
        ];
        if let Some(elapsed) = exec_elapsed {
            row = cards::pad_right(row, Span::styled(elapsed, dim), inner_w);
        }
        rows.push(Line::from(row));
        let mut parts: Vec<String> = Vec::new();
        parts.extend(run_part);
        parts.extend(seed_part);
        parts.push("evidence → witness only, no self-report".to_string());
        rows.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(parts.join(" · "), dim),
        ]));
    }

    // Tamper exclusion, when the pipeline stated it: a dim confirmation
    // under execute that the flip's credit rests on untouched witness files.
    if proof.witness_intact == Some(true) {
        rows.push(Line::from(vec![
            Span::raw(if accessible { "" } else { "   " }),
            Span::styled(
                if accessible {
                    "· tamper check passed · witness files untouched".to_string()
                } else {
                    "tamper check ✓ witness files untouched".to_string()
                },
                dim,
            ),
        ]));
    }

    // ── result ─────────────────────────────────────────────────────────
    let required = proof.flip.runs_required.unwrap_or(1);
    let (result_desc, result_detail, result_tone): (&str, Vec<Span<'static>>, Style) =
        if proof.flip_refused {
            // The distinct refused state: the pass fixed a different failure,
            // so the flip earns no credit — that is a statement about the
            // evidence, not a generic failure.
            (
                "flip refused",
                vec![Span::styled(
                    "the pass fixed a different failure · no credit".to_string(),
                    Style::new().fg(theme::WARN),
                )],
                Style::new().fg(theme::WARN),
            )
        } else if proof.unstable_flip {
            (
                "flip unstable",
                vec![Span::styled(
                    "the confirmation replay did not pass".to_string(),
                    Style::new().fg(theme::WARN),
                )],
                Style::new().fg(theme::WARN),
            )
        } else if phase == WitnessPhase::Result && proof.flip.achieved() {
            (
                "oracle flipped",
                vec![
                    Span::styled("red", red),
                    Span::styled(" ──▸ ", dim),
                    Span::styled("green", Style::new().fg(theme::OK)),
                    Span::styled(
                        format!(" · {required}/{required} observed"),
                        Style::new().fg(theme::OK),
                    ),
                ],
                Style::new().fg(theme::OK),
            )
        } else if phase == WitnessPhase::Result {
            (
                "no flip",
                vec![Span::styled(
                    "still fails on the patched worktree".to_string(),
                    Style::new().fg(theme::WARN),
                )],
                Style::new().fg(theme::WARN),
            )
        } else {
            (
                "awaiting flip",
                vec![
                    Span::styled("red", red),
                    Span::styled(" ──▸ ", dim),
                    Span::styled("green", dim),
                    Span::styled(format!(" · {required}/{required} required"), dim),
                ],
                dim,
            )
        };
    if accessible {
        let oracle_word = if proof.flip.achieved() && phase == WitnessPhase::Result {
            "green"
        } else {
            "red"
        };
        rows.push(Line::from(Span::styled(
            format!("· phase result · {result_desc} · oracle {oracle_word} · required {required} of {required}"),
            primary,
        )));
    } else {
        let (glyph, style) = record_glyph(WitnessPhase::Result);
        rows.push(Line::from(vec![
            glyph,
            Span::styled("result", style.add_modifier(Modifier::BOLD)),
            Span::styled(format!("   — {result_desc}"), result_tone),
        ]));
        let mut detail: Vec<Span<'static>> = vec![Span::raw("   ")];
        detail.extend(result_detail);
        rows.push(Line::from(detail));
    }

    rows
}

/// Render the witness panel over `frame` for the focused agent.
pub fn render(model: &WorkspaceModel, ui: &DeckUi, frame: Rect, buf: &mut Buffer) {
    let Some(agent) = model.agents.get(ui.focused) else {
        return;
    };
    let mut rows = body_rows(agent, model.now_ms, CARD_INNER_W, ui.accessible);
    // The pipeline surface half: when staged turns are off, this panel is
    // where that fact lives now that the statline box is gone.
    if !model.pipeline {
        rows.push(Line::default());
        rows.push(Line::from(Span::styled(
            if ui.accessible {
                "· staged pipeline off · /pipeline arms witness-verified turns".to_string()
            } else {
                "staged pipeline off · /pipeline arms witness-verified turns".to_string()
            },
            Style::new().fg(theme::TEXT_TERTIARY),
        )));
    }
    let area = cards::card_area(frame, rows.len() as u16);
    // Right title: the verify slot's model id (the role that replays the
    // oracle) + where it runs. The internal role identifier never renders —
    // this panel's label for it is `verify`.
    let verify_model = model
        .role_pins
        .get(&PipelineRole::Judge)
        .map(|pin| pin.model.clone())
        .unwrap_or_else(|| "verify unassigned".to_string());
    let inner = cards::card_frame(
        area,
        "witness",
        vec![Span::styled(
            "run ends only on oracle flip",
            Style::new().fg(theme::TEXT_TERTIARY),
        )],
        &format!("{verify_model} · shadow sandbox"),
        buf,
    );
    cards::render_body(rows, None, inner, buf);
}

/// The inner width the body rows are laid out against — the max card width
/// minus its two border columns. Rows are re-clipped by the real rect at
/// paint time, so a narrower terminal only loses the right edge.
const CARD_INNER_W: usize = (cards::CARD_MAX_W - 2) as usize;

#[cfg(test)]
mod tests {
    use super::*;
    use stella_protocol::{ProofStep, ProofTree};

    fn proof_with(steps: &[ProofStep]) -> ProofState {
        let mut p = ProofState::default();
        for s in steps {
            p.apply(s);
        }
        p
    }

    fn oracle(passed: bool, tree: ProofTree, run: Option<u32>) -> ProofStep {
        ProofStep::Oracle {
            command: "cargo test -p x".into(),
            passed,
            tree,
            run,
            runs_required: Some(3),
            seed: Some(7741),
        }
    }

    #[test]
    fn the_phase_walks_author_execute_result_in_order() {
        let mut p = ProofState::default();
        assert_eq!(current_phase(&p), WitnessPhase::Author);
        p.apply(&oracle(false, ProofTree::Baseline, None));
        assert_eq!(current_phase(&p), WitnessPhase::Execute, "red pre-patch");
        p.apply(&oracle(true, ProofTree::Candidate, Some(1)));
        p.apply(&oracle(true, ProofTree::Candidate, Some(2)));
        assert_eq!(
            current_phase(&p),
            WitnessPhase::Execute,
            "2 of 3 replays in — still executing"
        );
        p.apply(&oracle(true, ProofTree::Candidate, Some(3)));
        assert_eq!(current_phase(&p), WitnessPhase::Result, "3/3 settles it");
    }

    #[test]
    fn the_statline_collapse_label_carries_the_run_fraction() {
        let p = proof_with(&[
            oracle(false, ProofTree::Baseline, None),
            oracle(true, ProofTree::Candidate, Some(2)),
        ]);
        assert_eq!(phase_label(&p), "witness executing 2/3");
    }

    #[test]
    fn a_failing_candidate_resolves_to_result_immediately() {
        let p = proof_with(&[
            oracle(false, ProofTree::Baseline, None),
            oracle(false, ProofTree::Candidate, Some(1)),
        ]);
        assert_eq!(
            current_phase(&p),
            WitnessPhase::Result,
            "no flip is settled"
        );
    }
}
