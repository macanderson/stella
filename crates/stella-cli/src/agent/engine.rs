//! Engine tuning and pipeline provider wiring.
//!
//! `EngineConfig` construction per agent kind, and the role->provider
//! resolution for pipeline runs. Every wiring failure here is soft: an
//! unroutable role rides the worker provider, so configuration can never
//! turn a runnable pipeline into an error.

use super::*;

/// EngineConfig for the session's agent: defaults + the workspace root as
/// hook `cwd`, with the agent's `agent_engine_config` tuning applied —
/// temperature and max_tokens override the engine defaults only when set (the
/// "Include" contract), effort/reasoning/params land verbatim (they default to
/// `None` anyway).
///
/// `catalog_ref` is the `(provider_id, model_id)` the catalog-based clamps
/// below (context-window compaction budget, reasoning capability) are
/// computed against — the model the calls actually land on, which is
/// `cfg.provider.id`/`cfg.model_id`.
///
/// It stays a parameter rather than being read off `cfg` because the two can
/// legitimately differ, and #276 is the record of what it costs when they are
/// conflated: a call routed to a smaller-context or non-reasoning model still
/// got the session model's clamps, which is the wire-shape/400 class #273
/// exists to warn about. The role that used to diverge here was the worker's;
/// the ones that will diverge again are plugin-declared seats (#3909), which
/// resolve their own model through [`crate::agent::seats`].
fn tuned_engine_config(cfg: &Config, catalog_ref: (&str, &str)) -> EngineConfig {
    let (provider_id, model_id) = catalog_ref;
    let mut engine = EngineConfig {
        cwd: cfg.workspace_root.display().to_string(),
        // Every role gets the same turn budget: the deadline it guards is the
        // process's, not one agent's, so a worker that declines a continuation
        // while a verifier spends the remaining time past it would defeat the
        // point. `None` unless `--turn-timeout` was given.
        turn_budget: cfg.turn_timeout,
        // Phase 2 (#713): the adaptive-context lifecycle switch, off by
        // default. Read here rather than at each receipt site so every engine
        // this session builds — default, worker, verifier — agrees on it.
        lifecycle_enabled: crate::memory::session_lifecycle_enabled(&cfg.workspace_root),
        // Durability for every role of the SESSION'S OWN turn — the deck's
        // lead turn and its pipeline, worker and verifier alike. This is the
        // single place an engine is tuned in this crate, so attaching it here
        // is what stops a role being left out.
        //
        // It covers the roles that run one at a time, and deliberately not the
        // lanes that run *beside* the lead turn. `SessionDurability` is one
        // cell keyed on one session record, so a lane that inherits this sink
        // is a second writer of one resume point, not a second resume point:
        // its steps overwrite the lead's transcript and its terminal path
        // `discard`s the lead's point while the lead still needs it. Two doors
        // reach that hazard and each is closed at its own end — a dispatched
        // sub-agent by `stella_core::subagent` (which strips the sink it is
        // handed), and a deck sub-session by `subsession_engine_config_for`
        // (which the engine crate cannot see, because a sub-session is built
        // through `Engine::with_sleeper` rather than `run_sub_agent`).
        //
        // It does not reach the fleet at all: a fleet worker never binds this
        // handle, so `sink()` answers `None` for every worker turn and the
        // whole fan-out runs without step-level durability (#3232).
        //
        // `None` while the session has not bound its durable location — a
        // command with no turn to checkpoint, or a driver that has not yet
        // resolved its session record. That is not a silent downgrade: it is
        // what stops `persist_checkpoint` from serializing a whole transcript
        // per step only to discard it.
        checkpoint_sink: cfg.durability.sink(),
        // What this session already learned its providers will fund (#3307).
        // Attached here for the same reason `lifecycle_enabled` is read here —
        // this is the one place an engine is tuned in this crate, so every role
        // the session builds shares one view of the account paying for all of
        // them, and none is left re-learning a refused ceiling by paying its
        // own 402.
        //
        // Deliberately NOT stripped for sub-agents, candidates or sub-sessions,
        // which is the opposite of `checkpoint_sink` above. That handle is one
        // session record and a second writer corrupts it; this one is one
        // account balance, and a second reader is exactly the point.
        session_output_ceilings: Some(cfg.output_ceilings.clone()),
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
    // What the model itself can write, once the catalog has answered — kept
    // so the per-model override below can be clamped to it rather than
    // trusted to be sane. `None` means the catalog has no ceiling for this
    // row, which is the case the seed and provider discovery exist to make
    // rare (#1290).
    let mut model_ceiling: Option<u32> = None;
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
            model_ceiling = Some(ceiling);
        }
    }
    if let Some(settings) = &cfg.engine_settings {
        // The operator's deliberate per-model cap (`[models.output_caps]`),
        // between the model's own ceiling above and the per-agent tuning
        // below. That ordering is the whole design: the DEFAULT is always the
        // model's maximum, and spending less than it is something someone has
        // to ask for by name. Nothing here can make the default lower.
        //
        // Clamped to the model's ceiling rather than obeyed blindly. Asking
        // for more than the model can write is not a bigger answer, it is a
        // provider-side rejection on every step — and the operator who typed
        // an extra digit meant "as much as possible", which is what the
        // catalog number already is. Clamping grants that; forwarding it
        // breaks the run. When the ceiling is unknown the entry is honored
        // as written: there is nothing to clamp against, and refusing it
        // would leave the engine's global default in place, which is lower
        // than anything the operator would have been typing here.
        if let Some(cap) = settings.model_output_cap(provider_id, model_id) {
            engine.max_output_tokens = Some(match model_ceiling {
                Some(ceiling) => cap.min(ceiling),
                None => cap,
            });
        }
        let tuning = crate::engine_config::tuning_for(settings);
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
        // the process, not to one agent. A verifier left on the default while the
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
    // `--max-output-tokens`, last and therefore highest (#1290). It outranks
    // both settings surfaces because it is the most specific thing anyone can
    // say — this invocation, right now — and because it is what an operator
    // reaches for when a CONFIGURED value is the thing going wrong. A flag
    // that a settings file could quietly beat would be useless in exactly
    // that moment.
    //
    // Applies to every role for the same reason the turn budget does: what it
    // bounds is the process's spend, not one agent's. Capping the worker while
    // a verifier kept the model's full ceiling would move the cost rather than
    // reduce it.
    //
    // Clamped to the model's own ceiling, like the per-model setting. The flag
    // is the likeliest of the three to carry a fat-fingered digit — it is
    // typed fresh each run, with no file to review it — and over-asking is not
    // a longer answer, it is a rejection on every step that bills the round
    // trip and needs another call to retry.
    if let Some(cap) = cfg.max_output_tokens {
        engine.max_output_tokens = Some(match model_ceiling {
            Some(ceiling) => cap.min(ceiling),
            None => cap,
        });
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
    tuned_engine_config(cfg, (cfg.provider.id, &cfg.model_id))
}

/// [`engine_config_for`] for a deck sub-session (`crate::subsession`) — the
/// session's own tuning, with the checkpoint sink re-keyed to `durability`
/// instead of the lead's `cfg.durability`.
///
/// A sub-session is a real engine session dispatched *because* the lead is
/// mid-turn, so the two run at once. Both are built from the same `Config`, and
/// [`crate::durability::SessionDurability`] is an `Arc` cell keyed on one
/// session record — so inheriting `cfg.durability` (what `engine_config_for`
/// attaches) would make the lane a second writer of the lead's one resume
/// point, not a second resume point: every lane step would overwrite the
/// lead's transcript and the lane's terminal path would `discard` the lead's
/// point while the lead still needs it. `stella_core::subagent` names the same
/// damage for a dispatched child reached through `run_sub_agent`; a
/// sub-session reaches it through a different door — it is a full engine
/// session built via `Engine::with_sleeper`, not a child, so the engine crate
/// never sees a parent to strip the sink from.
///
/// The fix here is to re-key, not strip: `durability` is the lane's OWN
/// handle, bound by the caller (`crate::subsession::run_worker`) to its own
/// journal key (`{session}/{lane}`), so its steps checkpoint into their own
/// ref namespace and can never collide with the lead's, or with a sibling
/// lane's (#3233).
pub(crate) fn subsession_engine_config_for(
    cfg: &Config,
    durability: &crate::durability::SessionDurability,
) -> EngineConfig {
    EngineConfig {
        checkpoint_sink: durability.sink(),
        ..engine_config_for(cfg)
    }
}

/// [`engine_config_for`], with the prove-it completion gate (#2663) switched
/// by the turn's execution kind: a headless `"run"` turn is graded on its
/// result and must prove a mutation before its declaration stands; a
/// `"chat"` turn is conversational and keeps the ungated shape. Keyed on the
/// same `kind` string `begin_execution` records, so the gate and the
/// telemetry cannot disagree about which mode a turn ran in.
///
/// The only caller is `stella run`'s raw one-shot path (`agent.rs`), so
/// #3381's default flip (every door raw by default) widened this gate's
/// *population* — a plain `stella run` with no flags now reaches it, not only
/// an explicit `--no-pipeline` — without changing the gate's own logic.
/// `goal`'s and `fleet`'s raw rounds build their `EngineConfig` from
/// [`engine_config_for`] directly (`agent/goal.rs`, `fleet_cmd.rs`) and never
/// call this function at all, on either side of the flip — the `"goal"`/
/// `"fleet"` `kind` strings `begin_execution` records for those doors would
/// fail the `== "run"` match here even if they did, so extending the gate to
/// them is a distinct, undone change, not a consequence of this one.
pub(crate) fn engine_config_for_kind(cfg: &Config, kind: &str) -> EngineConfig {
    let mut engine = engine_config_for(cfg);
    engine.completion_gate = kind == "run";
    engine
}

/// Fire `SessionStart` hooks once and return their stdout — the additional
/// session context `stella_core::hooks` documents. `None` when no hooks are
/// configured or they printed nothing. Called once per session by each
/// driver, never per turn.
///
/// This is the single owner of `SessionStart` firing (#2674). The engine
/// deliberately has no method for it: every driver fires hooks here, while
/// assembling the system prompt — before any `Engine` exists — and the
/// diagnostics below must reach stderr, which the no-I/O engine cannot do.
/// `stella-parity`'s `hooks.lifecycle` row pins that no second owner grows
/// back.
pub(crate) async fn session_start_hook_context(cfg: &Config) -> Option<String> {
    let hooks = cfg.hooks.as_ref()?;
    let outcome = stella_core::hooks::run_hooks(
        &HostHookRunner,
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
        system_prompt.push_str(SESSION_HOOK_CONTEXT_HEADER);
        system_prompt.push_str(&clamp_hook_context(&context));
    }
    system_prompt
}

/// The hook-context section's opening bytes, named for the reason
/// [`crate::agent::prompt::SESSION_ENVIRONMENT_HEADER`] is: it is the last
/// marker `crate::agent::prompt::provenance` splits a reconstructed prompt on,
/// and a reworded heading here would relabel a span in every inspector.
pub(crate) const SESSION_HOOK_CONTEXT_HEADER: &str =
    "\n\nSession context (from SessionStart hooks):\n";

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
    /// The model `Role::Worker`/`Role::Plan` actually resolve to: the
    /// worker's own `pipeline_worker_model`/`agents.worker.*` pin when one
    /// is configured and its provider is credentialed (issue #276), else
    /// the session default this wiring was built with. Callers building the
    /// worker's own [`EngineConfig`] (catalog-based context-window and
    /// reasoning-capability clamps) must key off THIS, not `cfg` directly.
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
        entry.aux.clone(),
        // Routed management roles fire in bursts within a turn — their
        // inter-call gaps are seconds, so the 5-minute cache window is the
        // right ask whatever surface the session runs on (#1839).
        stella_model::CacheTtl::default(),
    ) {
        Ok(provider) => {
            for &role in roles {
                wiring.pins.pin(role, pinned.clone());
            }
            // A profile for the routed provider keeps the router's provider
            // list honest (breaker bookkeeping, `providers()` introspection,
            // and — critically — the router's own unpinned-verifier cross-
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

/// Resolve the engine wiring for a run whose session-default model is
/// `worker_ref` (already resolved by `Config` — an explicit `--model` flag
/// beats `default_model`/`agents.default.*` there, see
/// `Config::load_with_settings`; it beats the settings spec here too, via
/// `cfg.model_pinned_by_flag`). `configured` is the caller's own
/// [`crate::config::discover_configured_providers`] snapshot — injected rather
/// than rediscovered here so this function is a plain, testable one over owned
/// data.
///
/// Routing rules:
/// - The session role honors `default_model`/`agents.default.*`
///   ([`crate::engine_config::model_spec_for`]) when configured and its
///   provider is credentialed; unset or unroutable falls back to
///   `worker_ref` (issue #276). An explicit `--model`
///   (`cfg.model_pinned_by_flag`) suppresses that settings override
///   entirely — the flag exists to pin the model for one invocation, so it
///   outranks the config file.
/// - A pin equal to the session-default model needs no extra adapter — the
///   primary resolver entry already serves it.
///
/// It resolved four more roles until #3908 — verifier, triage, research and
/// plan, each from its own `pipeline_<role>_model` key, each pinned into the
/// [`RoleTable`]. Those pins were **read by nothing**: the only live
/// `Router::resolve` call sites are `SessionFallback::resolve_fallback`
/// (`Role::Worker` alone) and [`resolve_cross_family_verifier`], and the
/// latter builds its router with `RoleTable::new()`. So an operator could
/// set `pipeline_verifier_model` and watch it resolve, log, and steer
/// nothing — including at the one place a second model actually runs. The
/// keys are retired (`settings::unknown`) rather than dropped in silence, and
/// a model for a participant other than the session's own is now a
/// plugin-declared seat ([`crate::agent::seats`]).
///
/// The `Role::Worker` pin below survives because it is the one that was ever
/// read. Pins deliberately bypass the circuit breaker (`RoleTable`
/// semantics — an explicit pin wins unconditionally).
pub(crate) fn resolve_engine_wiring(
    cfg: &Config,
    worker_ref: &ModelRef,
    configured: &[crate::config::ConfiguredProvider],
) -> EngineWiring {
    use crate::engine_config::model_spec_for;

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
        worker_model: worker_ref.clone(),
        notices: Vec::new(),
    };
    let Some(engine) = cfg.engine_settings.clone() else {
        // No engine settings at all is the stock posture: the session rides
        // the resolved default.
        return wiring;
    };

    // Credentialed providers only — a model spec naming a provider without
    // a resolvable key is reported and skipped, never a hard error.
    let is_provider = |id: &str| configured.iter().any(|c| c.config.id == id);

    // Issue #276: the session's own settings override, which the capability
    // clamp downstream needs to be computed against.
    //
    // ...unless the invocation carries an explicit `--model`. That flag's
    // documented job IS pinning the model for one run, so settings must lose
    // to it — otherwise the pin is not merely ignored but unobservable: the
    // run reports the model that was asked for while a different one does the
    // work and bills for it.
    let worker_spec = match model_spec_for(&engine, &is_provider) {
        Some(spec) if cfg.model_pinned_by_flag => {
            // An empty slug is the "provider pin without a model" form.
            let configured_label = if spec.model.is_empty() {
                spec.provider.clone()
            } else {
                format!("{}/{}", spec.provider, spec.model)
            };
            wiring.notices.push(format!(
                "engine config: model `{configured_label}` skipped — `--model` pinned \
                 `{worker_ref}` for this invocation"
            ));
            None
        }
        other => other,
    };
    // `Role::Worker` alone. It was pinned alongside `Role::Plan` and
    // `Role::Research` so those would follow an overridden worker rather than
    // reverting to the session default; both of those roles are gone (#3908),
    // and `Role::Worker` is the one `SessionFallback::resolve_fallback`
    // actually resolves.
    let effective_worker_ref = match &worker_spec {
        Some(spec) => pin_role(
            &mut wiring,
            &[Role::Worker],
            "model",
            spec,
            worker_ref,
            configured,
            &format!("rides the session default (`{worker_ref}`)"),
        )
        .unwrap_or_else(|| worker_ref.clone()),
        None => worker_ref.clone(),
    };
    wiring.worker_model = effective_worker_ref;
    wiring
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
        cfg.aux_credentials.clone(),
        cfg.effective_cache_ttl(),
    )
}

