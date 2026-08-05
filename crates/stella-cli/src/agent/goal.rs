//! Goal-driven turns: judged rounds until a verifier confirms the goal is met.
//!
//! `run_goal_cmd` drives either the staged pipeline (default) or the raw
//! `Engine::run_goal` step-loop. The goal verifier is independent of the worker
//! model and answers "does the whole effort meet the goal?" — distinct from
//! the pipeline's per-change verification verifier.

use super::*;

/// Run a one-shot prompt through the raw step-loop (Engine::run_turn).
/// Selected via `--no-pipeline`.
pub(crate) async fn run_raw_one_shot(
    cfg: &Config,
    prompt: &str,
    budget_limit: Option<f64>,
    format: OutputFormat,
) -> Result<(), String> {
    let provider = build_provider(cfg)?;
    let registry_options = registry_options(cfg);
    // Concrete `Arc<ToolRegistry>` (not `Arc<dyn ToolExecutor>`) so the
    // files-touched ledger is reachable after the turn — the trait object
    // hides it. It still coerces to `&dyn ToolExecutor` for the engine.
    let registry: std::sync::Arc<ToolRegistry> = std::sync::Arc::new(
        new_tool_registry(cfg.workspace_root.clone(), registry_options.clone()).await,
    );
    populate_schema_index(&registry, &cfg.workspace_root)?;

    crate::subagent::install_for_session(cfg, &registry)?;
    let active_rules =
        crate::rules::enforce_workspace_rules(&registry, &cfg.workspace_root, &cfg.authority);
    // Auto-build + live-refresh the code graph in the background so a
    // multi-step one-shot turn can reach for `graph_query` once the index is
    // ready. Status goes to stderr — stdout may be machine-readable JSON.
    let (_session_graph, _graph_build) = spawn_session_graph(
        &cfg.workspace_root,
        registry.clone(),
        Box::new(|line| eprintln!("  {line}")),
        Box::new(|| {}),
    );
    let process_free = crate::enterprise_telemetry::process_free_authority_active();
    let mcp = if process_free {
        None
    } else {
        connect_mcp(
            cfg,
            registry.clone(),
            Some(registry.mcp_usage_ledger()),
            format == OutputFormat::Text,
        )
        .await?
    };
    let base_tools: &dyn ToolExecutor = match &mcp {
        Some(set) => set.as_ref(),
        None => &*registry,
    };
    let custom_tools = if process_free {
        Vec::new()
    } else {
        discover_custom_tools(cfg, format == OutputFormat::Text).await
    };
    let mut budget = build_budget_guard(budget_limit);
    let store = open_store(&cfg.workspace_root);
    let calibration = seed_calibration(&store, cfg);

    if format == OutputFormat::Text {
        tui::section_header("Stella");
        println!("  {}\n", prompt.dimmed());
    }

    let mut messages = vec![
        CompletionMessage::system(
            with_session_hook_context(
                build_system_prompt(cfg, &cfg.workspace_root, &active_rules),
                cfg,
            )
            .await,
        ),
        crate::attachments::user_message_in(prompt, &cfg.workspace_root),
    ];

    // The self-improvement loop (memory.rs): recall relevant memories +
    // skills into a volatile block after the stable system prefix (L-E8)…
    let mut memory = SessionMemory::open_for_session(
        &cfg.workspace_root,
        format == OutputFormat::Text,
        &cfg.authority,
        &active_rules,
    );
    if let Some(m) = &mut memory {
        // Conformance-gated external CGP providers join before the first
        // recall, or are refused with a reason (#453).
        m.register_external_providers(|message| eprintln!("  {} {message}", "!".yellow()))
            .await;
        // The A/B recall control, armed before the one recall this process
        // makes (#1221). The counter is durable, so "every rate-th turn"
        // means something on a surface that is one turn per process.
        m.arm_recall_control();
    }
    // Phase 2 (#713): carried forward rather than emitted here — the turn's
    // event channel is created inside `run_turn`, after the messages recall
    // contributes to have been assembled.
    let mut recall_event = None;
    if let Some(m) = &memory {
        let recalled = m.recall_block_reported(prompt).await;
        recall_event = recalled.telemetry_event();
        inject_recall_block(&mut messages, recalled.text);
    }

    let started_unix = crate::memory::unix_now_secs();
    // Machine-wide presence: findable in the deck's SESSIONS overlay and
    // replayable from its journal after this process exits.
    let mut presence = SessionPresence::announce(cfg, prompt, &registry);
    let outcome = run_turn(
        &*provider,
        base_tools,
        &custom_tools,
        &registry,
        &mut messages,
        &mut budget,
        &calibration,
        cfg,
        format,
        &store,
        "run",
        prompt,
        Some(presence.id()),
        &crate::discovery::new_activation(),
        recall_event,
        memory.as_mut(),
    )
    .await;
    // Episodic memory first (works even for a failed turn — failures are
    // exactly the episodes worth recalling)…
    if let Some(m) = &memory {
        let files = registry.files_touched();
        if turn_warrants_reflection(&messages) || !files.is_empty() {
            let episode_outcome = if outcome.is_ok() {
                EpisodeOutcome::Success
            } else {
                EpisodeOutcome::Failure
            };
            m.record_episode(prompt, episode_outcome, &files, started_unix, None)
                .await;
        }
    }
    // …and reflect on the completed turn, recording domain-tagged lessons
    // (recurring ones auto-promote to SKILL.md files). Best-effort: never
    // fails or slows the turn that just ran. Reflect on success AND failure —
    // a failed one-shot is a prime learning signal (root-cause prompt via
    // `succeeded=false`). Gated on `turn_warrants_reflection` so a tool-free
    // turn (nothing to mine, failure almost certainly external) never spends a
    // model call. The report is surfaced so a model-call error is never silent.
    // The raw one-shot closes its execution inside `run_turn`; unlike the
    // staged pipeline it has no post-turn event phase before that terminal
    // barrier. Keep machine streams strict by not dispatching an unframed
    // reflection call after `Complete` (text retains the best-effort loop).
    // `one_shot_reflection_enabled` additionally honors the benchmark
    // adapter's `STELLA_DISABLE_REFLECTION` opt-out.
    if format == OutputFormat::Text
        && one_shot_reflection_enabled(format)
        && turn_warrants_reflection(&messages)
        && let Some(m) = &mut memory
    {
        let mut report = m
            .reflect_and_record(
                &*provider,
                &cfg.model_id,
                &messages,
                format != OutputFormat::Text,
                outcome.is_ok(),
                crate::agent::remaining_budget(&budget),
            )
            .await;
        settle_reflection_budget(&mut report, &mut budget);
        surface_reflection(&report, format);
    }
    if let Some(set) = &mcp {
        set.close_all().await;
    }
    // Terminal registry status + the headless → `/inbox` flow (failure
    // always notifies; success only past the looked-away threshold —
    // `SessionPresence::one_shot_notification`, shared with the pipeline path).
    let run_secs =
        u64::try_from(crate::memory::unix_now_secs().saturating_sub(started_unix)).unwrap_or(0);
    let notify = presence.one_shot_notification(outcome.is_ok(), run_secs, prompt);
    presence.finish(outcome.is_ok(), notify);
    outcome
}

