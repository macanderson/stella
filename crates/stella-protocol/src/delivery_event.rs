//! The payload of [`crate::AgentEvent::CandidateDelivery`] — what happened to the
//! winning candidate's work, said out loud instead of inferred (#2942).
//!
//! # Why this signal exists
//!
//! Adopting a candidate workspace into the real tree is one of the most
//! consequential things the pipeline does, and until this module it emitted
//! nothing of its own. The only trace was the burst of
//! [`crate::AgentEvent::FileChange`] rows adoption re-emits afterwards, and reading
//! that burst as "adoption ran" is an **inference**, not a declared signal —
//! the failure invariant "every emitted signal names its consumer" describes
//! from the other side.
//!
//! Worse, the inference is not invertible. Three different runs produce the
//! same evidence — no `file_change` rows:
//!
//! * a candidate whose work was **withheld** on an integrity refusal,
//! * a candidate that adopted an **empty** patch (the `NothingAttempted` rung,
//!   whose own premise is that nothing changed),
//! * a candidate whose adoption **failed** on a conflict.
//!
//! Every conclusion in #2927 had to be reconstructed by joining `verdict`
//! timestamps against `file_change` timestamps in `stella-events.jsonl` by
//! hand. #2943 made that strictly harder, not easier: delivery is no longer
//! gated on the verdict, so there are now several reasons a delivery happens
//! and one reason it is declined, and none of them was on the wire.
//!
//! # Why one enum and not two `Option`s
//!
//! [`DeliveryOutcome`] is a sum, so "delivered" and "declined" cannot both be
//! present and cannot both be absent. The alternative — one optional field per
//! arm — is the same either/or encoding that made a bench trial's verdict cell
//! render `✗ failed` for a trial that had scored reward 1.0: when a record can
//! represent two answers at once, the reader picks one and the other is
//! silently lost. A reader here must match, so it cannot skip an arm.
//!
//! # What this is a projection of
//!
//! Nothing here decides anything. `stella-pipeline`'s `pipeline::delivery`
//! module already holds the decision as a typed value, and these types are its
//! wire shape — which is why no new logic was needed to say what happened.

use serde::{Deserialize, Serialize};

/// What the pipeline did with the winning candidate's workspace.
///
/// Internally tagged on `outcome`, so a `jq` reader selects an arm by field
/// rather than by the presence or absence of a sibling key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "outcome", rename_all = "snake_case")]
#[cfg_attr(
    feature = "schema",
    schemars(
        description = "What the pipeline did with the winning candidate's workspace, and why. Internally tagged on `outcome`, so a reader selects an arm by that field rather than by the presence or absence of a sibling key."
    )
)]
pub enum DeliveryOutcome {
    /// The candidate's work was adopted into the real tree. The counts are
    /// what adoption itself measured (git's `numstat` plus the applied patch),
    /// not a re-derivation — so a zero-file delivery is a real, reportable
    /// answer and is distinguishable from a decline.
    Delivered {
        /// Files that did not exist in the real tree before this delivery.
        created: u32,
        /// Files whose contents changed.
        modified: u32,
        /// Files the delivery removed.
        deleted: u32,
        /// Lines added across every delivered file.
        lines_added: u64,
        /// Lines removed across every delivered file.
        lines_removed: u64,
        /// Whether a **passing verdict** stood behind the work.
        ///
        /// `false` is the ordinary case for a run that ran out of clock or came
        /// to rest on the revise rung, and it is deliberately not a reason to
        /// withhold: a verdict is a claim about the work, not what decides
        /// whether the work exists (#2927/#2943). Delivery never implies proof,
        /// and this field is what keeps the write from reading as a pass.
        proven: bool,
    },
    /// The work was not adopted, with the reason.
    Declined {
        /// Why. Typed, because a caller that has to branch on prose is
        /// parsing prose.
        reason: DeliveryDecline,
    },
}

/// Why a candidate's work did not reach the real tree.
///
/// Exhaustive over the pipeline's decision: the three arms are the two
/// `Withhold` paths of `pipeline::delivery::decide` plus the one way a
/// sanctioned delivery can still fail at the git layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DeliveryDecline {
    /// No candidate workspace was ever created — a degradable infrastructure
    /// failure, so no model call was made and there are no bytes anywhere to
    /// deliver. Distinct from an empty delivery, which had a workspace and
    /// found nothing in it.
    NothingCreated,
    /// The pipeline refused this candidate on an **integrity** finding: it
    /// wrote through its isolation, its witness artifact was mutated after its
    /// baseline was pinned, its worktree moved after verification, its seal
    /// could not be taken, or a mutation audit could not restore the tree.
    ///
    /// The one case where "deliver anyway" is wrong, and for a reason unrelated
    /// to quality: the bytes in the workspace are not the bytes any stage
    /// examined, so delivering them ships something nothing ever looked at.
    IntegrityRefusal,
    /// The delivery was sanctioned and `git apply` refused it — typically the
    /// user edited the same files mid-run. Adoption is all-or-nothing, so
    /// **nothing** was applied and the workspace is preserved at `root` for the
    /// work to be recovered by hand.
    AdoptFailed,
}

impl DeliveryOutcome {
    /// Whether the work reached the real tree. Convenience for a reader that
    /// only needs the one bit; the arm still carries the rest.
    #[must_use]
    pub fn delivered(&self) -> bool {
        matches!(self, Self::Delivered { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Invariant #4: every type crossing a crate boundary round-trips through
    /// `serde_json`.
    #[test]
    fn every_outcome_round_trips() {
        let cases = [
            DeliveryOutcome::Delivered {
                created: 1,
                modified: 2,
                deleted: 3,
                lines_added: 40,
                lines_removed: 5,
                proven: false,
            },
            DeliveryOutcome::Declined {
                reason: DeliveryDecline::NothingCreated,
            },
            DeliveryOutcome::Declined {
                reason: DeliveryDecline::IntegrityRefusal,
            },
            DeliveryOutcome::Declined {
                reason: DeliveryDecline::AdoptFailed,
            },
        ];
        for case in cases {
            let json = serde_json::to_string(&case).expect("serializes");
            assert_eq!(
                serde_json::from_str::<DeliveryOutcome>(&json).expect("deserializes"),
                case
            );
        }
    }

    /// The tag is what a `jq` reader selects on, so it is contract rather than
    /// an artifact of the variant's Rust name.
    #[test]
    fn the_outcome_tag_is_a_field_not_a_presence_test() {
        let json = serde_json::to_value(DeliveryOutcome::Declined {
            reason: DeliveryDecline::IntegrityRefusal,
        })
        .expect("serializes");
        assert_eq!(json["outcome"], "declined");
        assert_eq!(json["reason"], "integrity_refusal");
        assert!(
            json.get("created").is_none(),
            "a declined outcome carries no delivery counts to misread: {json}"
        );
    }

    /// An empty delivery is a delivery. This is the distinction the inferred
    /// `file_change` burst could not make, and the reason this event exists.
    #[test]
    fn an_empty_delivery_is_not_a_decline() {
        let empty = DeliveryOutcome::Delivered {
            created: 0,
            modified: 0,
            deleted: 0,
            lines_added: 0,
            lines_removed: 0,
            proven: true,
        };
        assert!(empty.delivered());
        assert!(
            !DeliveryOutcome::Declined {
                reason: DeliveryDecline::NothingCreated,
            }
            .delivered()
        );
    }
}
