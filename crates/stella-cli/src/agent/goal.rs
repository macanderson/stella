//! Goal-driven turns: judged rounds until a verifier confirms the goal is met.
//!
//! [`run_goal_cmd`] binds an installed wrapper plugin and hands it the turn
//! (#3911, slice 7 of `doc:roleless-core`). The rounds, the verifier call and
//! the verdict all belong to that plugin and to the host's own `judge`/`again`
//! — this door resolves, funds and reports. The built-in loop it replaces
//! (`Engine::run_goal` against `Engine::assess`) was the last stage core
//! performed and the last multi-model path it shipped.
//!
//! [`run_raw_one_shot`] lives here beside it because the two doors are the
//! same shape now: one prompt, one wrapper socket, one report.

use super::*;

#[cfg(test)]
mod tests;

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
#[expect(
    clippy::too_many_arguments,
    reason = "this is `run_turn`'s door-side twin and follows its expect: the turn-entry \
              argument bundle is worth introducing across all four entry points at once, \
              not piecemeal here"
)]
pub(crate) async fn run_raw_one_shot(
    full_cfg: &Config,
    prompt: &str,
    budget_limit: Option<f64>,
    format: OutputFormat,
    pipeline: crate::wrapper_plugin::PipelineChoice<'_>,
    test_command: Option<&str>,
    require_verdict: bool,
    // `stella skill run`: the invoked skill this one-shot runs, with
    // the turn scope its directives asked for — the grant narrowing and
    // effort override `run_turn` applies while the invocation is live.
    // `None` for a plain `stella run`, which is every caller there was
    // before the verb existed.
    invoked_skill: Option<crate::extensions::InvokedSkill>,
) -> Result<(), crate::failure::CliFailure> {
    // Fired before anything else is built: a provider, a tool registry, an
    // MCP connection. So a denied prompt costs nothing but the hook itself.
    let prompt_owned = match user_prompt_submit_hook(full_cfg, prompt).await {
        Ok(prompt) => prompt,
        Err(reason) => return Err(crate::failure::CliFailure::error(reason)),
    };
    let prompt: &str = &prompt_owned;
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
    // claim: an identity snapshotted afterwards vouches for nothing. The same
    // call observes what the tests say before the work, which is the red half a
    // flip needs (#1292) — and is a whole test run, so it says what it found.
    let candidate = match &resolved {
        Some(_) => Some(
            crate::wrapper_candidate::grant_shared_tree(&cfg.workspace_root, test_command)
                .await
                .map_err(crate::failure::CliFailure::from)?,
        ),
        None => None,
    };
    if let Some(line) = candidate
        .as_ref()
        .and_then(|granted| granted.baseline_notice.as_deref())
    {
        eprintln!("  ! {line}");
    }
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
                .map_err(crate::failure::CliFailure::from)?
                // What a plugin puts in front of the model spends the same
                // allowance this turn's records, skills and frames spend.
                .with_context_allowance(crate::plugin_steering::allowance(cfg)),
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
    let mut recall = crate::memory::OpeningRecall::default();
    if let Some(m) = &memory {
        let touched = stella_core::driver::loop_evidence::turn_evidence(&messages).touched_paths;
        let recalled = m.recall_block_reported(prompt, &touched).await;
        recall =
            crate::memory::inject_opening_recall(&mut messages, recalled, &cfg.steering_ledger);
    }
    // After recall, exactly where the interactive loop notes its `/slug`
    // expansions: the skill's injection event and its turn scope ride the
    // same seam whichever door invoked it.
    recall.note_invoked_skill(invoked_skill);

    let started_unix = crate::memory::unix_now_secs();
    // Machine-wide presence: findable in the deck's SESSIONS overlay and
    // replayable from its journal after this process exits.
    let mut presence = SessionPresence::announce(cfg, prompt);
    // Agent whistle: this door runs in non-interactive mode (no deck, no
    // `stella-serve`), so until now it had nowhere to steer from — `stella
    // whistle` reaches it over this session's own control socket, into the
    // same `TurnControls` seam `wrapper_plugin::RawTurnDriver` already
    // carried for exactly this (its own doc comment named `run_raw_one_shot`
    // as the non-interactive door that would one day publish something
    // other than `none()` here).
    // `whistle` is held for the run's duration — its `Drop` is what unbinds
    // and removes the socket file.
    let whistle = crate::whistle::SessionWhistle::open(Some(presence.id()));
    let controls = whistle.controls();
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
            let wrapper_id = bound.wrapper_id();
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
                    door: "run",
                    wrapper_id: wrapper_id.as_str(),
                    recall,
                    memory: memory.as_mut(),
                    watch: &candidate.watch,
                    // Agent whistle (see above): this non-interactive door now
                    // publishes a real steering tap, still no pause gate —
                    // whistle only ever steers, never pauses (see
                    // `crate::whistle`'s module docs on scope).
                    controls: controls.clone(),
                    results: Vec::new(),
                    friction: &mut friction,
                    rounds: crate::turn_row::TurnRow::new(),
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
                recall,
                memory.as_mut(),
                controls.clone(),
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

/// The wrapper plugin `stella goal` binds when the caller names none.
///
/// The verb keeps its meaning — work in judged rounds until an independent
/// verifier assesses the goal as met — and stops carrying its own loop to mean
/// it (#3911, `doc:roleless-core` §6). The rounds are held open by an installed
/// arbiter-grade plugin's `again`, decided by the host's `judge` against that
/// plugin's declared `[requirements]`/`[oracle]`, and assessed by the verifier
/// turn its `after_turn` spends. `plugins/stella-goal` is the reference
/// implementation and declares this id.
pub(crate) const DEFAULT_GOAL_WRAPPER: &str = "goal-v1";

/// How many judged rounds `stella goal` funds.
///
/// Read off `stella_core::GoalConfig`, which is the number the verb's own
/// built-in loop ran before it moved onto the wrapper socket (#3911) and the
/// number `stella-serve`'s goal route still runs — so the two surfaces cannot
/// disagree about what the word "goal" buys. Read rather than restated,
/// because a second copy of a default is how the first one drifts.
fn goal_rounds() -> u32 {
    u32::try_from(stella_core::GoalConfig::default().max_rounds).unwrap_or(u32::MAX)
}

/// The refusal `stella goal` owes when no wrapper plugin supplies the verb.
///
/// The shape [`crate::wrapper_plugin::classic_removed_message`] uses for the
/// removed staged pipeline, and for the same reason: a verb whose built-in
/// implementation is gone must name the install that restores it rather than
/// fail as though the user mistyped something. `reason` is the resolver's own
/// account — which variants *are* installed, or why the named one cannot run —
/// so a typo and an empty roster stay distinguishable.
pub(crate) fn goal_plugin_missing_message(variant: &str, reason: &str) -> String {
    format!(
        "`stella goal` runs an installed wrapper plugin: {reason}\nInstall the reference \
         goal-supervision plugin and try again:\n    stella plugin install \
         ./plugins/stella-goal\nOr pass `--pipeline <variant>` naming another installed \
         arbiter-grade wrapper. This run asked for `{variant}`."
    )
}

/// Resolve the wrapper plugin that supplies the goal verb, and pin the
/// candidate grant its `[oracle]` observes.
///
/// Both happen before the provider is built and before a single paid call — a
/// `--pipeline` naming nothing installed must fail as a typo, a workspace with
/// nothing installed must fail as an install, and a `--test-command` the
/// host's parser refuses must stop the run here rather than after it is paid
/// for.
///
/// The grant is minted once for the arc, never per round. What the watch
/// covers is the artifacts the test command names — the witness the flip is
/// judged against — and those do not legitimately change while the loop runs.
/// A baseline retaken at the top of round 3 would vouch for a witness the
/// worker rewrote in round 2, which is the laundering the watch exists to
/// refuse. So the finding is sticky: once a witness has moved under the run,
/// no later round earns a `Clean` (#3835).
///
/// The test baseline this call also observes is minted once for the same
/// reason and a sharper one: after round 1 the tree is the worker's, so a
/// second observation would report the work rather than what preceded it, and
/// a suite that went green in round 1 would erase the red the flip is measured
/// against.
pub(crate) async fn resolve_goal_wrapper(
    cfg: &Config,
    variant: &str,
    test_command: Option<&str>,
) -> Result<
    (
        crate::wrapper_plugin::ResolvedWrapper,
        crate::wrapper_candidate::GrantedCandidate,
    ),
    crate::failure::CliFailure,
> {
    let resolved = crate::wrapper_plugin::resolve(&cfg.workspace_root, variant, &mut |line| {
        eprintln!("  ! {line}");
    })
    .map_err(|reason| {
        crate::failure::CliFailure::error(goal_plugin_missing_message(variant, &reason))
    })?;
    let candidate = crate::wrapper_candidate::grant_shared_tree(&cfg.workspace_root, test_command)
        .await
        .map_err(crate::failure::CliFailure::from)?;
    if let Some(line) = candidate.baseline_notice.as_deref() {
        eprintln!("  ! {line}");
    }
    Ok((resolved, candidate))
}

/// Bind a resolved goal wrapper to what this host will do for it: `recall`,
/// `child_turn` and `run_test` (#3833, #4536).
///
/// Separate from [`resolve_goal_wrapper`] because a child-turn plane needs
/// this session's dispatcher, which needs a provider — and the resolve above
/// must happen before that provider exists. One host per member of the
/// selection, built from that member's own manifest: the plane reads
/// `[roles]` and `[loop] max_calls`, so a shared one would let a second plugin
/// name the first's role intents (`ResolvedWrapper::serving`, #4094).
///
/// # What this door funds
///
/// [`goal_rounds`] rounds, on both ceilings that bound them: the hold loop's
/// and the child-turn plane's. The host's defaults are lower, so a goal run
/// bound to them would stop after three rounds where the built-in loop ran
/// eight — a loss the user can see, in the one change that was supposed to
/// leave the verb's meaning alone.
pub(crate) fn bind_goal_wrapper(
    cfg: &Config,
    resolved: crate::wrapper_plugin::ResolvedWrapper,
    sub_agents: &std::sync::Arc<crate::subagent::SessionSubAgents>,
    candidate: &crate::wrapper_candidate::GrantedCandidate,
) -> Result<crate::wrapper_plugin::BoundWrapper, crate::failure::CliFailure> {
    Ok(resolved
        .serving(|manifest| {
            crate::wrapper_plugin::round_driver_host(
                &cfg.workspace_root,
                manifest,
                std::sync::Arc::clone(sub_agents)
                    as std::sync::Arc<dyn stella_core::subagent::SubAgentDispatcher>,
                // The grant minted above, so a plugin's `run_test` re-runs the
                // invocation this arc is judged against rather than being told
                // the capability has nothing behind it (#4536).
                Some(&candidate.grant),
                // One verifier turn per round this door funds. A plugin
                // holding N rounds open asks once in each, so a lower ceiling
                // ends the arc `Undecided { MeasurementMissing }` rather than
                // on a verdict.
                Some(goal_rounds()),
            )
        })
        .map_err(crate::failure::CliFailure::from)?
        .funding_rounds(goal_rounds())
        // The same allowance every other steering source spends.
        .with_context_allowance(crate::plugin_steering::allowance(cfg)))
}

/// Run a goal to its verdict (non-interactive): `stella goal "…"`, and `stella
/// monitor` composed on top of it.
///
/// # One loop, one arbiter
///
/// This door is a **resolver** (`#3911`, slice 7 of `doc:roleless-core`): it
/// binds an installed wrapper plugin and hands it the turn, exactly as
/// `stella run --pipeline <id>` does. The plugin's `again` decides whether a
/// round holds open, its `after_turn` spends the verifier call, and the
/// host's `judge` maps that evidence to a verdict against the plugin's own
/// declared rule. What it replaces is a loop of its own —
/// `stella_core::Engine::run_goal` driving working turns against
/// `Engine::assess`, the last stage core performed on this door and the last
/// multi-model path it reached.
///
/// `reject_arbiter_wrapper_on_goal` is gone with the loop it protected. That
/// rung refused every arbiter-grade plugin here, because a wrapper's own
/// hold loop would have judged a round the built-in arbiter was already
/// judging. There is no first arbiter to collide with, so arbiter grade is
/// what this door *wants* — the only grade that may hold a completion open
/// past its first turn (`stella_runtime::wrapper::verdict`'s `again`, rule
/// 1). A steering or observer wrapper still binds and still runs; it cannot
/// ask for a second round, so the goal is judged once.
///
/// # What the caller's flags mean here
///
/// `pipeline` names the wrapper to bind. With none named it is
/// [`DEFAULT_GOAL_WRAPPER`], so `stella goal "…"` stays a verb rather than
/// becoming a flag exercise. `test_command` and `require_verdict` are always
/// honoured now instead of being refused for want of a wrapper: every goal run
/// has one.
///
/// The bind happens before the provider is built and before a single paid
/// call — the ordering [`run_raw_one_shot`] documents, for the same reason. A
/// `--pipeline` naming nothing installed must fail as a typo, and a goal run
/// with nothing installed at all must fail as an install, not after the money
/// is spent.
pub async fn run_goal_cmd(
    cfg: &Config,
    goal: &str,
    budget_limit: Option<f64>,
    pipeline: crate::wrapper_plugin::PipelineChoice<'_>,
    // `--test-command`: the oracle a bound wrapper's `[oracle]` observes its
    // flip with (#3835).
    test_command: Option<&str>,
    // `--require-verdict` (#3554, this door #4543): the wrapper's own
    // conclusion gates the exit status.
    require_verdict: bool,
) -> Result<(), crate::failure::CliFailure> {
    crate::enterprise_telemetry::authorize_execution_surface(
        crate::enterprise_telemetry::ExecutionSurface::Goal,
    )?;
    let variant = pipeline.plugin().unwrap_or(DEFAULT_GOAL_WRAPPER);
    let (resolved, candidate) = resolve_goal_wrapper(cfg, variant, test_command).await?;
    let provider = build_provider(cfg)?;
    let registry: std::sync::Arc<ToolRegistry> =
        std::sync::Arc::new(crate::write_dirs::registry_for(cfg));

    let sub_agents = crate::subagent::install_for_session(cfg, &registry)?;
    let bound = bind_goal_wrapper(cfg, resolved, &sub_agents, &candidate)?;
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
    // Breaker feedback for this arc's turns, shared by every round the
    // wrapper holds open.
    let router = session_router(cfg, &ModelRef::new(cfg.provider.id, cfg.model_id.clone()));

    plain::section_header("Stella — goal mode");
    println!("  {}\n", goal.dimmed());

    let mut messages = vec![
        CompletionMessage::system(
            with_session_hook_context(
                build_system_prompt(cfg, &cfg.workspace_root, &active_rules),
                cfg,
            )
            .await,
        ),
        crate::attachments::user_message_in(goal, &cfg.workspace_root),
    ];
    let mut memory =
        SessionMemory::open_for_session(&cfg.workspace_root, true, &cfg.authority, &active_rules);
    // Phase 2 (#713): carried to the turn runner, which owns the event channel
    // the events ride and assembles the tool stack the scopes narrow.
    let mut recall = crate::memory::OpeningRecall::default();
    if let Some(m) = &mut memory {
        // One arm for the whole goal run (#1221): the rounds a wrapper holds
        // open are stages of one turn — they share this run's episode, so they
        // must share its arm, and re-arming per round would count one prompt
        // as N turns of the schedule.
        m.arm_recall_control();
        let touched = stella_core::driver::loop_evidence::turn_evidence(&messages).touched_paths;
        let recalled = m.recall_block_reported(goal, &touched).await;
        recall =
            crate::memory::inject_opening_recall(&mut messages, recalled, &cfg.steering_ledger);
    }

    let started_unix = crate::memory::unix_now_secs();
    // Machine-wide presence: a goal run is exactly the long-lived headless
    // session the SESSIONS overlay + replay exist for.
    let mut presence = SessionPresence::announce(cfg, goal);
    // Agent whistle (#4769): one listener for the whole arc rather than one
    // per round. Held for the arc's duration; its `Drop` unbinds and removes
    // the socket.
    let whistle = crate::whistle::SessionWhistle::open(Some(presence.id()));
    let controls = whistle.controls();
    // This arc's friction, one ledger per round the wrapper held open (#3962,
    // #3976), read by the reflection further down.
    let mut friction: Vec<TurnFriction> = Vec::new();
    // Bound once: a composition's variant id is assembled on demand (#3801),
    // so the driver borrows this rather than a temporary.
    let wrapper_id = bound.wrapper_id();
    let outcome = crate::wrapper_plugin::run_wrapped(
        &bound,
        goal,
        crate::wrapper_plugin::pre_turn_signals(test_command.is_some(), budget_limit.is_some()),
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
            format: OutputFormat::Text,
            store: &store,
            prompt: goal,
            session: presence.id(),
            door: "goal",
            wrapper_id: wrapper_id.as_str(),
            recall,
            memory: memory.as_mut(),
            watch: &candidate.watch,
            controls: controls.clone(),
            results: Vec::new(),
            friction: &mut friction,
            rounds: crate::turn_row::TurnRow::new(),
        },
    )
    .await;
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
        && crate::agent::output::should_reflect_on_turn(
            turn_warrants_reflection(&messages),
            memory.is_some(),
            crate::agent::reflection_explicitly_disabled(),
        )
        && let Some(m) = &mut memory
    {
        // One ledger per round (#3962): this reflection covers an arc of
        // several turns, and a single fold over the whole stream would report
        // round 1's tools as the round that ran last.
        let mut report = crate::memory::reflect_routed(
            m,
            cfg,
            &*provider,
            crate::memory::TurnEvidence::with_rounds(&messages, &friction, outcome.is_ok()),
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
