//! Engine tuning and pipeline provider wiring.
//!
//! `EngineConfig` construction per agent kind, and the role->provider
//! resolution for pipeline runs. Every wiring failure here is soft: an
//! unroutable role rides the worker provider, so configuration can never
//! turn a runnable pipeline into an error.

use super::*;

/// EngineConfig for `kind`: defaults + the workspace root as hook `cwd`,
/// with the agent's `agent_engine_config` tuning applied — temperature and
/// max_tokens override the engine defaults only when set (the "Include"
/// contract), effort/reasoning/params land verbatim (they default to
/// `None` anyway).
///
/// `catalog_ref` is the `(provider_id, model_id)` the catalog-based clamps
/// below (context-window compaction budget, reasoning capability) are
/// computed against — the model THIS kind's calls actually land on, not
/// necessarily `cfg`'s. For `Default`/`Judge` that is `cfg.provider.id`/
/// `cfg.model_id`; for `Worker` it is the wiring's resolved worker model
/// (issue #276 — honoring `pipeline_worker_model`/`agents.worker.*` must
/// also clamp against the model it actually routes to, or a worker pinned to
/// a smaller-context or non-reasoning model still gets the DEFAULT model's
/// clamps, which is exactly the wire-shape/400 class issue #273 exists to
/// warn about).
fn tuned_engine_config(
    cfg: &Config,
    kind: crate::settings::EngineAgentKind,
    catalog_ref: (&str, &str),
) -> EngineConfig {
    let (provider_id, model_id) = catalog_ref;
    let mut engine = EngineConfig {
        cwd: cfg.workspace_root.display().to_string(),
        // Every role gets the same turn budget: the deadline it guards is the
        // process's, not one agent's, so a worker that declines a continuation
        // while a judge spends the remaining time past it would defeat the
        // point. `None` unless `--turn-budget` was given.
        turn_budget: cfg.turn_budget,
        // Phase 2 (#713): the adaptive-context lifecycle switch, off by
        // default. Read here rather than at each receipt site so every engine
        // this session builds — default, worker, judge — agrees on it.
        lifecycle_enabled: crate::memory::session_lifecycle_enabled(&cfg.workspace_root),
        // Durability, for every role at once. This is the single place an
        // engine is tuned in this crate, so attaching the sink here covers the
        // deck's lead turn and its pipeline, sub-agents, sub-sessions and the
        // fleet without six chances to forget one.
        //
        // `None` while the session has not bound its durable location — a
        // command with no turn to checkpoint, or a driver that has not yet
        // resolved its session record. That is not a silent downgrade: it is
        // what stops `persist_checkpoint` from serializing a whole transcript
        // per step only to discard it.
        checkpoint_sink: cfg.durability.sink(),
        ..EngineConfig::default()
    };
    // Compaction must fire BEFORE the provider's context window overflows:
    // the engine default (150k) exceeds some catalog windows (deepseek-chat
    // is 128k), where provider-side overflow would land before compaction
    // ever triggered. The window only ever LOWERS the budget — 3/4 leaves
    // headroom for the estimator's error band plus the next step's output.
    // A settings override lands FIRST so the clamp applies to it too: an
    // operator can shape context handling per arm (#1285), never schedule a
    // provider-side overflow.
    if let Some(settings) = &cfg.engine_settings
        && let Some(budget) = settings.compaction_budget_tokens
    {
        engine.compaction_budget_tokens = budget;
    }
    if let Ok(entry) = stella_model::catalog::Catalog::current().resolve_for(provider_id, model_id)
    {
        let window = entry.context_window as u64;
        if window > 0 {
            engine.compaction_budget_tokens = engine
                .compaction_budget_tokens
                .min(window.saturating_mul(3) / 4);
        }
        // The model's own completion ceiling replaces the global default,
        // in BOTH directions. `EngineConfig::default()` carries one 16384 for
        // every model on every provider and its own comment called per-model
        // caps the eventual refinement; this is that refinement, and the data
        // has been on the model card the whole time.
        //
        // Raising matters where the work is long: a comparator on the same
        // model, same API and same effort spent 45,001–64,000 output tokens
        // finishing tasks whose Stella runs ended at the cap with no tool
        // call. Lowering matters too — a row whose ceiling is below 16384 was
        // being asked for more than the provider will serve.
        //
        // Settings still win: this lands before the `engine_settings` block,
        // so an explicit `params.max_tokens` overrides it.
        if let Some(ceiling) = entry.max_output_tokens.filter(|c| *c > 0) {
            engine.max_output_tokens = Some(ceiling);
        }
    }
    if let Some(settings) = &cfg.engine_settings {
        let tuning = crate::engine_config::tuning_for(settings, kind);
        if tuning.temperature.is_some() {
            engine.temperature = tuning.temperature;
        }
        if tuning.max_output_tokens.is_some() {
            engine.max_output_tokens = tuning.max_output_tokens;
        }
        engine.effort = tuning.effort;
        engine.reasoning = tuning.reasoning;
        engine.params = tuning.params;
        // The output cap's partner ceiling, and it is set here rather than
        // per-role for the reason the turn budget is: what it bounds belongs to
        // the process, not to one agent. A judge left on the default while the
        // worker's cap tripled would stop first on exactly the runs the raise
        // was for.
        //
        // `0` disables the backstop (`None`), restoring the unbounded await.
        // That is spelled rather than refused because it is the only way to say
        // "no ceiling" in a field whose absence already means "the default" —
        // and the two are genuinely different requests.
        if let Some(secs) = settings.model_timeout_secs {
            engine.model_timeout = (secs > 0).then(|| std::time::Duration::from_secs(secs));
        }
        // `0` disables the retention pass (the same "0 opts out" convention
        // as `model_timeout_secs` above), restoring pure budget-triggered
        // compaction.
        if let Some(steps) = settings.tool_result_horizon_steps {
            engine.tool_result_horizon_steps = (steps > 0).then_some(steps as usize);
        }
    }
    // Capability clamp: a catalog-confirmed non-reasoning model must not
    // carry effort/reasoning onto the wire — providers reject or silently
    // ignore them, and both outcomes are worse than omitting the fields
    // (the auto modes set effort for every role without knowing the
    // model). Unknown capability passes through: the provider stays the
    // authority.
    if crate::engine_config::model_supports_reasoning(provider_id, model_id) == Some(false) {
        engine.effort = None;
        engine.reasoning = None;
    }
    engine
}

