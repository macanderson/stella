//! Goal-driven turns: judged rounds until a verifier confirms the goal is met.
//!
//! `run_goal_cmd` drives either the raw `Engine::run_goal` step-loop (the
//! default since #3381) or the staged pipeline, opted into with `--pipeline
//! classic`. The goal verifier is independent of the worker model and
//! answers "does the whole effort meet the goal?" — distinct from the
//! pipeline's per-change verification verifier.

use super::*;

/// The session task board, tool by tool — everything the catalog files under
/// the `task` group *except* the delegation tool that shares the group name.
///
/// See [`bare_loop_config`]'s `# Blast radius` for why this is a name list and
/// not the one-line group key, and for the catalog assertion that keeps it
/// complete.
const WITHHELD_BOARD_TOOLS: [&str; 6] = [
    "task_create",
    "task_list",
    "task_start",
    "task_complete",
    "task_cancel",
    "task_assign",
];

/// The caller's config with the task board withheld — what the bare step loop
/// actually runs on.
///
/// The board is what makes a long autonomous run legible and resumable — and a
/// one-shot turn has nothing to resume, so here it is pure overhead charged at
/// the most expensive rate there is: a model round trip per card. Measured over
/// nine solved Terminal-Bench trials, 26% of every tool call this loop made was
/// board bookkeeping and 14% of its steps did nothing else — steps that read
/// nothing, changed no file, and still paid for a full request under a 900s
/// ceiling.
///
/// Narrowed rather than switched off at construction:
/// [`narrow_with`](stella_tools::policy::ToolPolicy::narrow_with) can only ever
/// remove capability, so an operator who has already disabled something keeps
/// it disabled, and this cannot grant anything back. It is the same seam
/// `tools.bash: "off"` travels through, which is why the board disappears from
/// MCP-wrapped and custom tool stacks too rather than only from the built-in
/// registry.
///
/// # Blast radius
///
/// The switch names the six board tools one at a time, and deliberately not
/// the `task` **group** key #2410 shipped: the catalog files sub-agent
/// delegation — the tool literally named `task` — in that same group, so the
/// group key took delegation with the board and the bare loop could not spawn
/// a child at all.
///
/// That was collateral, not a decision, and #2580 settled it that way: the
/// measurement above, #2410's prose, its reviewer-facing behaviour-change note
/// and all three of its witnesses say *the board* throughout. Delegation is a
/// different capability with a different cost model — it does work and spends
/// money, where a board call reads nothing and changes nothing — so none of
/// the evidence for withholding the board reaches it. Restoring it is also
/// what makes [`run_raw_one_shot`]'s
/// [`install_for_session`](crate::subagent::install_for_session) pay for
/// itself, rather than installing a dispatcher behind a tool the policy always
/// stripped.
///
/// Naming the six costs the group's future-proofing — a seventh board tool
/// would be offered here until someone added it to [`WITHHELD_BOARD_TOOLS`] —
/// so `the_withheld_list_is_the_whole_board_minus_delegation` below reads the
/// catalog and fails when the group grows.
///
/// # Why this is a function
///
/// Both the bare loop and its witnesses must run *this* expression. When the
/// narrowing was four inline lines and the tests re-implemented it in a local
/// helper, all three passed with the production narrowing deleted: they
/// witnessed `narrow_with`, a `stella-tools` property that was already true
/// (#2511).
fn bare_loop_config(cfg: &Config) -> Config {
    let mut bare = cfg.clone();
    bare.tool_policy
        .narrow_with(&stella_tools::policy::ToolPolicy::from_switches(
            WITHHELD_BOARD_TOOLS.map(|name| (name.to_string(), false)),
        ));
    bare
}

