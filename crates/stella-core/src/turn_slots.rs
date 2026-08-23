// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Which `turn_instance` a model call lands on when several of them share one
//! execution row (#3833, #3882).
//!
//! # The constraint
//!
//! Context receipts key on `(execution_id, turn_instance, step, call_seq)` and
//! every turn restarts `step` at 0, so two calls sharing an execution row and a
//! `turn_instance` overwrite each other's `step_manifest`/`step_receipt` rows
//! unless they also differ in `call_seq` — and `call_seq` is a small closed set
//! ([`RECEIPT_SEQ_WORKER`](crate::receipts::RECEIPT_SEQ_WORKER) and its
//! siblings), not a counter anyone may extend. So `turn_instance` is the axis
//! that has to keep them apart.
//!
//! # The rule
//!
//! **A `turn_instance` is a lane plus a sequence number within that lane:**
//! `slot = seq * TURN_LANES + lane`, so `slot % TURN_LANES` names the lane and
//! `slot / TURN_LANES` names the call's position within it. Four lanes are
//! reserved ([`WORKER_LANE`], [`VERIFIER_LANE`], [`CHILD_TURN_LANE`],
//! [`FANOUT_LANE`]), and two calls in different lanes can never land on the
//! same slot however many calls each lane makes.
//!
//! # Why lanes rather than a window per round
//!
//! Because the two counters have different owners and neither can see the
//! other. A door counts *its own rounds*: `Engine::run_goal` knows it is on
//! round 3, `stella fleet`'s attempt driver knows it is on its second internal
//! turn. A host plane counts *its own calls*: `ChildTurns` knows it is spending
//! its fourth child turn, and has no idea which round it sits between — a
//! wrapper plugin's points run between the rounds they are about, so by
//! construction there is no round index at hand when the call is made.
//!
//! Handing each round a window and asking the host to write inside the current
//! one needs the host to be *told* which round it is in, at a moment when the
//! only thing that knows is a driver that is not running. That is the
//! allocation #3833 and #3882 were each blocked on, and it is the shape this
//! module exists to avoid: interleaving by residue lets each counter count
//! only what it owns and still never collide.
//!
//! # Who uses which lane
//!
//! | Lane | Sequence | Allocated by |
//! |---|---|---|
//! | [`WORKER_LANE`] | the round index, from 0 | `Engine::run_goal` via [`goal_round_turn_offset`](crate::goal::goal_round_turn_offset); `stella fleet`'s attempt driver |
//! | [`VERIFIER_LANE`] | the round index, from 0 | `Engine::assess`, which is handed the round's worker slot and adds one |
//! | [`CHILD_TURN_LANE`] | the child turn's index in the run | a wrapper plugin's `child_turn` plane |
//! | [`FANOUT_LANE`] | the fan-out's index in the run | a wrapper plugin's `candidate_fanout` plane |
//!
//! The last two are lanes this crate never writes to itself. They are declared
//! here anyway rather than in the host that fills them, because the whole
//! content of the rule is that the four counters agree on one partition, and a
//! partition stated in two places is a partition that drifts.
//!
//! A door that opens a **fresh execution row per turn** — `stella run`'s
//! one-shot driver — is outside the constraint entirely: its rounds are already
//! separated by `execution_id`. It still allocates through this rule, because
//! the host planes beside it cannot tell which kind of door they are serving.

/// How many lanes a `turn_instance` is partitioned into.
///
/// Four because four counters can run at once under one execution row: a
/// door's worker turn, its verifier, and the two planes a wrapper plugin's
/// host serves. Widening this renumbers every lane's slots, which is
/// observability-only — nothing reads a historical `turn_instance` back to
/// decide anything — but it does mean a run split across two builds would
/// number its receipts two ways, so it is not a number to change casually.
pub const TURN_LANES: u32 = 4;

/// The lane a door's own working turns take.
pub const WORKER_LANE: u32 = 0;

/// The lane an independent verifier's assessment of a working turn takes.
///
/// Deliberately [`WORKER_LANE`]`+ 1`: `Engine::assess` is handed the round's
/// worker slot and adds one, so the two stay adjacent and a reader scanning
/// receipts sees a round's pair together.
pub const VERIFIER_LANE: u32 = 1;

