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
//! case cannot escape it.

use serde::{Deserialize, Serialize};

// Doc-link target only: the type is named in `ModelCallRole`'s docs but not
// used in this module's code. `cfg(doc)` keeps rustdoc's intra-doc link
// resolving without an import that a normal build would flag as unused.
#[cfg(doc)]
use super::AgentEvent;

/// Concrete purpose of one provider call. This is more precise than the
/// router's tier role: an auxiliary call and the worker's own must stay
/// distinguishable in the paid-call ledger even when they share a
/// provider/model.
///
/// It names the calls the engine itself makes. A call the host spends for a
/// plugin is [`Self::Plugin`]. The seat name the plugin chose rides beside
/// it as data. A closed list cannot hold words it does not know, and a cost
/// report over an open one stops being auditable.
///
/// This vocabulary grows, and it is **not** forward-tolerant: [`Self::Unknown`]
/// is the `serde(default)` for an *absent* `role`, not a `serde(other)`
/// catch-all for an unrecognized one. A role token this build has never seen
/// fails its whole event — `step_usage`, `step_manifest`, `usage_incomplete` —
/// because a known `"type"` with a body that does not fit stays a hard error by
/// design (see the module docs). Adding a case here is therefore a
/// one-directional change in a way adding an [`AgentEvent`] case no longer
/// is.
//
// Everything a *maintainer* needs on top of the above is a
// non-doc comment: this doc comment is the `description` field of
// `docs/wire/agentevent.schema.json` and its TypeScript twin, so a note about
// Rust match exhaustiveness would ship to consumers who have no Rust. The
// tripwire for adding a case is documented on `model_call_roles!` below.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ModelCallRole {
    /// Legacy events written before call-role attribution existed. The default
    /// for an absent `role` field only — an unrecognized one is an error.
    #[default]
    Unknown,
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
    /// A call the host spent for a plugin, at a seat the plugin declared.
    ///
    /// Which seat is data, not a case here. The word is the plugin's own.
    /// This list can neither hold it nor read it, so it rides on the
    /// `sub_agent` bracket the child turn ran under. A consumer reads the
    /// name that ran from there.
    ///
    /// Six retired spellings land here too: `triage`, `research`, `plan`,
    /// `plan_repair`, `witness_author` and `witness_repair`. Each named a
    /// stage of a pipeline this engine does not run. A stream recorded while
    /// it did still parses, so the row keeps its cost, its tokens and its
    /// model.
    #[serde(
        alias = "triage",
        alias = "research",
        alias = "plan",
        alias = "plan_repair",
        alias = "witness_author",
        alias = "witness_repair"
    )]
    Plugin,
}

/// Declares the role family once and derives [`ModelCallRole::ALL`] from it,
/// so a consumer enumerating the family cannot be handed a short list.
///
/// The completeness argument is the point, and it is the compiler's, not a
/// reviewer's: the same token list produces both `ALL` and an exhaustive
/// `match` over [`ModelCallRole`]. A case added to the enum but not named
/// here fails that match with `E0004`, so the list is provably a superset of
/// the cases; `ALL` is built from that same list, so it is provably total.
/// There is no case count anywhere to fall out of date — Rust cannot count
/// cases on stable, and a hand-written length is exactly the drift this
/// replaces.
///
/// Modelled on `agent_event_tags!` in [the parent module](super), which binds
/// [`KNOWN_TYPE_TAGS`](super::KNOWN_TYPE_TAGS) to `AgentEvent` the same way.
macro_rules! model_call_roles {
    ($($variant:ident),* $(,)?) => {
        impl ModelCallRole {
            /// Every case of this enum, in declaration order.
            ///
            /// Derived from the `model_call_roles!` declaration, so it is
            /// total by construction — prefer it to any local "all roles"
            /// array. Order is the enum's own and is not a wire contract:
            /// treat it as a set unless you are rendering the family for a
            /// human, where declaration order is the readable one.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            /// Compile-time proof that [`Self::ALL`] names every case.
            ///
            /// Never called at runtime and does nothing: its
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
    Worker,
    DistressGuidance,
    Verdict,
    AgentAuthor,
    SkillAuthor,
    DomainInference,
    Reflection,
    Summarization,
    Plugin,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ALL` is derived from the macro list, so this cannot catch a *missing*
    /// case — the `E0004` does that, before this test can run. What it
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
            "ALL should open with the declaration-order first case"
        );
        assert_eq!(
            ModelCallRole::ALL.last(),
            Some(&ModelCallRole::Plugin),
            "ALL should close with the declaration-order last case"
        );
    }

    /// The retirement witness. Six spellings named stages of a pipeline this
    /// workspace deleted. Every recorded session still carries them.
    /// `step_usage` has no catch-all, so without the aliases below each one
    /// fails its whole event. The row would lose its cost, its tokens and
    /// its model along with the label.
    #[test]
    fn every_retired_spelling_still_parses() {
        for token in [
            "triage",
            "research",
            "plan",
            "plan_repair",
            "witness_author",
            "witness_repair",
        ] {
            let role: ModelCallRole = serde_json::from_str(&format!("\"{token}\""))
                .unwrap_or_else(|err| panic!("a recorded \"{token}\" does not parse: {err}"));
            assert_eq!(
                role,
                ModelCallRole::Plugin,
                "\"{token}\" names a stage this engine does not run"
            );
        }
    }

    /// Every role in the family must survive the wire, since `ALL` is what
    /// downstream parity witnesses iterate. A case whose serde tag does
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
