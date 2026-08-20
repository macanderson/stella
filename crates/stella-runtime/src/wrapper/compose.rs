//! What several plugins have to agree on before they can serve one
//! `--pipeline` selection together (#3801).
//!
//! # Why this exists
//!
//! `doc:pipeline-as-plugins` §7's extraction plan turns each pipeline stage
//! into its own plugin — research, plan, witness, verify. If a selection can
//! only ever name one of them, then the only way to get what the deleted
//! staged pipeline did is one plugin that reimplements every stage, which is
//! the monolith again in a different language. So a selection names several,
//! and this module is the arithmetic of "several".
//!
//! # It decides at bind time, and it decides by refusing
//!
//! Everything here runs once, when the composition is bound, and every
//! disagreement is an error rather than a precedence rule. That is the whole
//! design: a silent winner between two manifests would be a decision no
//! manifest declared and no user consented to at install, taken at the exact
//! moment it is least visible.
//!
//! **Two** disagreements, and the fact that it is two rather than four is the
//! interesting part:
//!
//! - **Stage order** ([`WrapperError::ConflictingStageOrder`]). The merged
//!   order decides what a later stage can read, because a stage's
//!   contribution reaches the next one through `BeforeTurnRequest::published`.
//!   Picking one member's order over another's would silently decide whose
//!   grounding the other one plans against.
//! - **Two arbiters** ([`WrapperError::TwoArbiters`]) — two things holding one
//!   turn open and deciding when it is done.
//!
//! The first draft of this module also refused two `[oracle]` blocks and one
//! requirement name meaning two things. Both were **unreachable**, and finding
//! that out is what makes the rule here as small as it is:
//! `ManifestError::OracleRequiresArbiter` and `RequirementsRequireArbiter`
//! refuse their block below arbiter grade, so two oracles or two requirement
//! sets are *already* two arbiters. One rule subsumes all three.
//!
//! Those checks were deleted rather than kept as belt-and-braces. An error
//! variant that cannot fire is worse than no variant: it reads to the next
//! maintainer as a guarantee somebody tested, and it is dead code that
//! survives every gate.
//!
//! # What it deliberately does not do
//!
//! It does not merge behaviour, only declarations. The per-round fold — whose
//! messages land in what order, which role intent wins, how scopes union — is
//! [`super::dispatch`]'s, because that fold already exists there across the
//! stages of one plugin and composition is the same fold one level up.

use std::collections::BTreeMap;

use stella_plugin::{LoopGrant, Participation, PluginManifest, StageName, VerdictRule};

use super::error::WrapperError;

/// What a set of members agreed on, computed once at bind time.
pub(crate) struct Composition {
    /// Every stage any member declares, in the one order they all agree with.
    ///
    /// This is the *declared* order, not the resolved one: conditions are
    /// answered per member at run time against that member's own signals, and
    /// the resolved subsets are walked in this order.
    pub(crate) stage_order: Vec<StageName>,
    /// The union of what the members require, and the one oracle among them.
    pub(crate) rule: VerdictRule,
    /// The grant `again` consults to decide whether a round may be held open.
    ///
    /// The arbiter's, when the composition has one — and it has at most one,
    /// because [`WrapperError::TwoArbiters`] refuses a second. With no arbiter
    /// it is the first member's, which is a non-arbiter grant and therefore
    /// cannot hold anything open: the composition stops after one round, which
    /// is exactly what a set of steering-grade contributors should do.
    ///
    /// Deliberately one member's grant rather than a merged one. `max_holds`
    /// is a number a human consented to beside a named plugin at install; a
    /// merged ceiling would be a number nobody agreed to.
    pub(crate) hold_grant: LoopGrant,
}

/// Merge the members' declarations, or name the first disagreement.
///
/// # Errors
///
/// [`WrapperError::EmptyComposition`] for no members, and one of the four
/// conflict variants in this module's header for members that cannot be
/// reconciled.
pub(crate) fn compose(manifests: &[PluginManifest]) -> Result<Composition, WrapperError> {
    if manifests.is_empty() {
        return Err(WrapperError::EmptyComposition);
    }
    let stage_order = merge_stage_order(manifests)?;
    let rule = merge_rule(manifests)?;
    let hold_grant = manifests
        .iter()
        .find(|manifest| manifest.loop_grant.participation == Participation::Arbiter)
        .or_else(|| manifests.first())
        .map(|manifest| manifest.loop_grant.clone())
        // Unreachable: the empty case returned above. A default grant is
        // `none`, which holds nothing open — the inert answer, not a guess.
        .unwrap_or_default();
    Ok(Composition {
        stage_order,
        rule,
        hold_grant,
    })
}

