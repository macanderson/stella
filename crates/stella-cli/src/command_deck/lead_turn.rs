//! One engine turn for the lead agent (`run_lead_turn`) — the deck-mode
//! analogue of `agent::run_turn`: same engine, same tool stack, same
//! persistence, with the stdout renderer replaced by [`spawn_forwarder`].
//!
//! Split out of `command_deck.rs` (closed to growth) the way `skills.rs` and
//! `authoring.rs` were — the driver loop's single call site passes everything
//! it needs as explicit arguments, so the function carries no state of its
//! own beyond what it is handed.

use std::sync::Arc;

use stella_core::ports::{Principal, ToolExecutor};
use stella_core::{BudgetGuard, CalibrationMap, Engine, TurnOutcome};
use stella_model::provider::Provider;
use stella_protocol::{AgentEvent, CompletionMessage};
use stella_store::Store;
use stella_tools::ToolRegistry;
use stella_tools::custom::CustomTool;
use stella_tools::hook_runner::HostHookRunner;
use stella_tui::{AgentStatus, Inbound};
use tokio::sync::mpsc::{self, UnboundedSender};

use super::task_tap::{PlanSetup, TaskTap};
use super::{LEAD, agent, close_turn_stream, forwarder, lead_control, spawn_forwarder};
use crate::claims::ClaimTap;
use crate::config::Config;
use crate::memory::{SessionMemory, TurnFriction};
use crate::runtime::TokioSleeper;
use crate::subsession::{self, SupervisorMsg};

