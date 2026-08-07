//! The call-role vocabulary and its own enumeration.
//!
//! Split out of `event.rs` (#1977) for two reasons. The parent was three
//! lines under the file-size ratchet, so the enumeration below had nowhere to
//! land there; and the enumeration belongs beside the enum in the first
//! place — every hand-written "every role" array elsewhere in the workspace
//! was a copy of this list that nothing checked.
//!
//! [`ModelCallRole::ALL`] is derived, not written: see the
//! `model_call_roles!` invocation at the bottom of this file for why a
//! variant cannot escape it.

use serde::{Deserialize, Serialize};

// Doc-link target only: the type is named in `ModelCallRole`'s docs but not
// used in this module's code. `cfg(doc)` keeps rustdoc's intra-doc link
// resolving without an import that a normal build would flag as unused.
#[cfg(doc)]
use super::AgentEvent;

/// Concrete purpose of one provider call. This is more precise than the
/// router's tier role: repair and guidance calls must remain distinguishable
/// in the paid-call ledger even when they share a provider/model.
///
/// This vocabulary grows, and it is **not** forward-tolerant: [`Self::Unknown`]
/// is the `serde(default)` for an *absent* `role`, not a `serde(other)`
/// catch-all for an unrecognized one. A role token this build has never seen
/// fails its whole event — `step_usage`, `step_manifest`, `usage_incomplete` —
/// because a known `"type"` with a body that does not fit stays a hard error by
/// design (see the module docs). Adding a variant here is therefore a
/// one-directional change in a way adding an [`AgentEvent`] variant no longer
/// is.
//
// Everything a *maintainer* needs on top of the above is deliberately a
// non-doc comment: this doc comment is the `description` field of
// `docs/wire/agentevent.schema.json` and its TypeScript twin, so a note about
// Rust match exhaustiveness would ship to consumers who have no Rust. The
// tripwire for adding a variant is documented on `model_call_roles!` below.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ModelCallRole {
    /// Legacy events written before call-role attribution existed. The default
    /// for an absent `role` field only — an unrecognized one is an error.
    #[default]
    Unknown,
    /// Prompt classification and tier routing.
    Triage,
    /// A read-only research sub-agent answering one of triage's pre-plan
    /// questions (#1778).
    Research,
    /// Authoring the ordered plan.
    Plan,
    /// Re-authoring a plan the parser or the scope gate rejected.
    PlanRepair,
    /// Writing the witness test that arms the flip oracle.
    WitnessAuthor,
    /// Fixing a witness that did not fail on the current code.
    WitnessRepair,
    /// The tool-calling loop that actually changes the workspace.
    Worker,
    /// Course-correction handed to a worker that is looping or stuck.
    DistressGuidance,
    /// The verifier's verdict call, on inconclusive deterministic evidence.
    ///
    /// Aliased: this call role shipped as `judge`, so every recorded model
    /// call in every stored session names it that way.
    #[serde(alias = "judge")]
    Verdict,
    /// Generating an agent definition.
    AgentAuthor,
    /// Generating a skill definition.
    SkillAuthor,
    /// Inferring the workspace's domains, for memory tagging and recall.
    DomainInference,
    /// Post-turn self-reflection writing improvement memories.
    Reflection,
    /// The overflow summarizer that replaces a history span with a summary.
    Summarization,
}

/// Declares the role family once and derives [`ModelCallRole::ALL`] from it,
/// so a consumer enumerating the family cannot be handed a short list.
///
/// The completeness argument is the point, and it is the compiler's, not a
/// reviewer's: the same token list produces both `ALL` and an exhaustive
/// `match` over [`ModelCallRole`]. A variant added to the enum but not named
/// here fails that match with `E0004`, so the list is provably a superset of
/// the variants; `ALL` is built from that same list, so it is provably total.
/// There is no variant count anywhere to fall out of date — Rust cannot count
/// variants on stable, and a hand-written length is exactly the drift this
/// replaces.
///
/// Modelled on `agent_event_tags!` in [the parent module](super), which binds
/// [`KNOWN_TYPE_TAGS`](super::KNOWN_TYPE_TAGS) to `AgentEvent` the same way.
macro_rules! model_call_roles {
    ($($variant:ident),* $(,)?) => {
        impl ModelCallRole {
            /// Every variant of this enum, in declaration order.
            ///
            /// Derived from the `model_call_roles!` declaration, so it is
            /// total by construction — prefer it to any local "all roles"
            /// array. Order is the enum's own and is not a wire contract:
            /// treat it as a set unless you are rendering the family for a
            /// human, where declaration order is the readable one.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// Compile-time proof that [`Self::ALL`] names every variant.
            ///
            /// Never called at runtime and deliberately does nothing: its
            /// body is an exhaustive `match`, and *that* is the assertion.
            /// The `const` item below forces it to be evaluated, so this
            /// cannot rot into dead code.
            const fn every_variant_is_in_all(self) {
                match self {
                    $(Self::$variant => (),)*
                }
            }
        }

        const _: () = ModelCallRole::Unknown.every_variant_is_in_all();
    };
}

model_call_roles! {
    Unknown,
    Triage,
    Research,
    Plan,
    PlanRepair,
    WitnessAuthor,
    WitnessRepair,
    Worker,
    DistressGuidance,
    Verdict,
    AgentAuthor,
    SkillAuthor,
    DomainInference,
    Reflection,
    Summarization,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ALL` is derived from the macro list, so this cannot catch a *missing*
    /// variant — the `E0004` does that, before this test can run. What it
    /// pins is the two ways a total list can still be wrong: a name repeated
    /// in the declaration, and the claim that the order is the enum's own.
    #[test]
    fn all_lists_every_role_once_in_declaration_order() {
        let mut seen = ModelCallRole::ALL.to_vec();
        seen.sort_by_key(|role| format!("{role:?}"));
        seen.dedup();
        assert_eq!(
            seen.len(),
            ModelCallRole::ALL.len(),
            "a role is named twice in model_call_roles!"
        );
        assert_eq!(
            ModelCallRole::ALL.first(),
            Some(&ModelCallRole::Unknown),
            "ALL should open with the declaration-order first variant"
        );
        assert_eq!(
            ModelCallRole::ALL.last(),
            Some(&ModelCallRole::Summarization),
            "ALL should close with the declaration-order last variant"
        );
    }

    /// Every role in the family must survive the wire, since `ALL` is what
    /// downstream parity witnesses iterate. A variant whose serde tag does
    /// not round-trip would make those witnesses assert against a role the
    /// store can never actually name.
    #[test]
    fn every_role_in_all_round_trips_through_serde() {
        for role in ModelCallRole::ALL {
            let json = serde_json::to_string(role).expect("role serializes");
            let back: ModelCallRole = serde_json::from_str(&json).expect("role deserializes");
            assert_eq!(&back, role, "role {role:?} did not round-trip as {json}");
        }
    }
}
