// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Both sides of the seam matrix.
//!
//! The macro holds the shape. It spells every seam for every lane. The
//! pattern below breaks when the engine grows a slot.
//!
//! These tests hold the rest. Each row is read back against the literal its
//! lane writes. Each named test has to exist.

use stella_core::TurnCapabilities;
use stella_protocol::ModelCallRole;

use super::*;
use crate::lane::{LaneBinding, lane_sources, source_named};

/// The capability literal a lane writes, as source text.
///
/// Start at the row's anchor. Take the next open brace of the struct. Read
/// to the brace that closes it. Each failure names the file and the anchor,
/// so a moved literal says where to look.
fn literal(site: &LaneSite) -> &'static str {
    let source = source_named(site.file);
    let from_anchor = source.find(site.anchor).map(|at| &source[at..]);
    let from_anchor = from_anchor.unwrap_or_else(|| {
        panic!(
            "{} does not contain `{}`, so its capability literal cannot be found",
            site.file, site.anchor,
        )
    });
    let body = from_anchor
        .find("TurnCapabilities {")
        .map(|at| &from_anchor[at..])
        .unwrap_or_else(|| {
            panic!(
                "{} opens no capability literal after `{}`",
                site.file, site.anchor,
            )
        });

    let mut depth = 0usize;
    for (index, character) in body.char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &body[..=index];
                }
            }
            _ => {}
        }
    }
    panic!(
        "the capability literal after `{}` in {} never closes",
        site.anchor, site.file,
    )
}

/// **The totality witness.** Every slot the engine has is a seam here.
///
/// The pattern has no rest arm. A new slot stops this file building. The fix
/// needs a new [`Seam`] case. That case needs a line in the row macro. That
/// line stops every lane block until each lane answers.
#[test]
fn every_capability_slot_is_a_seam() {
    let TurnCapabilities {
        hooks,
        hook_approvals,
        calibration,
        gate,
        steering,
        requery,
        bus,
        outcomes,
        fallback,
        call_role,
        lane,
    } = TurnCapabilities::none();

    let slots = [
        (Seam::Hooks, hooks.is_none()),
        (Seam::HookApprovals, hook_approvals.is_none()),
        (Seam::Calibration, calibration.is_none()),
        (Seam::Gate, gate.is_none()),
        (Seam::Steering, steering.is_none()),
        (Seam::Requery, requery.is_none()),
        (Seam::Bus, bus.is_none()),
        (Seam::Outcomes, outcomes.is_none()),
        (Seam::Fallback, fallback.is_none()),
        (Seam::CallRole, call_role == ModelCallRole::Worker),
        (Seam::Lane, lane.is_none()),
    ];

    assert_eq!(
        slots.len(),
        Seam::ALL.len(),
        "the engine's slots and this matrix's seams must be the same list",
    );
    for &(seam, bare) in &slots {
        assert!(
            bare,
            "the bare capability set answers `{}` with something",
            seam.field(),
        );
    }
    for seam in Seam::ALL {
        assert!(
            slots.iter().any(|(named, _)| named == seam),
            "`{}` is a seam no engine slot is mapped to",
            seam.field(),
        );
    }
}

/// Every lane gets a block, and every block answers every seam once.
#[test]
fn every_builtin_lane_answers_every_seam_once() {
    for builtin in BuiltinLane::ALL {
        let block = row(builtin)
            .unwrap_or_else(|| panic!("`{builtin}` has no capability block in this matrix"));
        assert_eq!(
            block.seams.len(),
            Seam::ALL.len(),
            "`{builtin}` answers {} seams, and the engine has {}",
            block.seams.len(),
            Seam::ALL.len(),
        );
        for seam in Seam::ALL {
            let rows = block.seams.iter().filter(|row| row.seam == *seam).count();
            assert_eq!(
                rows,
                1,
                "`{builtin}` answers `{}` {rows} times",
                seam.field(),
            );
        }
    }
    assert_eq!(
        LANE_CAPABILITIES.len(),
        BuiltinLane::ALL.len(),
        "a block names something that is not a builtin lane",
    );
}

/// **The matrix witness.** Each claim is read back against the literal its
/// lane writes.
///
/// A `Bound` row names the text its lane writes. That text has to be there.
/// A row that does not bind makes the other claim. Its field has to be
/// written as nothing. Flip any row and this fails. So does a lane that
/// stops binding.
#[test]
fn a_seam_claim_agrees_with_its_lane_literal() {
    for block in LANE_CAPABILITIES {
        let Some(site) = &block.site else {
            continue;
        };
        let text = literal(site);

        for row in block.seams {
            let field = row.seam.field();
            let unbound = format!("{field}: None");
            match row.requested {
                SeamRequest::Bound { binds, .. } => {
                    assert!(
                        text.contains(binds),
                        "`{}` claims it binds `{field}` as `{binds}`, and {} writes no such text",
                        block.lane,
                        site.file,
                    );
                    assert!(
                        !text.contains(&unbound),
                        "`{}` claims it binds `{field}`, and {} writes `{unbound}`",
                        block.lane,
                        site.file,
                    );
                }
                SeamRequest::Declined { .. } | SeamRequest::Deferred { .. } => {
                    assert!(
                        text.contains(&unbound),
                        "`{}` claims it does not bind `{field}`, and {} does not write \
                         `{unbound}`",
                        block.lane,
                        site.file,
                    );
                }
            }
        }
    }
}