/// EngineConfig for a session's default (interactive/step-loop) agent.
pub(crate) fn engine_config_for(cfg: &Config) -> EngineConfig {
    tuned_engine_config(
        cfg,
        crate::settings::EngineAgentKind::Default,
        (cfg.provider.id, &cfg.model_id),
    )
}

/// EngineConfig for a pipeline's execute turns — the WORKER agent's tuning
/// (plan and witness ride it too, matching the router's tiering).
/// `worker_model` is [`EngineWiring::worker_model`]: the model the worker
/// role actually resolves to, honoring `pipeline_worker_model`/
/// `agents.worker.*` when set (issue #276), falling back to the session
/// default (`cfg.provider`/`cfg.model_id`) when unset.
pub(crate) fn pipeline_engine_config_for(cfg: &Config, worker_model: &ModelRef) -> EngineConfig {
    tuned_engine_config(
        cfg,
        crate::settings::EngineAgentKind::Worker,
        (&worker_model.provider, &worker_model.model_id),
    )
}

/// The safe default for CLI-owned headless surfaces: no host approval port,
/// so scope expansion stops at the named pipeline error — unless
/// `agent_engine_config.headless_scope_bypass` opts a `stella run` out (see
/// `pipeline_config_for_approval_capability`, which ORs this constant with
/// that setting). Output modes never alter this.
pub(crate) const HEADLESS_SCOPE_REVIEW_BYPASS: bool = false;
pub(crate) const HEADLESS_APPROVAL_GATE: AlwaysAbortGate = AlwaysAbortGate;

/// Approval port the one-shot host can actually service. This is explicit so
/// output serialization cannot silently stand in for execution authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PipelineApprovalCapability {
    Stdio,
    Unavailable,
}

/// Which approval capability a one-shot host run can actually service, given
/// the output format and whether stdin/stdout are real terminals. A pure
/// function over already-observed booleans (rather than calling
/// `IsTerminal` itself) so the exact condition is directly unit-testable.
/// Stdio approval requires a text-safe renderer PLUS both terminal handles:
/// stdin must accept the
/// decision and stdout must present the prompt. A redirected/piped
/// text-format run is still rendered as text, but must stay headless and
/// fail closed at scope review — never read stdin for a decision no one is
/// there to give.
pub(crate) fn approval_capability_for(
    is_text: bool,
    stdin_is_terminal: bool,
    stdout_is_terminal: bool,
) -> PipelineApprovalCapability {
    if is_text && stdin_is_terminal && stdout_is_terminal {
        PipelineApprovalCapability::Stdio
    } else {
        PipelineApprovalCapability::Unavailable
    }
}

