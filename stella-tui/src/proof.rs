// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The proof rail: what this turn has established about its own work, folded
//! live from `AgentEvent::Proof` and the verdict that closes it.
//!
//! # Why this is a panel and not transcript lines
//!
//! Every other narration in the TUI scrolls. Proof cannot: the interesting
//! question is never "what did the warrant say four screens ago", it is
//! *"right now, is this run proving what it is doing"* — a small state machine
//! whose current value matters and whose history does not. Rendered as
//! transcript rows the answer is buried under whatever the worker printed
//! since; rendered as a rail it is one glance, beside the work, the whole
//! time.
//!
//! # An absent proof is still a proof
//!
//! The rows are fixed and always present once the rail is up. A warranted
//! witness that could not be produced reads `unavailable`, and a change with
//! nothing to prove reads its stated reason — never a blank. That is the same
//! contract `stella_pipeline::witness::warrant` holds itself to (a test *or*
//! a stated reason there isn't one), carried to the surface a user actually
//! looks at. Silence is the one thing this must never render.
//!
//! Pure: this module folds and formats, and returns [`ProofRow`]s for a
//! renderer to style. No ratatui types, so the fold is unit-testable without a
//! terminal (L-T1, and the buffer-not-ANSI discipline in `deck_ui::tests`).

use stella_protocol::{JudgeEvidence, ProofStep, ProofTree};

use crate::textline::Tone;

/// How the flip oracle stands: what the tracked command did against each of
/// the two code states a witness must span.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Flip {
    /// The normalized command the oracle locked onto.
    pub command: Option<String>,
    /// What it did on the pre-execution tree; `None` until observed.
    pub baseline_passed: Option<bool>,
    /// What it did on the executed tree; `None` until observed.
    pub candidate_passed: Option<bool>,
}

impl Flip {
    /// Whether a genuine fail→pass flip has been observed across the two
    /// trees. Deliberately stricter than "the tests pass": a command that only
    /// ever passed proves the code reacts to nothing.
    pub fn achieved(&self) -> bool {
        self.baseline_passed == Some(false) && self.candidate_passed == Some(true)
    }
}

/// What the witness half of the proof came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WitnessStanding {
    /// An independent model wrote the failing test, and its bytes are pinned.
    Authored { path: String, fingerprint: String },
    /// One was warranted and could not be produced. The work stands; it is
    /// unproven, and the reason is shown rather than swallowed.
    Unavailable { reason: String },
}

/// The verdict that closes the rail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerdictStanding {
    pub passed: bool,
    /// `true` when the deterministic ladder decided it, `false` for a model
    /// judge. Never conflated — the rail says which one spoke (L-E11).
    pub deterministic: bool,
    pub summary: String,
}

/// The whole rail state for one turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProofState {
    /// `Some((required, reason, diff_lines))` once the warrant has read the
    /// diff. `reason` is populated only when no test is warranted.
    pub warrant: Option<(bool, Option<String>, u32)>,
    pub witness: Option<WitnessStanding>,
    pub flip: Flip,
    pub verdict: Option<VerdictStanding>,
}

impl ProofState {
    /// Nothing observed yet — the rail stays hidden rather than showing five
    /// rows of dashes on a turn that never reaches verification (a greeting, a
    /// lookup, a cancelled turn).
    pub fn is_empty(&self) -> bool {
        self.warrant.is_none()
            && self.witness.is_none()
            && self.verdict.is_none()
            && self.flip == Flip::default()
    }

    /// Fold one proof step.
    pub fn apply(&mut self, step: &ProofStep) {
        match step {
            ProofStep::Warrant {
                required,
                reason,
                diff_lines,
            } => self.warrant = Some((*required, reason.clone(), *diff_lines)),
            ProofStep::WitnessAuthored {
                path,
                command,
                fingerprint,
            } => {
                self.witness = Some(WitnessStanding::Authored {
                    path: path.clone(),
                    fingerprint: fingerprint.clone(),
                });
                self.flip.command = Some(command.clone());
            }
            ProofStep::WitnessUnavailable { reason } => {
                self.witness = Some(WitnessStanding::Unavailable {
                    reason: reason.clone(),
                });
            }
            ProofStep::Oracle {
                command,
                passed,
                tree,
            } => {
                self.flip.command = Some(command.clone());
                match tree {
                    ProofTree::Baseline => self.flip.baseline_passed = Some(*passed),
                    ProofTree::Candidate => self.flip.candidate_passed = Some(*passed),
                }
            }
        }
    }

