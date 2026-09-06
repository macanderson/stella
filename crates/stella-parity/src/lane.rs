// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The turn-lane matrix — which builtin lanes say which lane they are.
//!
//! `stella_protocol::BuiltinLane` names every place a turn runs.
//! `Engine::assemble` lets each of them put its own name on
//! `agent.turn.started`. Having a name and saying it are two facts. A site
//! that builds its engine with `Engine::with_sleeper` writes `lane: None` and
//! cannot say anything. Its turns then land in one `null` bucket. This table
//! keeps the two facts together.
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
//! The second column answers a different question: what a lane leaves behind
//! when it dies. [`stella_protocol::ResumeAuthority`] decides who owes that —
//! a lane that resumes itself owes nothing, a lane read by its parent or
//! re-run by a supervisor owes a terminal frame. The authority is not stored
//! here. It is read out of `BuiltinLane::resume_authority`, so a row can say
//! what the lane *does about* its authority and cannot invent the authority
//! itself.
//!
//! What a lane *binds* is the neighbouring table, [`capability`]. This one
//! answers who names a lane and what it leaves; that one answers what a lane
//! takes.

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

/// What a lane's [`stella_protocol::ResumeAuthority`] obliges it to leave
/// behind when it dies,
/// and what proves the obligation is met.
///
/// The authority itself is not stored. It is read out of
/// [`BuiltinLane::resume_authority`], so this column cannot claim one the type
/// does not hold — the tests below check the two agree.
#[derive(Debug)]
pub enum FrameObligation {
    /// The lane re-enters its own turn, so its record is a resume point and no
    /// terminal frame is owed. Legal only under
    /// [`stella_protocol::ResumeAuthority::Own`].
    ResumesItself {
        /// Where the lane picks its own turn back up, as prose for the reader.
        how: &'static str,
    },
    /// The lane writes a terminal frame when it dies. Checked: the writer must
    /// name `LaneRecorder::new`, the reader must name `TerminalFrame::read`,
    /// and the witness must exist.
    Frames {
        /// The workspace-relative file that gives this lane a recorder.
        writer: &'static str,
        /// The workspace-relative file that reads the frame back, which is
        /// what stops a write side landing with nothing to consume it.
        reader: &'static str,
        /// Name of the test proving a dead lane's frame reaches that reader.
        witness: &'static str,
    },
    /// The lane owes a frame under its authority and writes none. The row
    /// carries the reason and cites where the gap is decided.
    Unframed {
        /// Why no frame, with the issue.
        reason: &'static str,
    },
}

