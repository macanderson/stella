// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Properties of the one budgeter (#3243 Phase 2).

use super::*;
use proptest::prelude::*;

fn candidate(
    source: SteeringSource,
    handle: &str,
    score: f64,
    est_tokens: u64,
) -> SteeringCandidate {
    SteeringCandidate {
        source,
        handle: handle.into(),
        score,
        why: format!("because {handle}"),
        est_tokens,
    }
}

/// Nothing is invented and nothing vanishes: every candidate handed in comes
/// back exactly once, in `selected` or in `dropped`.
///
/// This is the property that makes a drop report trustworthy. A budgeter that
/// can lose a candidate silently produces the failure every existing plane
/// has — "it wasn't injected and nobody can say why".
#[test]
fn every_candidate_is_either_selected_or_reported_dropped() {
    let candidates = vec![
        candidate(SteeringSource::Record, "^db-url", 0.9, 40),
        candidate(SteeringSource::Skill, "sql-style", 0.5, 80),
        candidate(SteeringSource::Memory, "nod_a", 0.7, 500),
        candidate(SteeringSource::Tool, "read_file", 0.4, 30),
    ];

    let set = pack_to_budget(candidates.clone(), 200);

    let seen: Vec<&str> = set
        .selected
        .iter()
        .map(|c| c.handle.as_str())
        .chain(set.dropped.iter().map(|d| d.handle.as_str()))
        .collect();
    assert_eq!(
        seen.len(),
        candidates.len(),
        "no candidate is lost: {set:?}"
    );
    for c in &candidates {
        assert!(
            seen.contains(&c.handle.as_str()),
            "{} is unaccounted",
            c.handle
        );
    }
}

/// A record outranks a tool outranks a skill outranks a memory, whatever the
/// scores say — the four engines' numbers are not commensurable, so source
/// precedence decides across sources and `score` only within one.
#[test]
fn source_precedence_beats_score_across_sources() {
    // The memory scores highest and still loses: it is last in precedence.
    let set = pack_to_budget(
        vec![
            candidate(SteeringSource::Memory, "nod_a", 0.99, 100),
            candidate(SteeringSource::Record, "^rule", 0.01, 100),
        ],
        100,
    );

    assert_eq!(set.selected.len(), 1);
    assert_eq!(set.selected[0].handle, "^rule");
    assert_eq!(set.dropped[0].handle, "nod_a");
}

/// One oversized candidate must not evict everything behind it: the packer
/// skips what does not fit and keeps going.
#[test]
fn an_unaffordable_candidate_does_not_starve_the_cheap_ones_behind_it() {
    let set = pack_to_budget(
        vec![
            candidate(SteeringSource::Record, "^huge", 0.9, 10_000),
            candidate(SteeringSource::Skill, "cheap", 0.1, 10),
        ],
        100,
    );

    assert_eq!(
        set.selected
            .iter()
            .map(|c| c.handle.as_str())
            .collect::<Vec<_>>(),
        vec!["cheap"],
        "the affordable candidate still lands"
    );
    assert_eq!(set.dropped[0].handle, "^huge");
}

/// An estimate near `u64::MAX` must read as "does not fit", not wrap the
/// running sum back under the budget. With wrapping arithmetic the second
/// candidate below computes `10 + u64::MAX = 9`, "fits" a budget of 100, and
/// the budgeter selects the one candidate least able to afford — while the
/// cheap candidate behind it is squeezed out by the corrupted counter.
#[test]
fn a_token_estimate_near_u64_max_cannot_wrap_into_the_budget() {
    let set = pack_to_budget(
        vec![
            candidate(SteeringSource::Record, "^first", 0.9, 10),
            candidate(SteeringSource::Skill, "absurd", 0.5, u64::MAX),
            candidate(SteeringSource::Memory, "nod_cheap", 0.5, 10),
        ],
        100,
    );

    assert_eq!(
        set.selected
            .iter()
            .map(|c| c.handle.as_str())
            .collect::<Vec<_>>(),
        vec!["^first", "nod_cheap"],
        "the overflowing estimate is dropped and the cheap one still lands"
    );
    assert_eq!(set.dropped[0].handle, "absurd");
}

/// The block a selection renders into is prompt bytes, and prompt bytes feed
/// the cache — so the same candidates must pack to the same order every time,
/// including when scores tie. A tie broken by hash order would reorder the
/// volatile tail between two identical turns and re-bill it (invariant 7).
#[test]
fn ties_break_deterministically_by_handle() {
    let tied = || {
        vec![
            candidate(SteeringSource::Skill, "zebra", 0.5, 10),
            candidate(SteeringSource::Skill, "alpha", 0.5, 10),
            candidate(SteeringSource::Skill, "mango", 0.5, 10),
        ]
    };

    let first = pack_to_budget(tied(), 1_000);
    let again = pack_to_budget(tied(), 1_000);

    assert_eq!(first, again, "packing is a function of its input");
    assert_eq!(
        first
            .selected
            .iter()
            .map(|c| c.handle.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "mango", "zebra"]
    );
}