/// The per-dialect provider factory, over already-resolved parts rather than
/// a whole [`Config`]. Both the worker path ([`build_provider`]) and the
/// goal loop's routed verifier ([`resolve_cross_family_verifier`]) go through this
/// one match, so the wire-dialect selection — and the anti-phantom-slug
/// catalog check — live in exactly one place. `effective_base_url` is the
/// base URL requests go to (override-or-default); `base_url_override` is the
/// raw `--base-url`, which only the Vertex/Bedrock arms consume (they build
/// region/project-scoped URLs themselves). `aux` carries whatever the provider
/// needs beyond one key — Bedrock's AWS secret access key, optional session
/// token, and region — already resolved through the credential chain by
/// `config::aux_credentials::provider_aux`; empty for every other provider.
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
pub(crate) fn build_provider_parts(
    provider_config: &crate::config::ProviderConfig,
    model_id: &str,
    api_key: ApiKey,
    effective_base_url: String,
    base_url_override: Option<&str>,
    aux: stella_model::AuxCredentials,
    cache_ttl: stella_model::CacheTtl,
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
        aux,
        cache_ttl,
    ))
    .map_err(|error| error.to_string())
}

/// Cross-family grouping key for verifier selection. Same-vendor providers must
/// count as the SAME family so a routed verifier is genuinely a different model,
/// not the same weights behind a second endpoint: a Gemini verifier assessing
/// Gemini-via-Vertex work carries the same bias, as does an Anthropic Claude
/// verifier over Bedrock Claude. Anything without a known sibling is its own
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