/// One builtin lane: how it is produced, and what it leaves when it dies.
#[derive(Debug)]
pub struct Lane {
    /// The builtin lane this row is about.
    pub lane: BuiltinLane,
    /// Whether something stamps it, and what proves that.
    pub binding: LaneBinding,
    /// What this lane's resume authority obliges it to leave behind.
    pub frame: FrameObligation,
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
        frame: FrameObligation::ResumesItself {
            how: "the deck's own session record. `session_persist::restore_conversation` \
                  re-enters it at the next start of the same session",
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
        frame: FrameObligation::ResumesItself {
            how: "this lane is the re-entry itself — `agent::resume` replays the checkpoint \
                  a prior turn of the same session left",
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
        frame: FrameObligation::Frames {
            writer: "crates/stella-cli/src/subsession.rs",
            reader: "crates/stella-cli/src/subsession/terminal_frame.rs",
            witness: "a_lane_that_failed_leaves_its_parent_a_frame_naming_the_last_committed_step",
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
        frame: FrameObligation::Unframed {
            reason: "`Engine::run_sub_agent` strips the checkpoint sink from the child config \
                     it builds, so a fork holds no durable record to write a frame into, and \
                     a killed child's talk reaches the parent only as the tool result the \
                     dispatch returns. A fork also has no identity a later process can name — \
                     it opens no execution row — so a key to bind a record under has to be \
                     decided before this row can move. Refs #6201",
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
        frame: FrameObligation::Frames {
            writer: "crates/stella-cli/src/fleet_cmd/durability.rs",
            reader: "crates/stella-cli/src/fleet_cmd/durability.rs",
            witness: "a_redispatched_attempt_re_enters_the_transcript_the_engine_discarded",
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
        frame: FrameObligation::Unframed {
            reason: "no turn runs on this lane, so none can die on it. The row above says why \
                     the case is kept for reading rather than writing, and a lane with no \
                     producer owes nothing. A verification plugin that stages its turns owes \
                     the frame under its own plugin lane. Refs #3881",
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
        frame: FrameObligation::ResumesItself {
            how: "the host that opened the session drives its next turn, so the session's own \
                  record is the resume point and the host is never a reader of somebody \
                  else's report",
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
        frame: FrameObligation::ResumesItself {
            how: "`SessionPresence::announce` binds the door's own session record, and \
                  `stella resume` re-enters it",
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
        frame: FrameObligation::ResumesItself {
            how: "a goal arc runs on the door's own session record, the same one the raw \
                  turn above binds, and `stella resume` re-enters it",
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
pub(crate) fn lane_sources() -> [(&'static str, &'static str); 7] {
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
        // The deck lane's frame: the writer that gives it a recorder…
        (
            "crates/stella-cli/src/subsession.rs",
            include_str!("../../stella-cli/src/subsession.rs"),
        ),
        // …and the reader the deck driver calls, with its witness.
        (
            "crates/stella-cli/src/subsession/terminal_frame.rs",
            include_str!("../../stella-cli/src/subsession/terminal_frame.rs"),
        ),
        // The fleet attempt's frame, writer and reader in one file.
        (
            "crates/stella-cli/src/fleet_cmd/durability.rs",
            include_str!("../../stella-cli/src/fleet_cmd/durability.rs"),
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

    use stella_protocol::ResumeAuthority;

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
    /// five rows at once. A site that builds its engine with
    /// `Engine::with_sleeper` takes no lane, so it never spells one.
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

    /// **The frame witness, and the `Own` arm's.** A row's obligation must
    /// match the authority the type holds: only a self-resuming lane may say
    /// it resumes itself, and every other lane must say what it writes or why
    /// it writes nothing.
    ///
    /// This is the check that could not exist before `ResumeAuthority` did.
    /// The three answers lived in prose — two doc comments naming a type the
    /// tree did not have — so nothing could disagree with anything.
    #[test]
    fn every_lane_declares_the_frame_its_authority_obliges() {
        for row in LANES {
            let authority = row.lane.resume_authority();
            let resumes_itself = matches!(row.frame, FrameObligation::ResumesItself { .. });
            assert_eq!(
                resumes_itself,
                authority == ResumeAuthority::Own,
                "`{}` resumes as `{authority}`, which does not match its frame row — a lane \
                 read by somebody else owes a frame, and one that comes back to its own turn \
                 must not leave a dead attempt's talk beside the live resume point",
                row.lane,
            );
            assert_eq!(
                !resumes_itself,
                authority.owes_a_terminal_frame(),
                "`{}` and its authority disagree about whether a frame is owed",
                row.lane,
            );
        }
    }

    /// A `Frames` row's writer really gives the lane a recorder, its reader
    /// really reads a frame back, and its witness exists.
    ///
    /// A write side with nothing reading it costs a serialization per dead
    /// lane and buys nothing, so a row names its reader and this check holds
    /// the name to a file that really reads a frame.
    #[test]
    fn every_framed_lane_writes_and_is_read() {
        for row in LANES {
            let FrameObligation::Frames {
                writer,
                reader,
                witness,
            } = &row.frame
            else {
                continue;
            };
            assert!(
                source_named(writer).contains("LaneRecorder::new"),
                "`{}` is declared to frame at {writer}, which gives no lane a recorder",
                row.lane,
            );
            assert!(
                source_named(reader).contains("TerminalFrame::read"),
                "`{}`'s frame is declared read at {reader}, which reads none — a write side \
                 with no reader is the trade this column exists to refuse",
                row.lane,
            );
            let needle = format!("fn {witness}(");
            assert!(
                lane_sources()
                    .iter()
                    .any(|(_, source)| source.contains(&needle)),
                "frame witness for `{}` not found: {witness}",
                row.lane,
            );
        }
    }

    /// A lane that owes a frame and writes none says why, and cites where that
    /// is decided. A lane with no producer at all can only be unframed: it
    /// runs no turn, so none can die on it.
    #[test]
    fn an_unframed_lane_says_why_and_a_lane_with_no_producer_is_one() {
        for row in LANES {
            if let FrameObligation::Unframed { reason } = &row.frame {
                assert!(
                    reason.contains("Refs #"),
                    "`{}` owes a frame, writes none, and cites nothing. A silence is what \
                     this column exists to refuse.",
                    row.lane,
                );
            }
            if matches!(row.binding, LaneBinding::NoProducer { .. }) {
                assert!(
                    matches!(row.frame, FrameObligation::Unframed { .. }),
                    "`{}` has no producer, so no turn of it can die — it cannot claim to \
                     write anything",
                    row.lane,
                );
            }
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
