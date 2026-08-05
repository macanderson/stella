//! Durable event forwarding from engine turns into command-deck lanes.
//! `spawn_forwarder` is the one seam shared by every deck lane (the lead's
//! turns and every `crate::subsession` worker).

use std::sync::Arc;

use stella_protocol::AgentEvent;
use stella_store::Store;
use stella_tui::Inbound;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::agent;
use crate::cache_insight::cache_insight_for;

/// Warn that execution closeout could not write its audit record (files
/// touched / memory citations / outcome).
///
/// `record_execution_end` folds `persistence_complete` into its own answer, so
/// a turn the forwarder already warned about arrives here guaranteed-false.
/// Emitting again restated one condition as two independent failures — the
/// pair of near-identical "store write failed" rows users read as compounding
/// damage. Only speak when the audit writes are the *new* thing that went
/// wrong; the caller re-asserts terminal status either way.
pub(crate) fn warn_audit_record_incomplete(
    inbound: &UnboundedSender<Inbound>,
    lane: &str,
    persistence_complete: bool,
) {
    if !persistence_complete {
        return;
    }
    let _ = inbound.send(Inbound::Event {
        agent: lane.to_string(),
        event: AgentEvent::Error {
            message: "store write failed — the audit record (files touched / memory \
                      citations / outcome) for this execution is incomplete"
                .to_string(),
            retryable: true,
        },
    });
}

/// Persist each event (via the shared [`agent::persist_event`] write path)
/// and forward it to the deck as `agent`'s `Inbound::Event`, plus a derived
/// `Inbound::CacheInsight` when the event carries pricing-relevant usage
/// (issues #267/#269). The returned bit is false after any persistence
/// failure and must be carried into execution closeout — callers fail
/// closed on incomplete telemetry rather than silently treating a partial
/// record as complete.
/// Forward engine events into a deck lane.
///
/// `plan_board` is the lane's task board (`ToolRegistry::task_board`). When a
/// `ScopeReview` goes past, the approved plan's steps are seeded onto it —
/// see [`stella_core::tasks::TaskBoard::seed_from_plan`] for why the two used
/// to be unconnected, and what that cost. `None` for lanes with no board of
/// their own.
pub(crate) fn spawn_forwarder(
    mut rx: UnboundedReceiver<AgentEvent>,
    execution: Option<(Arc<Store>, i64)>,
    provider_id: String,
    inbound: UnboundedSender<Inbound>,
    lane: String,
    plan_board: Option<stella_tools::tasks::TaskBoardHandle>,
) -> tokio::task::JoinHandle<bool> {
    tokio::spawn(async move {
        let mut seq = 0u64;
        // Two independent latches, not one. A single shared flag meant an
        // early usage gap — the benign, self-healing condition — silenced any
        // later store-write failure, which is the one that actually points at
        // a broken database. The less serious warning must never be able to
        // suppress the more serious one.
        let mut usage_warned = false;
        let mut store_warned = false;
        let mut persistence_complete = true;
        // The deck's half of the diagnostic timeline. The renderer covers the
        // one-shot and pipeline paths; every deck lane and sub-session worker
        // arrives here instead, and a bridge on only one of the two would make
        // the log silently depend on which shell the user happened to run in.
        let mut bridge = crate::diag_bridge::DomainBridge::new(
            crate::diag_boot::dx(),
            Some(crate::diag_boot::workspace_root()),
        );
        while let Some(event) = rx.recv().await {
            bridge.observe(&event);
            // A plan that reached the gate is the plan whose progress the
            // board records, so this is where it becomes one. Seeded on the
            // proposal rather than on approval: the ids must already resolve
            // when the first `task_start` arrives, and the gate's own outcome
            // reaches this task only as the absence of further events.
            // `seed_from_plan` is a no-op on a board that already has rows,
            // so a declined or revised plan cannot clobber live work.
            if let (AgentEvent::ScopeReview { proposal }, Some(board)) = (&event, &plan_board) {
                let mut guard = board.lock().unwrap_or_else(|p| p.into_inner());
                guard.seed_from_plan(&proposal.steps);
            }
            if let Some((store, id)) = &execution {
                let outcome = agent::persist_event_detailed(store, *id, seq, &event, &provider_id);
                if !outcome.is_complete() {
                    persistence_complete = false;
                    // One warning per condition per turn, each naming what
                    // actually occurred: a failed INSERT points at the store,
                    // while unreported provider usage does not — conflating
                    // them sent users hunting for a database fault that was
                    // really a truncated model stream.
                    //
                    // The scope word matters as much as the condition. A
                    // usage gap is scoped to "one model call" because that is
                    // all it ever was; saying "this session" turned a retried
                    // network blip into what looked like a session-wide
                    // accounting failure, and left that as the last word on
                    // the deck for the rest of the run.
                    let (warned, scope) = match outcome {
                        crate::agent::PersistOutcome::StoreWriteFailed => {
                            (&mut store_warned, "this session")
                        }
                        _ => (&mut usage_warned, "one model call"),
                    };
                    if !*warned && let Some(message) = outcome.message(scope) {
                        *warned = true;
                        let _ = inbound.send(Inbound::Event {
                            agent: lane.clone(),
                            event: AgentEvent::Error {
                                message,
                                retryable: true,
                            },
                        });
                    }
                }
                seq += 1;
            }
            // Derived BEFORE the event is moved into the send below, but
            // emitted AFTER it: the insight annotates the usage the event
            // carries, so the deck must fold the event first or the annotation
            // lands on a lane state that does not yet know about it.
            let cache_insight = cache_insight_for(&provider_id, &lane, &event);
            let _ = inbound.send(Inbound::Event {
                agent: lane.clone(),
                event,
            });
            if let Some(insight) = cache_insight {
                let _ = inbound.send(insight);
            }
        }
        bridge.finish();
        persistence_complete
    })
}