/// The lane a host's `child_turn` calls take, made on a wrapper plugin's
/// behalf.
pub const CHILD_TURN_LANE: u32 = 2;

/// The lane a host's `candidate_fanout` calls take, made on a wrapper plugin's
/// behalf.
///
/// One slot per fan-out, not per candidate: the candidates of a single fan-out
/// are told apart by `call_seq` within this slot, because a width the host
/// clamps is not a number that may reach a database key.
pub const FANOUT_LANE: u32 = 3;

/// The `turn_instance` of the `seq`-th call in `lane`.
///
/// Saturates into the top of its own lane rather than at `u32::MAX`, which
/// belongs to exactly one lane and would otherwise be handed to all four.
#[must_use]
pub fn slot(lane: u32, seq: u32) -> u32 {
    let lane = lane % TURN_LANES;
    // `u32::MAX / TURN_LANES` is the largest sequence whose slot still fits,
    // for every lane — the clamp is on the multiplicand rather than the
    // product so that saturation cannot move a call into a neighbour's lane.
    let seq = seq.min(u32::MAX / TURN_LANES);
    seq * TURN_LANES + lane
}

/// Which lane a `turn_instance` belongs to — the inverse of [`slot`]'s lane
/// argument, and how a reader of a receipt tells a verifier's call from the
/// worker's.
#[must_use]
pub fn lane_of(slot: u32) -> u32 {
    slot % TURN_LANES
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four lanes are four, and they are the whole partition.
    #[test]
    fn every_lane_is_distinct_and_within_the_partition() {
        let lanes = [WORKER_LANE, VERIFIER_LANE, CHILD_TURN_LANE, FANOUT_LANE];
        for lane in lanes {
            assert!(lane < TURN_LANES, "lane {lane} is outside the partition");
        }
        let mut sorted = lanes.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            lanes.len(),
            "two lanes share a residue, so their slots collide"
        );
        assert_eq!(
            usize::try_from(TURN_LANES).expect("a small count"),
            lanes.len(),
            "a lane exists that nothing names, or a name exists outside the partition"
        );
    }

    /// **Witness.** No two lanes ever produce the same slot, whatever each
    /// lane's counter has reached — the property #3833 and #3882 were each
    /// blocked on.
    #[test]
    fn no_two_lanes_ever_collide() {
        let lanes = [WORKER_LANE, VERIFIER_LANE, CHILD_TURN_LANE, FANOUT_LANE];
        for seq in 0..64u32 {
            for other in 0..64u32 {
                for (a, b) in lanes.iter().zip(lanes.iter().cycle().skip(1)) {
                    assert_ne!(
                        slot(*a, seq),
                        slot(*b, other),
                        "lane {a} seq {seq} collides with lane {b} seq {other}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_slot_reports_the_lane_it_was_allocated_in() {
        for lane in [WORKER_LANE, VERIFIER_LANE, CHILD_TURN_LANE, FANOUT_LANE] {
            for seq in [0, 1, 2, 17, 1_000, u32::MAX] {
                assert_eq!(lane_of(slot(lane, seq)), lane);
            }
        }
    }

    /// Saturation stays inside the lane it started in. A `saturating_mul` on
    /// the product would land every lane on `u32::MAX`, which is one lane's
    /// alone.
    #[test]
    fn saturation_stays_in_its_own_lane() {
        assert_eq!(slot(FANOUT_LANE, u32::MAX), u32::MAX);
        assert_eq!(slot(WORKER_LANE, u32::MAX), u32::MAX - 3);
        assert_eq!(lane_of(slot(WORKER_LANE, u32::MAX)), WORKER_LANE);
        assert_eq!(lane_of(slot(VERIFIER_LANE, u32::MAX)), VERIFIER_LANE);
        assert_eq!(lane_of(slot(CHILD_TURN_LANE, u32::MAX)), CHILD_TURN_LANE);
    }

    /// A round's worker and its verifier are adjacent, because `Engine::assess`
    /// reaches its own lane by adding one to the slot it was handed.
    #[test]
    fn the_verifier_lane_is_one_past_the_worker_s() {
        for round in 0..8u32 {
            assert_eq!(
                slot(WORKER_LANE, round) + 1,
                slot(VERIFIER_LANE, round),
                "assess adds one and must land in the verifier lane"
            );
        }
    }
}
