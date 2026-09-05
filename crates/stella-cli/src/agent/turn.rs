// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `run_turn` — the shared turn entry point every raw door calls
//! (`run_interactive`, `/goal`, `run_raw_one_shot`, and the deck's own
//! worker lane). Split out of `agent.rs` (the `driver/settlement.rs`
//! pattern, AGENTS.md § God files) — the parent sat within a handful of
//! lines of the 1500-line ratchet. `use super::*` carries over everything
//! `agent.rs` already had in scope, including the sibling `engine`/`output`/
//! `persistence`/`summary`/`turn_close` modules this file's body names; the
//! parent re-exports [`run_turn`] and its private helper
//! [`mirror_task_board`] so every caller keeps its path.

use super::*;

/// Run one full turn through `stella_core::Engine`, rendering its
/// `AgentEvent` stream live via a spawned draining task. Ordinary runs
/// enqueue to an unbounded channel; benchmark stream-json runs synchronously
/// append+flush each event before enqueueing it, so paid-call evidence
/// survives a paused/cancelled renderer. The drain task ([`spawn_renderer`])
/// persists every event and each `StepUsage` to the workspace store when one
/// is open. `registry` is the concrete tool registry (its ledgers close the
/// execution's audit record); `base_tools` is the same registry as the
/// engine's executor, possibly MCP-wrapped.
#[expect(
    clippy::too_many_arguments,
    reason = "the engine's own turn entry point, and the one the other three follow; `messages`, \
              `budget`, `session_memory` and the friction out-parameter are four `&mut` borrows \
              of separate caller locals held for the turn, so the bundle is a struct of disjoint \
              mutable borrows and is worth doing across all four entry points at once rather \
              than here alone"
)]
pub(crate) async fn run_turn(
    provider: &dyn Provider,
    base_tools: &dyn ToolExecutor,
    custom_tools: &[CustomTool],
    registry: &ToolRegistry,
    messages: &mut Vec<CompletionMessage>,
    budget: &mut BudgetGuard,
    calibration: &CalibrationMap,
    // Session-scoped breaker feedback: the engine reports outcomes.
    router: &Router,
    cfg: &Config,
    format: OutputFormat,
    store: &Option<Arc<Store>>,
    door: persistence::TurnDoor<'_>,
    prompt: &str,
    session: Option<&str>,
    // This turn's `ContextRecall`, if recall ran, plus the
    // opening block's produced handles. Recall happens before the
    // turn's event channel exists — it has to, because its frames go into the
    // messages the turn is built from — so the caller hands the residue
    // forward rather than emitting into a stream that is not there yet.
    // Passed rather than re-derived: re-running recall to report it would
    // double the retrieval cost of every interactive turn.
    recall: crate::memory::OpeningRecall,
    // The caller's session memory, borrowed for the duration of the turn so
    // the execution seam can stamp this execution's id and record its
    // skill-version usage before the turn runs — the caller reflects with the
    // same memory afterwards, and a reflection that cannot name its execution
    // files an id-less row (NULL `self_rating`).
    mut session_memory: Option<&mut SessionMemory>,
    // This turn's boundary controls — the pause gate and steering tap, when
    // its caller has one. Every raw door but the deck publishes
    // `TurnControls::none()` (a non-interactive run has nobody to pause for and,
    // until agent whistle, nowhere to steer from); `crate::agent::goal`'s
    // one-shot doors now publish a `whistle::tap::HeadlessSteerTap` here
    // instead, exactly the seam this field was already documented for on
    // `wrapper_plugin::RawTurnDriver`.
    controls: stella_core::ports::TurnControls,
    // Where this turn's friction ledger lands, for a caller that reflects
    // afterwards. An out-parameter rather than a second return value
    // because the wrapped door reaches this function through the wrapper
    // socket's `TurnDriver`, whose `DrivenTurn` shape is a wire contract —
    // widening the return type to serve reflection would push a reflection
    // concern into the socket. `None` for a caller that does not reflect.
    friction: Option<&mut TurnFriction>,
) -> Result<TurnOutcome, CliFailure> {
    budget.begin_turn();
    let turn_start = Instant::now();
    let execution = begin_execution(store, door.kind, prompt, cfg, session, door.variant);
    stamp_and_record_skill_usage(
        &execution,
        session_memory.as_deref_mut(),
        prompt,
        &cfg.workspace_root,
        &recall.invoked_skills(),
    );
    let (raw_tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
    let (tx, durable_pre_persisted) = output::raw_event_sender_for_run(raw_tx, format, &door);
    // The proactive re-query: the engine consults this at
    // every step boundary; the adapter's hysteresis makes an undrifted turn
    // free. Seeded from `messages` so the turn-opening block is never
    // re-injected, from that block's own handles so the first answer does not
    // repeat its frames, and given `tx` so its own recall is metered.
    // This turn's directive-carrying skills — invoked or
    // auto-selected: each span is live for the whole turn — `active_skill_slugs`
    // reports them, and every declared `allowed-tools` grant narrows every
    // dispatch to operator ∧ grant, intersected across spans. The guards
    // drop with this function, lifting the narrowing structurally. With no
    // scope the plane is inert and the scoped views below are pure
    // pass-throughs, so every turn takes one path. Read off the recall
    // before its seed and events are handed on below.
    let skill_plane = stella_tools::skill_plane::SkillInvocationPlane::new();
    let _skill_spans = recall.mount_skill_spans(&skill_plane);
    let skill_effort = recall.skill_effort();
    let requery = crate::memory::requery_for_turn(
        session_memory.as_deref(),
        messages,
        tx.clone(),
        recall.produced,
    );
    persistence::attach_run_streams(registry, cfg, &tx, execution.as_ref());
    let renderer = spawn_renderer(
        rx,
        format,
        execution.clone(),
        cfg.provider.id.to_string(),
        durable_pre_persisted,
        Some(prompt.to_string()),
    );
    // Recall's frames, then this run's own opening stage boundary — see
    // `output::open_raw_turn` for the ordering and for why it lives there.
    output::open_raw_turn(&tx, recall.events, cfg.authority.withheld.as_ref());

    // Mid-turn fallback: on an exhausted retry ladder the engine
    // re-resolves the worker role through this session router.
    let fallback = engine::SessionFallback::new(router);
    // The scoped tool set must drop its tx clone before awaiting the renderer.
    let outcome = if crate::enterprise_telemetry::process_free_authority_active() {
        // Even when process-free authority strips the MCP/custom/interactive
        // layers, the `"tools"` policy (operator/managed-org tool switches)
        // and the authorization gate must still hold above the session tool
        // stack — mirroring every other driver path, so disabled tools cannot
        // be invoked here either.
        let bus = registry.hook_bus();
        let permitted = tool_stack::policy_stack(registry, cfg, Principal::User, bus);
        // The invocation plane sits above the operator's policy layer — the
        // position `skill_grant`'s module docs specify — so the grant can
        // select within the operator surface but never re-enable below it.
        let permitted =
            stella_tools::skill_plane::SkillScopedTools::new(&permitted, skill_plane.clone());
        let mut config = engine::engine_config_for_kind(cfg, door.kind);
        if let Some(effort) = skill_effort {
            // The invoked skill's `effort:` override, for this turn.
            config.effort = Some(effort);
        }
        // TODO(#6109): still the builder path, so this turn reports no lane.
        // `stella run` is a door, not one of the seven lanes `BuiltinLane`
        // names, and `Lead` documents itself as the deck's turn. Until it is
        // settled whether that definition widens, whether an eighth case
        // arrives, or whether these doors stay unattributed, guessing here
        // would make the lane say something its own definition denies.
        let mut engine = Engine::with_sleeper(provider, &permitted, config, &TokioSleeper)
            .with_calibration(calibration)
            .with_provider_outcomes(router)
            .with_fallback_resolver(&fallback);
        if let Some(requery) = &requery {
            engine = engine.with_requery(requery);
        }
        if let Some(gate) = controls.gate.as_deref() {
            engine = engine.with_gate(gate);
        }
        if let Some(steering) = controls.steering.as_deref() {
            engine = engine.with_steering(steering);
        }
        engine.run_turn_with_sender(messages, budget, &tx).await
    } else {
        // Customs, the operator's switches, and the authorization gate,
        // outermost-last — one assembly for every driver.
        let bus = registry.hook_bus();
        let tools =
            tool_stack::session_stack(base_tools, custom_tools.to_vec(), cfg, Principal::User, bus);
        // Above the whole session chain, for the same reason as the
        // process-free arm: the grant narrows the assembled surface —
        // customs and MCP included — and can never widen it.
        let tools = stella_tools::skill_plane::SkillScopedTools::new(&tools, skill_plane.clone());
        let hook_runner = HostHookRunner;
        // A PreToolUse hook's `require_approval` parks on the broker
        // flow. Snapshotted here, after assembly attached any
        // responder and bus, so the route asks the surface this run has.
        let hook_approvals = stella_tools::hook_bridge::BrokerApprovalRoute::for_registry(registry);
        let mut config = engine::engine_config_for_kind(cfg, door.kind);
        if let Some(effort) = skill_effort {
            // The invoked skill's `effort:` override, for this turn.
            config.effort = Some(effort);
        }
        // Unattributed for the reason the process-free arm above gives: this
        // is the same door, waiting on the same decision.
        let mut engine = Engine::with_sleeper(provider, &tools, config, &TokioSleeper)
            .with_calibration(calibration)
            .with_provider_outcomes(router)
            .with_fallback_resolver(&fallback);
        if let Some(hooks) = &cfg.hooks {
            engine = engine
                .with_hooks(hooks, &hook_runner)
                .with_hook_approval_route(&hook_approvals);
        }
        if let Some(requery) = &requery {
            engine = engine.with_requery(requery);
        }
        if let Some(gate) = controls.gate.as_deref() {
            engine = engine.with_gate(gate);
        }
        if let Some(steering) = controls.steering.as_deref() {
            engine = engine.with_steering(steering);
        }
        engine.run_turn_with_sender(messages, budget, &tx).await
    };
    // This path owns its run — one raw engine turn, no pipeline above it — so
    // it owes both of the boundary's debts: what the turn changed in the shared
    // tree, then the run's terminator. Emitted *before* the
    // close below, because these are the turn's own events and a consumer
    // folding the stream must see them inside the turn they describe. See
    // `crate::turn_files::close_turn_boundary` for the ordering, for why the
    // tree is measured rather than inferred from tool inputs, and for the deck
    // defect that made the two one call.
    crate::turn_files::close_turn_boundary(cfg, registry, &tx, execution.as_ref(), &outcome);
    mirror_task_board(execution.as_ref(), session, registry);
    // The re-query adapter holds an `EventSender` clone of this run's channel
    // for telemetry, so it must be released here too — otherwise it keeps
    // the channel open and the renderer's `recv()` loop never ends.
    drop(requery);
    // Releasing every sender — the registry's clones included — closes the
    // channel, ending the renderer's `recv()` loop; awaiting it ensures every
    // already-queued event has actually printed before this function returns.
    let rendered = close_event_stream(registry, tx, renderer).await;
    let persistence_complete = rendered.persistence_complete;
    let collected = rendered.events;
    // The turn's friction, folded from the journal the renderer just finished
    // draining. Folded here rather than live because this is the first
    // point at which the whole stream is both complete and still owned — and
    // the fold reads durations off the events themselves, so the result is the
    // same one a live tap would have built. Borrowed before `collected` is
    // moved into the JSON envelope below.
    if let Some(slot) = friction {
        *slot = TurnFriction::from_events(&collected);
    }

    let (outcome_label, cost) = match &outcome {
        TurnOutcome::Completed { cost_usd, .. } => ("completed", *cost_usd),
        TurnOutcome::Aborted { cost_usd, .. } => ("aborted", *cost_usd),
    };
    turn_close::close_turn(
        cfg,
        store,
        &execution,
        registry,
        session,
        turn_close::TurnOutcomeRecord {
            label: outcome_label,
            cost_usd: cost,
            persistence_complete,
        },
    );

    if format == OutputFormat::Json {
        // One final JSON object: the outcome summary plus the full event log
        // (the same objects stream-json would have emitted line by line).
        summary::print_json_summary(cfg, &outcome, collected);
    }

    if let TurnOutcome::Completed { cost_usd, .. } = &outcome
        && format == OutputFormat::Text
    {
        plain::cost_summary(
            *cost_usd,
            &format!("{}/{}", cfg.provider.id, cfg.model_id),
            turn_start.elapsed(),
        );
        println!();
    }
    match outcome {
        TurnOutcome::Aborted { reason, kind, .. } => Err(CliFailure::from_abort(reason, kind)),
        completed => Ok(completed),
    }
}

/// Mirror this turn's final task board into the store's `tasks` table — the
/// same write the deck's lead turn makes at its own turn end
/// (`command_deck`) and `/clear` makes at its own boundary
/// (`command_deck::session_clear`). `TaskUpdate` rides the turn stream
/// and lands in `events` for every door; this raw one-shot / non-interactive door
/// (`run_turn`) was the one left with no write site of its own, so a
/// non-interactive `stella run` that used the task board was replayable
/// from the journal but invisible to a `tasks` query.
///
/// Silent and best-effort like every sibling call site: no open store, or a
/// board nobody touched, leaves nothing to mirror.
///
/// `pub(crate)`, widened from the private visibility it had inside
/// `agent.rs`: `agent::tests` (a sibling of this module now, not a
/// descendant) still calls it directly, and the parent module re-exports it
/// so that access keeps working unqualified via `super::*` — see
/// `agent.rs`'s `pub(crate) use turn::{...}`.
pub(crate) fn mirror_task_board(
    execution: Option<&(Arc<Store>, i64)>,
    session: Option<&str>,
    registry: &ToolRegistry,
) {
    let Some((store, id)) = execution else {
        return;
    };
    let items: Vec<TaskItem> = registry
        .task_board()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .items()
        .to_vec();
    if items.is_empty() {
        return;
    }
    let _ = store.record_task_board(*id, session, &items, crate::command_deck::now_ms());
}