proptest! {
    /// The budget is never exceeded, for any input. The one thing a budgeter
    /// may not do is the thing it exists to prevent.
    #[test]
    fn the_budget_is_never_exceeded(
        costs in proptest::collection::vec(0u64..500, 0..40),
        budget in 0u64..2_000,
    ) {
        let candidates: Vec<_> = costs
            .iter()
            .enumerate()
            .map(|(i, &cost)| candidate(SteeringSource::Skill, &format!("s{i}"), 0.5, cost))
            .collect();

        let set = pack_to_budget(candidates, budget);

        prop_assert!(
            set.est_tokens() <= budget,
            "spent {} of {budget}",
            set.est_tokens()
        );
        prop_assert_eq!(set.selected.len() + set.dropped.len(), costs.len());
    }
}

// #3349 — the adapters: existing selector output mapped, never re-selected.

/// A registry whose volatile channel renders one record and budget-drops
/// another, for exercising both halves of the record adapter.
fn two_record_registry() -> crate::records::Registry {
    let file = crate::rules::RuleFile {
        path: ".stella/rules/ctx.acme.pair.toml".to_string(),
        contents: r#"
schema = "context-record/v0.1"
set_id = "acme"

[[record]]
lineage_id = "ctx.acme.staging-url"
kind = "preference"
statement = "The staging URL is https://stage.example."
status = "active"
origin = "user"

[record.steering]
force = "may"
precedence = 40

[[record]]
lineage_id = "ctx.acme.long-tail"
kind = "preference"
statement = "A long low-precedence fact that will not fit a tight budget at all."
status = "active"
origin = "user"

[record.steering]
force = "info"
"#
        .to_string(),
    };
    crate::records::registry::load(&[], &[file], &crate::records::Facts::default())
}

/// The skill adapter's estimate is measured over the exact block the section
/// renderer emits — one producer, so cost and bytes cannot drift.
#[test]
fn a_skill_candidate_costs_exactly_its_rendered_block() {
    let sel = crate::skills::SelectedSkill {
        skill: crate::skills::Skill {
            name: "reviewer".into(),
            description: "database review".into(),
            domains: vec![],
            body: "ALWAYS_REVIEW_DATABASES".into(),
            source_path: ".stella/skills/reviewer/SKILL.md".into(),
            origin: crate::skills::SkillOrigin::Workspace,
        },
        score: 0.42,
        matched_terms: vec!["database".into(), "review".into()],
        matched_domains: vec![],
    };

    let candidates = adapt::skill_candidates(std::slice::from_ref(&sel));

    assert_eq!(candidates.len(), 1);
    let c = &candidates[0];
    assert_eq!(
        (c.source, c.handle.as_str()),
        (SteeringSource::Skill, "reviewer")
    );
    assert_eq!(c.score, 0.42);
    assert_eq!(
        c.est_tokens,
        stella_protocol::estimate_tokens(&crate::skills::rendered_skill_block(&sel)),
        "the estimate and the section renderer read the same bytes"
    );
    assert!(
        c.why.contains("database"),
        "the why carries the selector's evidence: {}",
        c.why
    );
}

/// The record adapter maps the channel's rendered handles to candidates and
/// its budget-evicted handles to the drop ledger — it never re-decides either
/// list, so the two together are exactly what the channel selected.
#[test]
fn record_candidates_and_drops_partition_the_channels_own_decision() {
    let registry = two_record_registry();
    let facts = crate::records::TurnFacts {
        text: "anything",
        paths: &[],
    };
    // A budget the first (stronger-force) record fits and the second does not.
    let rendered = registry.render_volatile_for_turn(&facts, Some(80));
    assert_eq!(rendered.rendered, vec!["staging-url"]);
    assert_eq!(rendered.dropped, vec!["long-tail"]);

    let candidates = adapt::record_candidates(&registry, &rendered);
    let drops = adapt::record_drops(&registry, &rendered);

    assert_eq!(candidates.len(), 1);
    let c = &candidates[0];
    assert_eq!(
        (c.source, c.handle.as_str()),
        (SteeringSource::Record, "staging-url")
    );
    assert!(c.est_tokens > 0, "a rendered record costs its bullet line");
    assert!(
        c.why.contains("^staging-url") && c.why.contains("precedence 40"),
        "the why names the handle and the declared rank: {}",
        c.why
    );
    assert_eq!(
        drops
            .iter()
            .map(|d| (d.source, d.handle.as_str()))
            .collect::<Vec<_>>(),
        vec![(SteeringSource::Record, "long-tail")],
        "the channel's named drops become the generalized ledger"
    );
}

