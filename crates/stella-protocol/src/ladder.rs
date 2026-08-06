// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The verification ladder's wire vocabulary: the rung a verdict came to rest
//! on, and the deterministic evidence it was decided from.
//!
//! Split out of [`crate::event`] (which carries the verdict *event*) because
//! these types are the ladder's own contract and now have three readers rather
//! than one: `replay` explains a past decision from them, an escalating verifier
//! prompt renders them, and reward extraction (#1043) reads
//! [`LadderSnapshot::rung`] to label a trajectory. Re-exported from the crate
//! root, so `stella_protocol::LadderSnapshot` is unchanged.

use serde::{Deserialize, Serialize};

/// Which rung of the evidence ladder a `Verdict` actually came to rest
/// on (#1043).
///
/// # Why the rung is on the wire instead of inferred
///
/// `VerdictEvidence` already carries `passed` and `deterministic`, and for a
/// while that looked like enough. It is not, and the gap is not academic:
///
/// - `deterministic: true, passed: false` is emitted by **two** rungs — a red
///   touched test ([`LadderRung::Revise`], a genuine failure) and a turn that
///   dispatched nothing ([`LadderRung::NothingAttempted`], where the honest
///   label is "no evidence", not "wrong").
/// - `deterministic: true, passed: true` is emitted both by the full
///   deterministic pass ([`LadderRung::SubmitFast`]) and by a *waived* review
///   where the warrant found nothing to prove ([`LadderRung::Waived`]) — so a
///   reader inferring "deterministic pass" from those two flags would label a
///   turn nothing checked as the ladder's strongest possible result.
/// - `deterministic: false` covers a real model verifier, a heuristic verdict
///   after the verifier call *failed*, and the abstain rung. The first is soft
///   signal, the second is not signal at all, and the only thing separating
///   them in the old shape was the wording of a summary string.
///
/// Re-deriving the rung by re-running the ladder over
/// [`LadderSnapshot`] cannot close any of that: it reproduces the *decision*
/// but not which of the three ways the verifier arm resolved, and it silently
/// disagrees with reality whenever the run's `veto_warnings` setting differed
/// from the reader's assumption. So the pipeline states the rung it took.
///
/// Wire-compatible in the additive direction only, like every nested
/// vocabulary in this crate: a reader that meets a rung it does not know fails
/// the event rather than laundering it (see the [`crate::event`] docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LadderRung {
    /// Deterministic pass: a fail→pass flip of the tracked command, touched
    /// tests green, diff within budget, no fresh diagnostics. The verifier was
    /// skipped because nothing was left to ask.
    SubmitFast,
    /// Deterministic failure: the touched tests were red. No verifier call — the
    /// failure was already conclusive.
    Revise,
    /// The turn dispatched no call capable of changing the workspace. A
    /// determinate finding about the turn, and the one place this ladder reads
    /// an absence as evidence.
    NothingAttempted,
    /// Every evidence channel was blind, so the ladder abstained. Nothing is
    /// claimed about the work — in particular not that it failed.
    Unverifiable,
    /// The verifier answered on genuinely inconclusive evidence. Named for
    /// the *evidence class* — a model's opinion — not for the model: it is
    /// deliberately not called `ModelVerifier`, because the whole ladder is
    /// verification and this is the one rung that is only an opinion.
    ///
    /// The alias is what keeps this rename additive. This rung shipped as
    /// `model_judge`, so every session already on disk spells it that way; a
    /// bare rename would make those streams fail to parse — and this enum's
    /// own contract is that an unknown rung fails the event rather than being
    /// laundered, so the failure would be loud and total. New writes use
    /// `model_verdict`; old reads still land.
    #[serde(alias = "model_judge")]
    ModelVerdict,
    /// The verdict call itself failed or returned something unparseable, and
    /// the conservative heuristic verdict stood in for it. A verdict about the
    /// verifier's availability, not about the work.
    HeuristicFallback,
    /// No independent review was bought at all: triage waived it and the
    /// warrant agreed, or the change warranted no witness test. A pass
    /// carrying no evidence either way.
    Waived,
}

impl LadderRung {
    /// The wire token — the same string serde emits, so a rendered
    /// explanation and a JSON stream name the rung identically.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            LadderRung::SubmitFast => "submit_fast",
            LadderRung::Revise => "revise",
            LadderRung::NothingAttempted => "nothing_attempted",
            LadderRung::Unverifiable => "unverifiable",
            LadderRung::ModelVerdict => "model_verdict",
            LadderRung::HeuristicFallback => "heuristic_fallback",
            LadderRung::Waived => "waived",
        }
    }

    /// Whether this rung's verdict rests on deterministic evidence — a real
    /// test observation — as opposed to a model's opinion or an abstention.
    ///
    /// [`LadderRung::Waived`] answers `false` despite riding on evidence
    /// values that are themselves deterministic: what was determined is that
    /// *nothing needed checking*, which is not a determination about the work.
    #[must_use]
    pub fn is_deterministic(self) -> bool {
        matches!(self, LadderRung::SubmitFast | LadderRung::Revise)
    }
}