/// One engine turn for the lead agent: the deck-mode analogue of
/// `agent::run_turn` — same engine, same tool stack, same persistence —
/// with the stdout renderer replaced by [`spawn_forwarder`].
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_lead_turn(
    provider: &dyn Provider,
    base_tools: &dyn ToolExecutor,
    custom_tools: &[CustomTool],
    registry: &ToolRegistry,
    messages: &mut Vec<CompletionMessage>,
    budget: &mut BudgetGuard,
    calibration: &CalibrationMap,
    cfg: &Config,
    execution: Option<(Arc<Store>, i64)>,
    in_tx: &UnboundedSender<Inbound>,
    sup_tx: &UnboundedSender<SupervisorMsg>,
    claim_holder: &str,
    steering: &Arc<subsession::SteeringTap>,
    // Owned by the driver loop, so its input arms can flip it mid-turn (#1219).
    pause: &lead_control::LeadPause,
    // Phase 2 (#713): this turn's `ContextRecall` and the opening block's
    // re-query seed (#4498), carried in because recall precedes this channel.
    recall: crate::memory::OpeningRecall,
    session_memory: Option<&SessionMemory>, // #3243 Phase 3: behind the re-query
    friction: &mut TurnFriction,            // #3962: filled from the lane's own stream
) -> Result<(), crate::failure::CliFailure> {
    budget.begin_turn();
    let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
    let requery = crate::memory::requery_for_turn(
        session_memory,
        messages,
        tx.clone().into(),
        recall.produced,
    );
    let forwarder = spawn_forwarder(
        rx,
        execution.clone(),
        crate::cache_insight::InsightScope::from_config(cfg),
        in_tx.clone(),
        LEAD.to_string(),
        Some(registry.task_board()),
    );
    // First event of the turn: what recall put in front of the model.
    if let Some(event) = recall.event {
        let _ = tx.send(event);
    }

    // Claim-on-first-write over the shared tree (crate::claims): wraps the
    // base executor, so a refused write surfaces as the tool's own error.
    // Released after the turn settles, cancel included.
    let claims = ClaimTap::new(
        base_tools,
        execution.as_ref().map(|(store, _)| store.clone()),
        claim_holder,
    );
    // Registry-born events (task board, sub-agent lifecycle) and this turn's
    // per-call work-tree measurement both ride this turn's channel.
    crate::turn_files::open_turn_streams_raw(registry, cfg, &tx, execution.as_ref());
    // ...and this turn's stop AND pause reach the sub-agents it dispatches
    // (`lead_control::turn_controls`). The guard takes them down on return.
    let _controls = registry.attach_turn_controls(lead_control::turn_controls(steering, pause));

    // Same structural drop-order rule as `agent::run_turn`: every tx clone
    // lives in this scope so dropping `tx` after it closes the channel.
    let outcome = {
        // Customs, the operator's switches, and the authorization gate
        // (#3283) — the deck's lead turn acts as the human at the keyboard.
        let permitted = agent::tool_stack::session_stack(
            &claims,
            custom_tools.to_vec(),
            cfg,
            Principal::User,
            registry.hook_bus(),
        );
        // Both read before the engine borrows `messages` mutably: the plan
        // gate's setup (`task_tap::plan_gate`, #4594/#4611) and this turn's
        // id, which every lane it spawns records (#4628).
        let plan = PlanSetup::for_turn(messages, cfg);
        let turn = execution.as_ref().map(|(_, id)| *id);
        let tap = TaskTap::new(&permitted, tx.clone(), registry, Some(sup_tx), plan, turn);
        let hook_runner = HostHookRunner;
        let mut engine =
            Engine::with_sleeper(provider, &tap, agent::engine_config_for(cfg), &TokioSleeper)
                .with_calibration(calibration)
                .with_steering(steering.as_ref())
                .with_gate(pause.turn_gate());
        if let Some(hooks) = &cfg.hooks {
            engine = engine.with_hooks(hooks, &hook_runner);
        }
        if let Some(requery) = &requery {
            engine = engine.with_requery(requery); // #3243 Phase 3
        }
        engine.run_turn(messages, budget, &tx).await
    };
    crate::turn_files::close_turn_boundary_raw(cfg, registry, &tx, execution.as_ref(), &outcome);
    // The model is done and the deck already painted "done". Everything below is
    // bookkeeping that can take real time (the forwarder persists every event of the
    // turn) while the driver's `select!` still reads input — so latch the flag that
    // tells its prompt arm what arrives is the next turn, not a sidecar request.
    steering.mark_settling();
    // The re-query adapter holds an `EventSender` clone of this turn's channel
    // (#3366 telemetry), so it is one of the sender clones `close_turn_stream`
    // requires gone; otherwise the forwarder's `recv()` stays pending forever
    // and the turn future wedges after the deck painted the turn done (#2290).
    drop(requery);
    let ended = close_turn_stream(registry, tx, forwarder).await;
    let persistence_complete = ended.persistence_complete;
    *friction = ended.friction; // this turn's reflection evidence (#3962)
    claims.release_all();

    if let Some((store, id)) = &execution {
        let (outcome_label, cost) = match &outcome {
            TurnOutcome::Completed { cost_usd, .. } => ("completed", *cost_usd),
            TurnOutcome::Aborted { cost_usd, .. } => ("aborted", *cost_usd),
        };
        if !agent::record_execution_end(
            store,
            *id,
            registry,
            outcome_label,
            cost,
            persistence_complete,
        ) {
            forwarder::warn_audit_record_incomplete(in_tx, LEAD, persistence_complete);
            // That warning lands AFTER the turn's Complete event, and the
            // deck's status fold maps a retryable Error back to Running — so
            // without this re-assert a finished turn would show as running
            // forever. Restate the turn's terminal status explicitly.
            let _ = in_tx.send(Inbound::Status {
                agent: LEAD.to_string(),
                status: match &outcome {
                    TurnOutcome::Completed { .. } => AgentStatus::Done,
                    TurnOutcome::Aborted { .. } => AgentStatus::Failed,
                },
            });
        }
    }

    // The abort's typed kind rides through (#1862): the session-exit writer
    // reads it off the same projection as every other terminal writer.
    agent::outcome::turn_outcome_result(&outcome)
}