/// Resolve one seat's model, and build the adapter that serves it — the
/// session's whole answer to "does this role run on a model of its own?".
///
/// Builds a role [`Router`] whose most-preferred provider is the active worker
/// (`worker_id`/`worker_model`, so the `--model` pin is honored) followed by
/// every OTHER configured provider, then resolves `role` through it. What that
/// buys depends on the role, and the difference is the router's to make, not
/// this function's: `Role::Verifier` prefers a healthy provider whose family
/// differs from the worker's (`Router::resolve_verifier`), while the tier roles
/// take the most-preferred available provider's matching tier.
///
/// - The router lands back on the worker's own provider → `None`, and no second
///   adapter is built. This is the single-family case for the verifier and the
///   ordinary case for every tier role today, because [`profile_for`] points a
///   provider's three tiers at one `default_model`. A seat that cannot diverge
///   says so by returning `None` rather than by building a duplicate adapter.
/// - A distinct provider is selected → its concrete adapter and id.
///
/// Returns `None` on ANY failure — degradation to the worker, a resolve error,
/// an unknown provider, or an adapter build failure — so seat routing can never
/// break the loop that asked for it. Every caller's `None` arm is "use the
/// worker's provider", which is what the session did before seats existed.
pub(crate) fn resolve_seat_provider(
    role: Role,
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
    let decision = router.resolve(role).ok()?;

    // Same provider as the worker → single-family/degraded: reuse the worker
    // provider directly, never build a duplicate.
    if decision.model_ref.provider == worker_id {
        return None;
    }

    // Build the concrete adapter from the discovered credential for the chosen
    // provider. A missing entry or a build error falls back to the worker.
    let entry = configured
        .iter()
        .find(|c| c.config.id == decision.model_ref.provider)?;
    let seat = build_provider_parts(
        &entry.config,
        &decision.model_ref.model_id,
        entry.api_key.clone(),
        entry.config.base_url.to_string(),
        None,
        entry.aux.clone(),
        // A routed seat's calls land in bursts within a run; the 5-minute
        // window is the right ask regardless of surface (#1839).
        stella_model::CacheTtl::default(),
    )
    .ok()?;
    Some((seat, decision.model_ref.provider))
}