/// Run a one-shot prompt through the raw step-loop (Engine::run_turn).
/// Selected via `--no-pipeline`, or via `--pipeline <variant>`, which runs the
/// same step-loop with an installed wrapper plugin around it (#3494).
///
/// `pipeline` is the caller's already-resolved choice, read for its variant id
/// here rather than at the call site: `PipelineChoice::Raw` is `--no-pipeline`,
/// where nothing is dispatched, no execution row records a variant, and the
/// behaviour is exactly what it was before the socket had a caller.
///
/// The parameter is `full_cfg`, not `cfg`, deliberately: the loop below runs on
/// the narrowed [`bare_loop_config`] and nothing else, so dropping that call
/// leaves `cfg` unbound and fails to compile. A shadowing `let cfg = …` would
/// have let the same deletion fall silently back to the un-narrowed argument —
/// the half of #2511 no unit test can reach, since this function builds a
/// provider, connects MCP and opens the store.
///
/// `test_command` is the user's `--test-command`. On the raw path it arms no
/// ladder of this crate's own — there is none — but it is what lets a wrapper
/// plugin's `[oracle]` observe a fail→pass flip at all: it crosses the host's
/// strict parser and rides the candidate grant as a [`stella_plugin::TestPlan`]
/// (#3553). Dropped before, which is why `pre_turn_signals` published
/// `test_command: false` unconditionally.
pub(crate) async fn run_raw_one_shot(
    full_cfg: &Config,
    prompt: &str,
    budget_limit: Option<f64>,
    format: OutputFormat,
    pipeline: crate::wrapper_plugin::PipelineChoice<'_>,
    test_command: Option<&str>,
) -> Result<(), crate::failure::CliFailure> {
    let bare = bare_loop_config(full_cfg);
    let cfg = &bare;
    // Resolved before the provider is built and before a single paid call: a
    // `--pipeline` that names nothing installed must fail as a typo, not after
    // the run it was meant to shape. The grant is minted in the same breath and
    // for the same reason — a `--test-command` the host's parser refuses must
    // stop the run here, not after it is paid for.
    let bound = match pipeline.plugin() {
        Some(variant) => Some(
            crate::wrapper_plugin::resolve(&cfg.workspace_root, variant, &mut |line| {
                eprintln!("  ! {line}");
            })
            .map_err(crate::failure::CliFailure::from)?,
        ),
        None => None,
    };
    // Pinned *before* the turn runs, which is the whole content of the tamper
    // claim: an identity snapshotted afterwards vouches for nothing.
    let candidate = match &bound {
        Some(_) => Some(
            crate::wrapper_candidate::grant_shared_tree(&cfg.workspace_root, test_command)
                .map_err(crate::failure::CliFailure::from)?,
        ),
        None => None,
    };
    let provider = build_provider(cfg)?;
    // Concrete `Arc<ToolRegistry>` (not `Arc<dyn ToolExecutor>`) so the
    // registry's ledgers are reachable after the turn — the trait object
    // hides them. It still coerces to `&dyn ToolExecutor` for the engine.
    let registry: std::sync::Arc<ToolRegistry> =
        std::sync::Arc::new(crate::write_dirs::registry_for(cfg));

    crate::subagent::install_for_session(cfg, &registry)?;
    // The one derivation of "a human is here to answer" — the #2676 approval
    // responder and the rules-enforcement prompt both read this value.
    let ask = human_is_present(format == OutputFormat::Text);
    let active_rules =
        crate::rules::enforce_workspace_rules(&registry, &cfg.workspace_root, &cfg.authority, ask);
    // Auto-build + live-refresh the code graph in the background so
    // `stella search` and the deck's Graph tab have a fresh index. Status
    // goes to stderr — stdout may be machine-readable JSON.
    let (_session_graph, _graph_build) = spawn_session_graph(
        &cfg.workspace_root,
        Box::new(crate::agent::init::stderr_narrator()),
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
    // Breaker feedback for the bare one-shot turn (#2673) — one turn per
    // process, so this is pure ledger today; #2679 reads it mid-turn.
    let router = session_router(cfg, &ModelRef::new(cfg.provider.id, cfg.model_id.clone()));

    if format == OutputFormat::Text {
        plain::section_header("Stella");
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
    let mut presence = SessionPresence::announce(cfg, prompt);
    // The wrapper socket's first driver (#3494). With no `--pipeline` the arm
    // below is the one that has always run, unchanged: one turn, no dispatch,
    // and a NULL `pipeline_variant` because no wrapper ran over it.
    let outcome = match (&bound, &candidate) {
        // Both arms of one `Option` are set together above, so the mismatched
        // pairs are unreachable; matching the tuple keeps that visible instead
        // of unwrapping a grant on the strength of having checked the wrapper.
        (Some(bound), Some(candidate)) => {
            crate::wrapper_plugin::run_wrapped(
                bound,
                prompt,
                crate::wrapper_plugin::pre_turn_signals(
                    test_command.is_some(),
                    budget_limit.is_some(),
                ),
                Some(candidate.grant.clone()),
                crate::wrapper_plugin::RawTurnDriver {
                    provider: &*provider,
                    base_tools,
                    custom_tools: &custom_tools,
                    registry: &registry,
                    messages: &mut messages,
                    budget: &mut budget,
                    calibration: &calibration,
                    router: &router,
                    cfg,
                    format,
                    store: &store,
                    prompt,
                    session: presence.id(),
                    variant: bound.variant(),
                    recall_event,
                    memory: memory.as_mut(),
                    watch: &candidate.watch,
                    results: Vec::new(),
                },
            )
            .await
        }
        _ => run_turn(
            &*provider,
            base_tools,
            &custom_tools,
            &registry,
            &mut messages,
            &mut budget,
            &calibration,
            &router,
            cfg,
            format,
            &store,
            persistence::TurnDoor::new("run"),
            prompt,
            Some(presence.id()),
            recall_event,
            memory.as_mut(),
        )
        .await
        .map(|_| ()),
    };
    // Episodic memory first (works even for a failed turn — failures are
    // exactly the episodes worth recalling)…
    if let Some(m) = &memory
        && turn_warrants_reflection(&messages)
    {
        let episode_outcome = if outcome.is_ok() {
            EpisodeOutcome::Success
        } else {
            EpisodeOutcome::Failure
        };
        m.record_episode(prompt, episode_outcome, &[], started_unix, None)
            .await;
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
        // No friction ledger on this path yet: the raw step loop's events ride a
        // bare `UnboundedSender`, so there is nothing to wrap (#2483). The
        // transcript here does carry every tool call and its typed result, which
        // is what the digest selects on.
        let mut report = crate::memory::reflect_routed(
            m,
            cfg,
            &*provider,
            crate::memory::TurnEvidence::from_transcript(&messages, outcome.is_ok()),
            format != OutputFormat::Text,
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
    // The failure itself, not a bool: an unsupervised deliberate stop must
    // reach the registry as `Stopped`, never age into it as a crash (#1826).
    let status = crate::daemon::outcome_status(outcome.as_ref().map(|_| ()));
    let notify = presence.one_shot_notification(status, run_secs, prompt);
    presence.finish(status, notify);
    outcome
}

/// Run a one-shot goal loop (non-interactive): work in judged rounds until
/// a verifier model assesses the goal as met (`stella goal "…"`, and `stella
/// monitor` composed on top of it). The verifier is routed by role: when a
/// second provider family is configured (BYOK), `run_goal_turn` builds a
/// role `Router` and resolves `Role::Verifier` to a DIFFERENT family than the
/// worker for bias-resistant assessment; with a
/// single family it stays the worker provider, identical to before. The
/// worker turns get the full tool stack (built-ins + MCP + custom), same as
/// `run_one_shot`.
///
/// `pipeline` selects the driver for each working round (#3381): `Classic`
/// (`--pipeline classic`) runs the staged pipeline (triage → recall → plan →
/// execute → witness → verify → verdict); `Raw` — the default since #3381,
/// with or without `--no-pipeline` — falls back to the raw `Engine::run_goal`
/// step-loop. A named plugin `Variant` is refused before this is ever called
/// (`wrapper_plugin::reject_plugin_variant_for_door`, main.rs's `Goal` arm) —
/// this door only ever sees `Classic` or `Raw`.
pub async fn run_goal_cmd(
    cfg: &Config,
    goal: &str,
    budget_limit: Option<f64>,
    pipeline: crate::wrapper_plugin::PipelineChoice<'_>,
) -> Result<(), crate::failure::CliFailure> {
    // Only `Classic`/`Raw` reach this door (see the doc above); either reads
    // as one bool for the rest of this function's branching, same shape as
    // before #3381.
    let use_pipeline = pipeline.is_classic();
    crate::enterprise_telemetry::authorize_execution_surface(
        crate::enterprise_telemetry::ExecutionSurface::Goal,
    )?;
    let provider = build_provider(cfg)?;
    let registry: std::sync::Arc<ToolRegistry> =
        std::sync::Arc::new(crate::write_dirs::registry_for(cfg));

    crate::subagent::install_for_session(cfg, &registry)?;
    // Goal mode always renders human-readable output, so its half of the
    // fact is fixed at `true`; the stdio handles settle the rest.
    let ask = human_is_present(true);
    let active_rules =
        crate::rules::enforce_workspace_rules(&registry, &cfg.workspace_root, &cfg.authority, ask);
    // Auto-build + live-refresh the code-graph index in the background so
    // `stella search` and the deck's Graph tab stay current without a manual
    // `stella init`. Non-blocking; status to stderr. Kept alive until the
    // goal returns.
    let (_session_graph, _graph_build) = spawn_session_graph(
        &cfg.workspace_root,
        Box::new(crate::agent::init::stderr_narrator()),
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

    plain::section_header("Stella — goal mode");
    println!("  {}\n", goal.dimmed());

    // Persona matches the driver: pipeline rounds get the pipeline worker
    // persona (methodology ladder + `agents.worker.prompt` override) instead
    // of the generic REPL prompt only `stella run` used to carry.
    let mut messages = vec![CompletionMessage::system(
        with_session_hook_context(
            if use_pipeline {
                // Assembled before this run resolves its engine wiring, so the
                // worker may still be re-routed: no model line rather than a
                // possibly-false one (#2721 threads the wiring here).
                build_pipeline_system_prompt(cfg, &cfg.workspace_root, &active_rules, None)
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
    let mut presence = SessionPresence::announce(cfg, goal);
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
    if let Some(m) = &memory
        && turn_warrants_reflection(&messages)
    {
        let episode_outcome = if outcome.is_ok() {
            EpisodeOutcome::Success
        } else {
            EpisodeOutcome::Failure
        };
        m.record_episode(goal, episode_outcome, &[], started_unix, None)
            .await;
    }
    // Reflect on success AND failure, matching the one-shot path above — a
    // failed goal run is a prime learning signal (root-cause prompt via
    // `succeeded=false`). Only a user-chosen soft stop is excluded
    // (issue #373, item 7).
    if crate::memory::should_reflect_on(&outcome)
        && turn_warrants_reflection(&messages)
        && let Some(m) = &mut memory
    {
        // Transcript-only evidence, as on the raw one-shot above (#2483).
        let mut report = crate::memory::reflect_routed(
            m,
            cfg,
            &*provider,
            crate::memory::TurnEvidence::from_transcript(&messages, outcome.is_ok()),
            false,
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
    let notify = match &outcome {
        Ok(()) => format!("{}: goal met ({goal_secs}s)", presence.name()),
        Err(failure) if failure.is_deliberate_stop() => {
            format!("{}: goal run stopped by policy", presence.name())
        }
        Err(_) => format!("{}: goal run FAILED", presence.name()),
    };
    // The typed abort survives to this terminal write (#1862): the same
    // decider as every other registry writer projects a deliberate stop as
    // `Stopped`, a user interrupt as `Cancelled`, and a crash as `Error`
    // (#1653, #1826).
    presence.finish(
        crate::daemon::outcome_status(outcome.as_ref().map(|_| ())),
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
    // The caller's session memory, so the execution seam can stamp this
    // round's execution id and record its skill-version usage before the turn
    // runs — reflection stores the self-review 1:1 with an execution, and an
    // unstamped round files an id-less row.
    session_memory: Option<&mut crate::memory::SessionMemory>,
) -> Result<(), crate::failure::CliFailure> {
    let turn_start = Instant::now();
    // This function is the RAW arm — `run_goal_cmd` calls it only when
    // `pipeline.is_classic()` is false — so `variant: None` is the honest
    // answer every time, not a placeholder (#3381, #3388): nothing wrapped
    // this round.
    let execution = begin_execution(store, "goal", goal, cfg, session, None);
    stamp_and_record_skill_usage(&execution, session_memory, goal, &cfg.workspace_root);

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
        Some(goal.to_string()),
    );
    // First event of the turn: what recall put in front of the model.
    if let Some(event) = recall_event {
        let _ = tx.send(event);
    }

    let outcome = {
        let tools = super::tool_stack::session_stack(
            base_tools,
            custom_tools.to_vec(),
            cfg,
            Principal::User,
            registry.hook_bus(),
        );
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
    // This goal loop owns the stream, so it — not any single round — emits the
    // run's one ending (#3398). A goal that ends unmet still *ended*: the
    // rounds ran, the money was spent, and the outcome is carried by the
    // execution record, not by withholding the terminator.
    let (GoalOutcome::Met { cost_usd, .. } | GoalOutcome::Unmet { cost_usd, .. }) = &outcome;
    persistence::emit_run_complete_on_raw(&tx, &cfg.model_id, *cost_usd);
    drop(tx);
    let persistence_complete = renderer.await.unwrap_or_default().persistence_complete;

    if let Some((store, id)) = &execution {
        let (outcome_label, cost) = match &outcome {
            GoalOutcome::Met { cost_usd, .. } => ("goal_met", *cost_usd),
            GoalOutcome::Unmet { cost_usd, .. } => ("goal_unmet", *cost_usd),
        };
        if !record_execution_end(
            store,
            *id,
            registry,
            outcome_label,
            cost,
            persistence_complete,
        ) {
            warn_store_write_failed("the audit record (agent uses / MCP usage / outcome)");
        }
    }

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
            plain::cost_summary(
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
            kind,
        } => {
            plain::cost_summary(
                cost_usd,
                &format!("{}/{}", cfg.provider.id, cfg.model_id),
                turn_start.elapsed(),
            );
            // The typed kind survives the loop (#1862): a working turn's
            // deliberate stop exits `3` and records `Stopped`.
            Err(outcome::goal_unmet_failure(rounds, &reason, kind))
        }
    }
}

/// One staged-pipeline goal turn: keep running the pipeline (triage → recall →
/// plan → execute → witness → verify → verdict) until an independent goal verifier
/// assesses the goal as met, or a backstop ends the loop. This is the pipeline
/// analogue of [`run_goal_turn`] — same goal-loop structure, same judgment,
/// but each working round goes through the staged pipeline instead of the raw
/// `Engine::run_turn`.
///
/// The goal-loop verifier is distinct from the pipeline's verify verifier: the verify
/// verifier (inside [`stella_pipeline::Pipeline::run`]) answers "did this change pass its tests?",
/// while the goal verifier here answers "does the whole effort meet the goal?".
/// Both are independent of the worker model.
#[allow(clippy::too_many_arguments)]
async fn run_goal_pipeline_turn(
    provider: &dyn Provider,
    base_tools: &dyn ToolExecutor,
    custom_tools: &[CustomTool],
    registry: &std::sync::Arc<ToolRegistry>,
    messages: &mut Vec<CompletionMessage>,
    budget: &mut BudgetGuard,
    calibration: &CalibrationMap,
    cfg: &Config,
    store: &Option<Arc<Store>>,
    goal: &str,
    session: Option<&str>,
    active_rules: crate::rules::ResolvedRules,
    mcp: Option<Arc<stella_mcp::McpToolSet>>,
    // Phase 2 (#713): this turn's `ContextRecall`, carried from the caller.
    recall_event: Option<AgentEvent>,
    // Same contract as `run_goal_turn`: the execution seam stamps the id into
    // the caller's memory and records this round's skill-version usage before
    // the turn runs, so reflection can name its row.
    session_memory: Option<&mut crate::memory::SessionMemory>,
) -> Result<(), crate::failure::CliFailure> {
    let turn_start = Instant::now();
    // This function is the CLASSIC arm — `run_goal_cmd` calls it only when
    // `pipeline.is_classic()` is true — so the row must name the wrapper that
    // actually drove this round rather than leave `pipeline_variant` NULL
    // (#3381, #3388, #3684): NULL on this path used to mean "raw OR classic",
    // which made a per-variant comparison over `goal` rounds silently wrong.
    let execution = begin_execution(
        store,
        "goal",
        goal,
        cfg,
        session,
        Some(persistence::PIPELINE_VARIANT_CLASSIC),
    );
    // Rebound mutable and NOT consumed by the seam, so the same memory can
    // double as the pipeline's recall port below.
    let mut session_memory = session_memory;
    stamp_and_record_skill_usage(
        &execution,
        session_memory.as_deref_mut(),
        goal,
        &cfg.workspace_root,
    );
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
        Some(goal.to_string()),
    );
    // First event of the turn: what recall put in front of the model.
    if let Some(event) = recall_event {
        let _ = tx.send(event);
    }

    // Run the loop; the result is folded into `goal_result` so there is exactly
    // one teardown path (drop tx → await renderer → record execution → return).
    let goal_result = {
        let tools = super::tool_stack::session_stack(
            base_tools,
            custom_tools.to_vec(),
            cfg,
            Principal::User,
            registry.hook_bus(),
        );

        let breaker = CircuitBreaker::new(Box::new(SystemClock::new()));
        let router = Router::new(wiring.pins.clone(), wiring.profiles.clone(), breaker);

        let ws_ports = workspace_ports(
            cfg.workspace_root.clone(),
            cfg,
            active_rules.clone(),
            mcp,
            SessionPlane::new(stella_core::EventSender::new(tx.clone()))
                .with_sub_agents(registry.sub_agent_dispatcher()),
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
        let scoped = VerifierScopedTools::new(&tools);
        let read_only = stella_core::ports::ReadOnlyTools::new(&scoped);
        let verifier_engine = Engine::with_sleeper(
            verifier,
            &read_only,
            verifier_engine_config_for(cfg),
            &TokioSleeper,
        )
        .with_calibration(calibration)
        // The round verifier's outcomes feed the same breaker the next
        // round's pipeline resolves against (#2673).
        .with_provider_outcomes(&router);

        let mut total_cost_usd = 0.0f64;
        let mut result: Option<Result<(), crate::failure::CliFailure>> = None;
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
                diagnostics: &ws_ports.diagnostic_runner,
                tests: &ws_ports.test_runner,
                lint: None,
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
                // No interactive steer tap on goal rounds.
                steering: None,
            };
            let pipeline =
                crate::resume_frame::pipeline(&cfg.durability, ports, tx.clone(), pipeline_config);
            // The same kickoff wording as the raw goal loop and the served
            // one — `stella_core::goal` owns the bytes for all three.
            let round_goal = stella_core::goal::goal_kickoff_text(goal);
            match pipeline.run(&round_goal, messages, budget).await {
                Ok(outcome) => {
                    total_cost_usd += outcome.total_cost_usd;
                    // The fold keeps the abort's typed kind (#1862), so the
                    // terminal registry write can tell a policy stop from a
                    // crash. A completed round is no break at all — the loop
                    // goes on to its verifier assessment below.
                    if let Some(failure) = outcome::goal_round_break(&outcome.status) {
                        result = Some(Err(failure));
                        break;
                    }
                }
                Err(e) => {
                    result = Some(Err(crate::failure::CliFailure::error(e.to_string())));
                    break;
                }
            }

            // Goal assessment (same verifier + read-only tools as the raw goal loop).
            let _ = tx.send(AgentEvent::Stage {
                name: stella_protocol::StageKind::Verdict,
                scope: stella_protocol::StageScope::Run,
            });
            let (verdict, verifier_cost) = match verifier_engine
                .with_turn_instance(round_turn)
                .assess(verifier, goal, messages, budget, &tx, &goal_config)
                .await
            {
                Ok(pair) => pair,
                Err(reason) => {
                    result = Some(Err(crate::failure::CliFailure::error(format!(
                        "goal not met: verifier unavailable: {reason}"
                    ))));
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
                println!(
                    "\n  {} goal met after {round} round{}: {}",
                    "✓".green().bold(),
                    if round == 1 { "" } else { "s" },
                    verdict.reasoning
                );
                plain::cost_summary(
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
                plain::cost_summary(
                    total_cost_usd,
                    &format!("{}/{}", cfg.provider.id, cfg.model_id),
                    turn_start.elapsed(),
                );
                Err(crate::failure::CliFailure::error(format!(
                    "goal not met after {} round(s): round cap reached without a passing verdict",
                    goal_config.max_rounds
                )))
            }
        }
    };

    // The shared guard is the settled ledger, including a verifier turn that
    // aborted after spending and therefore returned no `verifier_cost` value.
    let total_cost_usd = budget.session_spent_usd();
    // ONE terminator for the whole goal run (#3398). Each round drives its own
    // `Pipeline::run` over this one stream, and the pipeline used to emit a
    // terminator per round — several per run, which `validate_terminal` calls
    // a violation and which no consumer could reconcile.
    persistence::emit_run_complete_on_raw(&tx, &cfg.model_id, total_cost_usd);
    drop(tx);
    let persistence_complete = renderer.await.unwrap_or_default().persistence_complete;
    if let Some((store, id)) = &execution {
        let outcome_label = match &goal_result {
            Ok(()) => "goal_met",
            Err(_) => "goal_unmet",
        };
        if !record_execution_end(
            store,
            *id,
            registry,
            outcome_label,
            total_cost_usd,
            persistence_complete,
        ) {
            warn_store_write_failed("the audit record (agent uses / MCP usage / outcome)");
        }
    }
    goal_result
}

/// The tools the goal verifier may call: the catalog's read-only built-ins,
/// and nothing else. Pinned against the catalog by a test below so the two
/// cannot drift.
///
/// `read_file` and `search` lead the list, and they are the reason the list is
/// worth having: a verifier restricted to the task board, the scratch plane
/// and an environment report can inspect no evidence at all, so it could only
/// ever re-assert what the worker told it. Both are read-only rows, so they
/// enter by the same rule as the rest rather than as an exemption — the
/// narrowing this constant performs is against *mutation*, never against
/// looking.
const VERIFIER_TOOL_ALLOWLIST: &[&str] = &[
    "read_file",
    "search",
    "task_list",
    "get_state",
    "list_state",
    "get_environment",
];

/// The goal verifier's tool surface (#1783): the session stack narrowed to
/// [`VERIFIER_TOOL_ALLOWLIST`] BEFORE the read-only view applies. A bare
/// `ReadOnlyTools` wrap admits every schema claiming `read_only: true` —
/// including any MCP/custom tool that self-declares read-only, an outbound
/// egress channel from a role that reads worker-influenced content (a
/// prompt-injection hazard). The pipeline's verdict call carries zero tools;
/// this is the goal loop's equivalent posture: exactly the catalog's
/// read-only rows, enforced at execution rather than by prompt.
struct VerifierScopedTools<'a> {
    inner: &'a dyn stella_core::ToolExecutor,
}

impl<'a> VerifierScopedTools<'a> {
    fn new(inner: &'a dyn stella_core::ToolExecutor) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl stella_core::ToolExecutor for VerifierScopedTools<'_> {
    fn schemas(&self) -> Vec<stella_protocol::ToolSchema> {
        self.inner
            .schemas()
            .into_iter()
            .filter(|schema| VERIFIER_TOOL_ALLOWLIST.contains(&schema.name.as_str()))
            .collect()
    }

    /// Forwarded with exactly the filter `schemas()` applies (#3287) — the
    /// verifier's narrowed surface must not advertise contracts for tools it
    /// refuses.
    fn contracts(&self) -> Vec<stella_protocol::ToolContract> {
        self.inner
            .contracts()
            .into_iter()
            .filter(|contract| VERIFIER_TOOL_ALLOWLIST.contains(&contract.name()))
            .collect()
    }

    async fn execute(&self, name: &str, input: &serde_json::Value) -> stella_protocol::ToolOutput {
        if !VERIFIER_TOOL_ALLOWLIST.contains(&name) {
            return stella_protocol::ToolOutput::error(format!(
                "`{name}` is not available to the goal verifier: only the tools its \
                     instructions name ({}) may be called",
                VERIFIER_TOOL_ALLOWLIST.join(", ")
            ));
        }
        self.inner.execute(name, input).await
    }

    // Forwarded, not zeroed (the trait doc's decorator rule): nothing on the
    // allowlist dispatches sub-agents today, but a wrapper that drops spend
    // is wrong the day that changes.
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.inner.drain_sub_agent_spend_usd()
    }

    // (`drain_wait_request` and `parallel_safe_names` are NOT forwarded here,
    // which is a defect this view predates rather than a decision — #2819.)
    //
    // Forwarded for the same decorator rule, and NOT filtered by the
    // allowlist: a service the round's worker left running is a fact about
    // the workspace the verifier is judging, not a capability this view
    // grants. The verifier cannot start or stop one — no allowlisted tool
    // mutates — so the assertion (#2764) only tells it what is up, which is
    // exactly what a verifier is for.
    fn live_services(&self) -> Vec<stella_core::LiveService> {
        self.inner.live_services()
    }
}

#[cfg(test)]
mod bare_loop_board_tests {
    use super::*;
    use crate::config::PROVIDERS;
    use stella_tools::policy::ToolPolicy;

    /// The policy the bare step loop ends up running under, given whatever the
    /// operator configured.
    ///
    /// This calls the production [`bare_loop_config`], which is the entire
    /// point of these three tests. The previous version re-applied
    /// `narrow_with` in a local helper, so it witnessed
    /// [`ToolPolicy::narrow_with`] — a `stella-tools` property true long before
    /// the bare loop ever narrowed anything — and stayed green with the
    /// narrowing deleted from production (#2511).
    ///
    /// The provider row is arbitrary: nothing below the narrowing reads it, and
    /// [`Config::for_tests`] needs one only because [`Config`] has the field.
    fn bare_loop_policy(operator: ToolPolicy) -> ToolPolicy {
        let provider = PROVIDERS[0].clone();
        let model_id = provider.default_model.to_string();
        let mut cfg = Config::for_tests(provider, model_id);
        cfg.tool_policy = operator;
        bare_loop_config(&cfg).tool_policy
    }

    /// All six tools `stella_tools::tasks` registers, not the five that were
    /// listed while `task_assign` shipped: the assertion has to name the whole
    /// board, or regrouping the one tool it omits goes unnoticed (#2511).
    ///
    /// Spelled out here rather than looped over [`WITHHELD_BOARD_TOOLS`] on
    /// purpose — a test that reads the same list production reads passes for
    /// any list at all, including an empty one.
    #[test]
    fn the_bare_loop_withholds_every_board_tool() {
        let policy = bare_loop_policy(ToolPolicy::allow_all());
        for tool in [
            "task_create",
            "task_list",
            "task_start",
            "task_complete",
            "task_cancel",
            "task_assign",
        ] {
            assert!(!policy.allows(tool), "{tool} should be withheld");
        }
    }

    /// Sub-agent delegation survives the board's withdrawal.
    ///
    /// #2410 narrowed with the `task` **group** key, and the catalog files
    /// delegation — the tool named `task` — in that group, so the bare loop
    /// silently lost the capability too. #2580 settled that as collateral
    /// rather than a decision: the evidence for withholding the board is about
    /// bookkeeping that reads nothing and changes nothing, and reaches no
    /// tool that does work.
    #[test]
    fn withholding_the_board_leaves_sub_agent_delegation_alone() {
        let policy = bare_loop_policy(ToolPolicy::allow_all());
        assert!(
            policy.allows("task"),
            "sub-agent delegation shares the board's group but not its rationale"
        );
    }

    /// What the group key gave for free and a name list does not: cover.
    ///
    /// [`WITHHELD_BOARD_TOOLS`] must be exactly the `task` group minus the
    /// delegation tool, so a seventh board tool added to the catalog fails
    /// here instead of being quietly offered to the bare loop. Asserted
    /// against the group, not against a `task_*` name prefix — the group is
    /// what `narrow_with` used to resolve, and a board tool named without the
    /// prefix would slip a prefix test.
    #[test]
    fn the_withheld_list_is_the_whole_board_minus_delegation() {
        let expected: Vec<&str> = stella_tools::catalog::CATALOG
            .iter()
            .filter(|entry| entry.group == "task" && entry.name != "task")
            .map(|entry| entry.name)
            .collect();
        let mut withheld = WITHHELD_BOARD_TOOLS.to_vec();
        withheld.sort_unstable();
        let mut expected_sorted = expected.clone();
        expected_sorted.sort_unstable();
        assert_eq!(
            withheld, expected_sorted,
            "the `task` group changed: reconcile WITHHELD_BOARD_TOOLS with the catalog"
        );
    }

    #[test]
    fn withholding_the_board_leaves_the_working_tools_alone() {
        let policy = bare_loop_policy(ToolPolicy::allow_all());
        for tool in ["bash", "read_file", "write_file", "edit_file", "grep"] {
            assert!(policy.allows(tool), "{tool} must still be available");
        }
    }

    /// Narrowing can only remove. An operator who has already switched
    /// something off keeps it off, and this must never hand it back — which is
    /// why the board is withheld through the policy seam rather than by
    /// skipping registration.
    #[test]
    fn narrowing_never_grants_back_what_an_operator_denied() {
        let operator =
            ToolPolicy::from_switches([("bash".to_string(), false), ("task".to_string(), true)]);
        let policy = bare_loop_policy(operator);
        assert!(!policy.allows("bash"), "operator's denial must survive");
        assert!(
            !policy.allows("task_create"),
            "an operator grant must not resurrect the board here"
        );
    }

    /// The other direction of the same guarantee, and the one this narrowing
    /// newly depends on: handing delegation back must hand it back only to
    /// operators who never took it away.
    ///
    /// An operator who switched the whole `task` group off has denied
    /// delegation too — the group key means here exactly what it meant to
    /// #2410 — and no longer naming that key in the narrowing must not turn
    /// their denial into a grant.
    #[test]
    fn an_operator_who_denied_the_task_group_keeps_delegation_denied() {
        let policy = bare_loop_policy(ToolPolicy::from_switches([("task".to_string(), false)]));
        assert!(
            !policy.allows("task"),
            "the operator's group denial covers delegation and must survive"
        );
        assert!(
            !policy.allows("task_create"),
            "the operator's group denial covers the board too"
        );
    }
}

#[cfg(test)]
mod verifier_tools_tests {
    use super::*;
    use stella_protocol::{ToolOutput, ToolSchema};

    struct FakeStack;
    #[async_trait::async_trait]
    impl stella_core::ToolExecutor for FakeStack {
        fn schemas(&self) -> Vec<ToolSchema> {
            ["task_list", "get_state", "mcp__srv__read", "save_state"]
                .into_iter()
                .map(|name| ToolSchema {
                    name: name.into(),
                    description: String::new(),
                    input_schema: serde_json::json!({}),
                    // Everything claims read-only — the exact shape a
                    // self-declaring MCP tool presents.
                    read_only: true,
                    speculation_safe: false,
                })
                .collect()
        }
        async fn execute(&self, name: &str, _input: &serde_json::Value) -> ToolOutput {
            ToolOutput::Ok {
                content: format!("ran {name}"),
                data: None,
            }
        }
    }

    /// #1783's witness: the goal verifier is offered exactly the allowlist —
    /// a read-only claim alone (a self-declared MCP read) does not admit a
    /// tool, and execution is enforced, not prompted.
    #[tokio::test]
    async fn the_goal_verifier_gets_only_the_allowlisted_tools() {
        let stack = FakeStack;
        let scoped = VerifierScopedTools::new(&stack);
        let names: Vec<_> = scoped
            .schemas()
            .into_iter()
            .map(|schema| schema.name)
            .collect();
        assert_eq!(names, vec!["task_list", "get_state"]);
        for denied in ["mcp__srv__read", "save_state", "task"] {
            assert!(
                scoped
                    .execute(denied, &serde_json::json!({}))
                    .await
                    .is_error(),
                "{denied} must be refused at execution"
            );
        }
        assert!(matches!(
            scoped.execute("task_list", &serde_json::json!({})).await,
            ToolOutput::Ok { .. }
        ));
    }

    /// The allowlist must be exactly the catalog's read-only rows — the
    /// drift this pins is exactly how the surface once grew to ~25 unnoticed.
    #[test]
    fn the_allowlist_is_exactly_the_catalogs_read_only_rows() {
        let expected: Vec<&str> = stella_tools::catalog::CATALOG
            .iter()
            .filter(|entry| entry.read_only)
            .map(|entry| entry.name)
            .collect();
        assert_eq!(VERIFIER_TOOL_ALLOWLIST, expected.as_slice());
    }
}