/// Whether a TRUSTED engine posture explicitly names a witness/judge author
/// other than the worker's model — i.e. the posture's own hash claims the
/// authored-witness arm, so a run that silently loses that author reports a
/// number the posture misdescribes (#1147).
///
/// Three deliberate narrowings:
///
/// * **Trusted postures only.** The settings scope chain keeps its soft
///   degradation (a judge whose provider has no credential leaves a notice and
///   rides the worker, exactly as [`EngineWiring`] documents). Nothing is
///   published about an ordinary session's wiring, so nothing about it can be
///   misdescribed.
/// * **An EXPLICIT judge key only** — `agents.judge.model` or the flat
///   `pipeline_judge_model`, never [`crate::settings::AgentEngineConfig::model_for`]'s
///   fallback to `default_model`. The control arm reaches the judge role
///   through that fallback, and reading it here would arm the refusal on the
///   very arm that is *supposed* to run one model for every role.
/// * **Compared as the caller wrote it.** The posture writes fully-qualified
///   `provider/slug` specs (the benchmark adapter and the TUI both do), which
///   is the same shape as [`ModelRef`]'s `Display`. A bare slug that happens
///   to name the worker's model would arm the refusal spuriously — and that is
///   the safe direction: the run stops loudly instead of scoring quietly.
pub(crate) fn trusted_posture_requires_independent_witness(
    cfg: &Config,
    worker_model: &ModelRef,
) -> bool {
    if !cfg.engine_settings_trusted {
        return false;
    }
    let Some(engine) = cfg.engine_settings.as_ref() else {
        return false;
    };
    engine
        .agent(crate::settings::EngineAgentKind::Judge)
        .and_then(|agent| agent.model.as_deref())
        .or(engine.pipeline_judge_model.as_deref())
        .map(str::trim)
        .filter(|judge| !judge.is_empty())
        .is_some_and(|judge| judge != worker_model.to_string())
}

/// Build the one-shot pipeline config from the host's approval capability.
/// Rendering remains a separate concern owned by the event renderer.
/// `worker_model` is [`EngineWiring::worker_model`], threaded through to
/// `pipeline_engine_config_for` so the worker's clamps key off the model the
/// role actually resolves to (issue #276).
pub(crate) fn pipeline_config_for_approval_capability(
    cfg: &Config,
    approval: PipelineApprovalCapability,
    test_command: Option<&str>,
    worker_model: &ModelRef,
) -> PipelineConfig {
    PipelineConfig {
        engine: pipeline_engine_config_for(cfg, worker_model),
        headless: approval == PipelineApprovalCapability::Unavailable,
        plan_mode: cfg.plan_mode,
        // The constant is the safe default; a workspace may opt out of it
        // where the tree is disposable (see `headless_scope_bypass`).
        headless_bypass_scope_review: cfg
            .engine_settings
            .as_ref()
            .is_some_and(|engine| engine.headless_scope_bypass_on())
            || HEADLESS_SCOPE_REVIEW_BYPASS,
        test_command: test_command.map(str::to_string),
        // A posture that PUBLISHED an independent witness author must not
        // quietly run without one — see the predicate's doc comment.
        require_independent_witness: trusted_posture_requires_independent_witness(
            cfg,
            worker_model,
        ),
        // Where this run's work happens. Resolved from settings here and asked
        // at triage, once the class says whether anything is going to change.
        create_worktrees: cfg.create_worktrees.policy(),
        ..apply_pipeline_tuning(cfg, PipelineConfig::default())
    }
}

/// Overlay the settings-file pipeline tuning knobs (`pipeline_max_revisions`,
/// `pipeline_candidates`) onto `config`, leaving each field at whatever it
/// already held when the corresponding key is absent.
///
/// Applied as a *transform over a base* rather than as two accessors returning
/// values, because the "absent" case has to mean **the pipeline's own default**
/// and that default lives in `PipelineConfig::default()`. An accessor would
/// have to restate `2` and `None` here, and a restated default drifts silently
/// the day the pipeline changes its mind — the failure mode being a run tuned
/// by a number nobody chose.
///
/// Every driver calls this, not just `stella run`. These are cost/quality
/// knobs rather than safety gates, so unlike `headless_scope_bypass` — which
/// `stella goal` and fleet workers deliberately hold hard-off because it
/// governs whether unattended work may land unreviewed — there is no surface
/// where honouring the user's setting is the unsafe choice.
pub(crate) fn apply_pipeline_tuning(cfg: &Config, mut config: PipelineConfig) -> PipelineConfig {
    let Some(engine) = cfg.engine_settings.as_ref() else {
        return config;
    };
    if let Some(max_revisions) = engine.pipeline_max_revisions {
        config.max_revisions = max_revisions;
    }
    if let Some(candidates) = engine.pipeline_candidates {
        // Stored as written, including `0`. `PipelineConfig::candidate_count`
        // floors at 1, so a zero reads as single-shot rather than as "run
        // nothing" — the same reading `None` gets, and the only one that can
        // produce a result at all.
        config.candidates = Some(candidates);
    }
    config
}

/// EngineConfig for the goal loop's standalone judge engine — the JUDGE
/// agent's tuning.
pub(crate) fn judge_engine_config_for(cfg: &Config) -> EngineConfig {
    tuned_engine_config(
        cfg,
        crate::settings::EngineAgentKind::Judge,
        (cfg.provider.id, &cfg.model_id),
    )
}