/// Within the record source, the adapter's flattened score orders candidates
/// exactly as the channel's own eviction rank does: force first, declared
/// precedence within it.
#[test]
fn record_scores_preserve_the_channels_force_then_precedence_rank() {
    let registry = two_record_registry();
    let facts = crate::records::TurnFacts {
        text: "anything",
        paths: &[],
    };
    let rendered = registry.render_volatile_for_turn(&facts, None);
    assert_eq!(rendered.rendered.len(), 2, "no budget: both render");

    let candidates = adapt::record_candidates(&registry, &rendered);
    let may = candidates
        .iter()
        .find(|c| c.handle == "staging-url")
        .unwrap();
    let info = candidates.iter().find(|c| c.handle == "long-tail").unwrap();
    assert!(
        may.score > info.score,
        "`may` precedence 40 outranks `info` precedence 0: {} vs {}",
        may.score,
        info.score
    );
}

// ---- skill drops (#3358) ----

/// A skill whose name and description both carry the prompt's terms, so
/// selection is decided by score order rather than by whether it matched.
fn scoring_skill(name: &str, body_bytes: usize) -> crate::skills::Skill {
    crate::skills::Skill {
        name: name.to_string(),
        description: "format sql queries nicely".to_string(),
        domains: vec!["sql".to_string()],
        body: "x".repeat(body_bytes),
        source_path: format!("{name}.md"),
        origin: crate::skills::SkillOrigin::Workspace,
    }
}

/// **Witness (#3358, skills / top-k).** A skill that matched, scored, and lost
/// the last seat to `SelectionConfig::max_skills` is named in the ledger.
///
/// Fails on base, where `select_skills` `truncate`s and returns only the
/// survivors: the tail is gone before any adapter can see it, so a skill that
/// systematically loses its seat is indistinguishable from one that never
/// matched — the exact coverage gap #2709 made observable for records.
#[test]
fn a_skill_cut_by_top_k_is_named_in_the_ledger() {
    let skills: Vec<_> = ["a", "b", "c"]
        .iter()
        .map(|n| scoring_skill(n, 10))
        .collect();
    let config = crate::skills::SelectionConfig {
        max_skills: 2,
        ..crate::skills::SelectionConfig::default()
    };

    let selection = crate::skills::select_skills_reporting(
        &skills,
        "please format the sql for me",
        &["sql".to_string()],
        &config,
    );
    assert_eq!(
        selection.selected.len(),
        2,
        "top-k still cuts at max_skills"
    );

    let drops = adapt::skill_drops(&selection);
    let handles: Vec<&str> = drops.iter().map(|d| d.handle.as_str()).collect();
    assert_eq!(handles, vec!["c"], "the cut skill is named: {drops:?}");
    assert!(
        drops.iter().all(|d| d.source == SteeringSource::Skill
            && d.est_tokens
                == stella_protocol::estimate_tokens(&crate::skills::rendered_skill_block(
                    &selection.over_top_k[0]
                ))),
        "and costed over the block it would have rendered: {drops:?}"
    );
}

/// **Witness (#3358, skills / section budget).** A skill the section renderer
/// omits to fit `SKILLS_SECTION_TOKEN_BUDGET` is named in the ledger — and the
/// rendered bytes are untouched, because the renderer still makes that cut
/// itself.
///
/// Fails on base: the omission was an inline prose marker and nothing else, so
/// which skills it stood for reached neither stderr nor the plane.
#[test]
fn a_skill_omitted_by_the_section_budget_is_named_without_changing_the_bytes() {
    // Four skills whose bodies each fill the per-skill budget: three fit the
    // section budget, and the fourth is the one the renderer omits.
    let skills: Vec<_> = ["a", "b", "c", "d"]
        .iter()
        .map(|n| scoring_skill(n, 40_000))
        .collect();
    let selection = crate::skills::select_skills_reporting(
        &skills,
        "please format the sql for me",
        &["sql".to_string()],
        &crate::skills::SelectionConfig::default(),
    );
    assert_eq!(selection.selected.len(), 4, "all four survive top-k");

    let rendered = crate::skills::render_skills_section(&selection.selected);
    assert!(
        rendered.contains("### c") && !rendered.contains("### d"),
        "the renderer's own cut is unchanged: {rendered}"
    );

    let drops = adapt::skill_drops(&selection);
    assert_eq!(
        drops.iter().map(|d| d.handle.as_str()).collect::<Vec<_>>(),
        vec!["d"],
        "the omitted skill is named rather than left to the prose marker: {drops:?}"
    );
}