/// Resolve the VERIFIER seat for the goal loop.
///
/// The named entry point the goal loop calls, kept as its own function rather
/// than open-coding [`resolve_seat_provider`]'s `Role::Verifier` at the call
/// site: the goal loop prints an operator-facing notice about *cross-family
/// verification specifically*, so the role it asks for is part of that
/// surface's contract rather than an argument a later edit could quietly
/// retune.
pub(crate) fn resolve_cross_family_verifier(
    worker_id: &str,
    worker_model: &str,
    configured: &[crate::config::ConfiguredProvider],
) -> Option<(Box<dyn Provider>, String)> {
    resolve_seat_provider(Role::Verifier, worker_id, worker_model, configured)
}

/// The session-scoped role [`Router`] for a bare (non-pipeline) loop — the
/// same wiring the pipeline paths build per run, held for the whole session
/// so its breaker accumulates the observed outcome of every turn's model
/// calls (#2673). The bare loops feed this router every turn and consult it
/// when a retry ladder exhausts ([`SessionFallback`], #2679); their primary
/// provider is still fixed at session start, which is why wiring notices are
/// deliberately not surfaced here — the start-of-turn routing decisions they
/// describe are not operative on this path yet.
pub(crate) fn session_router(cfg: &Config, worker_ref: &ModelRef) -> Router {
    let configured = crate::config::discover_configured_providers();
    let wiring = resolve_engine_wiring(cfg, worker_ref, &configured);
    Router::new(
        wiring.pins,
        wiring.profiles,
        CircuitBreaker::new(Box::new(SystemClock::new())),
    )
}