/// One flip-oracle observation, in the order the pipeline made it — together
/// they are the oracle trace a verdict carries (#864).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OracleObservation {
    /// Which tree the observation ran against.
    pub tree: ProofTree,
    /// Whether the tracked command's assertions passed. Infra outcomes never
    /// appear here — an unobservable run is not an oracle observation.
    pub passed: bool,
}

/// The deterministic evidence the ladder decided a verdict from, snapshotted
/// at decision time (#865). Everything here existed when the decision was
/// made; attaching it to the verdict is what makes "why?" answerable later
/// without re-deriving — and re-deriving is exactly what a replay of an
/// event stream cannot do, because the world the probes read is gone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LadderSnapshot {
    /// The rung this verdict came to rest on (#1043). Absent on events
    /// recorded before it existed, which is the one case a reader must handle
    /// as "unknown" rather than guess at — see [`LadderRung`] for why the
    /// guess is not available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rung: Option<LadderRung>,
    /// The normalized test command the flip oracle locked onto, when it
    /// armed at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracked_command: Option<String>,
    /// The oracle's observations in order (baseline, candidate runs, the
    /// pre-submit confirmation). Infra runs are absent by construction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub oracle_trace: Vec<OracleObservation>,
    /// Whether the oracle's flip was achieved — after the confirmation run,
    /// so an unconfirmed flip reads `false` here with `unstable_flip: true`.
    pub flip_achieved: bool,
    /// A flip was observed but its confirmation re-run did not pass (#859).
    pub unstable_flip: bool,
    /// A would-be flip was refused because the passing run named its tests
    /// and none of the baseline's failing tests were among them — the pass
    /// demonstrably fixed a *different* failure (#867), most concretely a
    /// deleted or renamed failing test. `serde(default)` so pre-#867
    /// snapshots keep parsing.
    #[serde(default)]
    pub flip_refused_different_failure: bool,
    /// Touched-tests result: `None` is "could not be observed", not a pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub touched_tests_passed: Option<bool>,
    /// Why the test run observed nothing, when it didn't (`timed_out`,
    /// `infra_failure`) — the #860 distinction between "the suite failed"
    /// and "the suite could not be watched".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_infra: Option<String>,
    /// Lines changed, and the budget they were judged against.
    pub diff_lines: u32,
    pub diff_budget: u32,
    /// Whether the diff probe could read the working tree at all.
    pub diff_available: bool,
    /// Mutating file touches the recorder observed.
    pub file_change_events: u32,
    /// Dispatched tool calls capable of changing the workspace.
    pub mutating_actions: u32,
    /// New lint/typecheck errors/warnings over the pre-execution baseline
    /// (#861); zeros when the probe was unavailable or never consulted.
    pub new_diag_errors: u32,
    pub new_diag_warnings: u32,
    /// The witness-tamper check's result: `None` when no witness was armed,
    /// `Some(true)` when every witness artifact matched its pinned identity.
    /// `Some(false)` never reaches a verdict — tampering aborts the
    /// candidate — so its presence here is the *stated* proof the check ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness_intact: Option<bool>,
    /// The mutation audit's finding (#870): `Some(true)` = the witness
    /// failed under at least one trivial mutant of the changed lines (it
    /// constrains the change); `Some(false)` = it stayed green under every
    /// observed mutant (tautological — the deterministic credit was
    /// withheld); `None` = the check never ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness_mutation: Option<bool>,
    /// Whether the test run executed the lines the change added (#1291):
    /// `covered`, `not_covered`, or `unmeasured`.
    ///
    /// A string rather than a bool because the third value is the whole
    /// point. "The test did not run the changed lines" and "no coverage tool
    /// could say" are different findings, and only the first is about the
    /// work — collapsing them into `Option<bool>` would put the reader back
    /// where #973 found them, reading a statement about the instrument as a
    /// statement about the world.
    ///
    /// Absent on snapshots recorded before this existed, and on every run
    /// where no coverage probe was wired — which a reader must treat as
    /// `unmeasured`, never as either verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff_coverage: Option<String>,
    /// Whether the model that graded this verdict was independent of the
    /// worker that produced the work (#1795): `Some(false)` is a self-graded
    /// verdict — the verdict call resolved to the worker's own model — and
    /// `Some(true)` a distinct grader.
    ///
    /// A structured fact rather than the once-per-run prose caveat, because
    /// the caveat scrolls away while the verdict is stored: a reader of a
    /// stored verdict must be able to see the grader was not independent
    /// without the transcript. Absent when no model verdict was bought (the
    /// deterministic, waived, and abstaining rungs — nothing graded, so
    /// independence is not a fact about them), when the worker's own
    /// resolution failed (nothing to compare against), and on snapshots
    /// recorded before this existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier_independent: Option<bool>,
}

impl LadderSnapshot {
    /// This snapshot with `rung` set — the spelling every emit site uses, so a
    /// verdict cannot be attached to a snapshot that does not say which rung
    /// produced it.
    #[must_use]
    pub fn with_rung(&self, rung: LadderRung) -> Self {
        Self {
            rung: Some(rung),
            ..self.clone()
        }
    }