/// Fire `SessionStart` hooks once and return their stdout — the additional
/// session context `stella_core::hooks` documents. `None` when no hooks are
/// configured or they printed nothing. Called once per session by each
/// driver, never per turn.
pub(crate) async fn session_start_hook_context(cfg: &Config) -> Option<String> {
    let hooks = cfg.hooks.as_ref()?;
    let outcome = stella_core::hooks::run_hooks(
        &ShellHookRunner,
        Some(hooks),
        &stella_core::hooks::HookPayload::session_start(cfg.workspace_root.display().to_string()),
    )
    .await;
    // A SessionStart hook that failed stays non-fatal, but must not vanish:
    // without this line a typo'd hook silently contributes no context all
    // session and nothing ever says why (issue #373, item 6).
    for diagnostic in &outcome.diagnostics {
        eprintln!("  ! SessionStart {diagnostic}");
    }
    (!outcome.output.is_empty()).then_some(outcome.output)
}

/// Ceiling on hook-contributed session context, in chars (~1k tokens). Every
/// other system-prompt section carries a budget (memories 16k, exploration
/// index 2k, records 2k); this one had none, so a hook that cats a log file
/// silently inflated every model call for the whole session — inside the
/// cached prefix, where the write premium re-bills it on any miss.
pub(crate) const SESSION_HOOK_CONTEXT_BUDGET_CHARS: usize = 4_000;

/// Head-clamp hook output to [`SESSION_HOOK_CONTEXT_BUDGET_CHARS`] with a
/// visible marker — the same posture as every other budgeted prompt section:
/// the truncation is stated in-band, never silent.
pub(crate) fn clamp_hook_context(context: &str) -> String {
    let mut kept: String = context
        .chars()
        .take(SESSION_HOOK_CONTEXT_BUDGET_CHARS)
        .collect();
    if kept.chars().count() < context.chars().count() {
        kept.push_str("\n[… SessionStart hook output truncated at the session-context budget …]");
    }
    kept
}

/// Append any `SessionStart` hook context to an assembled system prompt.
/// The result is still byte-stable for the session: hooks fire once, here,
/// and the prompt never changes afterwards.
///
/// Byte-stable for the session is the guarantee; byte-stable *across*
/// sessions is the hook author's side of the bargain. Output that differs
/// per run — a timestamp, a `git status`, a branch SHA — rewrites the whole
/// cached prefix on every new session, which is exactly the
/// `CacheCause::PrefixInstability` the cache panel diagnoses. The clamp
/// bounds the cost of that mistake; it cannot remove it.
pub(crate) async fn with_session_hook_context(mut system_prompt: String, cfg: &Config) -> String {
    if let Some(context) = session_start_hook_context(cfg).await {
        system_prompt.push_str("\n\nSession context (from SessionStart hooks):\n");
        system_prompt.push_str(&clamp_hook_context(&context));
    }
    system_prompt
}

// Pipeline port adapters

/// Everything `agent_engine_config` resolves for one pipeline run: the
/// role router inputs (profiles + pins), owned adapters for roles routed
/// to a model other than the worker's, the per-role request overrides,
/// and human-readable notices about wiring decisions (a provider without
/// a credential, an adapter that failed to build). Every failure is soft:
/// the affected role rides the worker, exactly as before this config
/// existed — configuration must never turn a runnable pipeline into an
/// error.
pub(crate) struct EngineWiring {
    pub(crate) profiles: Vec<ProviderProfile>,
    pub(crate) pins: RoleTable,
    /// Adapters for pinned off-worker models, keyed by the exact
    /// [`ModelRef`] the pins route to (adapters bind their model id at
    /// construction, so each distinct ref needs its own instance).
    pub(crate) extra_providers: Vec<(ModelRef, Box<dyn Provider>)>,
    pub(crate) role_overrides: stella_pipeline::PipelineRoleOverrides,
    /// The model `Role::Worker`/`Role::Plan` actually resolve to: the
    /// worker's own `pipeline_worker_model`/`agents.worker.*` pin when one
    /// is configured and its provider is credentialed (issue #276), else
    /// the session default this wiring was built with. Callers building the
    /// worker's own [`EngineConfig`] (catalog-based context-window and
    /// reasoning-capability clamps) must key off THIS, not `cfg` directly —
    /// see `pipeline_engine_config_for`.
    pub(crate) worker_model: ModelRef,
    pub(crate) notices: Vec<String>,
}

