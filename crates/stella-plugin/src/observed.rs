//! What a plugin may report, and the host-owned region beside it.
//!
//! # The contract this module exists to make satisfiable
//!
//! Three decisions used to compose into a rule no honest plugin could keep
//! (#3499, found identically by all three of Track C's reference plugins, in
//! three languages, which is what makes it a contract defect rather than an
//! implementation mistake):
//!
//! 1. `[oracle] tamper` was mandatory and had one value.
//! 2. Tamper snapshotting is **host-side** by design
//!    (`doc:pipeline-as-plugins` §4 A10) — the host owns the candidate
//!    worktree and the authoring-time identity snapshot; the plugin never sees
//!    either.
//! 3. Yet the `tamper` field sat on the evidence the **plugin** returns.
//!
//! So a plugin declared a policy it could not execute, could only ever answer
//! [`TamperFinding::NotChecked`], and `judge` — correctly refusing to credit a
//! flip whose artifacts nobody compared — returned
//! [`UndecidedReason::TamperUnchecked`](crate::UndecidedReason) forever. Every
//! run undecided, on evidence that was never the plugin's to give.
//!
//! # The fix is a type, not a rule
//!
//! [`ObservedEvidence`] is what a wrapper returns: the flip it watched and the
//! numbers it measured. It has **no tamper field in any language** — not a
//! field a well-behaved plugin declines to set, not a field the host remembers
//! to overwrite, but a field that does not exist on the wire the plugin
//! speaks. `deny_unknown_fields` finishes the argument: a plugin that sends one
//! anyway is refused rather than believed.
//!
//! The host then merges its own snapshot result through
//! [`EvidenceSet::from_observed`], which takes the [`TamperFinding`] as an
//! argument it cannot omit, and the resulting [`EvidenceSet`] is what `judge`
//! reads. "The host owns tamper" is therefore a property of the types rather
//! than a sentence in a spec someone has to have read — the same move
//! [`VolatileContext`](crate::VolatileContext) makes for invariant 7.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::wire::{EvidenceProvenance, EvidenceSet, FlipObservation, TamperFinding};

/// The evidence a wrapper gathered — the plugin-owned half of an
/// [`EvidenceSet`].
///
/// Every field here is something a plugin's own process can honestly observe:
/// it ran the witness and watched it flip, or it ran a benchmark and counted.
/// Nothing here is a verdict, and nothing here is a fact only the host holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedEvidence {
    /// What the wrapper saw of the fail→pass flip.
    pub flip: FlipObservation,
    /// The numbers the oracle reported, by declared measurement name. A name
    /// absent here is *missing*, never a satisfied budget — see
    /// `stella_runtime::wrapper::judge`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub measurements: BTreeMap<String, u64>,
    /// What the wrapper's own process wants the next round told, in its own
    /// words — **advisory, and never read by `judge`** (#3840).
    ///
    /// The one string on this type, and the constraint that keeps it safe is
    /// stated in [`EvidenceSet`]'s doc comment: `judge` must be total over a
    /// fixed set of fields whose values are closed enums, which is the
    /// compiler's job rather than an argument. So this does not reach
    /// [`EvidenceSet`] at all. It travels beside the verdict, onto
    /// [`UnmetRequirement::detail`](crate::UnmetRequirement::detail), where the
    /// only thing that reads it is the correction a held-open round renders.
    ///
    /// What it exists for: `stella_core::goal::GoalVerifierVerdict` carries
    /// `met` **and** `reasoning` **and** `feedback`, and the built-in loop hands
    /// the worker that `feedback` verbatim every round. A plugin reporting the
    /// same verdict as `{"met": 0}` had no way to carry the sentence, so every
    /// round's correction was the same host-rendered statement of the static
    /// `[requirements]` clause regardless of what the verifier actually found.
    ///
    /// A plugin that has nothing to add leaves it absent, and the correction is
    /// exactly what it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ObservedEvidence {
    /// What a wrapper reports when it observed nothing at all — its process
    /// failed, timed out, or answered unreadably.
    ///
    /// [`FlipObservation::Unobservable`] rather than an empty set for
    /// [`EvidenceSet::unobserved`]'s reason: silence must make `judge` abstain,
    /// never read as "nothing was wrong".
    #[must_use]
    pub fn nothing() -> Self {
        Self {
            flip: FlipObservation::Unobservable,
            measurements: BTreeMap::new(),
            detail: None,
        }
    }
}

