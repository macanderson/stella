// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where a worker lane's control verbs land, driver-side.
//!
//! Split out of [`super`] (a god file, closed to growth) following the
//! `settle`/`steer` pattern. The deck's half — which key sends which verb,
//! and the two-press arming — lives in `stella_tui`'s SUB-AGENTS overlay;
//! this file is only what the driver does with a [`stella_tui::AgentControl`]
//! once it arrives, plus the ledger of verbs that must wait for a live
//! worker's `Ended` before they can finish.

use tokio::sync::mpsc::UnboundedSender;

use crate::config::Config;
use crate::subsession::{self, SubSessions, SupervisorMsg};
use stella_tui::{AgentStatus, Inbound};

/// Verbs accepted while a lane's worker was live, finished when its `Ended`
/// arrives. Restart must wait because a respawn before the old worker
/// settles would put two workers on one lane, sharing (and corrupting) its
/// channels; Delete must wait because a row removed ahead of the worker's
/// terminal status would be re-registered by it.
#[derive(Default)]
pub(super) struct Pending {
    pub(super) restarts: std::collections::HashSet<String>,
    pub(super) deletes: std::collections::HashSet<String>,
}

/// Route one Pause/Resume/Stop/Restart/Delete at a worker lane. Pause parks
/// the worker at its next step boundary (never mid-tool — the engine's
/// `TurnGate`); Resume releases it; Restart respawns the lane from its
/// retained spec, stopping the live worker first when necessary; Delete
/// takes the lane's row off the deck for good — stopping a live worker
/// first, spec dropped either way so a later Restart cannot revive it.
#[allow(clippy::too_many_arguments)]
pub(super) fn service(
    lane: &str,
    control: stella_tui::AgentControl,
    subs: &mut SubSessions,
    pending: &mut Pending,
    cfg: &Config,
    budget_limit: Option<f64>,
    session_id: &str,
    workspace_name: &str,
    in_tx: &UnboundedSender<Inbound>,
    sup_tx: &UnboundedSender<SupervisorMsg>,
) {
    match control {
        stella_tui::AgentControl::Stop => {
            subs.stop(lane);
        }
        stella_tui::AgentControl::Pause => {
            if subs.set_paused(lane, true) {
                let _ = in_tx.send(Inbound::Status {
                    agent: lane.to_string(),
                    status: AgentStatus::Paused,
                });
            }
        }
        stella_tui::AgentControl::Resume => {
            if subs.set_paused(lane, false) {
                let _ = in_tx.send(Inbound::Status {
                    agent: lane.to_string(),
                    status: AgentStatus::Running,
                });
            }
        }
        stella_tui::AgentControl::Restart => {
            if subs.is_live(lane) {
                pending.restarts.insert(lane.to_string());
                subs.stop(lane);
            } else {
                let _ = subsession::respawn(
                    lane,
                    subs,
                    cfg,
                    budget_limit,
                    session_id,
                    workspace_name,
                    in_tx,
                    sup_tx,
                );
            }
        }
        stella_tui::AgentControl::Delete => delete(lane, subs, pending, in_tx),
    }
}

/// Delete a lane: stop a live worker and park the deregister for its
/// `Ended` ([`finish_delete`]) — a row removed ahead of the worker's
/// terminal status would be re-registered by it — or, with no worker
/// behind the lane, drop the spec and take the row down now.
fn delete(
    lane: &str,
    subs: &mut SubSessions,
    pending: &mut Pending,
    in_tx: &UnboundedSender<Inbound>,
) {
    if subs.is_live(lane) {
        pending.deletes.insert(lane.to_string());
        pending.restarts.remove(lane);
        subs.stop(lane);
    } else {
        subs.forget(lane);
        let _ = in_tx.send(Inbound::Deregister {
            agent: lane.to_string(),
        });
    }
}

/// The Delete verb's second half, at the freed lane's `Ended`: `true` when a
/// delete was pending — the spec is dropped and the row deregistered, and
/// the caller must not respawn a restart that was armed earlier (the later
/// intent won at [`service`]).
pub(super) fn finish_delete(
    lane: &str,
    pending: &mut Pending,
    subs: &mut SubSessions,
    in_tx: &UnboundedSender<Inbound>,
) -> bool {
    if !pending.deletes.remove(lane) {
        return false;
    }
    pending.restarts.remove(lane);
    subs.forget(lane);
    let _ = in_tx.send(Inbound::Deregister {
        agent: lane.to_string(),
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// **The witness for Delete on an idle lane.** The row comes down at
    /// once — deregister sent, spec forgotten, so a later Restart has
    /// nothing to respawn from.
    #[tokio::test]
    async fn delete_on_an_ended_lane_deregisters_and_forgets_the_spec() {
        let mut subs = SubSessions::new();
        let generation = subs.started_for_test("req:1");
        subs.stop("req:1");
        assert!(subs.ended("req:1", generation), "the worker settled");
        assert!(subs.spec("req:1").is_some(), "the spec outlives the end");

        let (in_tx, mut in_rx) = mpsc::unbounded_channel();
        let mut pending = Pending::default();
        delete("req:1", &mut subs, &mut pending, &in_tx);
        assert!(subs.spec("req:1").is_none(), "delete drops the spec");
        assert!(pending.deletes.is_empty(), "nothing to wait for");
        match in_rx.recv().await {
            Some(Inbound::Deregister { agent }) => assert_eq!(agent, "req:1"),
            other => panic!("expected the row's deregister, got {other:?}"),
        }
    }

    /// **The witness for Delete on a live lane.** The verb stops the worker
    /// and parks the deregister; `finish_delete` at `Ended` sends it, drops
    /// the spec, and outranks a Restart armed earlier.
    #[tokio::test]
    async fn delete_on_a_live_lane_waits_for_ended_and_outranks_a_restart() {
        let mut subs = SubSessions::new();
        let generation = subs.started_for_test("req:1");
        let (in_tx, mut in_rx) = mpsc::unbounded_channel();
        let mut pending = Pending::default();
        pending.restarts.insert("req:1".to_string());

        delete("req:1", &mut subs, &mut pending, &in_tx);
        assert!(pending.deletes.contains("req:1"), "parked for Ended");
        assert!(
            pending.restarts.is_empty(),
            "the delete wins over the earlier restart"
        );
        assert!(
            subs.spec("req:1").is_some(),
            "a winding-down lane keeps its spec — the worker still owns it"
        );

        assert!(subs.ended("req:1", generation));
        assert!(finish_delete("req:1", &mut pending, &mut subs, &in_tx));
        assert!(subs.spec("req:1").is_none(), "spec dropped");
        match in_rx.recv().await {
            Some(Inbound::Deregister { agent }) => assert_eq!(agent, "req:1"),
            other => panic!("expected the row's deregister, got {other:?}"),
        }
        assert!(
            !finish_delete("req:1", &mut pending, &mut subs, &in_tx),
            "a delete finishes once"
        );
    }
}