/// A block's site must be the one its producer row already named.
#[test]
fn a_block_names_the_same_site_as_its_producer_row() {
    for block in LANE_CAPABILITIES {
        let producer = crate::lane::row(block.lane)
            .unwrap_or_else(|| panic!("`{}` has no producer row", block.lane));
        match (&producer.binding, &block.site) {
            (LaneBinding::Bound { site, .. }, Some(mine)) => assert_eq!(
                *site, mine.file,
                "`{}` is produced at one file and claims its seams at another",
                block.lane,
            ),
            (LaneBinding::NoProducer { .. }, None) => {}
            _ => panic!(
                "`{}` disagrees with its producer row about whether anything assembles it",
                block.lane,
            ),
        }
    }
}

/// A lane nothing assembles binds nothing. There is no literal to bind in.
#[test]
fn a_lane_with_no_site_binds_nothing() {
    for block in LANE_CAPABILITIES {
        if block.site.is_some() {
            continue;
        }
        for row in block.seams {
            assert!(
                !row.requested.is_bound(),
                "`{}` binds `{}`, and nothing assembles it",
                block.lane,
                row.seam.field(),
            );
        }
    }
}

/// Every named witness must exist in the swept sources.
#[test]
fn every_named_seam_witness_exists() {
    for block in LANE_CAPABILITIES {
        for row in block.seams {
            let Some(Witness::Test(name)) = row.requested.witness() else {
                continue;
            };
            let needle = format!("fn {name}(");
            assert!(
                lane_sources()
                    .iter()
                    .any(|(_, source)| source.contains(&needle)),
                "the `{}` lane's `{}` row names `{name}`, which no swept source declares",
                block.lane,
                row.seam.field(),
            );
        }
    }
}

/// The count of seams with no test is exact. It only goes down.
///
/// Exact, so writing a test lowers it in the same change. A row that borrows
/// a neighbour's test would read better and prove less.
#[test]
fn the_unwitnessed_seam_count_matches_the_ratchet() {
    let counted = LANE_CAPABILITIES
        .iter()
        .flat_map(|block| block.seams.iter())
        .filter(|row| matches!(row.requested.witness(), Some(Witness::Literal)))
        .count();
    assert_eq!(
        counted, UNWITNESSED_SEAMS,
        "{counted} bound seams name no test, and the ratchet says {UNWITNESSED_SEAMS}. Write \
         the test and lower the number; never raise it",
    );
}

/// A deferred seam names the issue that will settle it and what it waits on.
#[test]
fn a_deferred_seam_cites_an_issue_and_what_it_waits_on() {
    let mut deferred = 0;
    for block in LANE_CAPABILITIES {
        for row in block.seams {
            let SeamRequest::Deferred { issue, waiting_on } = row.requested else {
                continue;
            };
            deferred += 1;
            let number = issue.strip_prefix("Refs #").unwrap_or_default();
            assert!(
                !number.is_empty() && number.chars().all(|digit| digit.is_ascii_digit()),
                "`{}` defers `{}` and cites `{issue}`, which is not an issue citation",
                block.lane,
                row.seam.field(),
            );
            assert!(
                waiting_on.split_whitespace().count() >= 6,
                "`{}` defers `{}` and does not say what the answer waits on",
                block.lane,
                row.seam.field(),
            );
        }
    }
    assert!(
        deferred > 0,
        "no row is deferred. If every gap really closed, delete the case rather than keep a \
         posture nothing uses",
    );
}

/// A declined seam says why. A bare refusal reads like the silence this
/// matrix exists to end.
#[test]
fn a_declined_seam_says_why() {
    for block in LANE_CAPABILITIES {
        for row in block.seams {
            let SeamRequest::Declined { reason } = row.requested else {
                continue;
            };
            assert!(
                reason.split_whitespace().count() >= 6,
                "`{}` declines `{}` with no reason a reviewer can weigh",
                block.lane,
                row.seam.field(),
            );
        }
    }
}

/// Every lane here is builtin. Each gets what it asks for.
///
/// The two columns are for a lane a plugin brings. There a gate stands
/// between the ask and the answer. No lane here is one. A row that starts to
/// hold a seam back has to name the gate that did it.
#[test]
fn every_row_today_is_granted_as_asked() {
    for block in LANE_CAPABILITIES {
        assert_eq!(
            block.origin,
            LaneOrigin::Builtin,
            "`{}` is not a builtin lane, and this matrix only holds those",
            block.lane,
        );
        for row in block.seams {
            assert!(
                matches!(row.granted, SeamGrant::AsRequested),
                "`{}` gets less than it asks for at `{}`, and it compiles its own literal",
                block.lane,
                row.seam.field(),
            );
        }
    }
}
