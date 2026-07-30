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
//! # The invariant: every row resolves, on every path
//!
//! **Once a turn has ended, no row may read `pending`.** A tool that reports
//! only when things go well is a tool you cannot trust when they do not, and
//! `pending` on a finished turn is the worst of both — it is silence wearing
//! the costume of progress.
//!
//! Holding that is a *design* problem, not a diligence problem. `run_candidate`
//! alone has eighteen terminal exits; a rule of "every exit must remember to
//! report" is one refactor away from being false, and false invisibly. So the
//! invariant lives here, in the fold, in two halves:
//!
//! 1. **The plan is declared up front.** [`ProofStep::Assurance`] is emitted
//!    the moment triage decides, before any stage can fail or decline. Every
//!    row therefore has a stated intent from the first second — a witness is
//!    owed, or it is not and here is who said so — rather than an absence that
//!    only becomes meaningful in hindsight.
//! 2. **Unreported is terminal.** [`ProofState::finish`] runs on the turn's
//!    `Complete` and converts anything still awaiting an answer into an
//!    explicit *not reported*. That is an honest and different claim from
//!    `pending`: the pipeline never said, and the rail says so rather than
//!    implying more is coming. A pipeline path nobody has written yet cannot
//!    regress this, because the backstop does not depend on that path
//!    cooperating.
//!
//! `no_row_is_left_pending_after_a_turn_ends` proves it over arbitrary event
//! sequences rather than a handful of examples — the property IS the contract.
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

/// What triage decided this turn would buy, before any of it happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Assurance {
    pub witness: bool,
    pub judge: bool,
}

/// The whole rail state for one turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProofState {
    /// The turn's declared plan, from triage. `None` before triage answers and
    /// on conversational turns, which buy no assurance because there is no
    /// work to assure.
    pub assurance: Option<Assurance>,
    /// `Some((required, reason, diff_lines))` once the warrant has read the
    /// diff. `reason` is populated only when no test is warranted.
    pub warrant: Option<(bool, Option<String>, u32)>,
    pub witness: Option<WitnessStanding>,
    pub flip: Flip,
    pub verdict: Option<VerdictStanding>,
    /// Whether the turn has ended. Flips `pending` rows to *not reported* —
    /// see the module docs; this is the half of the invariant that does not
    /// depend on any pipeline path cooperating.
    finished: bool,
}

impl ProofState {
    /// Nothing observed yet — the rail stays hidden rather than showing five
    /// rows of dashes on a turn that never reaches verification (a greeting, a
    /// cancelled turn before triage answered).
    pub fn is_empty(&self) -> bool {
        self.assurance.is_none()
            && self.warrant.is_none()
            && self.witness.is_none()
            && self.verdict.is_none()
            && self.flip == Flip::default()
    }

    /// The turn ended. Anything still awaiting an answer becomes *not
    /// reported* — a statement, where `pending` would be a promise the turn
    /// can no longer keep.
    ///
    /// Deliberately not `apply`-driven off a proof step: the backstop has to
    /// hold for turns that emitted no proof steps at all, including ones that
    /// aborted before the pipeline reached any of them.
    pub fn finish(&mut self) {
        self.finished = true;
    }