/// Mid-turn fallback resolution for the bare (non-pipeline) loops (#2679):
/// when the worker's retry ladder exhausts, the engine asks this port for a
/// replacement, and the answer is a fresh [`Router::resolve`] of the worker
/// role — whose breaker the failing calls already fed via
/// `with_provider_outcomes`, so resolution routes around the sick provider.
/// The replacement adapter is built from the discovered credentials on
/// first use and owned here (set-once), so the engine can borrow it for the
/// rest of the turn. Every miss is soft, matching
/// [`resolve_cross_family_verifier`]: a resolve error, a resolution landing
/// back on the failed provider, an uncredentialed target, or an adapter
/// build failure all yield `None` — the turn then aborts exactly as it did
/// before this seam existed.
pub(crate) struct SessionFallback<'r> {
    router: &'r Router,
    built: std::sync::OnceLock<Box<dyn Provider>>,
}

impl<'r> SessionFallback<'r> {
    pub(crate) fn new(router: &'r Router) -> Self {
        Self {
            router,
            built: std::sync::OnceLock::new(),
        }
    }
}

impl stella_core::ports::FallbackResolver for SessionFallback<'_> {
    fn resolve_fallback(
        &self,
        failed_provider_id: &str,
    ) -> Option<stella_core::ports::ResolvedFallback<'_>> {
        let decision = self.router.resolve(Role::Worker).ok()?;
        if decision.model_ref.provider == failed_provider_id {
            return None;
        }
        let reason = match decision.fallback {
            Some(fallback) => fallback.reason,
            None => format!(
                "worker role re-resolved to `{}` after `{failed_provider_id}` exhausted its \
                 retries",
                decision.model_ref.provider
            ),
        };
        let configured = crate::config::discover_configured_providers();
        let entry = configured
            .iter()
            .find(|c| c.config.id == decision.model_ref.provider)?;
        let provider = build_provider_parts(
            &entry.config,
            &decision.model_ref.model_id,
            entry.api_key.clone(),
            entry.config.base_url.to_string(),
            None,
            entry.aux.clone(),
            // Same reasoning as the routed verifier above: burst-shaped
            // calls within a run want the 5-minute window (#1839).
            stella_model::CacheTtl::default(),
        )
        .ok()?;
        // The engine's own latch consumes at most one resolution per engine,
        // so the set-once slot cannot serve a stale adapter to a second,
        // different resolution.
        let slot = self.built.get_or_init(|| provider);
        Some(stella_core::ports::ResolvedFallback {
            provider: slot.as_ref(),
            reason,
        })
    }
}

