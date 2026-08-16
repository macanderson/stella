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
