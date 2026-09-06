// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The turn-lane matrix — which builtin lanes say which lane they are.
//!
//! `stella_protocol::BuiltinLane` names every place a turn runs.
//! `Engine::assemble` lets each of them put its own name on
//! `agent.turn.started`. Having a name and saying it are two facts. A site
//! that hands over a seam set writing `lane: None` says nothing, and its
//! turns land in one `null` bucket. This table keeps the two facts
//! together.
//!
//! Same instrument as the two matrices beside it, pointed at lanes. One row
//! per case. A witness test per row, named and checked. The compiler enforces
//! that no case is missed. A lane with no producer needs a
//! [`LaneBinding::NoProducer`] row and a reason.
//!
//! **What it does not prove.** A `Bound` row's `how` is prose for a reviewer.
//! The checks are narrower: the named site really spells its lane, and the
//! named witness really exists. That is enough to make an author say who
//! stamps a lane. It is also enough to fail when a site goes back to the
//! builder path.
//!
//! What a lane *binds* is the neighbouring table, [`capability`]. This one
//! answers who names a lane; that one answers what a lane takes.

use stella_protocol::BuiltinLane;

pub mod capability;

/// Whether a lane has a producer, and what proves it.
#[derive(Debug)]
pub enum LaneBinding {
    /// Some assembly site stamps this lane.
    Bound {
        /// The workspace-relative file whose source names the lane. Checked:
        /// this file must spell `BuiltinLane::<variant>`.
        site: &'static str,
        /// How the lane reaches the engine, as prose for the reader.
        how: &'static str,
        /// Name of the test that proves the site names this lane. Checked
        /// for existence, as the two matrices beside this one check theirs.
        witness: &'static str,
    },
    /// Nothing stamps this lane and nothing will. The row carries the reason
    /// and the decision it cites. This is not "not yet": a lane waiting on
    /// something else would be a different posture, and there is none here.
    NoProducer {
        /// Why nothing produces it, with the decision cited.
        reason: &'static str,
    },
}

/// One builtin lane and how it is produced.
#[derive(Debug)]
pub struct Lane {
    /// The builtin lane this row is about.
    pub lane: BuiltinLane,
    /// Whether something stamps it, and what proves that.
    pub binding: LaneBinding,
}

/// Every builtin lane, with its producer or its written reason for having
/// none.
///
/// The tests below check both directions. Every [`BuiltinLane`] gets exactly
/// one row. Every row's claim is checked against the source it names.
pub const LANES: &[Lane] = &[
    Lane {
        lane: BuiltinLane::Lead,
        binding: LaneBinding::Bound {
            site: "crates/stella-cli/src/lane_capabilities.rs",
            how: "the deck's lead turn, assembled in `command_deck::lead_turn` from \
                  `lane_capabilities::lead`",
            witness: "every_lane_this_crate_assembles_declares_itself",
        },
    },
    Lane {
        lane: BuiltinLane::Resume,
        binding: LaneBinding::Bound {
            site: "crates/stella-cli/src/lane_capabilities.rs",
            how: "a turn replayed from a checkpoint, assembled in `agent::resume` from \
                  `lane_capabilities::resume`",
            witness: "every_lane_this_crate_assembles_declares_itself",
        },
    },
    Lane {
        lane: BuiltinLane::SubSession,
        binding: LaneBinding::Bound {
            site: "crates/stella-cli/src/lane_capabilities.rs",
            how: "a deck worker lane, assembled in `subsession::run_worker` from \
                  `lane_capabilities::sub_session`",
            witness: "every_lane_this_crate_assembles_declares_itself",
        },
    },
    Lane {
        lane: BuiltinLane::SubagentFork,
        binding: LaneBinding::Bound {
            site: "crates/stella-core/src/subagent.rs",
            how: "the child `Engine::run_sub_agent` forks — the first lane to stamp itself, \
                  and the reason the engine carries the field at all",
            witness: "a_forked_child_stamps_the_subagent_fork_lane",
        },
    },
    Lane {
        lane: BuiltinLane::FleetWorker,
        binding: LaneBinding::Bound {
            site: "crates/stella-cli/src/lane_capabilities.rs",
            how: "one fleet attempt, assembled in `fleet_cmd::run_task` from \
                  `lane_capabilities::fleet_attempt`",
            witness: "every_lane_this_crate_assembles_declares_itself",
        },
    },
    Lane {
        lane: BuiltinLane::PipelineStage,
        binding: LaneBinding::NoProducer {
            reason: "the built-in staged pipeline this case was named for is gone from the \
                     workspace, and a verification plugin's stage turn is a plugin lane: it \
                     arrives as `TurnLane::Plugin` carrying the id its manifest declared. So \
                     there is no producer to re-home onto this case. It is kept for reading \
                     rather than writing — `BuiltinLane` is closed with no `serde(other)`, so \
                     deleting it would fail a recording made before the removal outright \
                     instead of demoting it. Refs #3881",
        },
    },
    Lane {
        lane: BuiltinLane::ServeSession,
        binding: LaneBinding::Bound {
            site: "crates/stella-serve/src/session.rs",
            how: "a turn a remote host drives over the wire, assembled from \
                  `session::served_capabilities`",
            witness: "a_served_turn_declares_the_serve_session_lane",
        },
    },
    Lane {
        lane: BuiltinLane::RawTurn,
        binding: LaneBinding::Bound {
            site: "crates/stella-cli/src/lane_capabilities.rs",
            how: "the shared raw turn, assembled in both arms of `agent::turn::run_turn` from \
                  `lane_capabilities::raw_turn`",
            witness: "each_door_that_is_not_the_deck_assembles_through_its_lane",
        },
    },
    Lane {
        lane: BuiltinLane::GoalArc,
        binding: LaneBinding::Bound {
            site: "crates/stella-cli/src/lane_capabilities.rs",
            how: "a judged goal arc, assembled in `agent::goal` and its wrapped arm from \
                  `lane_capabilities::goal_arc`",
            witness: "each_door_that_is_not_the_deck_assembles_through_its_lane",
        },
    },
];