/// Run a one-shot goal loop (non-interactive): work in judged rounds until
/// a verifier model assesses the goal as met (`stella goal "…"`, and `stella
/// monitor` composed on top of it). The verifier is routed by role: when a
/// second provider family is configured (BYOK), `run_goal_turn` builds a
/// role `Router` and resolves `Role::Verifier` to a DIFFERENT family than the
/// worker for bias-resistant assessment; with a
/// single family it stays the worker provider, identical to before. The
/// worker turns get the full tool stack (MCP + custom + interactive +
/// skills), same as `run_one_shot`.
///
/// `use_pipeline` (the default) runs each working round through the staged
/// pipeline (triage → recall → plan → witness → execute → verify → verifier);
/// `false` falls back to the raw `Engine::run_goal` step-loop.
pub async fn run_goal_cmd(
    cfg: &Config,
    goal: &str,
    budget_limit: Option<f64>,
    use_pipeline: bool,
) -> Result<(), String> {
    crate::enterprise_telemetry::authorize_execution_surface(
        crate::enterprise_telemetry::ExecutionSurface::Goal,
    )?;
    let provider = build_provider(cfg)?;
    let registry_options = registry_options(cfg);
    let registry: std::sync::Arc<ToolRegistry> = std::sync::Arc::new(
        new_tool_registry(cfg.workspace_root.clone(), registry_options.clone()).await,
    );
    populate_schema_index(&registry, &cfg.workspace_root)?;

    crate::subagent::install_for_session(cfg, &registry)?;
    let active_rules =
        crate::rules::enforce_workspace_rules(&registry, &cfg.workspace_root, &cfg.authority);
    // Auto-build + live-refresh the code-graph index in the background so
    // `graph_query` is available for the goal loop without a manual `stella
    // init`. Non-blocking; status to stderr. Kept alive until the goal returns.
    let (_session_graph, _graph_build) = spawn_session_graph(
        &cfg.workspace_root,
        registry.clone(),
        Box::new(|line| eprintln!("  {line}")),
        Box::new(|| {}),
    );
    let mcp = connect_mcp(
        cfg,
        registry.clone(),
        Some(registry.mcp_usage_ledger()),
        true,
    )
    .await?;
    let base_tools: &dyn ToolExecutor = match &mcp {
        Some(set) => set.as_ref(),
        None => &*registry,
    };
    let custom_tools = discover_custom_tools(cfg, true).await;
    let mut budget = build_budget_guard(budget_limit);
    let store = open_store(&cfg.workspace_root);
    let calibration = seed_calibration(&store, cfg);

    tui::section_header("Stella — goal mode");
    println!("  {}\n", goal.dimmed());

    // Persona matches the driver: pipeline rounds get the pipeline worker
    // persona (methodology ladder + `agents.worker.prompt` override) instead
    // of the generic REPL prompt only `stella run` used to carry.
    let mut messages = vec![CompletionMessage::system(
        with_session_hook_context(
            if use_pipeline {
                build_pipeline_system_prompt(cfg, &cfg.workspace_root, &active_rules)
            } else {
                build_system_prompt(cfg, &cfg.workspace_root, &active_rules)
            },
            cfg,
        )
        .await,
    )];
    let mut memory =
        SessionMemory::open_for_session(&cfg.workspace_root, true, &cfg.authority, &active_rules);
    // Phase 2 (#713): carried to the turn runner, which owns the event channel.
    let mut recall_event = None;
    if let Some(m) = &mut memory {
        // One arm for the whole goal run (#1221): the judged rounds below are
        // stages of one turn — they share this run's episode, so they must
        // share its arm, and re-arming per round would count one prompt as N
        // turns of the schedule.
        m.arm_recall_control();
        if use_pipeline {
            // Pipeline rounds recall frames through their own port (wired to
            // this same store in `run_goal_pipeline_turn`) and emit their own
            // `ContextRecall`; the CLI block carries only the sections the
            // port has no channel for. Injecting the full block here would
            // recall twice and bill the frames twice — the duplication the
            // one-shot path had.
            inject_recall_block(&mut messages, m.pipeline_recall_block(goal).await);
        } else {
            let recalled = m.recall_block_reported(goal).await;
            recall_event = recalled.telemetry_event();
            inject_recall_block(&mut messages, recalled.text);
        }
    }

    let started_unix = crate::memory::unix_now_secs();
    // Machine-wide presence: a goal run is exactly the long-lived headless
    // session the SESSIONS overlay + replay exist for.
    let mut presence = SessionPresence::announce(cfg, goal, &registry);
    let outcome = if use_pipeline {
        run_goal_pipeline_turn(
            &*provider,
            base_tools,
            &custom_tools,
            &registry,
            &mut messages,
            &mut budget,
            &calibration,
            cfg,
            &store,
            goal,
            Some(presence.id()),
            registry_options.clone(),
            active_rules.clone(),
            mcp.clone(),
            recall_event,
            memory.as_mut(),
        )
        .await
    } else {
        run_goal_turn(
            &*provider,
            base_tools,
            &custom_tools,
            &registry,
            &mut messages,
            &mut budget,
            &calibration,
            cfg,
            &store,
            goal,
            Some(presence.id()),
            recall_event,
            memory.as_mut(),
        )
        .await
    };
    if let Some(m) = &memory {
        let files = registry.files_touched();
        if turn_warrants_reflection(&messages) || !files.is_empty() {
            let episode_outcome = if outcome.is_ok() {
                EpisodeOutcome::Success
            } else {
                EpisodeOutcome::Failure
            };
            m.record_episode(goal, episode_outcome, &files, started_unix, None)
                .await;
        }
    }
    // Reflect on success AND failure, matching the one-shot path above — a
    // failed goal run is a prime learning signal (root-cause prompt via
    // `succeeded=false`). Only a user-chosen soft stop is excluded
    // (issue #373, item 7).
    if crate::memory::should_reflect_on(&outcome)
        && turn_warrants_reflection(&messages)
        && let Some(m) = &mut memory
    {
        let mut report = m
            .reflect_and_record(
                &*provider,
                &cfg.model_id,
                &messages,
                false,
                outcome.is_ok(),
                crate::agent::remaining_budget(&budget),
            )
            .await;
        settle_reflection_budget(&mut report, &mut budget);
        surface_reflection(&report, OutputFormat::Text);
    }
    if let Some(set) = &mcp {
        set.close_all().await;
    }
    // Goal runs are long by construction — always land the inbox
    // notification (Enter on it replays this session's journal).
    let goal_secs = crate::memory::unix_now_secs().saturating_sub(started_unix);
    let notify = if outcome.is_ok() {
        format!("{}: goal met ({goal_secs}s)", presence.name())
    } else {
        format!("{}: goal run FAILED", presence.name())
    };
    presence.finish(
        outcome.is_ok(),
        Some((notify, crate::command_deck::prompt_line(goal, 160))),
    );
    outcome
}

