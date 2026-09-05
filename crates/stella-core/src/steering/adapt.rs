// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Adapters: a selector's output, mapped into steering candidates (#3349).
//!
//! The migration contract is stated in the parent module and enforced by
//! `stella-cli`'s golden block test: an adapter maps what a selector **already
//! chose** into [`SteeringCandidate`]s — it does not re-select, re-rank, or
//! re-render. Every estimate is taken over the exact bytes the renderer will
//! emit, through the same producer function, so cost and bytes cannot drift.
//!
//! The record adapters live in `stella_records::adapt`; the engine does not
//! depend on that crate and names no record type.
//!
//! The recalled-frame adapter is not here either: `RecalledFrame`
//! was a staged-pipeline type when this was written (`crates/stella-pipeline`,
//! deleted in #3865; the type now lives in `stella-protocol::recall`), and the
//! adapter sits with the plane implementation in `stella-cli::memory::steering`.

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
///   plane: the section renderer, not the plane, made this cut,
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