/// Resolve one role's already-computed [`crate::engine_config::ModelSpec`] into a pin: find the
/// credentialed provider, and build its adapter unless the pin names the
/// exact same model the primary resolver entry already serves (`base_ref` —
/// always the literal session-default `ModelRef` the pre-built primary
/// provider is bound to, never an already-overridden ref, so this check
/// stays "does this need a NEW adapter instance" regardless of which role is
/// being pinned). `roles` lets one resolved model pin more than one router
/// role at once (`Role::Plan` shares `Role::Worker`'s tier — see
/// `resolve_engine_wiring`'s worker-override handling). Every failure here
/// is soft — a missing credential or a build error pushes a notice and
/// leaves every role in `roles` unpinned, degrading to `fallback` in the
/// router, never a hard error. Returns the resolved [`ModelRef`] on success.
fn pin_role(
    wiring: &mut EngineWiring,
    roles: &[Role],
    label: &str,
    spec: &crate::engine_config::ModelSpec,
    base_ref: &ModelRef,
    configured: &[crate::config::ConfiguredProvider],
    fallback: &str,
) -> Option<ModelRef> {
    let Some(entry) = configured.iter().find(|c| c.config.id == spec.provider) else {
        wiring.notices.push(format!(
            "engine config: {label} model `{}/{}` skipped — no resolvable credential for \
             provider `{}`; {label} {fallback}",
            spec.provider, spec.model, spec.provider
        ));
        return None;
    };
    // An empty slug is the "provider pin without a model" form — the
    // provider's own default model.
    let slug = if spec.model.is_empty() {
        entry.config.default_model.to_string()
    } else {
        spec.model.clone()
    };
    let pinned = ModelRef::new(entry.config.id, slug.clone());
    if pinned == *base_ref {
        // Same instance the primary resolver entry already serves: no new
        // adapter needed, the pin(s) still record the explicit choice.
        for &role in roles {
            wiring.pins.pin(role, pinned.clone());
        }
        return Some(pinned);
    }
    match build_provider_parts(
        &entry.config,
        &slug,
        entry.api_key.clone(),
        entry.config.base_url.to_string(),
        None,
    ) {
        Ok(provider) => {
            for &role in roles {
                wiring.pins.pin(role, pinned.clone());
            }
            // A profile for the routed provider keeps the router's provider
            // list honest (breaker bookkeeping, `providers()` introspection,
            // and — critically — the router's own unpinned-judge cross-
            // family lookup, which matches `resolve(Worker)`'s result against
            // a profile's `worker_model` field) even though the pin itself
            // short-circuits normal tiered resolution.
            wiring.profiles.push(
                ProviderProfile::new(
                    entry.config.id,
                    pinned.clone(),
                    pinned.clone(),
                    pinned.clone(),
                )
                .with_family(provider_family(entry.config.id)),
            );
            wiring.extra_providers.push((pinned.clone(), provider));
            Some(pinned)
        }
        Err(e) => {
            wiring.notices.push(format!(
                "engine config: {label} model `{}/{slug}` skipped — {e}; {label} {fallback}",
                entry.config.id
            ));
            None
        }
    }
}