/// Run one goal loop through `stella_core::Engine::run_goal`: working turns
/// interleaved with verifier assessments until the verifier passes it (or a
/// backstop — rounds, budget, abort — ends it with a named reason). The
/// worker gets the full tool stack (MCP + custom + interactive + skills) and
/// the verifier a read-only view of that same stack.
///
/// The verifier is routed by role (`resolve_cross_family_verifier`): when a second
/// provider family is configured and the `Router` selects it, the verifier runs
/// on a DIFFERENT model family than the worker (bias-resistant assessment)
/// and a one-line notice is printed. With a single
/// configured family — or on any discovery/build failure — the verifier is the
/// worker provider itself, identical to before: no second provider is built
/// and no extra cost is incurred. Text-mode rendering only — goal and
/// monitor never take `--output-format`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_goal_turn(
    provider: &dyn Provider,
    base_tools: &dyn ToolExecutor,
    custom_tools: &[CustomTool],
    registry: &ToolRegistry,
    messages: &mut Vec<CompletionMessage>,
    budget: &mut BudgetGuard,
    calibration: &CalibrationMap,
    cfg: &Config,
    store: &Option<Arc<Store>>,
    goal: &str,
    session: Option<&str>,
    // Phase 2 (#713): this turn's `ContextRecall`, carried from the caller
    // because recall necessarily precedes the channel it would be emitted on.
    recall_event: Option<AgentEvent>,
    // The caller's session memory, so this round's execution id is stamped
    // before the turn runs — reflection stores the self-review 1:1 with an
    // execution, and an unstamped round files an id-less row.
    session_memory: Option<&mut crate::memory::SessionMemory>,
) -> Result<(), String> {
    let turn_start = Instant::now();
    let execution = begin_execution(store, "goal", goal, cfg, session);
    if let (Some((_, id)), Some(m)) = (&execution, session_memory) {
        m.set_execution_id(*id);
    }
    let files_before = registry.files_touched().len();

    // Route the VERIFIER role. `Some` only when a distinct-family verifier was
    // selected AND built; the boxed provider must outlive the `run_goal`
    // call below, so it is bound here. `None` → the verifier is the worker
    // provider (single-family/failure fallback — the v1 behavior).
    let configured = crate::config::discover_configured_providers();
    let routed_verifier =
        resolve_cross_family_verifier(cfg.provider.id, &cfg.model_id, &configured);
    if let Some((_, verifier_id)) = &routed_verifier {
        println!(
            "  {} cross-family verifier: {} worker · {} verifier — independent, bias-resistant \
             assessment\n",
            "◆".bright_cyan(),
            cfg.provider.id.bright_magenta(),
            verifier_id.bright_green(),
        );
    }
    let verifier: &dyn Provider = match &routed_verifier {
        Some((boxed, _)) => &**boxed,
        None => provider,
    };

    let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
    let renderer = spawn_renderer(
        rx,
        OutputFormat::Text,
        execution.clone(),
        cfg.provider.id.to_string(),
        false,
    );
    // First event of the turn: what recall put in front of the model.
    if let Some(event) = recall_event {
        let _ = tx.send(event);
    }

    let outcome = {
        let customs = CustomToolSet::new(
            base_tools,
            custom_tools.to_vec(),
            cfg.workspace_root.clone(),
        );
        let interactive = InteractiveToolSet::new(&customs, tx.clone(), default_ask_io(true))
            .with_skill_registry(SkillRegistry::from_env(cfg.workspace_root.clone()));
        let permitted = PolicyToolSet::new(&interactive, session_tool_policy(cfg));
        let tools = crate::discovery::DiscoveryToolSet::new(&permitted, cfg.workspace_root.clone())
            .with_project_prompts_allowed(cfg.authority.project_prompts_allowed);
        let hook_runner = ShellHookRunner;
        let mut engine =
            Engine::with_sleeper(provider, &tools, engine_config_for(cfg), &TokioSleeper)
                .with_calibration(calibration);
        if let Some(hooks) = &cfg.hooks {
            engine = engine.with_hooks(hooks, &hook_runner);
        }
        engine
            .run_goal(
                verifier,
                goal,
                messages,
                budget,
                &tx,
                &GoalConfig::default(),
            )
            .await
    };
    drop(tx);
    let persistence_complete = renderer.await.unwrap_or_default().persistence_complete;

    let files = registry.files_touched();
    if let Some((store, id)) = &execution {
        let (outcome_label, cost) = match &outcome {
            GoalOutcome::Met { cost_usd, .. } => ("goal_met", *cost_usd),
            GoalOutcome::Unmet { cost_usd, .. } => ("goal_unmet", *cost_usd),
        };
        if !record_execution_end(
            store,
            *id,
            registry,
            files_before,
            outcome_label,
            cost,
            persistence_complete,
        ) {
            warn_store_write_failed(
                "the audit record (files touched / memory citations / outcome)",
            );
        }
    }
    tui::files_touched_panel(&files);

    match outcome {
        GoalOutcome::Met {
            rounds,
            verdict,
            cost_usd,
        } => {
            println!(
                "\n  {} goal met after {rounds} round{}: {}",
                "✓".green().bold(),
                if rounds == 1 { "" } else { "s" },
                verdict
            );
            tui::cost_summary(
                cost_usd,
                &format!("{}/{}", cfg.provider.id, cfg.model_id),
                turn_start.elapsed(),
            );
            println!();
            Ok(())
        }
        GoalOutcome::Unmet {
            rounds,
            reason,
            cost_usd,
        } => {
            tui::cost_summary(
                cost_usd,
                &format!("{}/{}", cfg.provider.id, cfg.model_id),
                turn_start.elapsed(),
            );
            Err(format!("goal not met after {rounds} round(s): {reason}"))
        }
    }
}