/// Where post-turn reflection dispatches, and on what posture.
///
/// [`Self::provider`] is `None` in the one case that is still a route: the
/// triage pin resolved to the model the worker adapter already serves. The
/// call rides that adapter, and the posture below still applies.
pub(crate) struct ReflectionRoute {
    /// The dedicated adapter for the triage-pinned model, when one was built.
    pub(crate) provider: Option<(ModelRef, Box<dyn Provider>)>,
    /// The triage agent's configured thinking posture.
    pub(crate) posture: crate::memory::ReflectionPosture,
}

/// Resolve where post-turn reflection dispatches (#1847): the configured
/// triage model when it is routable, else `None` — ride the worker.
///
/// Reflection ran on the WORKER model from the day it shipped, with a 2048
/// output-token allowance and no effort pin, while `crate::memory`'s own
/// module doc called it "one cheap model call". Nothing routed it to a cheap
/// tier. The triage pin (`pipeline_triage_model`/`agents.triage.*`) is
/// already the posture's declaration of "the cheap, fast model", so
/// reflection rides that declaration rather than adding a second cheap-model
/// knob the two settings could drift apart on.
///
/// Every miss is soft, matching [`resolve_cross_family_verifier`] and
/// `pin_role`: no engine settings, no triage spec, an uncredentialed
/// provider, or an adapter that will not build all yield `None`, and the
/// caller keeps dispatching on the worker exactly as every reflection call
/// always has. A triage spec that resolves to the session default model is
/// the one near-miss that is still a route: [`ReflectionRoute::provider`] is
/// `None` because the worker adapter already serves that exact model and a
/// second instance would buy only a second connection pool (the "no duplicate
/// adapter" rule the wiring's `pin_role` applies) — but the pin still chose
/// that model, so its posture still rides out.
///
/// Returns the triage agent's **posture** alongside the adapter, and returns
/// it even in the same-model case where no adapter is needed. Reflection runs
/// on the model this pin selected, so `agents.triage.reasoning` /
/// `agents.triage.effort` are the settings that govern the call — before
/// #2174 they chose the model and then could not reach it, so an operator who
/// switched thinking off still paid for a reasoning stream billed against
/// reflection's output cap.
pub(crate) fn reflection_route(
    cfg: &Config,
    configured: &[crate::config::ConfiguredProvider],
) -> Option<ReflectionRoute> {
    let engine = cfg.engine_settings.as_ref()?;
    let is_provider = |id: &str| configured.iter().any(|c| c.config.id == id);
    // The session's own model. It was the TRIAGE pin until #3908: reflection
    // is a short classification, so riding the cheapest configured role made
    // sense while the staged pipeline still had one. That role went with the
    // pipeline (#3865), and a reflection call routed through a key nothing
    // else read was routing through a setting that had stopped resolving.
    //
    // Reflection is a good candidate for a seat of its own — it is exactly the
    // "cheap, high-volume, latency-bound" shape a per-seat assignment exists
    // for — but core naming that seat itself would be core re-acquiring a role
    // vocabulary. It becomes a plugin-declared seat or it stays on the session
    // model; #3936 carries the choice.
    let spec = crate::engine_config::model_spec_for(engine, &is_provider)?;
    let entry = configured.iter().find(|c| c.config.id == spec.provider)?;
    // An empty slug is the "provider pin without a model" form — the
    // provider's own default model, exactly as `pin_role` reads it.
    let slug = if spec.model.is_empty() {
        entry.config.default_model.to_string()
    } else {
        spec.model
    };
    let routed = ModelRef::new(entry.config.id, slug.clone());
    // The agent's own tuning, resolved through the same helper the request
    // path uses so the auto modes apply identically. It rides out even when no
    // adapter is built below, because the posture is a fact about the pin, not
    // about whether the pin needed a second connection pool.
    let tuning = crate::engine_config::tuning_for(engine);
    let posture = crate::memory::ReflectionPosture {
        reasoning: tuning.reasoning,
        effort: tuning.effort,
    };
    if routed == ModelRef::new(cfg.provider.id, cfg.model_id.clone()) {
        // Same instance the primary resolver already serves: no new adapter
        // (the "no duplicate adapter" rule `pin_role` applies), but this is
        // still the model the settings chose, so its posture still governs.
        return Some(ReflectionRoute {
            provider: None,
            posture,
        });
    }
    let provider = build_provider_parts(
        &entry.config,
        &slug,
        entry.api_key.clone(),
        entry.config.base_url.to_string(),
        None,
        entry.aux.clone(),
        // One post-turn call per reflected turn: bursty within a session,
        // so the 5-minute cache window is the right ask (#1839).
        stella_model::CacheTtl::default(),
    )
    .ok()?;
    Some(ReflectionRoute {
        provider: Some((routed, provider)),
        posture,
    })
}
