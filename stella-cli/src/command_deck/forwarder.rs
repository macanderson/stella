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
pub(crate) fn spawn_forwarder(
    mut rx: UnboundedReceiver<AgentEvent>,
    execution: Option<(Arc<Store>, i64)>,
    provider_id: String,
    inbound: UnboundedSender<Inbound>,
    lane: String,
) -> tokio::task::JoinHandle<bool> {
    tokio::spawn(async move {
        let mut seq = 0u64;
        let mut store_warned = false;
        let mut persistence_complete = true;
        while let Some(event) = rx.recv().await {
            if let Some((store, id)) = &execution {
                let outcome = agent::persist_event_detailed(store, *id, seq, &event, &provider_id);
                if !outcome.is_complete() {
                    persistence_complete = false;
                    // One warning per turn, and it names the condition that
                    // actually occurred: a failed INSERT points at the store,
                    // while unreported provider usage does not — conflating
                    // them sent users hunting for a database fault that was
                    // really a truncated model stream.
                    if !store_warned && let Some(message) = outcome.message("this session") {
                        store_warned = true;
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
        persistence_complete
    })
}