/// One staged-pipeline goal turn: keep running the pipeline (triage → recall →
/// plan → witness → execute → verify → verifier) until an independent goal verifier
/// assesses the goal as met, or a backstop ends the loop. This is the pipeline
/// analogue of [`run_goal_turn`] — same goal-loop structure, same judgment,
/// but each working round goes through the staged pipeline instead of the raw
/// `Engine::run_turn`.
///
/// The goal-loop verifier is distinct from the pipeline's verify verifier: the verify
/// verifier (inside [`Pipeline::run`]) answers "did this change pass its tests?",
/// while the goal verifier here answers "does the whole effort meet the goal?".
/// Both are independent of the worker model.
#[allow(clippy::too_many_arguments)]
async fn run_goal_pipeline_turn(
    provider: &dyn Provider,
    base_tools: &dyn ToolExecutor,
    custom_tools: &[CustomTool],
    registry: &ToolRegistry,
    messages: &mut Vec<CompletionMessage>,
    budget: &mut BudgetGuard,
    calibration: &CalibrationMap,
    cfg: &Config,
    store: &Option<Arc<Store>>,
    goal: &str,
    session: Option<&str>,
    registry_options: stella_tools::RegistryOptions,
    active_rules: crate::rules::ResolvedRules,
    mcp: Option<Arc<stella_mcp::McpToolSet>>,
    // Phase 2 (#713): this turn's `ContextRecall`, carried from the caller.
    recall_event: Option<AgentEvent>,
    // Same contract as `run_goal_turn`: stamp the execution id into the
    // caller's memory before the turn runs, so reflection can name its row.
    session_memory: Option<&mut crate::memory::SessionMemory>,
) -> Result<(), String> {
    let turn_start = Instant::now();
    let execution = begin_execution(store, "goal", goal, cfg, session);
    // Rebound mutable and NOT consumed by the id stamp, so the same memory
    // can double as the pipeline's recall port below.
    let mut session_memory = session_memory;
    if let (Some((_, id)), Some(m)) = (&execution, session_memory.as_deref_mut()) {
        m.set_execution_id(*id);
    }
    let files_before = registry.files_touched().len();
    let model_ref = ModelRef::new(cfg.provider.id, cfg.model_id.clone());

    // Role wiring from `agent_engine_config` — the pinned/auto verifier (when
    // configured) also serves as the goal loop's round verifier below.
    let configured = crate::config::discover_configured_providers();
    let wiring = resolve_engine_wiring(cfg, &model_ref, &configured);
    for notice in &wiring.notices {
        eprintln!("  ! {notice}");
    }

    // Route the goal-loop verifier. The pipeline's verify verifier is the same role;
    // both want independence from the worker. An engine-config verifier pin
    // (explicit or auto_mode) wins; otherwise the discovery-based
    // cross-family routing applies exactly as before. (`configured` from the
    // wiring block above is reused — same disk state, nothing mutates it
    // in between; issue #373, item 4.)
    let engine_verifier: Option<(&dyn Provider, String)> = wiring
        .pins
        .get(Role::Verifier)
        .and_then(|pinned| {
            wiring
                .extra_providers
                .iter()
                .find(|(model_ref, _)| model_ref == pinned)
        })
        .map(|(model_ref, provider)| (&**provider, model_ref.provider.clone()));
    let routed_verifier = if engine_verifier.is_none() {
        resolve_cross_family_verifier(cfg.provider.id, &cfg.model_id, &configured)
    } else {
        None
    };
    let (verifier, verifier_id): (&dyn Provider, Option<String>) =
        match (&engine_verifier, &routed_verifier) {
            (Some((provider, id)), _) => (*provider, Some(id.clone())),
            (None, Some((boxed, id))) => (&**boxed, Some(id.clone())),
            (None, None) => (provider, None),
        };
    if let Some(verifier_id) = &verifier_id {
        println!(
            "  {} cross-family verifier: {} worker · {} verifier — independent, bias-resistant \
             assessment\n",
            "◆".bright_cyan(),
            cfg.provider.id.bright_magenta(),
            verifier_id.bright_green(),
        );
    }

    let goal_config = GoalConfig::default();
    let resolver = RoleProviderResolver::new(provider, model_ref.clone(), &wiring.extra_providers);

    let (tx, rx) = mpsc::unbounded_channel::<AgentEvent>();
    let renderer = spawn_renderer(
        rx,
        OutputFormat::Text,
        execution.clone(),
        cfg.provider.id.to_string(),
        false,
    );
    // First event of the turn: what recall put in front of the model.
    if let Some(event) = recall_event {
        let _ = tx.send(event);
    }

    // Run the loop; the result is folded into `goal_result` so there is exactly
    // one teardown path (drop tx → await renderer → record execution → return).
    let goal_result = {
        let customs = CustomToolSet::new(
            base_tools,
            custom_tools.to_vec(),
            cfg.workspace_root.clone(),
        );
        let interactive = InteractiveToolSet::new(&customs, tx.clone(), default_ask_io(true))
            .with_skill_registry(SkillRegistry::from_env(cfg.workspace_root.clone()));
        let permitted = PolicyToolSet::new(&interactive, session_tool_policy(cfg));
        let tools = crate::discovery::DiscoveryToolSet::new(&permitted, cfg.workspace_root.clone())
            .with_project_prompts_allowed(cfg.authority.project_prompts_allowed);

        let breaker = CircuitBreaker::new(Box::new(SystemClock::new()));
        let router = Router::new(wiring.pins.clone(), wiring.profiles.clone(), breaker);

        let ws_ports = workspace_ports(
            cfg.workspace_root.clone(),
            cfg,
            registry_options,
            active_rules.clone(),
            mcp,
            Some(stella_core::EventSender::new(tx.clone())),
        )?;
        let no_recall = NoContextRecall;
        // The workspace memory doubles as the recall port (as on the one-shot
        // path), so every round's planner and witness author see the same
        // durable lessons the worker's block carries. `NoContextRecall` here
        // starved them while the caller's full block fed only the worker —
        // and once the caller switched to the frames-free pipeline block,
        // nothing at all would have carried frames.
        let recall: &dyn ContextRecallPort = match session_memory.as_deref() {
            Some(m) => m,
            None => &no_recall,
        };
        let hook_runner = ShellHookRunner;

        // The goal verifier's provider/tool/config bundle. NOT reused across
        // rounds in any load-bearing way: `Engine::assess` constructs a
        // fresh inner engine (and re-wraps the tools in `ReadOnlyTools`) on
        // every call — this outer engine only carries the verifier provider,
        // the read-only tool view, the verifier tuning, and the session
        // calibration (keyed per model, so a cross-family verifier learns its
        // own drift) into each assessment.
        let read_only = stella_core::ports::ReadOnlyTools::new(&tools);
        let verifier_engine = Engine::with_sleeper(
            verifier,
            &read_only,
            verifier_engine_config_for(cfg),
            &TokioSleeper,
        )
        .with_calibration(calibration);

        let mut total_cost_usd = 0.0f64;
        let mut result: Option<Result<(), String>> = None;
        let mut goal_met = false;

        for round in 1..=goal_config.max_rounds {
            budget.begin_turn();
            // Each round is its own receipt turn: worker/pipeline calls take
            // the even slot, the round's verifier the odd slot beside it (the
            // `+ 1` lives in `Engine::assess`). All rounds share one
            // execution_id, and receipts key on `(turn_instance, step,
            // call_seq)` with steps restarting per turn — the slot math is
            // `stella_core::goal`'s, shared with the raw and served loops.
            let round_turn = stella_core::goal::goal_round_turn_offset(round);
            let pipeline_config = PipelineConfig {
                engine: EngineConfig {
                    turn_instance: round_turn,
                    ..pipeline_engine_config_for(cfg, &wiring.worker_model)
                },
                role_overrides: wiring.role_overrides.clone(),
                headless: true,
                headless_bypass_scope_review: HEADLESS_SCOPE_REVIEW_BYPASS,
                ..apply_pipeline_tuning(cfg, PipelineConfig::default())
            };
            let ports = PipelinePorts {
                router: &router,
                providers: &resolver,
                tools: &tools,
                recall,
                repo: &ws_ports.repo_structure,
                repo_status: &ws_ports.repo_status,
                touches: &crate::agent::RegistryTouches(registry),
                diagnostics: &ws_ports.diagnostic_runner,
                tests: &ws_ports.test_runner,
                lint: Some(&ws_ports.lint_probe),
                mutation: Some(&ws_ports.mutation_probe),
                coverage: Some(&ws_ports.coverage_probe),
                approvals: &HEADLESS_APPROVAL_GATE,
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
                // Goal pipeline rounds run without an interactive steer tap.
                steering: None,
            };
            let pipeline = Pipeline::new(ports, tx.clone(), pipeline_config);
            // The same kickoff wording as the raw goal loop and the served
            // one — `stella_core::goal` owns the bytes for all three.
            let round_goal = stella_core::goal::goal_kickoff_text(goal);
            match pipeline.run(&round_goal, messages, budget).await {
                Ok(outcome) => {
                    total_cost_usd += outcome.total_cost_usd;
                    match outcome.status {
                        PipelineStatus::Completed => {}
                        PipelineStatus::VerificationFailed { verdict } => {
                            result = Some(Err(format!(
                                "goal not met: verification failed: {}",
                                verdict.summary
                            )));
                            break;
                        }
                        PipelineStatus::Aborted { reason } => {
                            result = Some(Err(format!(
                                "goal not met: working round aborted: {reason}"
                            )));
                            break;
                        }
                    }
                }
                Err(e) => {
                    result = Some(Err(e.to_string()));
                    break;
                }
            }

            // Goal assessment (same verifier + read-only tools as the raw goal loop).
            let _ = tx.send(AgentEvent::Stage {
                name: stella_protocol::StageKind::Verifier,
            });
            let (verdict, verifier_cost) = match verifier_engine
                .with_turn_instance(round_turn)
                .assess(verifier, goal, messages, budget, &tx, &goal_config)
                .await
            {
                Ok(pair) => pair,
                Err(reason) => {
                    result = Some(Err(format!("goal not met: verifier unavailable: {reason}")));
                    break;
                }
            };
            total_cost_usd += verifier_cost;
            let _ = tx.send(AgentEvent::GoalVerdict {
                round,
                met: verdict.met,
                reasoning: verdict.reasoning.clone(),
                cost_usd: verifier_cost,
            });

            if verdict.met {
                tui::files_touched_panel(&registry.files_touched());
                println!(
                    "\n  {} goal met after {round} round{}: {}",
                    "✓".green().bold(),
                    if round == 1 { "" } else { "s" },
                    verdict.reasoning
                );
                tui::cost_summary(
                    total_cost_usd,
                    &format!("{}/{}", cfg.provider.id, cfg.model_id),
                    turn_start.elapsed(),
                );
                println!();
                goal_met = true;
                break;
            }

            messages.push(CompletionMessage::user(
                stella_core::goal::verifier_feedback_text(goal, &verdict),
            ));
        }

        // An explicit break result (abort/error/verifier-down) stands. If the goal
        // was met, success. Otherwise the round cap was reached unmet.
        match (result, goal_met) {
            (Some(r), _) => r,
            (None, true) => Ok(()),
            (None, false) => {
                tui::cost_summary(
                    total_cost_usd,
                    &format!("{}/{}", cfg.provider.id, cfg.model_id),
                    turn_start.elapsed(),
                );
                Err(format!(
                    "goal not met after {} round(s): round cap reached without a passing verdict",
                    goal_config.max_rounds
                ))
            }
        }
    };

    drop(tx);
    let persistence_complete = renderer.await.unwrap_or_default().persistence_complete;
    // The shared guard is the settled ledger, including a verifier turn that
    // aborted after spending and therefore returned no `verifier_cost` value.
    let total_cost_usd = budget.session_spent_usd();
    let files = registry.files_touched();
    if let Some((store, id)) = &execution {
        let outcome_label = match &goal_result {
            Ok(()) => "goal_met",
            Err(_) => "goal_unmet",
        };
        if !record_execution_end(
            store,
            *id,
            registry,
            files_before,
            outcome_label,
            total_cost_usd,
            persistence_complete,
        ) {
            warn_store_write_failed(
                "the audit record (files touched / memory citations / outcome)",
            );
        }
    }
    tui::files_touched_panel(&files);
    goal_result
}