/// The row for `lane`, or `None`. The tests below make every builtin lane
/// resolve, so a `None` means the caller named something that is not a lane.
#[must_use]
pub fn row(lane: BuiltinLane) -> Option<&'static Lane> {
    LANES.iter().find(|row| row.lane == lane)
}

/// Every file a row may name as a site or a witness home, as
/// `(workspace-relative path, source)`.
///
/// Source text, not the module tree. `provider_parity` names the trade.
/// A site or a witness that moves out of this list fails loudly. Extend
/// the list to fix it. It never fails silently.
///
/// Shared with [`capability`], which reads the same files to check what each
/// lane binds.
#[cfg(test)]
pub(crate) fn lane_sources() -> [(&'static str, &'static str); 4] {
    [
        // Where most of this workspace's lanes declare what they bind,
        // and where their shared witnesses live.
        (
            "crates/stella-cli/src/lane_capabilities.rs",
            include_str!("../../stella-cli/src/lane_capabilities.rs"),
        ),
        // The fork's own assembly.
        (
            "crates/stella-core/src/subagent.rs",
            include_str!("../../stella-core/src/subagent.rs"),
        ),
        // The fork's witness lives in a split-out test module, which
        // `include_str!` of the parent does not pull in.
        (
            "crates/stella-core/src/subagent/tests/seams.rs",
            include_str!("../../stella-core/src/subagent/tests/seams.rs"),
        ),
        // The served turn's assembly and its witness, in one file.
        (
            "crates/stella-serve/src/session.rs",
            include_str!("../../stella-serve/src/session.rs"),
        ),
    ]
}

/// The source of one swept file, by its workspace-relative path.
#[cfg(test)]
pub(crate) fn source_named(path: &str) -> &'static str {
    lane_sources()
        .into_iter()
        .find(|(name, _)| *name == path)
        .map(|(_, source)| source)
        .unwrap_or_else(|| panic!("a lane row names `{path}`, which `lane_sources` does not carry"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Completeness, from the compiler's side and the table's.
    ///
    /// The match has no `_` arm. A case added to [`BuiltinLane`] breaks this
    /// file until somebody says what produces it. That is what a closed enum
    /// is for.
    #[test]
    fn every_builtin_lane_has_exactly_one_row() {
        for builtin in BuiltinLane::ALL {
            match builtin {
                BuiltinLane::Lead
                | BuiltinLane::Resume
                | BuiltinLane::SubSession
                | BuiltinLane::SubagentFork
                | BuiltinLane::FleetWorker
                | BuiltinLane::PipelineStage
                | BuiltinLane::ServeSession
                | BuiltinLane::RawTurn
                | BuiltinLane::GoalArc => {}
            }
            let rows = LANES.iter().filter(|row| row.lane == builtin).count();
            assert_eq!(rows, 1, "`{builtin}` must have exactly one row, has {rows}");
            assert!(row(builtin).is_some());
        }
        assert_eq!(
            LANES.len(),
            BuiltinLane::ALL.len(),
            "a row names something that is not a builtin lane",
        );
    }

    /// **The matrix witness.** Every `Bound` row's site really spells its
    /// lane.
    ///
    /// On a tree where only the fork stamps itself, this fails for the other
    /// five rows at once. A site whose seam set leaves `lane` unset never
    /// spells one.
    #[test]
    fn every_bound_lane_is_stamped_at_its_site() {
        for row in LANES {
            let LaneBinding::Bound { site, .. } = &row.binding else {
                continue;
            };
            let needle = format!("BuiltinLane::{:?}", row.lane);
            assert!(
                source_named(site).contains(&needle),
                "`{}` is declared bound at {site}, which does not name `{needle}` — a lane \
                 whose site stopped stamping it reports `null` like an unattributed door",
                row.lane,
            );
        }
    }

    /// Every `Bound` row's witness must exist.
    #[test]
    fn every_lane_witness_exists() {
        for row in LANES {
            let LaneBinding::Bound { witness, .. } = &row.binding else {
                continue;
            };
            let needle = format!("fn {witness}(");
            assert!(
                lane_sources()
                    .iter()
                    .any(|(_, source)| source.contains(&needle)),
                "witness for `{}` not found: {witness}",
                row.lane,
            );
        }
    }

    /// A `NoProducer` row must name its reason and cite the decision, and
    /// nothing in the tree may quietly start stamping it.
    #[test]
    fn a_lane_with_no_producer_says_why_and_has_none() {
        for row in LANES {
            let LaneBinding::NoProducer { reason } = &row.binding else {
                continue;
            };
            assert!(
                reason.contains("Refs #"),
                "`{}` has no producer and cites no decision. A silence is what this \
                 matrix exists to refuse.",
                row.lane,
            );
            let needle = format!("BuiltinLane::{:?}", row.lane);
            for (path, source) in lane_sources() {
                assert!(
                    !source.contains(&needle),
                    "{path} stamps `{needle}`, which is declared to have no producer. \
                     Either the site is wrong or the row is — say which.",
                );
            }
        }
    }
}
