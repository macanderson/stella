// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Hold one thing back, so its worth can be measured.
//!
//! [`crate::comparison`] can only judge an arm it was given. It needs turns
//! that used the thing, and turns that did not. A normal session makes only
//! the first kind. A skill whose trigger matches is always put in, so one arm
//! fills up and the other stays empty.
//!
//! This is the schedule that makes the missing arm. On some turns it names
//! one item to leave out. The turn still runs, and the ledger gets a
//! control-arm row for that item.
//!
//! An item is a plain id string. This code never learns what the id names: a
//! skill, a memory, or a mined rule. [`crate::comparison`] keeps an arm
//! opaque the same way, so one schedule serves all three.
//!
//! A holdout costs the user a little quality, on purpose. [`disclosure`] is
//! the one sentence that says so.

/// Is `turn` one of the turns that holds an item back?
///
/// Every `rate`-th turn is. A `rate` of 0 or 1 never is. A rate of 1 would
/// hold something back every turn. That is an off switch, not a test.
///
/// `turn` has to be a stored count, not a time. `stella run` and a fleet task
/// are one turn per process. A count kept in the process is 1 on each of
/// them, so no turn is ever picked. The recall control keeps its own count in
/// the context store, and asks this same question of it, so the tree holds
/// one copy of the rule rather than two that can drift apart.
#[must_use]
pub fn is_scheduled(turn: u64, rate: u32) -> bool {
    ordinal(turn, rate).is_some()
}

/// Which holdout `turn` is, counting from zero, or `None` when it is not one.
///
/// A caller arms the schedule once per turn and picks later, so the two halves
/// run at different moments. This is what it carries between them: one number
/// that already knows the rate, rather than the turn and the rate again, which
/// a later read could get wrong.
#[must_use]
pub fn ordinal(turn: u64, rate: u32) -> Option<u64> {
    let rate = u64::from(rate);
    (rate > 1 && turn.is_multiple_of(rate)).then(|| turn / rate - 1)
}

/// Which item the `ordinal`-th holdout holds back, or `None` when there is
/// nothing to hold back.
///
/// One item at most. Hold two back and the turn cannot say which one the
/// outcome belongs to.
///
/// `eligible` is what the turn would otherwise use. Its order does not
/// matter. The pick reads a sorted copy, so it turns on the set and not on
/// how the caller ranked it. It also steps one place along that copy per
/// holdout, so every item earns an arm. A pick by score would only ever
/// measure whatever ranked first. A repeated id is left as it came: that is a
/// caller bug, not a fact about the schedule.
#[must_use]
pub fn pick<'a>(ordinal: u64, eligible: &[&'a str]) -> Option<&'a str> {
    if eligible.is_empty() {
        return None;
    }
    let mut sorted: Vec<&'a str> = eligible.to_vec();
    sorted.sort_unstable();
    // Both casts are exact. A `usize` fits a `u64` on every target here. The
    // rest of the division is smaller than the length, so it fits back.
    let index = (ordinal % sorted.len() as u64) as usize;
    sorted.get(index).copied()
}

/// The one line a surface shows to say what the holdout costs, or `None`
/// when nothing is held back.
///
/// `what` is the plain word for the thing, such as `"skill"`. The caller owns
/// it, because this code does not know what an id names.
#[must_use]
pub fn disclosure(rate: u32, what: &str) -> Option<String> {
    (rate > 1).then(|| {
        format!(
            "learning: 1 turn in {rate} runs with one matching {what} held back, \
             so Stella can measure whether it helps"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rate_th_turn_is_a_holdout_turn() {
        let scheduled: Vec<u64> = (1..=9).filter(|t| is_scheduled(*t, 3)).collect();
        assert_eq!(scheduled, vec![3, 6, 9]);
    }

    /// The ordinal counts holdouts, not turns, so the pick can rotate on it.
    #[test]
    fn the_ordinal_counts_the_holdouts() {
        assert_eq!(ordinal(3, 3), Some(0));
        assert_eq!(ordinal(6, 3), Some(1));
        assert_eq!(ordinal(9, 3), Some(2));
        assert_eq!(ordinal(4, 3), None);
    }

    /// A rate of 0 or 1 is off. One is the case to watch. It divides every
    /// turn, so a bare modulo would hold something back on all of them.
    #[test]
    fn a_rate_of_zero_or_one_holds_nothing_back() {
        for rate in [0, 1] {
            assert!((1..=20).all(|t| !is_scheduled(t, rate)), "rate {rate}");
            assert_eq!(ordinal(4, rate), None, "rate {rate}");
            assert_eq!(disclosure(rate, "skill"), None, "rate {rate}");
        }
    }

    /// The rotation is the point. Three holdouts over three items cover all
    /// three, so no item goes unmeasured forever.
    #[test]
    fn the_pick_walks_the_whole_set() {
        let items = ["cargo", "alpha", "bravo"];
        let picked: Vec<&str> = (0..4).filter_map(|n| pick(n, &items)).collect();
        // Sorted order is alpha, bravo, cargo — the pick does not follow the
        // caller's order.
        assert_eq!(picked, vec!["alpha", "bravo", "cargo", "alpha"]);
    }

    /// The pick is a function of the set, not of the order it arrives in.
    #[test]
    fn the_pick_ignores_the_callers_order() {
        let forward = pick(0, &["alpha", "bravo"]);
        let backward = pick(0, &["bravo", "alpha"]);
        assert_eq!(forward, backward);
        assert_eq!(forward, Some("alpha"));
    }

    #[test]
    fn an_empty_set_picks_nothing() {
        assert_eq!(pick(0, &[]), None);
        assert_eq!(pick(7, &[]), None);
    }

    #[test]
    fn the_disclosure_names_the_rate() {
        let line = disclosure(20, "skill").expect("a live rate says so");
        assert!(line.contains("1 turn in 20"), "{line}");
        assert!(line.contains("skill"), "{line}");
    }
}
