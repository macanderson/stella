// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Tests for [`super`] — the queue walk's pure decision, `next_claimable`.
//!
//! Split out of `drive.rs` rather than baselined: that file reached the
//! 1500-line ceiling `make file-size` enforces and carries no entry in
//! `scripts/file-size-baseline.txt`, so a crossing fails the gate outright
//! rather than being grandfathered (AGENTS.md § *God files* — plan around
//! them, never into them). `use super::*` resolves to `drive` exactly as it
//! did inline, so this is a pure move.
use super::*;
// Named rather than reached through `super::claim`: inside this module
// `super` is `drive`, not `self_driving_cmd`, and `crate::ingest_cmd`
// has a private `Claim` of its own that rustc offers when the path is
// wrong — a confusing suggestion for a plain scoping slip.
use crate::self_driving_cmd::claim::Claim;

/// A ranked queue whose top entry a peer is holding yields the next one,
/// and says why it skipped.
///
/// The second half is what stops this passing vacuously: the identical
/// input under [`ContentionPolicy::Proceed`] must take the contended
/// candidate, so a `next_claimable` that ignored the verdict entirely
/// would fail the first half rather than satisfy both.
#[test]
fn a_candidate_a_peer_is_working_is_skipped_with_evidence() {
    let ranked = || ["3691".to_owned(), "3702".to_owned()];
    let seen = |key: &str| {
        if key == "3691" {
            Contention {
                local_worktrees: vec!["/tmp/wip-3691-preserved".to_owned()],
                ..Contention::default()
            }
        } else {
            Contention::default()
        }
    };

    let mut skipped: Vec<(String, Vec<String>)> = Vec::new();
    let pick = next_claimable(
        ranked(),
        |_| false,
        ContentionPolicy::Defer,
        seen,
        |_| Claim::Unavailable,
        |key, evidence| skipped.push((key.to_owned(), evidence.to_vec())),
    );

    assert!(matches!(&pick, Pick::Take(key, _) if key == "3702"));
    assert_eq!(
        skipped,
        vec![(
            "3691".to_owned(),
            vec!["local worktree: /tmp/wip-3691-preserved".to_owned()]
        )]
    );

    let mut skipped_under_proceed: Vec<String> = Vec::new();
    let pick = next_claimable(
        ranked(),
        |_| false,
        ContentionPolicy::Proceed,
        seen,
        |_| Claim::Unavailable,
        |key, _| skipped_under_proceed.push(key.to_owned()),
    );

    assert!(matches!(&pick, Pick::Take(key, _) if key == "3691"));
    assert!(skipped_under_proceed.is_empty());
}

/// **Witness (#4317).** The walk stops at its probe budget instead of
/// re-probing a contended backlog end to end on every poll.
///
/// A deferral deliberately writes no `spent` entry, so an uncapped walk
/// re-probes the whole contended prefix forever — and each probe is a `gh`
/// search call. `seen` is the closure that costs, so counting its calls is
/// counting the rate-limit spend.
#[test]
fn the_contention_scan_stops_at_its_probe_budget() {
    let over_budget = MAX_CONTENTION_PROBES + 3;
    let ranked: Vec<String> = (0..over_budget).map(|n| n.to_string()).collect();

    // `seen` is `Fn` — the production probe has no state — so the tally
    // borrows through a `RefCell` rather than the signature being widened
    // to `FnMut` for a test's convenience.
    let probed: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
    let pick = next_claimable(
        ranked,
        |_| false,
        ContentionPolicy::Defer,
        |key| {
            probed.borrow_mut().push(key.to_owned());
            // Every candidate contended, which is the backlog this bound
            // exists for: another actor working the queue wholesale.
            Contention {
                local_worktrees: vec![format!("/tmp/wip-{key}")],
                ..Contention::default()
            }
        },
        |_| Claim::Unavailable,
        |_, _| {},
    );
    let probed = probed.into_inner();

    assert!(
        matches!(pick, Pick::Truncated),
        "a scan that stopped early must say so — reported as Exhausted it \
         would tell an operator the rest of the queue was clear when \
         nothing had asked: {pick:?}"
    );
    assert_eq!(
        probed.len(),
        MAX_CONTENTION_PROBES as usize,
        "exactly the budget, no more: an uncapped walk probes all {over_budget}"
    );
}

/// A candidate this run already holds costs no budget.
///
/// The bound is on probes, not candidates. Counted per candidate, a long
/// prefix of already-taken issues would shorten the walk that matters —
/// and `taken` is free: no `gh` call, no git read, no SQLite open.
#[test]
fn an_already_taken_candidate_does_not_spend_probe_budget() {
    // Every one of these is skipped for free, so the single claimable
    // candidate after them is still reached.
    let taken_count = MAX_CONTENTION_PROBES + 5;
    let mut ranked: Vec<String> = (0..taken_count).map(|n| format!("taken-{n}")).collect();
    ranked.push("free".to_owned());

    let probed = std::cell::Cell::new(0_u32);
    let pick = next_claimable(
        ranked,
        |key| key.starts_with("taken-"),
        ContentionPolicy::Defer,
        |_| {
            probed.set(probed.get() + 1);
            Contention::default()
        },
        |_| Claim::Unavailable,
        |_, _| {},
    );
    let probed = probed.get();

    assert!(
        matches!(&pick, Pick::Take(key, _) if key == "free"),
        "the claimable candidate must still be reached: {pick:?}"
    );
    assert_eq!(probed, 1, "only the candidate that was not already taken");
}

/// A peer that took the lease *between* the probe and the write is one
/// more deferral, not the end of the pass.
///
/// This is the race the read-only probe structurally cannot close — both
/// loops can read "free" before either writes — so the loser has to keep
/// walking the queue, with the same audit line and the same counter as any
/// other contention (#4300).
#[test]
fn a_candidate_whose_lease_a_peer_won_is_skipped_like_any_other_deferral() {
    let mut skipped: Vec<(String, Vec<String>)> = Vec::new();
    let pick = next_claimable(
        ["1".to_owned(), "2".to_owned()],
        |_| false,
        ContentionPolicy::Defer,
        |_| Contention::default(),
        |key| {
            if key == "1" {
                Claim::HeldBy("self-driving:99999".to_owned())
            } else {
                Claim::Unavailable
            }
        },
        |key, evidence| skipped.push((key.to_owned(), evidence.to_vec())),
    );

    assert!(matches!(&pick, Pick::Take(key, None) if key == "2"));
    assert_eq!(
        skipped,
        vec![(
            "1".to_owned(),
            vec!["ledger claim: issue:1 held by self-driving:99999".to_owned()]
        )]
    );
}

/// A queue whose every candidate is contended is exhausted, not merely
/// stalled on the first one.
#[test]
fn a_fully_contended_queue_defers_every_candidate() {
    let mut skipped: Vec<String> = Vec::new();
    let pick = next_claimable(
        ["1".to_owned(), "2".to_owned()],
        |_| false,
        ContentionPolicy::Defer,
        |_| Contention {
            ledger_claims: vec!["fleet-run-7".to_owned()],
            ..Contention::default()
        },
        |_| Claim::Unavailable,
        |key, _| skipped.push(key.to_owned()),
    );

    assert!(matches!(pick, Pick::Exhausted));
    assert_eq!(skipped, vec!["1".to_owned(), "2".to_owned()]);
}