    /// Fold the verdict that closes the turn.
    pub fn apply_verdict(&mut self, passed: bool, evidence: &JudgeEvidence) {
        self.verdict = Some(VerdictStanding {
            passed,
            deterministic: evidence.deterministic,
            summary: evidence.summary.clone(),
        });
    }

    /// The rail's rows, in fixed order. Always five, so the panel does not
    /// reflow as the proof accumulates — a row that has not happened yet reads
    /// `pending`, which is information, not filler.
    pub fn rows(&self) -> Vec<ProofRow> {
        vec![
            self.warrant_row(),
            self.witness_row(),
            self.oracle_row(),
            self.tamper_row(),
            self.verdict_row(),
        ]
    }

    fn warrant_row(&self) -> ProofRow {
        match &self.warrant {
            None => ProofRow::pending("warrant"),
            Some((true, _, lines)) => ProofRow::new(
                "warrant",
                format!("required · {lines} changed lines"),
                Tone::Info,
            ),
            // Not-required is a RESULT, and the reason is the whole of it.
            Some((false, reason, _)) => ProofRow::new(
                "warrant",
                reason
                    .clone()
                    .unwrap_or_else(|| "no test warranted".to_string()),
                Tone::Muted,
            ),
        }
    }

    fn witness_row(&self) -> ProofRow {
        match &self.witness {
            None if self.warrant_says_not_required() => ProofRow::new("witness", "—", Tone::Muted),
            None => ProofRow::pending("witness"),
            Some(WitnessStanding::Authored { path, .. }) => {
                ProofRow::new("witness", format!("authored  {path}"), Tone::Success)
            }
            // A warranted witness that could not be produced is the one row
            // that must shout: the work is finished and NOT proven.
            Some(WitnessStanding::Unavailable { reason }) => {
                ProofRow::new("witness", format!("unavailable · {reason}"), Tone::Warn)
            }
        }
    }

    fn oracle_row(&self) -> ProofRow {
        let (base, cand) = (self.flip.baseline_passed, self.flip.candidate_passed);
        match (base, cand) {
            (None, None) if self.warrant_says_not_required() => {
                ProofRow::new("oracle", "—", Tone::Muted)
            }
            (None, None) => ProofRow::pending("oracle"),
            // The only shape that proves anything: red before, green after.
            (Some(false), Some(true)) => ProofRow::new(
                "oracle",
                "✗ fails on base → ✓ passes on new".to_string(),
                Tone::Success,
            ),
            (Some(false), None) => ProofRow::new(
                "oracle",
                "✗ fails on base → running on new".to_string(),
                Tone::Info,
            ),
            (Some(false), Some(false)) => ProofRow::new(
                "oracle",
                "✗ fails on base → ✗ still fails".to_string(),
                Tone::Error,
            ),
            // Passing on the untouched tree means the test does not react to
            // the change — a green that proves nothing, and worth naming.
            (Some(true), _) => ProofRow::new(
                "oracle",
                "passes on base — no flip to observe".to_string(),
                Tone::Warn,
            ),
            (None, Some(passed)) => ProofRow::new(
                "oracle",
                format!(
                    "{} on new · no baseline observation",
                    if passed { "✓ passes" } else { "✗ fails" }
                ),
                Tone::Warn,
            ),
        }
    }

    fn tamper_row(&self) -> ProofRow {
        match &self.witness {
            Some(WitnessStanding::Authored { fingerprint, .. }) => ProofRow::new(
                "tamper",
                format!("pinned  {}", short(fingerprint)),
                Tone::Muted,
            ),
            _ if self.warrant_says_not_required() => ProofRow::new("tamper", "—", Tone::Muted),
            _ => ProofRow::pending("tamper"),
        }
    }

    fn verdict_row(&self) -> ProofRow {
        match &self.verdict {
            None => ProofRow::pending("verdict"),
            Some(v) => ProofRow::new(
                "verdict",
                format!(
                    "{} · {}",
                    if v.passed { "✓ passed" } else { "✗ failed" },
                    if v.deterministic {
                        "deterministic"
                    } else {
                        "model judge"
                    }
                ),
                match (v.passed, v.deterministic) {
                    (true, true) => Tone::Success,
                    // A pass nobody checked deterministically is not the same
                    // colour as a proven one.
                    (true, false) => Tone::Info,
                    (false, _) => Tone::Error,
                },
            ),
        }
    }

    fn warrant_says_not_required(&self) -> bool {
        matches!(self.warrant, Some((false, _, _)))
    }
}