/// Weave every member's declared stage list into one order all of them agree
/// with.
///
/// A member's list is read as a set of "this before that" constraints over the
/// stages it names, and nothing more — it says nothing about a stage it does
/// not declare, so a member naming `[research]` composes with one naming
/// `[triage, plan]` without either constraining the other.
///
/// The walk keeps a cursor into the order built so far. A stage already placed
/// must sit at or after the cursor, because everything before the cursor is a
/// stage this member has already passed; finding it earlier means this member
/// wants it *after* something the accumulated order puts it *before*, which is
/// the contradiction. A stage not yet placed is inserted at the cursor, which
/// is the only position consistent with both what this member said and
/// everything already agreed.
fn merge_stage_order(manifests: &[PluginManifest]) -> Result<Vec<StageName>, WrapperError> {
    let mut order: Vec<StageName> = Vec::new();
    // Which member first placed each stage — carried only so a conflict can
    // name both sides rather than only the one that noticed. An association
    // list rather than a map because `StageName` is deliberately not `Ord`
    // (it is an open vocabulary, #3963), and the list is as long as the stage
    // order, which is a handful of entries.
    let mut placed_by: Vec<(StageName, String)> = Vec::new();

    for manifest in manifests {
        let Some(wrapper) = manifest.wrapper.as_ref() else {
            continue;
        };
        let mut cursor = 0usize;
        let mut previous: Option<StageName> = None;

        for stage in &wrapper.stages {
            let name = &stage.name;
            match order.iter().position(|placed| placed == name) {
                Some(position) if position < cursor => {
                    // `previous` is Some whenever the cursor has moved, and the
                    // cursor has moved whenever `position < cursor` can hold.
                    let second = previous.clone().unwrap_or_else(|| name.clone());
                    return Err(WrapperError::ConflictingStageOrder {
                        wrapper: placed_by
                            .iter()
                            .find(|(placed, _)| placed == name)
                            .map_or_else(|| manifest.name.clone(), |(_, by)| by.clone()),
                        other: manifest.name.clone(),
                        first: name.to_string(),
                        second: second.to_string(),
                    });
                }
                Some(position) => cursor = position + 1,
                None => {
                    order.insert(cursor, name.clone());
                    placed_by.push((name.clone(), manifest.name.clone()));
                    cursor += 1;
                }
            }
            previous = Some(name.clone());
        }
    }
    Ok(order)
}

/// The union of the members' requirements, plus the one oracle among them.
///
/// # Why there is no requirement-collision check here
///
/// There cannot be a collision. `ManifestError::RequirementsRequireArbiter`
/// refuses `[requirements]` on any manifest below arbiter grade, and
/// [`WrapperError::TwoArbiters`] below refuses a second arbiter — so at most
/// one member of a valid composition carries requirements at all, and a union
/// over "at most one non-empty set" cannot disagree with itself.
///
/// That is a load-bearing assumption rather than an observation, which is why
/// `wrapper_composition.rs` pins both halves of it: a steering manifest
/// declaring `[requirements]` is refused at parse, and two arbiters are
/// refused at bind. If either ever loosens, this fold needs the check that is
/// deliberately absent, and the test that fails is the one that says so.
fn merge_rule(manifests: &[PluginManifest]) -> Result<VerdictRule, WrapperError> {
    let mut requirements: BTreeMap<String, String> = BTreeMap::new();
    let mut oracle = None;
    let mut arbiter: Option<String> = None;

    for manifest in manifests {
        // Arbiter first, so the grade conflict is what a caller is told about
        // when a composition collides on several axes at once — it is the most
        // structural of them, and the one that explains the others.
        if manifest.loop_grant.participation == Participation::Arbiter {
            if let Some(first) = arbiter.as_ref() {
                return Err(WrapperError::TwoArbiters {
                    wrapper: first.clone(),
                    other: manifest.name.clone(),
                });
            }
            arbiter = Some(manifest.name.clone());
        }

        requirements.extend(manifest.requirements.clone().unwrap_or_default());

        // No "two oracles" check: `ManifestError::OracleRequiresArbiter` means
        // an oracle implies arbiter grade, and the check above already refused
        // a second arbiter — so by the time control reaches here, at most one
        // member can have one.
        if let Some(declared) = manifest.oracle.as_ref() {
            oracle = Some(declared.clone());
        }
    }

    Ok(VerdictRule {
        requirements,
        oracle,
    })
}
