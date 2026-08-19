//! Durable event forwarding from engine turns into command-deck lanes.
//! `spawn_forwarder` is the one seam shared by every deck lane (the lead's
//! turns and every `crate::subsession` worker).

use std::sync::Arc;

use stella_protocol::AgentEvent;
use stella_store::Store;
use stella_tui::Inbound;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::agent;
use crate::cache_insight::{InsightScope, cache_insight_for};
use crate::memory::TurnFriction;

/// What a lane's closed turn stream leaves behind for its owner.
///
/// Two facts, not one, since #3962. `persistence_complete` was always the
/// forwarder's answer; `friction` is the fold of the same stream that
/// reflection reads — and it is folded HERE, in the one seam every deck lane
/// shares, rather than in `command_deck.rs`, because that file is a god file
/// closed to growth and because a lane that starts reflecting later should
/// inherit the ledger rather than re-derive it.
///
/// [`Default`] is `persistence_complete: false` — the answer a forwarder that
/// panicked or was cancelled owes, which is what its `unwrap_or(false)` said
/// before this struct existed.
#[derive(Default)]
pub(crate) struct LaneStreamEnd {
    pub(crate) persistence_complete: bool,
    pub(crate) friction: TurnFriction,
}

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
    scope: InsightScope,
    inbound: UnboundedSender<Inbound>,
    lane: String,
    plan_board: Option<stella_tools::tasks::TaskBoardHandle>,
) -> tokio::task::JoinHandle<LaneStreamEnd> {
    tokio::spawn(async move {
        let mut seq = 0u64;
        // The lane's friction ledger (#3962), folded as the stream goes past
        // rather than from a collected `Vec` the way `agent::run_turn` does:
        // this task forwards each event and keeps none, and a ledger is bounded
        // where a retained journal is not.
        //
        // This declaration is the one #3962 and #3969 composed away between
        // them: both PRs were green, and the textual merge took #3969's version
        // of the hunk that opened this task — which carried its own `reasoning`
        // binding and not this one — while keeping #3962's two *uses* below.
        // Neither author's tree was wrong, which is why no pre-merge check
        // could see it (AGENTS.md, `main-canary.yml`).
        let mut friction = TurnFriction::default();
        // This lane's open run of streamed reasoning fragments, joined into one
        // row per thought rather than one per few tokens (#3969). One run per
        // forwarder, and a forwarder is one lane — every lane has a registry
        // and a channel of its own, so two lanes' thoughts cannot fuse.
        let mut reasoning = agent::ReasoningRun::default();
        // Two independent latches, not one. A single shared flag meant an
        // early usage gap — the benign, self-healing condition — silenced any
        // later store-write failure, which is the one that actually points at
        // a broken database. The less serious warning must never be able to
        // suppress the more serious one.
        let mut usage_warned = false;
        let mut store_warned = false;
        let mut persistence_complete = true;
        // Shared with every blocking persistence hop below, so the provider id
        // is not re-allocated once per persisted event.
        let provider_id: Arc<str> = scope.provider_id.as_str().into();
        // The deck's half of the diagnostic timeline. The renderer covers the
        // one-shot and pipeline paths; every deck lane and sub-session worker
        // arrives here instead, and a bridge on only one of the two would make
        // the log silently depend on which shell the user happened to run in.
        let mut bridge = crate::diag_bridge::DomainBridge::new(
            crate::diag_boot::dx(),
            Some(crate::diag_boot::workspace_root()),
        );
        // The lane's own stage boundaries (#3416). Synthesized on the
        // receiving side because the deck's send side lives in
        // `command_deck.rs`, a god file closed to growth — and threaded
        // through this one seam so the lead's turns and every sub-session
        // worker cannot drift. Both go through the whole body below (persist,
        // bridge, forward) exactly as the engine's copies used to.
        //
        // `held` is how the closing boundary keeps its place *ahead* of the
        // terminal `TurnComplete` it annotates: the boundary is delivered in
        // the arriving event's slot and the `TurnComplete` rides the next
        // iteration. Latched, so the re-delivered event cannot match again.
        let mut held: Option<AgentEvent> = None;
        // The opening `Stage(Execute)` is emitted *lazily*, not pre-seeded,
        // so a leading `ContextRecall` rides ahead of it — the same
        // recall-before-stage order `agent/output.rs::open_raw_turn`
        // documents as an invariant. Recall was assembled before the turn
        // began, so a receipt that ordered the first stage ahead of it would
        // misdescribe when the context entered. The lead lane sends that
        // recall onto this channel *after* spawning the forwarder, so
        // pre-seeding `Execute` in `held` delivered it before recall could be
        // drained; deferring the open until the first non-recall event keeps
        // recall first.
        let mut owes_stage_execute = scope.opens_execute_stage;
        let mut owes_stage_complete = scope.opens_execute_stage;
        // The lane's run ending, as a backstop (#3379/#3398). The engine ends
        // each turn with `TurnComplete` and deliberately says nothing about
        // the run, so the run-terminal `RunComplete` is the owner's to emit.
        // Most deck lanes emit it themselves, because they alone know the
        // run's true total: a staged lane's cost spans roles that never appear
        // as a turn ending at all, so a total reconstructed here would
        // under-report it. A lane that emits nothing — the subsession worker —
        // still owes its deck row an ending, and this forwarder is the one
        // place that knows the *stream* closed rather than merely that a turn
        // did, which is what "the terminator is last" requires.
        //
        // So it synthesizes only for a stream that ended without one.
        // Unconditional synthesis would put two terminators on every lane that
        // does emit its own, which `replay::validate_terminal` calls a
        // violation. Doing it inside the loop rather than after it puts the
        // event through the same persist-then-forward path as everything else,
        // so the lane's journal and the deck agree.
        let mut turn_end: Option<(String, f64)> = None;
        let mut run_ended = false;
        loop {
            let event = match held.take() {
                Some(held) => held,
                None => match rx.recv().await {
                    Some(event) => event,
                    None => match turn_end.take().filter(|_| !run_ended) {
                        Some((model, cost_usd)) => AgentEvent::RunComplete { model, cost_usd },
                        // The run's owner already ended it, the stream sealed
                        // on a previous pass, or no turn of it ever succeeded
                        // — a failed run ends on `Error`, never `RunComplete`.
                        None => break,
                    },
                },
            };
            let event = if owes_stage_execute && !matches!(event, AgentEvent::ContextRecall { .. })
            {
                // First event that is not the turn's recall: open the stage
                // ahead of it, and let it ride the next iteration.
                owes_stage_execute = false;
                held = Some(event);
                AgentEvent::Stage {
                    name: stella_protocol::StageKind::Execute.into(),
                    scope: stella_protocol::StageScope::Run,
                }
            } else if owes_stage_complete && matches!(event, AgentEvent::TurnComplete { .. }) {
                owes_stage_complete = false;
                held = Some(event);
                AgentEvent::Stage {
                    name: stella_protocol::StageKind::Complete.into(),
                    scope: stella_protocol::StageScope::Run,
                }
            } else {
                event
            };
            match &event {
                AgentEvent::TurnComplete { model, cost_usd } => {
                    turn_end = Some((model.clone(), *cost_usd));
                }
                AgentEvent::RunComplete { .. } => run_ended = true,
                _ => {}
            }
            bridge.observe(&event);
            friction.observe(&event);
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
            let event = if let Some((store, id)) = &execution {
                // Fold a streamed reasoning fragment into the open run rather
                // than spending a row, a `seq` and a blocking hop on each one
                // (#3969); anything else closes the run and is written after
                // it, which is what keeps the table in stream order.
                let owed = reasoning.admit(&event);
                if owed.is_empty() {
                    // Buffered. The event still reaches the deck below — only
                    // the durable write coalesces, never the painting.
                    event
                } else {
                    // Off the reactor for the same reason the one-shot renderer's
                    // hop exists — see `agent::persistence::spawn_renderer`, which
                    // carries the measured numbers. This lane matters at least as
                    // much: the deck is the default interactive shell on a TTY, so
                    // a five-second `busy_timeout` stall here is a five-second
                    // unresponsive UI.
                    //
                    // The event moves in and back out rather than being cloned; it
                    // is still owed to `cache_insight_for` and the deck below.
                    let store = Arc::clone(store);
                    let id = *id;
                    let provider_id = Arc::clone(&provider_id);
                    // See the renderer's twin: a task that dies with its writes in
                    // an unknown state must over-advance `seq`, never under.
                    let owed_writes =
                        u64::from(owed.block.is_some()) + u64::from(owed.writes_arriving);
                    let joined = tokio::task::spawn_blocking(move || {
                        let (outcome, next) =
                            agent::persist_owed(&store, id, seq, owed, &event, &provider_id);
                        (event, outcome, next)
                    })
                    .await;
                    let (event, outcome, next_seq) = match joined {
                        Ok(triple) => triple,
                        // The event went with the panicking or shutting-down task,
                        // so this lane loses one rendered event; what it must not
                        // lose is the admission that persistence is incomplete.
                        Err(_) => {
                            persistence_complete = false;
                            seq += owed_writes;
                            continue;
                        }
                    };
                    seq = next_seq;
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
                    event
                }
            } else {
                event
            };
            // Derived BEFORE the event is moved into the send below, but
            // emitted AFTER it: the insight annotates the usage the event
            // carries, so the deck must fold the event first or the annotation
            // lands on a lane state that does not yet know about it.
            let cache_insight = cache_insight_for(&scope, &lane, &event);
            let _ = inbound.send(Inbound::Event {
                agent: lane.clone(),
                event,
            });
            if let Some(insight) = cache_insight {
                let _ = inbound.send(insight);
            }
        }
        // The lane's stream is closed, so its open reasoning run is final: a
        // turn the user interrupted mid-thought keeps what it had streamed.
        if !agent::flush_reasoning_tail(&mut reasoning, &execution, &provider_id, &mut seq).await {
            persistence_complete = false;
        }
        bridge.finish();
        LaneStreamEnd {
            persistence_complete,
            friction,
        }
    })
}