/// Resolve the engine wiring for a pipeline run whose session-default worker
/// is `worker_ref` (already resolved by `Config` — an explicit `--model`
/// flag beats `default_model`/`agents.default.*` there, see
/// `Config::load_with_settings`; it beats the WORKER-role settings here, via
/// `cfg.model_pinned_by_flag`). `configured`
/// is the caller's own [`crate::config::discover_configured_providers`]
/// snapshot — injected rather than rediscovered here so this function is a
/// plain, testable one over owned data.
///
/// Routing rules, in order:
/// - WORKER (and `Role::Plan`, which shares the worker's tier when unpinned
///   — `resolve_tier` in `stella-core`'s router) honors
///   `pipeline_worker_model`/`agents.worker.*`
///   ([`crate::engine_config::model_spec_for`]) when configured and its
///   provider is credentialed; unset or unroutable falls back to the
///   session default `worker_ref` (issue #276). An explicit `--model`
///   (`cfg.model_pinned_by_flag`) suppresses that settings override
///   entirely — the flag exists to pin the worker for one invocation, so it
///   outranks the config file.
/// - TRIAGE and JUDGE pins come from their configured model specs the same
///   way, but always fall back to the (possibly worker-overridden) worker
///   model on any failure — the pre-existing behavior.
/// - `auto_mode: on` replaces the judge spec with
///   [`crate::engine_config::auto_judge_spec`]'s pick from
///   `allowed_models` (cross-family from the ACTUAL worker model, then
///   price tier); when the allowed list yields nothing usable it falls back
///   to the explicit judge spec, then to normal router degradation.
/// - A pin equal to the session-default model needs no extra adapter — the
///   primary resolver entry already serves it.
///
/// Pins deliberately bypass the circuit breaker (`RoleTable` semantics —
/// an explicit pin wins unconditionally). If a pinned judge's provider
/// fails, the pipeline's judge call degrades to its heuristic verdict,
/// the same soft path an unreachable judge always took.
pub(crate) fn resolve_engine_wiring(
    cfg: &Config,
    worker_ref: &ModelRef,
    configured: &[crate::config::ConfiguredProvider],
) -> EngineWiring {
    use crate::engine_config::{
        ModelSpec, auto_judge_spec, model_spec_for, spec_family, tuning_for,
    };
    use crate::settings::EngineAgentKind;

    let worker_profile = ProviderProfile::new(
        worker_ref.provider.clone(),
        worker_ref.clone(),
        worker_ref.clone(),
        worker_ref.clone(),
    )
    .with_family(provider_family(&worker_ref.provider));

    let mut wiring = EngineWiring {
        profiles: vec![worker_profile],
        pins: RoleTable::new(),
        extra_providers: Vec::new(),
        role_overrides: stella_pipeline::PipelineRoleOverrides::default(),
        worker_model: worker_ref.clone(),
        notices: Vec::new(),
    };
    let Some(engine) = cfg.engine_settings.clone() else {
        return wiring;
    };

    // Credentialed providers only — a model spec naming a provider without
    // a resolvable key is reported and skipped, never a hard error.
    let is_provider = |id: &str| configured.iter().any(|c| c.config.id == id);

    // Issue #276: resolve the WORKER's own override first, before anything
    // that needs to know the worker's actual model (judge cross-family
    // selection, the capability clamp's "rides the worker" fallback below).
    // `Role::Plan` is pinned alongside `Role::Worker` to the same model —
    // unpinned, it shares the worker's tier (`resolve_tier` treats
    // `Worker`/`Plan` identically), so leaving it out would silently revert
    // plan/witness turns to the session default the moment the worker is
    // overridden, defeating "plan rides the worker" (`pipeline_engine_config_for`'s
    // doc comment).
    // ...unless the invocation carries an explicit `--model`. That flag's
    // documented job IS pinning the worker model for one run, so settings
    // must lose to it — otherwise the pin is not merely ignored but
    // unobservable: the run reports the model that was asked for while a
    // different one does the work and bills for it. Only the WORKER spec is
    // suppressed; `pipeline_triage_model`/`pipeline_judge_model` still apply,
    // since `--model` says nothing about those roles.
    let worker_spec = match model_spec_for(&engine, EngineAgentKind::Worker, &is_provider) {
        Some(spec) if cfg.model_pinned_by_flag => {
            // An empty slug is the "provider pin without a model" form.
            let configured_label = if spec.model.is_empty() {
                spec.provider.clone()
            } else {
                format!("{}/{}", spec.provider, spec.model)
            };
            wiring.notices.push(format!(
                "engine config: worker model `{configured_label}` skipped — `--model` pinned \
                 `{worker_ref}` for this invocation"
            ));
            None
        }
        other => other,
    };
    let effective_worker_ref = match &worker_spec {
        Some(spec) => pin_role(
            &mut wiring,
            &[Role::Worker, Role::Plan],
            "worker",
            spec,
            worker_ref,
            configured,
            &format!("rides the session default (`{worker_ref}`)"),
        )
        .unwrap_or_else(|| worker_ref.clone()),
        None => worker_ref.clone(),
    };
    wiring.worker_model = effective_worker_ref.clone();

    let triage_tuning = tuning_for(&engine, EngineAgentKind::Triage);
    let judge_tuning = tuning_for(&engine, EngineAgentKind::Judge);
    wiring.role_overrides.triage = stella_pipeline::RoleCallOverrides {
        prompt: triage_tuning.prompt,
        effort: triage_tuning.effort,
        reasoning: triage_tuning.reasoning,
        temperature: triage_tuning.temperature,
        max_output_tokens: triage_tuning.max_output_tokens,
        params: triage_tuning.params,
    };
    wiring.role_overrides.judge = stella_pipeline::RoleCallOverrides {
        prompt: judge_tuning.prompt,
        effort: judge_tuning.effort,
        reasoning: judge_tuning.reasoning,
        temperature: judge_tuning.temperature,
        max_output_tokens: judge_tuning.max_output_tokens,
        params: judge_tuning.params,
    };

    // The judge's cross-family preference must compare against the model the
    // worker ACTUALLY resolves to — comparing against the stale session
    // default here would let auto-mode pick a judge that turns out to share
    // the overridden worker's family (or vice versa), defeating the
    // bias-resistance the family comparison exists for.
    let worker_family = spec_family(&ModelSpec {
        provider: effective_worker_ref.provider.clone(),
        model: effective_worker_ref.model_id.clone(),
    });
    let judge_spec = if engine.auto_mode_on() {
        auto_judge_spec(&engine, &worker_family, &is_provider)
            .or_else(|| model_spec_for(&engine, EngineAgentKind::Judge, &is_provider))
    } else {
        model_spec_for(&engine, EngineAgentKind::Judge, &is_provider)
    };
    let triage_spec = model_spec_for(&engine, EngineAgentKind::Triage, &is_provider);

    // Capability clamp, mirroring `tuned_engine_config`: a role whose
    // model (pinned, provider-default, or riding the worker) is a
    // catalog-confirmed non-reasoning model must not carry effort or
    // reasoning onto the wire. Unknown capability passes through. "Riding
    // the worker" means the ACTUAL (possibly overridden) worker model.
    {
        let clamp = |overrides: &mut stella_pipeline::RoleCallOverrides,
                     spec: Option<&ModelSpec>| {
            let resolved: Option<(String, String)> = match spec {
                Some(s) if !s.model.is_empty() => Some((s.provider.clone(), s.model.clone())),
                // Provider pin without a model → the provider's default.
                Some(s) => crate::config::PROVIDERS
                    .iter()
                    .find(|p| p.id == s.provider && !p.default_model.is_empty())
                    .map(|p| (s.provider.clone(), p.default_model.to_string())),
                None => Some((
                    effective_worker_ref.provider.clone(),
                    effective_worker_ref.model_id.clone(),
                )),
            };
            if let Some((provider, model)) = resolved
                && crate::engine_config::model_supports_reasoning(&provider, &model) == Some(false)
            {
                overrides.effort = None;
                overrides.reasoning = None;
            }
        };
        clamp(&mut wiring.role_overrides.triage, triage_spec.as_ref());
        clamp(&mut wiring.role_overrides.judge, judge_spec.as_ref());
    }

    let role_specs = [
        (Role::Triage, "triage", triage_spec),
        (Role::Judge, "judge", judge_spec),
    ];

    for (role, label, spec) in role_specs {
        let Some(spec) = spec else { continue };
        pin_role(
            &mut wiring,
            &[role],
            label,
            &spec,
            worker_ref,
            configured,
            "rides the worker",
        );
    }
    wiring
}