/// One rendered rail row: a fixed-width label, its value, and the tone the
/// surface should style the value with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofRow {
    pub label: &'static str,
    pub value: String,
    pub tone: Tone,
}

impl ProofRow {
    fn new(label: &'static str, value: impl Into<String>, tone: Tone) -> Self {
        Self {
            label,
            value: value.into(),
            tone,
        }
    }

    fn pending(label: &'static str) -> Self {
        Self::new(label, "pending", Tone::Muted)
    }
}

/// Elide a fingerprint to its recognizable head. Full hashes are for
/// comparing, not for reading, and a rail row has ~40 columns.
fn short(fingerprint: &str) -> String {
    let head: String = fingerprint.chars().take(16).collect();
    if fingerprint.chars().nth(16).is_some() {
        format!("{head}…")
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn authored() -> ProofStep {
        ProofStep::WitnessAuthored {
            path: "tests/clear_reset.rs".into(),
            command: "cargo test clear_reset".into(),
            fingerprint: "sha256:9f3c1d2e4a5b6c7d8e9f".into(),
        }
    }

    fn oracle(passed: bool, tree: ProofTree) -> ProofStep {
        ProofStep::Oracle {
            command: "cargo test clear_reset".into(),
            passed,
            tree,
        }
    }

    #[test]
    fn a_fresh_state_hides_the_rail() {
        assert!(ProofState::default().is_empty());
    }

    #[test]
    fn a_warrant_alone_raises_the_rail() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Warrant {
            required: true,
            reason: None,
            diff_lines: 41,
        });
        assert!(!state.is_empty());
        assert_eq!(state.rows()[0].value, "required · 41 changed lines");
    }

    /// The witness for the module's own contract: a warranted witness that
    /// could not be produced must NEVER render as a blank or a pending. The
    /// work is done and unproven, and the row has to say so.
    #[test]
    fn an_unavailable_witness_is_stated_not_swallowed() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Warrant {
            required: true,
            reason: None,
            diff_lines: 12,
        });
        state.apply(&ProofStep::WitnessUnavailable {
            reason: "no author independent of the worker".into(),
        });
        let row = &state.rows()[1];
        assert_eq!(row.tone, Tone::Warn);
        assert!(
            row.value.starts_with("unavailable · "),
            "the reason must reach the rail: {}",
            row.value
        );
    }

    #[test]
    fn only_red_then_green_reads_as_a_flip() {
        let mut state = ProofState::default();
        state.apply(&authored());
        state.apply(&oracle(false, ProofTree::Baseline));
        assert_eq!(state.rows()[2].tone, Tone::Info, "still mid-flight");
        state.apply(&oracle(true, ProofTree::Candidate));
        assert!(state.flip.achieved());
        assert_eq!(state.rows()[2].tone, Tone::Success);
    }

    /// A test that was already green on the untouched tree proves the code
    /// reacts to nothing — the rail must not dress that as a pass.
    #[test]
    fn a_test_green_on_the_baseline_is_not_a_flip() {
        let mut state = ProofState::default();
        state.apply(&oracle(true, ProofTree::Baseline));
        state.apply(&oracle(true, ProofTree::Candidate));
        assert!(!state.flip.achieved());
        assert_eq!(state.rows()[2].tone, Tone::Warn);
    }

    #[test]
    fn a_change_with_nothing_to_prove_states_its_reason_and_dashes_the_rest() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Warrant {
            required: false,
            reason: Some("documentation only; prose has no runtime behavior to flip".into()),
            diff_lines: 4,
        });
        let rows = state.rows();
        assert!(rows[0].value.starts_with("documentation only"));
        assert_eq!(
            rows[1].value, "—",
            "no witness was owed, so none is pending"
        );
        assert_eq!(rows[2].value, "—");
    }

    #[test]
    fn a_model_judge_pass_is_not_coloured_like_a_proven_one() {
        let mut state = ProofState::default();
        state.apply_verdict(
            true,
            &JudgeEvidence {
                summary: "looks right".into(),
                deterministic: false,
                evidence_refs: vec![],
            },
        );
        let row = state.rows()[4].clone();
        assert_eq!(row.tone, Tone::Info);
        assert!(row.value.contains("model judge"));
    }

    #[test]
    fn a_fingerprint_is_elided_to_a_readable_head() {
        assert_eq!(short("sha256:9f3c1d2e4a5b6c7d8e9f"), "sha256:9f3c1d2e4…");
        assert_eq!(short("short"), "short");
    }
}
