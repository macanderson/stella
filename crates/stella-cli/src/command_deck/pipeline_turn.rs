// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The deck's staged-pipeline lead turn (`/pipeline` ON), split out of
//! [`super`] — a god file, closed to growth — following the `settle` /
//! `task_tap` / `session_clear` pattern.
//!
//! A coherent cut rather than a line-count one: this is the *whole* of one
//! execution mode. `command_deck.rs` keeps the driver loop and its input
//! routing; this file assembles the tool stack, the role wiring and the
//! workspace ports for a pipeline turn and runs it. Its step-loop sibling
//! (`run_lead_turn`) stays beside the driver because the driver's soft-stop
//! and settle rules are written against it directly.

use super::*;

/// One staged-pipeline turn for the lead agent (`/pipeline` ON): the deck
/// analogue of the `stella run` pipeline path — same tool stack, persistence,
/// and event forwarding as [`run_lead_turn`], with `Pipeline::run` (triage →
/// recall → plan → scope → execute → witness → verify → verdict → revise) in
/// place of the raw `Engine::run_turn`.
///
/// Deck-mode seams, all named:
/// - **Scope review is interactive** ([`DeckApprovalGate`]). A plan over the
///   thresholds raises the deck's approval card and the turn parks until the
///   user answers at the card — the deck runs `headless: false` because it *can*
///   ask. It fails closed only if the deck itself goes away mid-gate.
/// - **The session's system prompt stays.** It was assembled once at deck
///   startup (byte-stable for the cache prefix, L-E8); toggling `/pipeline`
///   must not rewrite history. The pipeline's stage prompts (witness, verifier,
///   planner) are its own regardless of the worker's system prompt.
/// - **Recall is the pipeline's port** (the workspace memory) — the driver
///   skips its own `inject_recall_block` for pipeline turns.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_lead_pipeline_turn(
    provider: &dyn Provider,
    base_tools: &dyn ToolExecutor,
    custom_tools: &[CustomTool],
    registry: &std::sync::Arc<ToolRegistry>,
    memory: Option<&SessionMemory>,
    prompt: &str,
    messages: &mut Vec<CompletionMessage>,
    budget: &mut BudgetGuard,
    cfg: &Config,
    active_rules: &crate::rules::ResolvedRules,
    execution: Option<(Arc<Store>, i64)>,
    in_tx: &UnboundedSender<Inbound>,
    scope_gate: &DeckApprovalGate,
    sup_tx: &UnboundedSender<SupervisorMsg>,
    claim_holder: &str,
    steering: &Arc<subsession::SteeringTap>,
    pause: &lead_control::LeadPause,
    mcp: Option<Arc<stella_mcp::McpToolSet>>,
) -> Result<(), crate::failure::CliFailure> {
    budget.begin_turn();

    let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
    let forwarder = spawn_forwarder(
        rx,
        execution.clone(),
        crate::cache_insight::InsightScope::from_config(cfg),
        in_tx.clone(),
        LEAD.to_string(),
        Some(registry.task_board()),
    );

    // Claim-on-first-write over the shared tree — same wiring as
    // `run_lead_turn` (see the comment there).
    let claims = ClaimTap::new(
        base_tools,
        execution.as_ref().map(|(store, _)| store.clone()),
        claim_holder,
    );
    // As in `run_lead_turn`: registry-born events (task board, sub-agent
    // lifecycle) ride this turn's channel. Candidate registries are
    // deliberately left unattached — a candidate's edits live in a shadow
    // worktree and are only the user's once adopted.
    registry.attach_events(stella_core::EventSender::new(tx.clone()));
    // As in `run_lead_turn`: one stop — or one pause — lands on the stage,
    // its tool calls, and their children alike.
    let _controls = registry.attach_turn_controls(lead_control::turn_controls(steering, pause));

    let result = {
        // Customs, the operator's switches, and the authorization gate
        // (#3283) — the deck's pipeline turn acts as the human at the
        // keyboard.
        let permitted = agent::tool_stack::session_stack(
            &claims,
            custom_tools.to_vec(),
            cfg,
            stella_core::ports::Principal::User,
            registry.hook_bus(),
        );
        let tapped = TaskTap::new(&permitted, tx.clone(), registry, Some(sup_tx.clone()));

        let model_ref = ModelRef::new(cfg.provider.id, cfg.model_id.clone());
        // Role wiring from `agent_engine_config`: worker/triage/verifier pins +
        // their adapters + per-role request overrides. Notices land in the
        // transcript — stderr is invisible under the alternate screen.
        let configured = crate::config::discover_configured_providers();
        let wiring = agent::resolve_engine_wiring(cfg, &model_ref, &configured);
        for notice in &wiring.notices {
            let _ = tx.send(AgentEvent::Text {
                text: format!("! {notice}\n"),
            });
        }
        let resolver =
            agent::RoleProviderResolver::new(provider, model_ref.clone(), &wiring.extra_providers);
        let breaker = CircuitBreaker::new(Box::new(SystemClock::new()));
        let router = Router::new(wiring.pins.clone(), wiring.profiles.clone(), breaker);

        let ws_ports = agent::workspace_ports(
            cfg.workspace_root.clone(),
            cfg,
            active_rules.clone(),
            mcp,
            agent::SessionPlane::new(stella_core::EventSender::new(tx.clone()))
                .with_sub_agents(registry.sub_agent_dispatcher()),
        )?;
        let no_recall = NoContextRecall;
        let recall: &dyn ContextRecallPort = match memory {
            Some(m) => m,
            None => &no_recall,
        };
        let hook_runner = ShellHookRunner;
        let ports = PipelinePorts {
            router: &router,
            providers: &resolver,
            tools: &tapped,
            recall,
            repo: &ws_ports.repo_structure,
            repo_status: &ws_ports.repo_status,
            diagnostics: &ws_ports.diagnostic_runner,
            tests: &ws_ports.test_runner,
            lint: None,
            mutation: Some(&ws_ports.mutation_probe),
            coverage: Some(&ws_ports.coverage_probe),
            approvals: scope_gate,
            sleeper: &TokioSleeper,
            hooks: cfg
                .hooks
                .as_ref()
                .map(|h| (h, &hook_runner as &dyn stella_core::hooks::HookRunner)),
            candidate_workspaces: Some(&ws_ports.candidate_workspaces),
            mcp_prefetch: ws_ports
                .mcp_prefetch
                .as_ref()
                .map(|p| p as &dyn McpPrefetchPort),
            // The deck's per-turn tap: `>` steers the execute engine mid-turn
            // (the same tap the step-loop lead turn uses).
            steering: Some(steering.as_ref()),
        };
        let config = PipelineConfig {
            engine: agent::pipeline_engine_config_for(cfg, &wiring.worker_model),
            role_overrides: wiring.role_overrides.clone(),
            // Not headless: the deck has a real approval surface, so scope
            // review asks instead of returning `ScopeReviewRequiredHeadless`.
            // The bypass flag is deliberately left off — it only exists to let
            // a run with *nobody to ask* proceed, and this run has someone.
            headless: false,
            headless_bypass_scope_review: false,
            ..agent::apply_pipeline_tuning(cfg, PipelineConfig::default())
        };
        // #1214's seam, driven from the deck — see `resume_frame::pipeline`.
        let pipeline = crate::resume_frame::pipeline(&cfg.durability, ports, tx.clone(), config)
            .with_turn_gate(pause.turn_gate());
        pipeline.run(prompt, messages, budget).await
    };
    // The pipeline no longer emits the run's ending (#3398), so this turn —
    // which owns the stream — does, on the same terms as `run_lead_turn`.
    if let Ok(outcome) = &result {
        agent::persistence::emit_run_complete_on_raw(&tx, &cfg.model_id, outcome.total_cost_usd);
    }
    // Same settle window as `run_lead_turn` — see `SteeringTap::mark_settling`.
    steering.mark_settling();
    let persistence_complete = close_turn_stream(registry, tx, forwarder).await;
    claims.release_all();

    if let Some((store, id)) = &execution {
        let (outcome_label, cost) = agent::pipeline_execution_closeout(&result);
        if !agent::record_execution_end(
            store,
            *id,
            registry,
            outcome_label,
            cost,
            persistence_complete,
        ) {
            forwarder::warn_audit_record_incomplete(in_tx, LEAD, persistence_complete);
            // Same re-assert as run_lead_turn: the retryable warning above
            // folds the lead back to Running, so restate the terminal state.
            let _ = in_tx.send(Inbound::Status {
                agent: LEAD.to_string(),
                status: match &result {
                    Ok(outcome) if matches!(outcome.status, PipelineStatus::Completed) => {
                        AgentStatus::Done
                    }
                    _ => AgentStatus::Failed,
                },
            });
        }
    }

    match result {
        // The shared projection keeps the abort's typed kind (#1862) and the
        // exact messages the string arms carried before.
        Ok(outcome) => agent::outcome::interactive_status_result(&outcome.status),
        Err(e) => Err(crate::failure::CliFailure::error(e.to_string())),
    }
}