/// Maps each pinned [`ModelRef`] to its adapter: the primary (worker)
/// provider plus the wiring's extra per-role adapters. The worker entry is
/// borrowed (the caller owns it — boxed in one-shot, `&dyn` in the deck
/// and goal paths); the extras are borrowed from the [`EngineWiring`].
pub(crate) struct RoleProviderResolver<'p> {
    primary: &'p dyn Provider,
    primary_ref: ModelRef,
    extra: &'p [(ModelRef, Box<dyn Provider>)],
}

impl<'p> RoleProviderResolver<'p> {
    pub(crate) fn new(
        primary: &'p dyn Provider,
        primary_ref: ModelRef,
        extra: &'p [(ModelRef, Box<dyn Provider>)],
    ) -> Self {
        Self {
            primary,
            primary_ref,
            extra,
        }
    }
}

impl ProviderResolver for RoleProviderResolver<'_> {
    fn provider_for(&self, model: &ModelRef) -> Option<&dyn Provider> {
        if *model == self.primary_ref {
            return Some(self.primary);
        }
        self.extra
            .iter()
            .find(|(model_ref, _)| model_ref == model)
            .map(|(_, provider)| &**provider)
    }
}

pub(crate) fn build_provider(cfg: &Config) -> Result<Box<dyn Provider>, String> {
    build_provider_parts(
        &cfg.provider,
        &cfg.model_id,
        // `cfg.api_key` is already an `ApiKey` (H3) — clone it rather than
        // reconstructing one from a revealed string.
        cfg.api_key.clone(),
        cfg.effective_base_url().to_string(),
        cfg.base_url_override.as_deref(),
    )
}

/// The per-dialect provider factory, over already-resolved parts rather than
/// a whole [`Config`]. Both the worker path ([`build_provider`]) and the
/// goal loop's routed judge ([`resolve_cross_family_judge`]) go through this
/// one match, so the wire-dialect selection — and the anti-phantom-slug
/// catalog check — live in exactly one place. `effective_base_url` is the
/// base URL requests go to (override-or-default); `base_url_override` is the
/// raw `--base-url`, which only the Vertex/Bedrock arms consume (they build
/// region/project-scoped URLs themselves).
///
/// The catalog is consulted first (provider-scoped, since the same slug
/// legitimately exists on several providers — `gemini-3-pro` on both `gemini`
/// and `vertex`) so an unrecognized model slug is a hard, immediate, named
/// error, never a silent construction of a provider that will simply fail its
/// first live call (L-M1/L-M2). `local` and never-synced custom endpoints are
/// exempt: their models are whatever the user pulled into them, and the
/// anti-phantom-slug rule exists to catch drift in OUR seed data, not to veto
/// the user's own endpoint.
///
/// Each wire dialect gets its own arm: OpenAI (Responses API), Anthropic
/// (Messages), Gemini direct + Vertex (generateContent), Bedrock (Converse,
/// SigV4). Everything else — Z.ai, xAI, DeepSeek, OpenRouter, local — is
/// genuinely the same Chat Completions shape behind different base URLs,
/// served by the shared adapter re-identified per provider so its
/// `Provider::id()` and error messages name the surface actually being called
/// (an xAI 401 must never read "Z.ai rejected the API key").
fn build_provider_parts(
    provider_config: &crate::config::ProviderConfig,
    model_id: &str,
    api_key: ApiKey,
    effective_base_url: String,
    base_url_override: Option<&str>,
) -> Result<Box<dyn Provider>, String> {
    // The full anti-invalid-slug ladder, for EVERY provider (not just seeded
    // ones): the seed floor always passes; a provider whose master-list rows
    // are synced (`stella models refresh`) gets hard validation with
    // suggestions; `local` and never-synced custom endpoints keep their
    // endpoint-is-the-authority posture. See
    // `crate::model_catalog::validate_model_slug` for the full ladder.
    //
    // The seed-floor half of this also runs inside
    // `stella_model::factory::build_provider`, so a caller that reaches the
    // factory without going through here — a second host, or stella-model's
    // own live smoke tests — still cannot construct a phantom-slug provider.
    // Running it here first is what buys the synced-catalog escalation and its
    // suggestions, which need the on-disk catalog this crate owns.
    //
    // PRECONDITION: the catalog is installed by the COMMAND, not lazily here
    // (#895). `model_catalog::STORE` is documented as never opened implicitly
    // by a reader, so that tests and library-style callers cannot reach the
    // user's real `~/.stella/catalog.db` — installing it from this factory
    // would break that, and would let one unit test's on-disk catalog decide
    // another's assertions. So every command that can reach here installs it
    // first: `main` via `model_catalog::bootstrap`, and `stella ingest
    // <paths>`, which dispatches earlier, via its own call. Without that the
    // ladder silently drops to its seed-only rung and a catalog-provable bad
    // slug dies at the provider instead — the exact #895 report.
    crate::model_catalog::validate_model_slug(provider_config, model_id)?;

    // The dialect match itself is `stella_runtime::build_provider`, so the
    // serve sidecar reaches the same factory without linking this binary.
    // What stays here is the *synced-catalog* escalation above: it needs the
    // on-disk catalog this crate owns, and a server has none (#971).
    stella_runtime::build_provider(&provider_config.runtime_parts(
        model_id,
        api_key,
        effective_base_url,
        base_url_override,
    ))
    .map_err(|error| error.to_string())
}

