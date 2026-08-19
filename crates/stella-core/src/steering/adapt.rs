// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Adapters: each existing selector's output, mapped into steering candidates
//! (#3349).
//!
//! The migration contract is stated in the parent module and enforced by
//! `stella-cli`'s golden block test: an adapter maps what a selector **already
//! chose** into [`SteeringCandidate`]s — it does not re-select, re-rank, or
//! re-render. Every estimate here is taken over the exact bytes the existing
//! renderer will emit, through the same producer function, so the cost the
//! budgeter sees and the bytes the prompt pays for cannot drift apart (the
//! #3334 single-producer discipline).
//!
//! The recalled-frame adapter is deliberately **not** here: `RecalledFrame` is
//! a `stella-protocol` type (it was `stella-pipeline`'s until that crate's
//! removal census retargeted it, #3865), and the adapter belongs to the host
//! above this crate. It lives with the plane implementation in
//! `stella-cli::memory::steering`.

use crate::records::{Registry, RenderedChannel, render};
use crate::skills::{self, SelectedSkill};

use super::{DroppedCandidate, SteeringCandidate, SteeringSource};

/// The skills [`crate::skills::select_skills`] chose, as candidates.
///
/// `score` is the selector's own (lexical coverage + domain boost + recency
/// tie-break) — comparable within the source, per the contract on
/// [`SteeringCandidate::score`]. `est_tokens` is measured over
/// [`skills::rendered_skill_block`], the block the section renderer emits.
pub fn skill_candidates(selected: &[SelectedSkill]) -> Vec<SteeringCandidate> {
    selected
        .iter()
        .map(|sel| SteeringCandidate {
            source: SteeringSource::Skill,
            handle: sel.skill.name.clone(),
            score: sel.score,
            why: skill_why(sel),
            est_tokens: stella_protocol::estimate_tokens(&skills::rendered_skill_block(sel)),
        })
        .collect()
}

/// The selector's own evidence, in the words its drop report already uses.
fn skill_why(sel: &SelectedSkill) -> String {
    match (sel.matched_terms.is_empty(), sel.matched_domains.is_empty()) {
        (false, false) => format!(
            "matched terms: {}; active domains: {}",
            sel.matched_terms.join(", "),
            sel.matched_domains.join(", ")
        ),
        (false, true) => format!("matched terms: {}", sel.matched_terms.join(", ")),
        (true, false) => format!("active domains: {}", sel.matched_domains.join(", ")),
        (true, true) => "selected".to_string(),
    }
}

/// The skills this turn's two skill budgets evicted, as ledger entries
/// (#3358) — the drops that used to be the loudest silence on the plane.
///
/// Two cuts, and they are genuinely different questions, which is why both
/// are reported and neither is inferred from the other:
///
/// - **Top-k** ([`skills::SkillSelection::over_top_k`]) — matched, scored,
///   and lost a seat to `SelectionConfig::max_skills`. Estimated over the
///   block it would have rendered, the same producer the survivors' costs
///   come from, so "what would it have cost to widen `max_skills`" is
///   answerable from the ledger rather than by re-running selection.
/// - **Section budget** — survived top-k and then did not fit
///   `render_skills_section`'s own token budget, per
///   [`skills::section_fit`]. These candidates are still *selected* on the
///   plane, deliberately: the section renderer, not the plane, made this cut,
///   and the plane re-enacting it would change the rendered bytes. The ledger
///   reports it; folding the section budget into the plane's shared budget is
///   Phase 4 behavior change (#3243), sequenced apart from this ledger slice.
pub fn skill_drops(selection: &skills::SkillSelection) -> Vec<DroppedCandidate> {
    let fit = skills::section_fit(&selection.selected);
    selection.selected[fit..]
        .iter()
        .chain(selection.over_top_k.iter())
        .map(|sel| DroppedCandidate {
            source: SteeringSource::Skill,
            handle: sel.skill.name.clone(),
            est_tokens: stella_protocol::estimate_tokens(&skills::rendered_skill_block(sel)),
        })
        .collect()
}

/// The records the volatile channel rendered this turn, as candidates.
///
/// `score` flattens the exact importance `render::survivors` drops by —
/// effective force first, declared precedence within it — so the ledger's
/// within-source order agrees with the order the channel's own budget would
/// evict in. The flattening is lossless: strength is a `u8` and precedence a
/// `u32`, so `strength * 2^32 + precedence` is exact in an `f64` and compares
/// exactly as the tuple does.
///
/// A handle the registry cannot resolve is skipped: `rendered` came from this
/// same registry moments ago, so an unresolvable handle is a caller passing
/// mismatched arguments, and inventing a zero-cost candidate for it would
/// corrupt the ledger it feeds.
pub fn record_candidates(
    registry: &Registry,
    rendered: &RenderedChannel,
) -> Vec<SteeringCandidate> {
    rendered
        .rendered
        .iter()
        .filter_map(|handle| {
            let entry = registry.by_handle(handle)?;
            let input = entry.render_input();
            let force = render::effective_force(input.record, input.disposition);
            Some(SteeringCandidate {
                source: SteeringSource::Record,
                handle: handle.clone(),
                score: f64::from(force.strength()) * 4_294_967_296.0
                    + f64::from(input.record.record.precedence()),
                why: format!(
                    "record channel selected ^{handle} for this turn (force {}, precedence {})",
                    force.as_str(),
                    input.record.record.precedence()
                ),
                est_tokens: stella_protocol::estimate_tokens(&render::bullet(&input)),
            })
        })
        .collect()
}

/// The records the volatile channel's own budget evicted, as ledger entries —
/// the named-drop behavior [`super::SteeringSet::dropped`] generalizes to
/// every source. An unresolvable handle keeps its name with a zero estimate:
/// for a drop, the name is the report and the cost is already not being paid.
pub fn record_drops(registry: &Registry, rendered: &RenderedChannel) -> Vec<DroppedCandidate> {
    rendered
        .dropped
        .iter()
        .map(|handle| DroppedCandidate {
            source: SteeringSource::Record,
            handle: handle.clone(),
            est_tokens: registry
                .by_handle(handle)
                .map(|entry| {
                    stella_protocol::estimate_tokens(&render::bullet(&entry.render_input()))
                })
                .unwrap_or(0),
        })
        .collect()
}