impl EvidenceSet {
    /// Merge what the plugin observed with what the host checked.
    ///
    /// The only path from an [`ObservedEvidence`] to the set `judge` reads, and
    /// it takes the host's [`TamperFinding`] as a parameter it cannot leave
    /// out. A host with no snapshot to compare passes
    /// [`TamperFinding::NotChecked`] and gets the abstention that answer has
    /// always earned — the difference is that it is now the host's answer about
    /// its own check, rather than a plugin's forced admission about a check it
    /// was never able to perform.
    ///
    /// The same argument decides the provenance, and decides it by
    /// construction rather than by rule: this function's input *is* the body
    /// of a plugin's `after_turn`, so the flip and the measurements are
    /// [`EvidenceProvenance::PluginReported`] and there is no argument by
    /// which a caller could say otherwise (#3513).
    #[must_use]
    pub fn from_observed(observed: ObservedEvidence, tamper: TamperFinding) -> Self {
        Self {
            provenance: EvidenceProvenance::PluginReported,
            flip: observed.flip,
            tamper,
            measurements: observed.measurements,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The structural half of #3499: `tamper` is not a word a plugin can say.
    /// A response carrying one is refused, exactly as an unknown manifest key
    /// is — this crate's whole posture is that a declaration which quietly does
    /// nothing is worse than one that refuses.
    #[test]
    fn a_plugin_cannot_report_a_tamper_finding() {
        let honest: ObservedEvidence =
            serde_json::from_str(r#"{"flip":"achieved","measurements":{"p50":103}}"#)
                .expect("the evidence a plugin can actually observe parses");
        assert_eq!(honest.flip, FlipObservation::Achieved);

        let smuggled =
            serde_json::from_str::<ObservedEvidence>(r#"{"flip":"achieved","tamper":"clean"}"#)
                .unwrap_err();
        assert!(
            smuggled.to_string().contains("tamper"),
            "a plugin claiming the host's tamper finding must be refused by \
             name, got {smuggled}"
        );
    }

    /// The merge is the only way to an [`EvidenceSet`] from a plugin's answer,
    /// and the host's finding is the one that lands in it.
    #[test]
    fn the_host_finding_is_what_reaches_the_judge() {
        let observed = ObservedEvidence {
            flip: FlipObservation::Achieved,
            measurements: BTreeMap::from([("p50".to_string(), 103)]),
            detail: Some("the timeout path is still unbounded".to_string()),
        };
        let merged = EvidenceSet::from_observed(observed.clone(), TamperFinding::Clean);
        assert_eq!(merged.flip, FlipObservation::Achieved);
        assert_eq!(merged.tamper, TamperFinding::Clean);
        assert_eq!(merged.measurements, observed.measurements);
        // And the advisory note does not cross: `EvidenceSet` is the closed
        // vocabulary `judge` is total over, so there is no field here for the
        // detail to land in (#3840). It rejoins after the verdict, on
        // `Verdict::with_detail`.
        assert_eq!(
            serde_json::to_value(&merged)
                .expect("an evidence set serializes")
                .get("detail"),
            None
        );

        let unchecked = EvidenceSet::from_observed(observed, TamperFinding::NotChecked);
        assert_eq!(
            unchecked.tamper,
            TamperFinding::NotChecked,
            "a host with no snapshot says so itself; it does not inherit a \
             plugin's inability to answer"
        );
    }

    #[test]
    fn an_observation_of_nothing_is_unobservable_not_empty() {
        let nothing = ObservedEvidence::nothing();
        assert_eq!(nothing.flip, FlipObservation::Unobservable);
        assert!(nothing.measurements.is_empty());

        // The two silences agree on everything `judge` reads — both abstain —
        // and differ on exactly one thing: whose silence it is. A plugin that
        // reported nothing said so itself; `unobserved` is the host's own
        // conclusion about a plugin that never answered (#3513).
        let reported = EvidenceSet::from_observed(nothing, TamperFinding::NotChecked);
        let host = EvidenceSet::unobserved();
        assert_eq!(reported.flip, host.flip);
        assert_eq!(reported.tamper, host.tamper);
        assert_eq!(reported.measurements, host.measurements);
        assert_eq!(reported.provenance, EvidenceProvenance::PluginReported);
        assert_eq!(host.provenance, EvidenceProvenance::HostObserved);
    }
}
