//! Goal-driven turns: judged rounds until a verifier confirms the goal is met.
//!
//! `run_goal_cmd` drives the raw `Engine::run_goal` step-loop (the default
//! since #3381) or an installed wrapper plugin per round (`--pipeline
//! <variant>`, [`goal_wrapped`], #3695 goal half) — the staged pipeline
//! (`--pipeline classic`) is gone (#3865). The goal verifier is
//! independent of the worker model and answers "does the whole effort meet
//! the goal?" — distinct from a wrapper plugin's own oracle — and stays the
//! same `Engine::assess` primitive on both arms.

use super::*;

mod goal_wrapped;

/// The session task board, tool by tool — everything the catalog files under
/// the `task` group *except* `delegate`, the sub-agent delegation tool that
/// shares the group.
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
/// delegation — `delegate`, which was itself named `task` until #3192 — in
/// that same group, so the group key took delegation with the board and the
/// bare loop could not spawn a child at all.
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
    require_verdict: bool,
) -> Result<(), crate::failure::CliFailure> {
    let bare = bare_loop_config(full_cfg);
    let cfg = &bare;
    // Resolved before the provider is built and before a single paid call: a
    // `--pipeline` that names nothing installed must fail as a typo, not after
    // the run it was meant to shape. The grant is minted right here, for the
    // same reason — a `--test-command` the host's parser refuses must
    // stop the run here, not after it is paid for.
    let resolved = match pipeline.plugin() {
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
    let candidate = match &resolved {
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

    let sub_agents = crate::subagent::install_for_session(cfg, &registry)?;
    // The gate is built *here*, not beside `resolve` above, and the ordering is
    // the whole reason the two halves are separate: a `--pipeline` naming
    // nothing installed must fail as a typo before a paid call, while a
    // plugin's `child_turn` needs this session's dispatcher, which needs the
    // provider built two lines up (#3576).
    let bound = match resolved {
        Some(resolved) => Some(
            resolved
                // One host per member, built from that member's own manifest:
                // the child-turn plane reads `[roles]` and `[loop] max_calls`,
                // so a shared one would let a second plugin name the first's
                // role intents (`ResolvedWrapper::serving`).
                .serving(|manifest| {
                    crate::wrapper_plugin::session_host(cfg, manifest, Arc::clone(&sub_agents))
                })
                .map_err(crate::failure::CliFailure::from)?,
        ),
        None => None,
    };
    // The one derivation of "a human is here to answer" — the #2676 approval
    // responder and the rules-enforcement prompt both read this value.
    let ask = human_is_present(format == OutputFormat::Text);
    let active_rules = crate::rules::enforce_workspace_rules(
        &registry,
        &cfg.workspace_root,
        &cfg.authority,
        crate::rules::MidTurnAsk::tty_when(ask),
    );
    // Auto-build + live-refresh the code graph in the background so
    // `stella search` and the deck's Graph tab have a fresh index. Status
    // goes to stderr — stdout may be machine-readable JSON.
    let (_session_graph, _graph_build) = spawn_session_graph(
        &cfg.workspace_root,
        Box::new(crate::agent::init::stderr_narrator()),
        Box::new(|| {}),
        Box::new(|_| {}),
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
    // The wall-clock task deadline, armed from `--turn-timeout` (#3868). This
    // is the one-shot door, and a one-shot run is one task, so the flag's
    // allowance IS the task's ceiling — `EngineConfig::turn_budget` (set from
    // the same flag) only lets the engine *decline to start* work it cannot
    // finish, and enforces nothing. Unarmed, an over-run died on the harness's
    // kill with its work discarded instead of stopping at a safe boundary with
    // a scorable partial; #2957 measured exactly that across 20 bench trials.
    let mut budget = crate::runtime::one_shot_budget_guard(budget_limit, cfg.turn_timeout);
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
        let touched = stella_core::driver::loop_evidence::turn_evidence(&messages).touched_paths;
        let recalled = m.recall_block_reported(prompt, &touched).await;
        recall_event = recalled.telemetry_event();
        inject_recall_block(&mut messages, recalled.text);
    }

    let started_unix = crate::memory::unix_now_secs();
    // Machine-wide presence: findable in the deck's SESSIONS overlay and
    // replayable from its journal after this process exits.
    let mut presence = SessionPresence::announce(cfg, prompt);
    // This prompt's friction (#3946), read by the reflection further down.
    //
    // A `Vec` because the wrapped arm is genuinely several turns: an
    // arbiter-grade plugin holds the round open and this door drives one turn
    // per hold, so it folds one ledger per driven turn exactly as a `/goal`
    // arc does (#3976). The raw arm pushes the single ledger `run_turn` fills.
    //
    // **What it covers is the turns this door drove, and not the wrapper's own
    // spend.** A plugin's `before_turn`/`after_turn` points buy their own child
    // turns through the host-call channel; those are reported beside the run
    // (`BoundWrapper::child_spends`) and are not in here. Naming that boundary
    // is the whole decision #3976 asked for: the alternative — reflecting with
    // nothing — left a wrapped run's reflection blind to cost, wall clock,
    // retries and loop firings, which is the #3946 regression scoped to one
    // path.
    let mut friction: Vec<TurnFriction> = Vec::new();
    // The wrapper socket's first driver (#3494). With no `--pipeline` the arm
    // below is the one that has always run, unchanged: one turn, no dispatch,
    // and a NULL `pipeline_variant` because no wrapper ran over it.
    let outcome = match (&bound, &candidate) {
        // Both arms of one `Option` are set together above, so the mismatched
        // pairs are unreachable; matching the tuple keeps that visible instead
        // of unwrapping a grant on the strength of having checked the wrapper.
        (Some(bound), Some(candidate)) => {
            // Bound once: a composition's variant id is assembled on demand
            // (#3801), so the driver borrows this rather than a temporary.
            let variant = bound.variant();
            crate::wrapper_plugin::run_wrapped(
                bound,
                prompt,
                crate::wrapper_plugin::pre_turn_signals(
                    test_command.is_some(),
                    budget_limit.is_some(),
                ),
                Some(candidate.grant.clone()),
                require_verdict,
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
                    variant: variant.as_str(),
                    recall_event,
                    memory: memory.as_mut(),
                    watch: &candidate.watch,
                    // Real ones the day a controlled surface drives a wrapped
                    // turn (#3554). This door is headless — it publishes no
                    // pause gate and installs no steering tap, the same reason
                    // its goal rounds pass `steering: None` — so there is
                    // nothing here to honour, and `none()` is the honest
                    // answer rather than a placeholder.
                    controls: stella_core::ports::TurnControls::none(),
                    results: Vec::new(),
                    friction: &mut friction,
                },
            )
            .await
        }
        _ => {
            let mut turn = TurnFriction::default();
            let result = run_turn(
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
                Some(&mut turn),
            )
            .await
            .map(|_| ());
            friction.push(turn);
            result
        }
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
    // Reflection is about what the turn *learned*, not about what it *printed*.
    //
    // This guard opened with `format == OutputFormat::Text`, which made the
    // three conditions after it unreachable for every machine format — and
    // machine formats are the only ones an autonomous caller uses.
    // `one_shot_reflection_enabled` names `Json` and `StreamJson` as supported
    // in its own body, `reflect_routed` already takes a quiet flag for
    // non-text, and `surface_reflection` already refuses to print on anything
    // but text. The whole path was built to reflect on JSON and one clause
    // switched it off.
    //
    // What that cost: a self-driving run against `oxagen-platform` executed 22
    // turns through `stella run --output-format json` across 16 merged pull
    // requests and finished with **no reflections, no memories and no promoted
    // skills** — every turn read the workspace's memory and wrote none back.
    //
    // The stream contract the old clause protected is still kept, by the piece
    // that owns it: `surface_reflection` emits nothing on a machine format, so
    // `Complete` remains the unique final frame. Recording and surfacing are
    // separate questions and only the second one is about framing.
    //
    // `one_shot_reflection_enabled` still honors the benchmark adapter's
    // `STELLA_DISABLE_REFLECTION` opt-out, which is the switch for "do not
    // spend the extra provider call".
    if should_reflect_after_one_shot(
        format,
        turn_warrants_reflection(&messages),
        memory.is_some(),
    ) && let Some(m) = &mut memory
    {
        // The friction this door folded above (#3946). It was empty here for
        // as long as the raw loop had no producer — so reflection saw every
        // tool call the transcript carried, but never what any of them cost,
        // how long they took, or that the turn had retried or looped, none of
        // which a `CompletionMessage` records. One entry on the raw arm; one
        // per driven turn on the wrapped one, which is why the slice
        // constructor is the one both arms share (#3976).
        let mut report = crate::memory::reflect_routed(
            m,
            cfg,
            &*provider,
            crate::memory::TurnEvidence::with_rounds(&messages, &friction, outcome.is_ok()),
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
/// `pipeline` selects the driver for each working round (#3381). It has two
/// arms, both live: `--pipeline classic` is refused at
/// [`crate::wrapper_plugin::PipelineChoice::resolve`] — the built-in staged
/// pipeline is gone (#3865) and the variant that named it with it (#3867), so
/// this door has no third case to handle. `Raw` — the default since #3381, with or
/// without `--no-pipeline` — falls back to the raw `Engine::run_goal`
/// step-loop; `Plugin(variant)` dispatches each round's worker turn through
/// the named installed wrapper ([`goal_wrapped::run_goal_wrapped_turn`],
/// #3695 goal half) while the goal verifier stays exactly what `Raw` uses —
/// **provided the named wrapper is not arbiter-grade**: this door's own
/// pre-flight rung refuses `participation = "arbiter"` before the provider
/// is ever built (`wrapper_plugin::reject_arbiter_wrapper_on_goal`, #3832),
/// because the goal loop is already this door's completion arbiter and a
/// second one held open inside a judged round is the doubled-supervisor
/// shape the wrapper design forbids; an arbiter-grade wrapper's designed
/// home is `stella run --pipeline <variant>` instead. Steering and observer
/// wrappers reach `Plugin(variant)` unaffected. `stella fleet` drives a named
/// plugin per worker attempt now, on its own driver
/// (`crate::fleet_cmd::wrapped`, #3695 fleet half) — and deliberately
/// without this door's arbiter refusal, because a fleet attempt has no
/// completion arbiter of its own for a hold loop to double.
pub async fn run_goal_cmd(
    cfg: &Config,
    goal: &str,
    budget_limit: Option<f64>,
    pipeline: crate::wrapper_plugin::PipelineChoice<'_>,
    // `--test-command`: the oracle a bound wrapper's `[oracle]` observes its
    // flip with (#3835). Refused before this function on the raw arm, where
    // there is no oracle to arm.
    test_command: Option<&str>,
) -> Result<(), crate::failure::CliFailure> {
    // `Plugin` reads as `Raw` for every branch below except the final
    // dispatch: the wrapped arm builds the same system prompt, the same
    // recall block and the same tool stack `Raw` does, and differs only in
    // which function drives the round loop. The staged pipeline itself is
    // gone (#3865) and so is the variant that named it (#3867), so `Raw` and
    // `Plugin` are the whole space and this door runs those two arms
    // unconditionally now.
    crate::enterprise_telemetry::authorize_execution_surface(
        crate::enterprise_telemetry::ExecutionSurface::Goal,
    )?;
    // Resolved before the provider is built and before a single paid call —
    // exactly the ordering `run_raw_one_shot` uses, and for the same reason:
    // a `--pipeline` naming nothing installed must fail as a typo, not after
    // the run it was meant to shape.
    let resolved = match pipeline.plugin() {
        Some(variant) => Some(
            crate::wrapper_plugin::resolve(&cfg.workspace_root, variant, &mut |line| {
                eprintln!("  ! {line}");
            })
            .map_err(crate::failure::CliFailure::from)?,
        ),
        None => None,
    };
    // Arbiter-grade wrappers are refused here, before binding and before any
    // paid call — the goal loop is this door's own completion arbiter, and a
    // wrapper that holds rounds open would run a second hold loop inside one
    // judged round (#3832). See
    // `crate::wrapper_plugin::reject_arbiter_wrapper_on_goal`'s doc comment.
    if let Some(resolved) = &resolved {
        crate::wrapper_plugin::reject_arbiter_wrapper_on_goal(resolved)
            .map_err(crate::failure::CliFailure::from)?;
    }
    // Pinned *before* the first round, which is the whole content of the
    // tamper claim, and pinned exactly once for the run rather than refreshed
    // per round (#3835).
    //
    // Re-pinning would read as the more careful choice and is the unsafe one.
    // What the watch covers is the artifacts the test command names — the
    // witness the flip is observed against, not whatever files a round
    // happened to touch — and those do not legitimately change while the loop
    // runs. A baseline taken at the top of round 3 would vouch for a witness
    // the worker rewrote in round 2, which is precisely the laundering the
    // watch exists to refuse. So the finding is sticky by design: once a
    // witness has moved under the run, no later round earns a `Clean`.
    //
    // The same breath as the resolve above, and for the same reason: a
    // `--test-command` the host's parser refuses must stop the run here, not
    // after it is paid for.
    let candidate = match &resolved {
        Some(_) => Some(
            crate::wrapper_candidate::grant_shared_tree(&cfg.workspace_root, test_command)
                .map_err(crate::failure::CliFailure::from)?,
        ),
        None => None,
    };
    let provider = build_provider(cfg)?;
    let registry: std::sync::Arc<ToolRegistry> =
        std::sync::Arc::new(crate::write_dirs::registry_for(cfg));

    let sub_agents = crate::subagent::install_for_session(cfg, &registry)?;
    // The goal door serves `recall` and `child_turn` (#3833). The plane is
    // built here rather than beside `resolve` above for the ordering
    // `run_raw_one_shot` documents: a `--pipeline` naming nothing installed
    // must fail as a typo before a paid call, while a child-turn plane needs
    // this session's dispatcher, which needs the provider built above. It
    // allocates its receipt slots through `stella_core::turn_slots`, whose
    // lanes are what let a plane counting its own calls run beside a goal
    // round's own worker/verifier pair without overwriting either.
    let bound = match resolved {
        Some(resolved) => Some(
            resolved
                // One host per member of the selection, built from that
                // member's own manifest: `round_driver_host`'s child-turn
                // plane reads the manifest's `[roles]` and `[loop] max_calls`,
                // so a shared one would let a second plugin name the first's
                // role intents (`ResolvedWrapper::serving`, #4094).
                .serving(|manifest| {
                    crate::wrapper_plugin::round_driver_host(
                        &cfg.workspace_root,
                        manifest,
                        std::sync::Arc::clone(&sub_agents)
                            as std::sync::Arc<dyn stella_core::subagent::SubAgentDispatcher>,
                    )
                })
                .map_err(crate::failure::CliFailure::from)?,
        ),
        None => None,
    };
    // Goal mode always renders human-readable output, so its half of the
    // fact is fixed at `true`; the stdio handles settle the rest.
    let ask = human_is_present(true);
    let active_rules = crate::rules::enforce_workspace_rules(
        &registry,
        &cfg.workspace_root,
        &cfg.authority,
        crate::rules::MidTurnAsk::tty_when(ask),
    );
    // Auto-build + live-refresh the code-graph index in the background so
    // `stella search` and the deck's Graph tab stay current without a manual
    // `stella init`. Non-blocking; status to stderr. Kept alive until the
    // goal returns.
    let (_session_graph, _graph_build) = spawn_session_graph(
        &cfg.workspace_root,
        Box::new(crate::agent::init::stderr_narrator()),
        Box::new(|| {}),
        Box::new(|_| {}),
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

    let mut messages = vec![CompletionMessage::system(
        with_session_hook_context(
            build_system_prompt(cfg, &cfg.workspace_root, &active_rules),
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
        let touched = stella_core::driver::loop_evidence::turn_evidence(&messages).touched_paths;
        let recalled = m.recall_block_reported(goal, &touched).await;
        recall_event = recalled.telemetry_event();
        inject_recall_block(&mut messages, recalled.text);
    }

    let started_unix = crate::memory::unix_now_secs();
    // Machine-wide presence: a goal run is exactly the long-lived headless
    // session the SESSIONS overlay + replay exist for.
    let mut presence = SessionPresence::announce(cfg, goal);
    // This arc's friction, one ledger per round, filled by whichever arm runs
    // below and read by the reflection further down (#3962, #3976). Both arms
    // fold it, from the same journal, at the same point: what differs is only
    // who drove each round's working turn.
    let mut rounds: Vec<TurnFriction> = Vec::new();
    // Both are set together above, so the mismatched pairs are unreachable;
    // matching the tuple keeps that visible rather than unwrapping a grant on
    // the strength of having checked the wrapper — `run_raw_one_shot`'s shape.
    let outcome = if let (Some(bound), Some(candidate)) = (&bound, &candidate) {
        goal_wrapped::run_goal_wrapped_turn(
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
            budget_limit,
            recall_event,
            memory.as_mut(),
            bound,
            candidate,
            Some(&mut rounds),
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
            Some(&mut rounds),
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
        // One ledger per round (#3962): this reflection covers an arc of
        // several turns, and a single fold over the whole stream would report
        // round 1's tools as the round that ran last.
        let mut report = crate::memory::reflect_routed(
            m,
            cfg,
            &*provider,
            crate::memory::TurnEvidence::with_rounds(&messages, &rounds, outcome.is_ok()),
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

/// Put this session's withheld-steering notice on a goal run's stream, if
/// there is one and no other door has already spent it (#4500).
///
/// `stella goal` reaches neither of the two openers that carry this —
/// `agent::output::open_raw_turn` (the raw door) and
/// `command_deck::steering::announce_withheld` (the deck's boot) — because it
/// drives `Engine::run_goal` itself rather than `agent::run_turn`. So an
/// untrusted checkout running `stella goal` had the refusal on stderr and on
/// no event stream: nothing in the journal, nothing in `stella export`,
/// nothing a harness could read.
///
/// Both arms call it, so a goal run announces once whichever pipeline it took,
/// and it is claimed through the session latch rather than a local flag: a
/// goal run is one process today, but a latch that is the same latch cannot
/// disagree with the other doors about whether the session has been told.
pub(super) fn announce_withheld_steering(tx: &mpsc::UnboundedSender<AgentEvent>, cfg: &Config) {
    let Some(withheld) = cfg.authority.withheld.as_ref() else {
        return;
    };
    if !crate::agent::claim_withheld_announcement() {
        return;
    }
    let _ = tx.send(withheld.event());
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
    // Where this arc's friction ledgers land, ONE PER ROUND, for a caller that
    // reflects afterwards (#3962). An out-parameter for the same reason
    // `run_turn`'s single-ledger slot is one: this function's return type says
    // whether the goal was met, and reflection's evidence is not that. `None`
    // for a caller that does not reflect.
    rounds: Option<&mut Vec<TurnFriction>>,
) -> Result<(), crate::failure::CliFailure> {
    let turn_start = Instant::now();
    // This function is the RAW arm — `run_goal_cmd` calls it only for
    // `PipelineChoice::Raw` — so `variant: None` is the honest answer every
    // time, not a placeholder (#3381, #3388): nothing wrapped this round.
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
    let events = stella_core::EventSender::new(tx.clone());
    // The registry's own streams and this turn's per-call work-tree
    // measurement, through the one seam (#4507). This door opened its channel
    // and attached NOTHING to it: `stella goal` rendered no task board, no
    // sub-agent lifecycle, and no diff under any mutating call for the whole
    // of a run — the #4175 silence, on the one door where the loop can run
    // dozens of rounds before a human sees the result.
    persistence::attach_run_streams(registry, cfg, &events, execution.as_ref());
    let renderer = spawn_renderer(
        rx,
        OutputFormat::Text,
        execution.clone(),
        cfg.provider.id.to_string(),
        false,
        Some(goal.to_string()),
    );
    // What this workspace's trust gate withheld, then what recall put in
    // front of the model — the ordering `output::open_raw_turn` argues for on
    // the raw door, and the door parity #4500 asks for: `stella goal` reached
    // neither opener, so an untrusted checkout's refusal was on stderr and on
    // no event stream at all.
    announce_withheld_steering(&tx, cfg);
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
        let hook_runner = HostHookRunner;
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
    // What this arc changed, measured before the stream closes (#4159).
    //
    // Once for the run rather than once per round, and that is a limit of
    // where this arm's loop lives rather than a choice: `Engine::run_goal`
    // drives every round inside `stella-core`, so there is no round boundary
    // in this function to measure at. What that costs is the *ordering* of
    // these events and nothing else — a goal run is one execution row and one
    // stream, so `files_touched`, the counts, and
    // `finalize_execution_reflection`'s `wrote_files` flag are the same
    // whichever boundary the reading is taken at; only "which round wrote it"
    // is unavailable, and this arm never had it. The wrapped arm drives its
    // own rounds and does measure per round (`goal_wrapped`).
    crate::turn_files::emit_shared_tree_changes_raw(cfg, &tx, execution.as_ref());
    // This goal loop owns the stream, so it — not any single round — emits the
    // run's one ending (#3398). A goal that ends unmet still *ended*: the
    // rounds ran, the money was spent, and the outcome is carried by the
    // execution record, not by withholding the terminator.
    let (GoalOutcome::Met { cost_usd, .. } | GoalOutcome::Unmet { cost_usd, .. }) = &outcome;
    persistence::emit_run_complete_on_raw(&tx, &cfg.model_id, *cost_usd);
    // The canonical teardown (#960): the registry now holds sender clones of
    // this channel, so it is detached before the renderer is awaited or the
    // run hangs on a channel that never closes.
    drop(tx);
    let rendered = persistence::close_event_stream(registry, events, renderer).await;
    let persistence_complete = rendered.persistence_complete;
    // The arc's friction, folded from the journal the renderer just finished
    // draining — split at each round's own `GoalVerdict` so no round is
    // credited with another's tools (#3962). Folded here for the same reason
    // `run_turn` folds here: this is the first point at which the whole stream
    // is both complete and still owned.
    if let Some(slot) = rounds {
        *slot = TurnFriction::per_goal_round(&rendered.events);
    }

    let (outcome_label, cost) = match &outcome {
        GoalOutcome::Met { cost_usd, .. } => ("goal_met", *cost_usd),
        GoalOutcome::Unmet { cost_usd, .. } => ("goal_unmet", *cost_usd),
    };
    crate::agent::turn_close::close_turn(
        cfg,
        store,
        &execution,
        registry,
        session,
        crate::agent::turn_close::TurnOutcomeRecord {
            label: outcome_label,
            cost_usd: cost,
            persistence_complete,
        },
    );

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