/// End a lane's turn stream and wait out its forwarder — the deck-lane twin
/// of [`agent::close_event_stream`], owing the same debt in the same order.
///
/// `drop(tx)` on its own never closes the channel: the turn handed the
/// registry an `EventSender` clone (`ToolRegistry::attach_events`) so
/// registry-born events — the task board, sub-agent lifecycle — ride the
/// turn's own stream, and the registry outlives the
/// turn — the deck's session registry by design, a worker lane's because the
/// closing future itself still holds the `Arc`. With that clone alive, the
/// forwarder's `recv()` loop stayed pending forever and awaiting it wedged
/// the turn future *after* the deck had painted the turn done: the driver
/// never left its mid-turn `select!`, every prompt queued as the "next turn"
/// of a turn that could not end, and only `/clear` — the one input that
/// breaks that `select!` — could free the session (#2290). One-shot
/// `stella run` hit the same shape as #960; the deck lanes reintroduced it
/// when #903 pointed the recorder at the turn channel without paying the
/// detach the fix demands.
///
/// Detaching unconditionally is safe against concurrent lanes because turns
/// do not overlap on one registry (the invariant
/// [`stella_tools::subagent::TurnControlsGuard`] documents): the lead's turns
/// run sequentially on the session registry, and every worker lane builds a
/// registry of its own.
pub(crate) async fn close_turn_stream(
    registry: &stella_tools::ToolRegistry,
    tx: UnboundedSender<AgentEvent>,
    forwarder: tokio::task::JoinHandle<LaneStreamEnd>,
) -> LaneStreamEnd {
    registry.detach_event_stream();
    drop(tx);
    forwarder.await.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The frozen-deck witness (#2290): a turn's tail must complete even
    /// though the registry still holds the `EventSender` the turn attached.
    /// Before [`close_turn_stream`], every lane's tail was `drop(tx)` +
    /// `forwarder.await` — with the registry's clone alive the forwarder
    /// never saw the channel close, the turn future never returned, and the
    /// deck queued prompts forever under a lane it had painted done.
    #[tokio::test]
    async fn the_turn_tail_completes_with_the_registry_still_attached() {
        let root = tempfile::tempdir().expect("root");
        let registry = stella_tools::ToolRegistry::new(root.path().to_path_buf());
        let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let forwarder = spawn_forwarder(
            rx,
            None,
            InsightScope {
                provider_id: "anthropic".into(),
                cache_ttl: stella_model::CacheTtl::default(),
                opens_execute_stage: true,
            },
            in_tx,
            "lead".to_string(),
            None,
        );
        registry.attach_events(stella_core::EventSender::new(tx.clone()));
        tx.send(AgentEvent::TurnComplete {
            model: "m".into(),
            cost_usd: 0.0,
        })
        .unwrap();

        let settled = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            close_turn_stream(&registry, tx, forwarder),
        )
        .await;

        let ended = settled.expect("the registry-held sender must not wedge the turn tail");
        assert!(
            ended.persistence_complete,
            "an event-only stream persists cleanly"
        );
        // The forwarded event reached the deck lane before the stream closed.
        assert!(matches!(in_rx.try_recv(), Ok(Inbound::Event { .. })));
    }

    /// Drain a finished lane's forwarded `AgentEvent`s in order.
    async fn lane_events(
        registry: &stella_tools::ToolRegistry,
        tx: UnboundedSender<AgentEvent>,
        forwarder: tokio::task::JoinHandle<LaneStreamEnd>,
        in_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Inbound>,
    ) -> Vec<AgentEvent> {
        close_turn_stream(registry, tx, forwarder).await;
        let mut events = Vec::new();
        while let Ok(inbound) = in_rx.try_recv() {
            if let Inbound::Event { event, .. } = inbound {
                events.push(event);
            }
        }
        events
    }

    fn stages(events: &[AgentEvent]) -> Vec<stella_protocol::StageName> {
        events
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Stage { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect()
    }

    /// A raw deck lane owns its own stage boundaries (#3416).
    ///
    /// The engine used to open every turn with `Stage(Execute)` and close a
    /// completed one with `Stage(Complete)`. It no longer emits either —
    /// `StageKind` is the run owner's vocabulary, and the engine cannot know
    /// which stage of which run its caller is in. The deck's HUD still reads
    /// both, and the deck's send side is in a god file, so this seam
    /// synthesizes them: the opener ahead of everything, the closer ahead of
    /// the `TurnComplete` it annotates — never behind it, which is where a
    /// post-turn emit would land and where the consumers that stop at the
    /// terminal event would never see it.
    #[tokio::test]
    async fn a_raw_lane_frames_its_turn_with_its_own_stage_boundaries() {
        let root = tempfile::tempdir().expect("root");
        let registry = stella_tools::ToolRegistry::new(root.path().to_path_buf());
        let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let forwarder = spawn_forwarder(
            rx,
            None,
            InsightScope {
                provider_id: "anthropic".into(),
                cache_ttl: stella_model::CacheTtl::default(),
                opens_execute_stage: true,
            },
            in_tx,
            "lead".to_string(),
            None,
        );
        tx.send(AgentEvent::Text { text: "hi".into() }).unwrap();
        tx.send(AgentEvent::TurnComplete {
            model: "m".into(),
            cost_usd: 0.0,
        })
        .unwrap();

        let events = lane_events(&registry, tx, forwarder, &mut in_rx).await;
        assert_eq!(
            stages(&events),
            vec![
                stella_protocol::StageName::from(stella_protocol::StageKind::Execute),
                stella_protocol::StageName::from(stella_protocol::StageKind::Complete)
            ],
            "a raw lane frames its turn: {events:?}"
        );
        let complete = events
            .iter()
            .position(|event| matches!(event, AgentEvent::TurnComplete { .. }))
            .expect("the terminal event survives");
        let stage_complete = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    AgentEvent::Stage { name, .. }
                        if name.kind() == Some(stella_protocol::StageKind::Complete)
                )
            })
            .expect("the closing boundary is emitted");
        assert!(
            stage_complete < complete,
            "the closing boundary rides AHEAD of the terminal event: {events:?}"
        );
    }

    /// The turn's `ContextRecall` rides AHEAD of the synthesized opening
    /// `Stage(Execute)` (#3416). Recall was assembled before the turn began,
    /// so a receipt that ordered the first stage ahead of it would
    /// misdescribe when the context entered — the same invariant
    /// `agent/output.rs::open_raw_turn` documents for the one-shot path. The
    /// lead lane sends recall onto the channel *after* spawning the
    /// forwarder, so the opener must be deferred until the first non-recall
    /// event rather than pre-seeded ahead of everything.
    #[tokio::test]
    async fn recall_precedes_the_synthesized_execute_boundary() {
        let root = tempfile::tempdir().expect("root");
        let registry = stella_tools::ToolRegistry::new(root.path().to_path_buf());
        let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let forwarder = spawn_forwarder(
            rx,
            None,
            InsightScope {
                provider_id: "anthropic".into(),
                cache_ttl: stella_model::CacheTtl::default(),
                opens_execute_stage: true,
            },
            in_tx,
            "lead".to_string(),
            None,
        );
        // Recall is the first event of the turn, queued after the forwarder
        // was spawned — exactly as `run_lead_turn` does it.
        tx.send(AgentEvent::ContextRecall {
            frames: Vec::new(),
            provider_mix: Vec::new(),
            tokens: 0,
            usage: None,
            latency_ms: 0,
            used_ann_index: None,
        })
        .unwrap();
        tx.send(AgentEvent::Text { text: "hi".into() }).unwrap();
        tx.send(AgentEvent::TurnComplete {
            model: "m".into(),
            cost_usd: 0.0,
        })
        .unwrap();

        let events = lane_events(&registry, tx, forwarder, &mut in_rx).await;
        let recall = events
            .iter()
            .position(|event| matches!(event, AgentEvent::ContextRecall { .. }))
            .expect("the recall event survives");
        let stage_execute = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    AgentEvent::Stage { name, .. }
                        if name.kind() == Some(stella_protocol::StageKind::Execute)
                )
            })
            .expect("the opening boundary is emitted");
        assert!(
            recall < stage_execute,
            "recall rides AHEAD of the opening stage boundary: {events:?}"
        );
    }

    /// A staged lane must get none of it: the pipeline emits every boundary
    /// itself, starting at `Triage`, and an `Execute` ahead of that is a
    /// backwards transition `replay::validate_stage_ordering` rejects.
    #[tokio::test]
    async fn a_staged_lane_synthesizes_no_boundary_of_its_own() {
        let root = tempfile::tempdir().expect("root");
        let registry = stella_tools::ToolRegistry::new(root.path().to_path_buf());
        let (in_tx, mut in_rx) = tokio::sync::mpsc::unbounded_channel();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let forwarder = spawn_forwarder(
            rx,
            None,
            InsightScope {
                provider_id: "anthropic".into(),
                cache_ttl: stella_model::CacheTtl::default(),
                opens_execute_stage: false,
            },
            in_tx,
            "lead".to_string(),
            None,
        );
        tx.send(AgentEvent::TurnComplete {
            model: "m".into(),
            cost_usd: 0.0,
        })
        .unwrap();

        let events = lane_events(&registry, tx, forwarder, &mut in_rx).await;
        assert!(
            stages(&events).is_empty(),
            "a staged lane's boundaries are the pipeline's alone: {events:?}"
        );
    }
}
