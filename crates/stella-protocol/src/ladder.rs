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
/// - `deterministic: false` covers two rungs that are not the same answer —
///   [`LadderRung::Unverifiable`] ("no probe could look") and
///   [`LadderRung::Unverified`] ("the probes looked and did not settle it") —
///   and the only thing separating them in the old shape was the wording of a
///   summary string.
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
    ///
    /// Reads as a claim about the **probes**: nothing could look. Its
    /// near-namesake [`LadderRung::Unverified`] is a claim about the
    /// **proof**: the probes looked and what they returned did not settle the
    /// question. The two words carry that difference exactly — `-able` is a
    /// capacity, `-ed` is a state — and conflating them would recreate the
    /// defect this enum exists to fix, since both emit `deterministic: false`
    /// and neither is a finding that the work is wrong.
    Unverifiable,
    /// Deterministic evidence was collected, and it did not amount to a proof.
    /// No model was asked, because there is no question a model could settle
    /// that the oracle could not.
    ///
    /// This is the terminal rung for genuinely inconclusive evidence — no
    /// fail→pass flip, or a diff over budget, or a test that could not be run.
    /// It is the ladder's honest "I do not know", and the run is scored
    /// `Unverified` on it.
    ///
    /// # Why the two rungs it replaces are gone
    ///
    /// Both described a model's involvement rather than the work: `model_judge`
    /// / `model_verdict` was "a second model read the diff and offered an
    /// opinion", and `heuristic_fallback` was "that model was unreachable, so a
    /// heuristic stood in". Neither is evidence about the change. Measured over
    /// an 89-task Terminal-Bench run the opinion agreed with the benchmark's
    /// grader 46% of the time and 17 of its false passes cost 5 tasks outright
    /// — a coin flip that could end a run — so the escalation was removed and
    /// the honest label put in its place.
    ///
    /// Both aliases are retained for the same reason `model_verdict` once
    /// aliased `model_judge`: sessions already on disk spell it those ways, and
    /// this enum's contract is that an unknown rung fails the event rather than
    /// laundering it, so a bare rename would make historical streams fail to
    /// parse loudly and totally. New writes use `unverified`; old reads still
    /// land, and land on the rung whose semantics they always actually had —
    /// unproven.
    #[serde(
        alias = "model_judge",
        alias = "model_verdict",
        alias = "heuristic_fallback"
    )]
    Unverified,
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
            LadderRung::Unverified => "unverified",
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
    /// The worker's own `verify_done` tool run printed `WITNESS CONFIRMED`
    /// this round (#2129): a deterministic fail-on-baseline / pass-on-new
    /// shadow observation the pipeline's flip oracle cannot make, because it
    /// tracks only its own command.
    ///
    /// On the wire because it **changes the verdict** and nothing else here
    /// records which channel carried it: it rescues the heuristic fallback
    /// from a verifier outage and counts as corroboration, so a reader of a
    /// stored verdict otherwise sees a pass whose reasoning cites
    /// `verify_done` with no structured field to count. `false` covers both
    /// "no confirmation was observed" and "recorded before this field
    /// existed"; like every other one-way channel here it is never a claim
    /// that the worker's witness failed.
    #[serde(default)]
    pub verify_done_flip: bool,
    /// Positive claim that this round had NO tracked test command at all
    /// (#2129) — neither a configured `--test-command` nor an authored
    /// witness — so "no flip" is a demand the task structurally cannot meet.
    ///
    /// On the wire for the same reason as [`Self::verify_done_flip`]: it turns
    /// a fallback FAIL into an upward abstention, and a reader auditing a pass
    /// that rests on nothing deterministic needs to see that the demand was
    /// unmeetable rather than unmet. `false` on snapshots recorded before
    /// this existed, which is the conservative reading — the dispensation is
    /// never assumed.
    #[serde(default)]
    pub no_test_surface: bool,
    /// Command chains this round that reported an error while exiting `0`
    /// (#2125) — the shape a cited measurement can silently stand on.
    ///
    /// Unlike [`Self::verify_done_flip`] and [`Self::no_test_surface`] this
    /// one never changes a verdict: an errored probe makes a cited quantity
    /// *unsubstantiated*, not
    /// *disproven*, so it informs the verifier's opinion and withholds
    /// nothing. It is here because the question it answers — how often a run
    /// cites a number that stood on a broken chain — is one only aggregate
    /// traces can answer, and aggregate traces read this snapshot. One-way:
    /// `0` is "the closed signature vocabulary matched nothing", never "this
    /// round's commands were clean".
    #[serde(default)]
    pub errored_commands: u32,
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
            verify_done_flip: false,
            no_test_surface: false,
            errored_commands: 0,
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
                .with_rung(LadderRung::Unverified)
                .with_verifier_independence(Some(false)),
            // #2194: the three channels that joined the snapshot after the
            // verdict already turned on them.
            LadderSnapshot {
                verify_done_flip: true,
                no_test_surface: true,
                errored_commands: 3,
                ..snapshot()
            },
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

    /// The three #2194 channels are additive in the direction that matters: a
    /// snapshot recorded before they existed parses, and reads as the
    /// conservative value on each — no confirmation, no dispensation, no
    /// errored command. Reading either bool as `true` by default would credit
    /// evidence nobody observed.
    #[test]
    fn the_late_channels_default_conservatively() {
        let legacy = r#"{"flip_achieved":false,"unstable_flip":false,"diff_lines":0,
            "diff_budget":0,"diff_available":false,"file_change_events":0,
            "mutating_actions":0,"new_diag_errors":0,"new_diag_warnings":0}"#;
        let parsed: LadderSnapshot = serde_json::from_str(legacy).unwrap();
        assert!(!parsed.verify_done_flip);
        assert!(!parsed.no_test_surface);
        assert_eq!(parsed.errored_commands, 0);
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
        let stamped = base.with_rung(LadderRung::Unverified);
        assert_eq!(stamped.rung, Some(LadderRung::Unverified));
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
            LadderRung::Unverified,
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
            serde_json::to_string(&LadderRung::Unverified).unwrap(),
            "\"unverified\""
        );
        assert_eq!(
            serde_json::to_string(&LadderRung::NothingAttempted).unwrap(),
            "\"nothing_attempted\""
        );
    }

    /// Every rung token this enum has ever written still parses, and the three
    /// that described a model's involvement land on [`LadderRung::Unverified`].
    ///
    /// This is the compatibility half of removing the escalation. The enum's
    /// stated contract is that an unknown rung fails the event rather than
    /// being laundered, so dropping `model_judge` / `model_verdict` /
    /// `heuristic_fallback` without aliases would make every session already on
    /// disk fail to parse — loudly and totally, which is the worst way to
    /// discover a vocabulary change. They map to `Unverified` because that is
    /// the claim they always actually supported: a model offered an opinion, or
    /// could not, and either way nothing deterministic proved the work.
    #[test]
    fn the_retired_model_rungs_parse_as_unverified() {
        for token in ["model_judge", "model_verdict", "heuristic_fallback"] {
            let parsed: LadderRung = serde_json::from_str(&format!("\"{token}\"")).unwrap();
            assert_eq!(
                parsed,
                LadderRung::Unverified,
                "`{token}` must still parse, as the unproven rung it always was"
            );
        }
    }
}