    /// Fold one proof step.
    pub fn apply(&mut self, step: &ProofStep) {
        match step {
            ProofStep::Assurance { witness, judge } => {
                self.assurance = Some(Assurance {
                    witness: *witness,
                    judge: *judge,
                });
            }
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
    /// reflow as the proof accumulates.
    ///
    /// Every row resolves: `pending` only while the turn runs, and never once
    /// [`Self::finish`] has been called — see the module docs.
    pub fn rows(&self) -> Vec<ProofRow> {
        vec![
            self.warrant_row(),
            self.witness_row(),
            self.oracle_row(),
            self.tamper_row(),
            self.verdict_row(),
        ]
    }

    /// The row for something the pipeline has not answered: `pending` mid-turn,
    /// and an explicit *not reported* once the turn is over. The single place
    /// the invariant is enforced, so no row can forget it.
    fn unanswered(&self, label: &'static str) -> ProofRow {
        if self.finished {
            // Amber, not muted: a turn that ended without reporting is a gap in
            // the tool, and it should look like one.
            ProofRow::new(label, "not reported — the run never said", Tone::Warn)
        } else {
            ProofRow::new(label, "pending", Tone::Muted)
        }
    }

    fn warrant_row(&self) -> ProofRow {
        match &self.warrant {
            // The warrant reads a diff, so a turn that never produced one
            // never asks the question. Saying that is better than implying an
            // answer is still coming.
            None if self.finished => ProofRow::new(
                "warrant",
                "not reached — no diff was read".to_string(),
                Tone::Muted,
            ),
            None => self.unanswered("warrant"),
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
            // THE case this rail exists for. Triage waiving the witness is the
            // most common outcome by far, and it emits nothing downstream —
            // `witness_on_demand` is handed `None` and returns immediately —
            // so before the assurance plan existed this row hung on `pending`
            // for the whole turn and then forever. Naming who declined it, and
            // saying so from the first second, is the difference between a
            // tool that reports and one that only reports good news.
            None if self.triage_waived_the_witness() => {
                ProofRow::new("witness", "waived by triage", Tone::Muted)
            }
            None if self.warrant_says_not_required() => ProofRow::new("witness", "—", Tone::Muted),
            None => self.unanswered("witness"),
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
            (None, None) if self.triage_waived_the_witness() => {
                ProofRow::new("oracle", "— no test to flip", Tone::Muted)
            }
            (None, None) if self.warrant_says_not_required() => {
                ProofRow::new("oracle", "—", Tone::Muted)
            }
            (None, None) => self.unanswered("oracle"),
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
            // No artifact means nothing to pin, which is a complete answer —
            // not a missing one.
            _ if self.triage_waived_the_witness() || self.warrant_says_not_required() => {
                ProofRow::new("tamper", "— no artifact to pin", Tone::Muted)
            }
            Some(WitnessStanding::Unavailable { .. }) => {
                ProofRow::new("tamper", "— no artifact to pin", Tone::Muted)
            }
            _ => self.unanswered("tamper"),
        }
    }

    fn verdict_row(&self) -> ProofRow {
        match &self.verdict {
            // A turn can end without a verdict — cancelled, aborted on budget,
            // a lookup that verified nothing. The rail says which of "no
            // verdict yet" and "no verdict, ever" applies.
            None if self.finished => ProofRow::new(
                "verdict",
                "none — the turn ended unverified".to_string(),
                Tone::Warn,
            ),
            None => self.unanswered("verdict"),
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

    /// Triage declared this turn buys no witness. Distinct from the warrant
    /// declining one: triage decides from the *prompt* before any work exists,
    /// the warrant decides from the *diff* after. Naming which one spoke is
    /// the difference between "nothing to prove" and "we chose not to check".
    fn triage_waived_the_witness(&self) -> bool {
        self.assurance.is_some_and(|a| !a.witness)
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

    // There is deliberately no `pending()` constructor. `pending` is reachable
    // from exactly one place — [`ProofState::unanswered`], which decides
    // between it and *not reported* by whether the turn is over — so the
    // invariant cannot be bypassed by a future row that hand-rolls the string.
    // Removing this constructor is what makes that structural rather than a
    // convention someone has to remember.
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
    use proptest::prelude::*;

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

    // ---- the invariant ----

    /// Every step shape the protocol can produce, as a proptest strategy. New
    /// `ProofStep` variants must be added here — the invariant is only worth
    /// what its input space covers.
    fn any_step() -> impl Strategy<Value = ProofStep> {
        prop_oneof![
            (any::<bool>(), any::<bool>())
                .prop_map(|(witness, judge)| ProofStep::Assurance { witness, judge }),
            (any::<bool>(), any::<Option<String>>(), any::<u32>()).prop_map(
                |(required, reason, diff_lines)| ProofStep::Warrant {
                    required,
                    reason,
                    diff_lines,
                }
            ),
            (any::<String>(), any::<String>(), any::<String>()).prop_map(
                |(path, command, fingerprint)| ProofStep::WitnessAuthored {
                    path,
                    command,
                    fingerprint,
                }
            ),
            any::<String>().prop_map(|reason| ProofStep::WitnessUnavailable { reason }),
            (any::<String>(), any::<bool>(), any::<bool>()).prop_map(
                |(command, passed, baseline)| ProofStep::Oracle {
                    command,
                    passed,
                    tree: if baseline {
                        ProofTree::Baseline
                    } else {
                        ProofTree::Candidate
                    },
                }
            ),
        ]
    }

    proptest! {
        /// **The invariant.** However a turn went — no steps at all, steps in
        /// any order, contradictory steps, an abort partway — once it has
        /// ended, not one row may still read `pending`.
        ///
        /// A property rather than examples because the failure this guards is
        /// a *path nobody thought of*: the original bug was triage waiving the
        /// witness, a case that emitted no steps and so appeared in no
        /// example-based test. Enumerating states by hand is how it was missed
        /// the first time.
        #[test]
        fn no_row_is_left_pending_after_a_turn_ends(steps in prop::collection::vec(any_step(), 0..8)) {
            let mut state = ProofState::default();
            for step in &steps {
                state.apply(step);
            }
            state.finish();
            for row in state.rows() {
                prop_assert_ne!(
                    row.value.as_str(),
                    "pending",
                    "`{}` still reads pending after the turn ended (steps: {:?})",
                    row.label,
                    steps
                );
            }
        }

        /// The rail always has exactly five rows, whatever happened. A surface
        /// whose height depends on how the turn went cannot be read at a
        /// glance, and cannot be laid out without reflowing the transcript.
        #[test]
        fn the_rail_is_always_five_rows(steps in prop::collection::vec(any_step(), 0..8), finished: bool) {
            let mut state = ProofState::default();
            for step in &steps {
                state.apply(step);
            }
            if finished {
                state.finish();
            }
            prop_assert_eq!(state.rows().len(), 5);
        }
    }

    /// The regression that started this: triage waives the witness, nothing
    /// downstream is emitted, and the row used to hang on `pending` for the
    /// whole turn and then forever. It must name who declined, immediately —
    /// before the turn ends, not only after.
    #[test]
    fn a_triage_waived_witness_says_so_while_the_turn_is_still_running() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Assurance {
            witness: false,
            judge: false,
        });
        assert!(!state.is_empty(), "the rail is up from triage onward");
        let rows = state.rows();
        assert_eq!(rows[1].value, "waived by triage");
        assert_eq!(rows[2].value, "— no test to flip");
        assert_eq!(rows[3].value, "— no artifact to pin");
        // And nothing about it changes into a false promise at the end.
        state.finish();
        assert_eq!(state.rows()[1].value, "waived by triage");
    }

    /// A turn that dies before reporting anything must still say what it does
    /// not know. `pending` on a dead turn is silence in the costume of
    /// progress.
    #[test]
    fn a_turn_that_reported_nothing_says_it_reported_nothing() {
        let mut state = ProofState::default();
        state.apply(&ProofStep::Assurance {
            witness: true,
            judge: true,
        });
        state.finish();
        let rows = state.rows();
        assert!(rows[1].value.starts_with("not reported"), "{:?}", rows[1]);
        assert_eq!(rows[1].tone, Tone::Warn, "a reporting gap looks like a gap");
        assert!(rows[4].value.starts_with("none —"), "{:?}", rows[4]);
    }

    #[test]
    fn a_fingerprint_is_elided_to_a_readable_head() {
        assert_eq!(short("sha256:9f3c1d2e4a5b6c7d8e9f"), "sha256:9f3c1d2e4…");
        assert_eq!(short("short"), "short");
    }
}
