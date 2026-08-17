// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The proof rail's wire vocabulary: the steps a turn emits as it buys
//! assurance for its own work.
//!
//! Split out of [`crate::event`] (which carries the proof *event*) the same
//! way [`crate::ladder`] was: the step vocabulary grows on its own schedule —
//! [`ProofStep::VerdictDegraded`] joined for #1787 — while the event envelope
//! does not. Re-exported from the crate root and from [`crate::event`], so
//! `stella_protocol::ProofStep` and `event::ProofStep` are unchanged.

use serde::{Deserialize, Serialize};

use crate::ladder::ProofTree;

/// One step of the proof a turn builds for its own work, in the order the
/// pipeline makes the observation. Carried by [`crate::event::AgentEvent::Proof`].
///
/// Additive in one direction only: an older reader that does not know the
/// `proof` type tag preserves the whole event via
/// [`crate::event::AgentEvent::Unknown`], but a reader that knows `Proof` and
/// meets a future `kind` fails the whole event — this nested enum is closed,
/// with no `Unknown` step (see [`crate::event`]'s module docs on nested
/// vocabularies).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(
    feature = "schema",
    schemars(
        description = "One step of the proof a turn builds for its own work, in the order the pipeline makes the observation. Carried by the `proof` event. Note the forward-compatibility asymmetry: a reader that does not know the `proof` event preserves it whole as an `unknown` event, but a reader that knows `proof` and meets a future `kind` here fails the whole event -- this nested vocabulary is closed and has no unknown step."
    )
)]
pub enum ProofStep {
    /// What assurance this turn is going to buy, stated by triage **before**
    /// any of it happens.
    ///
    /// Emitted first, and the reason the rail can be honest at all. Every
    /// other step reports something that *did* happen, so a turn where the
    /// answer is "we decided not to" produced no steps and left the surface
    /// with nothing to say — which is exactly the case that dominates in
    /// practice. A declared plan turns that silence into a statement: the
    /// witness row reads "waived by triage" from the first second of the
    /// turn instead of implying a test is still coming.
    Assurance {
        /// Whether an independently authored witness test was called for.
        witness: bool,
        /// Whether a model verifier was called for on inconclusive evidence.
        ///
        /// Aliased: this field shipped as `judge`, and every recorded session
        /// and golden fixture spells it that way. Renaming it without the
        /// alias makes those streams unparseable — which is exactly what the
        /// golden fixtures caught.
        #[serde(alias = "judge")]
        verifier: bool,
    },
    /// The warrant read the diff and answered "does this change need a test".
    /// Emitted once per candidate, before any witness is bought — a change
    /// with nothing to prove is a *stated* outcome here, never silence.
    Warrant {
        required: bool,
        /// The stated reason when no test is warranted; `None` when one is.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// Size of the change the answer was read from.
        diff_lines: u32,
    },
    /// An independent model authored the failing witness, and its bytes are
    /// pinned: any later change to `path` fails the candidate closed.
    WitnessAuthored {
        path: String,
        /// The command that arms the flip oracle.
        command: String,
        /// The accepted artifact's fingerprint — what tamper exclusion compares
        /// against for the rest of the run.
        fingerprint: String,
    },
    /// A warranted witness could not be *produced* (no independent author, an
    /// author that got stuck, a failed graft). The work stands; it is simply
    /// unproven, and saying so is the point.
    WitnessUnavailable { reason: String },
    /// **Every** evidence channel was blind, so the ladder abstained rather
    /// than judging: no flip oracle, no test result, a working tree the diff
    /// probe could not read, and no recorded file change.
    ///
    /// Distinct from a failing verdict, and the distinction is the point. The
    /// turn this exists for ended with a verifier asserting a file "likely does
    /// not exist" while the file sat in the container — a claim about the
    /// *instruments* delivered as a claim about the *work* (#973). Without a
    /// step of its own, an abstention reaches the rail as `✓ passed · model
    /// verifier`, which is the same silent outcome in the other direction.
    VerificationUnavailable { reason: String },
    /// The evidence channels worked, and what they returned did not amount to
    /// a proof ([`crate::LadderRung::Unverified`]).
    ///
    /// The near-twin of [`Self::VerificationUnavailable`] and deliberately not
    /// the same step, for the reason that one exists at all: a reader has to be
    /// able to tell "the instruments were blind" from "the instruments worked
    /// and the answer was not enough". They imply opposite repairs — fix the
    /// probe, versus produce the missing observation — and a rail that renders
    /// both as one row sends every reader to the wrong one half the time.
    ///
    /// This is the step that replaced the model verdict. Where a run used to
    /// record a second model's opinion on inconclusive evidence, it now records
    /// that the evidence was inconclusive and stops, so the trace states the
    /// limit of what was established rather than papering over it.
    #[cfg_attr(
        feature = "schema",
        schemars(
            description = "The evidence channels worked, and what they returned did not amount to a proof; the verdict rests on the `unverified` rung. Deliberately distinct from `verification_unavailable`: a reader has to be able to tell \"the instruments were blind\" from \"the instruments worked and the answer was not enough\", because they imply opposite repairs -- fix the probe, versus produce the missing observation."
        )
    )]
    VerificationUnproven { reason: String },
    /// The flip oracle observed one run of the tracked command against one
    /// tree. A fail in `Baseline` followed by a pass in `Candidate` is the
    /// flip; anything else is not.
    ///
    /// `run`/`runs_required`/`seed` are the witness surface's replay facts
    /// (D6) — which candidate replay this observation is, how many the flip
    /// requires, and the deterministic seed the replay pinned. All additive
    /// (`serde(default)`): observations recorded before they existed parse
    /// with each absent, and an emitter that has no replay discipline (a
    /// single-run oracle) simply omits them.
    Oracle {
        command: String,
        passed: bool,
        tree: ProofTree,
        /// Which candidate replay this observation is (1-based). `None` on
        /// baseline runs and on single-run oracles.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run: Option<u32>,
        /// How many passing candidate replays the flip requires, when the
        /// oracle runs more than one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        runs_required: Option<u32>,
        /// The deterministic seed the replay pinned, when one was pinned.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seed: Option<u64>,
    },
    /// This candidate's model verdict degraded to the deterministic heuristic:
    /// the verifier role was unresolvable, its response did not follow the
    /// verdict protocol, or the call failed outright.
    ///
    /// **Retired.** No run emits it — the pipeline makes no verdict call, so
    /// there is none to degrade. Retained because recorded streams carry it and
    /// this crate's contract is that a reader meeting an unknown step fails the
    /// event rather than laundering it.
    ///
    /// Emitted once per candidate, keyed by ordinal, because the once-per-run
    /// prose warning cannot say *which* of a best-of-N fan-out's candidates
    /// were judged by the heuristic — N candidates degrading used to leave one
    /// caveat and no record (#1787). The ladder rung records *that* a round
    /// degraded; this step records *whose* verdict and why.
    VerdictDegraded {
        /// Which candidate degraded (1-based, [`ProofStep::Oracle::run`]'s
        /// convention).
        candidate: u32,
        /// The stated reason a model verdict could not be rendered.
        reason: String,
    },
    /// This turn's task class came from the deterministic keyword floor
    /// (`triage::resolve_task_class` with no model answer), because the
    /// triage call never produced one.
    ///
    /// Emitted for the same reason [`ProofStep::VerdictDegraded`] is, one
    /// stage earlier. The fallback itself is correct by design and
    /// load-bearing — triage must never fail a run — but it became the
    /// *common* case while staying completely invisible: across three
    /// Terminal-Bench arm runs, 27 of 34 triage calls burned the full 10s
    /// latency ceiling and returned nothing, about four and a half minutes of
    /// wall clock purchasing zero bits, with nothing in the summary layer
    /// saying so (#2414). Every downstream decision that reads the class —
    /// whether to plan, whether a witness is warranted, whether research
    /// questions are even asked — then looks like a decision and is a default.
    ///
    /// A bench conclusion must not be drawn from a triage that never ran, and
    /// this is the record that makes that checkable.
    TriageDegraded {
        /// The stated reason no model classification was available: the call
        /// timed out at its ceiling, failed, could not be routed, or answered
        /// off-protocol.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Invariant 4: the new step round-trips byte-for-byte, and its wire
    /// spelling is pinned — a rename of the tag or a field breaks every
    /// recorded stream that carries it.
    #[test]
    fn verdict_degraded_round_trips_and_pins_its_wire_shape() {
        let step = ProofStep::VerdictDegraded {
            candidate: 2,
            reason: "the verifier call failed or timed out".into(),
        };
        let json = serde_json::to_string(&step).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"verdict_degraded","candidate":2,"reason":"the verifier call failed or timed out"}"#
        );
        let parsed: ProofStep = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, step);
    }

    /// Invariant 4 again, for #2414's step. The wire spelling is pinned here
    /// because the whole value of the record is that something outside this
    /// process can census it — a bench harness reading `stella-events.jsonl`
    /// to tell a real classification from a keyword-floor default.
    #[test]
    fn triage_degraded_round_trips_and_pins_its_wire_shape() {
        let step = ProofStep::TriageDegraded {
            reason: "the triage call timed out at its 30s ceiling".into(),
        };
        let json = serde_json::to_string(&step).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"triage_degraded","reason":"the triage call timed out at its 30s ceiling"}"#
        );
        let parsed: ProofStep = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, step);
    }
}