/// Cross-family grouping key for judge selection. Same-vendor providers must
/// count as the SAME family so a routed judge is genuinely a different model,
/// not the same weights behind a second endpoint: a Gemini judge assessing
/// Gemini-via-Vertex work carries the same bias, as does an Anthropic Claude
/// judge over Bedrock Claude. Anything without a known sibling is its own
/// family (its id).
pub(crate) fn provider_family(provider_id: &str) -> String {
    match provider_id {
        "gemini" | "vertex" => "google".to_string(),
        "anthropic" | "bedrock" => "anthropic".to_string(),
        other => other.to_string(),
    }
}

/// A `ProviderProfile` for a discovered provider, using its `default_model`
/// for all three role tiers (the finest model this layer knows without a
/// per-role catalog) and [`provider_family`] for cross-family grouping.
fn profile_for(config: &crate::config::ProviderConfig) -> ProviderProfile {
    let model = ModelRef::new(config.id, config.default_model);
    ProviderProfile::new(config.id, model.clone(), model.clone(), model)
        .with_family(provider_family(config.id))
}

/// Resolve the JUDGE role for the goal loop. Builds a role [`Router`] whose
/// most-preferred provider is the active worker (`worker_id`/`worker_model`,
/// so the `--model` pin is honored) followed by every OTHER configured
/// provider, then resolves `Role::Judge`. The router prefers a healthy
/// provider whose family differs from the worker's (`resolve_judge`), so:
///
/// - Only the worker's family configured → the router degrades to the worker
///   provider; `model_ref.provider == worker_id`, so we return `None` and no
///   second provider is built (behavior identical to before).
/// - A distinct family is selected → the concrete `ModelRef` is returned.
///
/// Returns `None` (→ caller reuses the worker as judge) on ANY failure —
/// same-family degradation, a resolve error, an unknown judge provider, or a
/// judge-adapter build failure — so judge routing can never break the loop.
/// On success returns the built judge provider and its id (for the notice).
pub(crate) fn resolve_cross_family_judge(
    worker_id: &str,
    worker_model: &str,
    configured: &[crate::config::ConfiguredProvider],
) -> Option<(Box<dyn Provider>, String)> {
    let worker_ref = ModelRef::new(worker_id, worker_model);
    let worker_profile = ProviderProfile::new(
        worker_id,
        worker_ref.clone(),
        worker_ref.clone(),
        worker_ref,
    )
    .with_family(provider_family(worker_id));

    let mut profiles = vec![worker_profile];
    for entry in configured {
        if entry.config.id == worker_id {
            continue; // the worker is already the preferred profile
        }
        profiles.push(profile_for(&entry.config));
    }

    let router = Router::new(
        RoleTable::new(),
        profiles,
        CircuitBreaker::new(Box::new(SystemClock::new())),
    );
    let decision = router.resolve(Role::Judge).ok()?;

    // Same provider as the worker → single-family/degraded: reuse the worker
    // provider directly, never build a duplicate.
    if decision.model_ref.provider == worker_id {
        return None;
    }

    // Build the concrete judge from the discovered credential for the chosen
    // provider. A missing entry or a build error falls back to the worker.
    let entry = configured
        .iter()
        .find(|c| c.config.id == decision.model_ref.provider)?;
    let judge = build_provider_parts(
        &entry.config,
        &decision.model_ref.model_id,
        entry.api_key.clone(),
        entry.config.base_url.to_string(),
        None,
    )
    .ok()?;
    Some((judge, decision.model_ref.provider))
}