    /// This snapshot with the grader-independence fact set (#1795). Stamped
    /// only on the model-verdict path — see [`Self::verifier_independent`]
    /// for why the other rungs stay absent.
    #[must_use]
    pub fn with_verifier_independence(self, verifier_independent: Option<bool>) -> Self {
        Self {
            verifier_independent,
            ..self
        }
    }
}

/// Which code state a `ProofStep::Oracle` observation was made against.
///
/// The distinction is the whole content of a flip: the same command failing in
/// `Baseline` and passing in `Candidate` is proof, while either result twice
/// against one tree is a tree observed twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProofTree {
    /// The pre-execution tree — the code as it was before this turn touched it.
    Baseline,
    /// The executed tree — the code as this turn left it.
    Candidate,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> LadderSnapshot {
        LadderSnapshot {
            rung: None,
            tracked_command: Some("cargo test -p stella-core".into()),
            oracle_trace: vec![
                OracleObservation {
                    tree: ProofTree::Baseline,
                    passed: false,
                },
                OracleObservation {
                    tree: ProofTree::Candidate,
                    passed: true,
                },
            ],
            flip_achieved: true,
            unstable_flip: false,
            flip_refused_different_failure: false,
            touched_tests_passed: Some(true),
            test_infra: None,
            diff_lines: 12,
            diff_budget: 400,
            diff_available: true,
            file_change_events: 2,
            mutating_actions: 3,
            new_diag_errors: 0,
            new_diag_warnings: 0,
            witness_intact: Some(true),
            witness_mutation: Some(true),
            diff_coverage: Some("covered".into()),
            verifier_independent: None,
        }
    }

    /// Invariant #4: the snapshot round-trips byte-for-byte, with and without
    /// a rung, and with the grader-independence fact in either polarity.
    #[test]
    fn the_snapshot_round_trips() {
        for value in [
            snapshot(),
            snapshot().with_rung(LadderRung::SubmitFast),
            snapshot()
                .with_rung(LadderRung::ModelVerdict)
                .with_verifier_independence(Some(false)),
        ] {
            let json = serde_json::to_string(&value).unwrap();
            let back: LadderSnapshot = serde_json::from_str(&json).unwrap();
            assert_eq!(value, back);
        }
    }

    /// The grader-independence fact is additive (#1795): a snapshot that never
    /// stated it emits no key and parses as unknown — never as either verdict.
    #[test]
    fn verifier_independence_is_additive() {
        let json = serde_json::to_string(&snapshot()).unwrap();
        assert!(
            !json.contains("verifier_independent"),
            "an unstated fact emits no key: {json}"
        );
        let stamped = snapshot().with_verifier_independence(Some(false));
        let json = serde_json::to_string(&stamped).unwrap();
        assert!(
            json.contains("\"verifier_independent\":false"),
            "a self-graded verdict states it: {json}"
        );
    }

    /// `rung` is additive: a snapshot recorded before it existed still parses,
    /// and a snapshot without one does not emit the key.
    #[test]
    fn the_rung_is_additive() {
        let json = serde_json::to_string(&snapshot()).unwrap();
        assert!(!json.contains("rung"), "an unset rung emits no key: {json}");
        let legacy = r#"{"flip_achieved":false,"unstable_flip":false,"diff_lines":0,
            "diff_budget":0,"diff_available":false,"file_change_events":0,
            "mutating_actions":0,"new_diag_errors":0,"new_diag_warnings":0}"#;
        let parsed: LadderSnapshot = serde_json::from_str(legacy).unwrap();
        assert_eq!(parsed.rung, None);
    }

    /// `with_rung` changes exactly one field.
    #[test]
    fn with_rung_preserves_every_other_field() {
        let base = snapshot();
        let stamped = base.with_rung(LadderRung::ModelVerdict);
        assert_eq!(stamped.rung, Some(LadderRung::ModelVerdict));
        assert_eq!(
            LadderSnapshot {
                rung: None,
                ..stamped
            },
            base
        );
    }

    /// Only the two evidence rungs are deterministic. `Waived` in particular is
    /// not: what it determined is that nothing needed checking.
    #[test]
    fn only_the_evidence_rungs_are_deterministic() {
        assert!(LadderRung::SubmitFast.is_deterministic());
        assert!(LadderRung::Revise.is_deterministic());
        for rung in [
            LadderRung::NothingAttempted,
            LadderRung::Unverifiable,
            LadderRung::ModelVerdict,
            LadderRung::HeuristicFallback,
            LadderRung::Waived,
        ] {
            assert!(!rung.is_deterministic(), "{rung:?} is not deterministic");
        }
    }

    /// The wire tokens are `snake_case`, which is what every other nested
    /// vocabulary in this crate uses.
    #[test]
    fn rung_tokens_are_snake_case() {
        assert_eq!(
            serde_json::to_string(&LadderRung::HeuristicFallback).unwrap(),
            "\"heuristic_fallback\""
        );
        assert_eq!(
            serde_json::to_string(&LadderRung::NothingAttempted).unwrap(),
            "\"nothing_attempted\""
        );
    }
}
