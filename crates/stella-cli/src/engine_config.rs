//! `agent_engine_config` resolution — turns the declarative settings
//! object ([`crate::settings::AgentEngineConfig`]) into concrete per-agent
//! tuning: which provider/model each engine agent runs on, its effective
//! prompt/effort/reasoning, and the request parameter overrides.
//!
//! Three consumers:
//! - `agent.rs` builds role routers and engine configs from
//!   [`AgentTuning`] / [`ModelSpec`] when a run starts;
//! - `config.rs` consults [`AgentEngineConfig::model_for`] (via a combined
//!   spec string) for the default agent's model precedence;
//! - `command_deck.rs` maps settings ↔ the TUI's `EngineConfigState`
//!   snapshot for the deck's engine panel.
//!
//! Everything here is pure functions over owned data (no I/O), so the
//! precedence and auto-mode rules are unit-testable without a workspace.

use stella_protocol::{GenerationParams, ReasoningEffort, ServiceTier, Verbosity};
use stella_tui::{EngineAgentState, EngineConfigState, EngineRole};

use crate::settings::{
    AgentEngineAgent, AgentEngineAgents, AgentEngineConfig, AgentEngineParams, Toggle,
};

// Model specs

/// A parsed model setting: which provider serves it (when known) and the
/// provider-native slug sent verbatim on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    /// Provider id (`"anthropic"`, `"openrouter"`, a settings-defined id).
    pub provider: String,
    /// Provider-native model slug (`"claude-fable-5"`, `"openai/gpt-5.5"`).
    pub model: String,
}

/// Parse one model string into a [`ModelSpec`], `--model` semantics:
/// `provider/slug` splits on the FIRST `/` when the prefix names a known
/// provider (so `openrouter/openai/gpt-5.5` routes `openai/gpt-5.5`
/// through OpenRouter); a bare slug resolves through the seed catalog
/// (`glm-5.2` → `zai`). `None` when neither matches — the caller decides
/// whether that's a loud error (an explicit setting) or a skip (an
/// `allowed_models` candidate).
pub fn parse_model_spec(raw: &str, is_provider: &dyn Fn(&str) -> bool) -> Option<ModelSpec> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Some((prefix, rest)) = raw.split_once('/')
        && !rest.is_empty()
        && is_provider(prefix)
    {
        return Some(ModelSpec {
            provider: prefix.to_string(),
            model: rest.to_string(),
        });
    }
    stella_model::catalog::Catalog::current()
        .resolve(raw)
        .ok()
        .map(|entry| ModelSpec {
            provider: entry.provider.to_string(),
            model: raw.to_string(),
        })
}

/// The session's effective [`ModelSpec`], honoring the agent's explicit
/// `provider` field: when set, the model string (from the same precedence
/// chain as [`AgentEngineConfig::model_for`]) is the verbatim slug for
/// THAT provider — no prefix splitting, which is what lets an OpenRouter
/// entry carry a slug that itself contains `/`.
///
/// One exception guards the pin: `default_model` holds a `provider/slug`
/// spec — the TUI writes it that way on every save — and the same precedence
/// chain hands it to a pinned agent. For a SEEDED pin the catalog arbitrates
/// the two readings:
/// when it rejects the verbatim one and the string parses as a qualified
/// spec, the spec's own provider wins (a stale `zai` pin over
/// `openrouter/openrouter/auto` must route to OpenRouter, not become the
/// phantom slug `zai/openrouter/openrouter/auto`). Unseeded pins
/// (openrouter, local, settings-defined) keep verbatim semantics — their
/// slug space is open, so the catalog has no veto.
///
/// An unseeded pin still gets ONE shape arbitration the catalog can't
/// provide: a string that starts with the pin's own `provider/` prefix and
/// whose residue is itself vendor-namespaced is the qualified flat form,
/// not a verbatim wire slug — a pinned `openrouter` over
/// `openrouter/openrouter/auto` sends the wire slug `openrouter/auto`, not
/// the doubled form OpenRouter 400s on. A bare residue keeps verbatim
/// semantics (`openrouter/auto` under the same pin IS the wire slug —
/// stripping it would send the unserved `auto`).
pub fn model_spec_for(
    engine: &AgentEngineConfig,
    is_provider: &dyn Fn(&str) -> bool,
) -> Option<ModelSpec> {
    let pinned_provider = engine
        .agent()
        .and_then(|a| a.provider.as_deref())
        .filter(|p| !p.trim().is_empty());
    let model = engine.model_for();
    match (pinned_provider, model) {
        (Some(provider), Some(model)) => {
            let pin_seeded = crate::config::PROVIDERS
                .iter()
                .any(|p| p.id == provider && p.seeded);
            if pin_seeded
                && stella_model::catalog::Catalog::seed()
                    .resolve_for(provider, model)
                    .is_err()
                && let Some(spec) = parse_model_spec(model, is_provider)
            {
                return Some(spec);
            }
            // Unseeded shape arbitration (see the doc comment): the pin's
            // own `provider/` prefix over a still-namespaced residue is the
            // TUI's qualified flat form — strip exactly one prefix.
            if !pin_seeded
                && let Some(rest) = model
                    .strip_prefix(provider)
                    .and_then(|m| m.strip_prefix('/'))
                && rest.contains('/')
            {
                return Some(ModelSpec {
                    provider: provider.to_string(),
                    model: rest.to_string(),
                });
            }
            Some(ModelSpec {
                provider: provider.to_string(),
                model: model.to_string(),
            })
        }
        // A provider pin with no model: the provider's own default model
        // applies — signalled with an empty slug the wiring layer fills
        // from `ProviderConfig::default_model`.
        (Some(provider), None) => Some(ModelSpec {
            provider: provider.to_string(),
            model: String::new(),
        }),
        (None, Some(model)) => parse_model_spec(model, is_provider),
        (None, None) => None,
    }
}

// Per-agent tuning

/// The resolved request-shaping settings for one agent: what lands on the
/// engine config / completion requests once the auto modes are applied.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentTuning {
    /// Custom system prompt (replaces the built-in base; memories/rules
    /// still append).
    pub prompt: Option<String>,
    pub effort: Option<ReasoningEffort>,
    pub reasoning: Option<bool>,
    pub temperature: Option<f32>,
    pub max_output_tokens: Option<u32>,
    pub params: Option<GenerationParams>,
}

/// `effort_auto: on` — the effort nobody has to think about.
///
/// A constant rather than a table since #3908: the table it replaces mapped
/// six personas onto four rungs (the verifier deliberated hard, triage and
/// research sat at the bottom, the worker and planner balanced), and five of
/// those six were pins on the staged pipeline deleted in #3865. Core has one
/// role, and the rung the interactive session has always taken is the middle
/// one.
///
/// The per-participant version of this judgement is not lost, it moved: a
/// plugin declaring a research seat that should run cheap is describing its
/// own process, so the rung belongs beside the seat's model assignment rather
/// than in a core table of words core is not supposed to know (#3936 carries
/// the per-seat tuning that is not built yet).
const AUTO_EFFORT: ReasoningEffort = ReasoningEffort::Medium;

/// `reasoning_auto: on` — thinking on, for [`AUTO_EFFORT`]'s reason: the two
/// personas it used to be off for (triage's one-token classification,
/// research's read-only lookup) are not core's to name.
const AUTO_REASONING: bool = true;

/// Resolve the session agent's tuning from the merged config, applying the
/// auto modes (`effort_auto`/`reasoning_auto` override the per-agent settings
/// when on — that is their contract: "you never worry about it").
pub fn tuning_for(engine: &AgentEngineConfig) -> AgentTuning {
    let agent = engine.agent();
    let effort = if engine.effort_auto_on() {
        Some(AUTO_EFFORT)
    } else {
        agent.and_then(|a| a.effort)
    };
    let reasoning = if engine.reasoning_auto_on() {
        Some(AUTO_REASONING)
    } else {
        agent.and_then(|a| a.reasoning).map(Toggle::is_on)
    };
    let (temperature, max_output_tokens, params) = agent
        .and_then(|a| a.params.as_ref())
        .map(split_params)
        .unwrap_or((None, None, None));
    AgentTuning {
        prompt: agent.and_then(|a| a.prompt.clone()),
        effort,
        reasoning,
        temperature,
        max_output_tokens,
        params,
    }
}

/// Split the settings-level params into the request's direct fields
/// (`temperature`, `max_output_tokens`) and the [`GenerationParams`]
/// rider (`Some` only when at least one field is set, so an all-default
/// params object keeps the wire body byte-identical to no params at all).
fn split_params(
    params: &AgentEngineParams,
) -> (Option<f32>, Option<u32>, Option<GenerationParams>) {
    let rider = GenerationParams {
        top_p: params.top_p,
        top_k: params.top_k,
        frequency_penalty: params.frequency_penalty,
        presence_penalty: params.presence_penalty,
        repetition_penalty: params.repetition_penalty,
        seed: params.seed,
        verbosity: params.verbosity,
        service_tier: params.service_tier,
    };
    let rider = (rider != GenerationParams::default()).then_some(rider);
    (params.temperature, params.max_tokens, rider)
}

// Per-model effort vocabulary

/// What the catalog knows about `provider/model`'s reasoning support.
/// `None` when the model is unknown to the catalog OR known without
/// capability data — both degrade to "the provider is the authority".
pub fn model_supports_reasoning(provider: &str, model: &str) -> Option<bool> {
    stella_model::catalog::Catalog::current()
        .resolve_for(provider, model)
        .ok()
        .and_then(|entry| entry.supports_reasoning)
}

// Provider default posture

/// The OpenRouter default posture, as the settings object a user would have
/// had to write by hand to get it.
///
/// One key reaches every vendor, so an OpenRouter instance can afford a
/// stronger posture than a single-vendor one: the driver wants a long-context
/// agentic model thinking hard, and Kimi K3's slug costs the instance no extra
/// configuration. That is the point of it being a default rather than
/// documentation telling people to write it out.
///
/// It used to carry two more rows — Opus 5 judging and GLM 5.2 triaging, on
/// `pipeline_verifier_model` and `pipeline_triage_model`. Both went with #3908:
/// the roles they staffed were the staged pipeline's (#3865), and by the time
/// that crate left the workspace the keys steered nothing, so the baseline was
/// quietly promising an instance two models it never bought.
///
/// The cross-family *idea* survives at the one place a second model still
/// runs — `stella goal`'s `resolve_cross_family_verifier`, which groups with
/// `agent::engine::provider_family` and has never read these settings. What
/// went with the keys is the separate `spec_family`/`auto_verifier_spec` pair
/// that existed only to pre-commit that choice into a settings file, for a
/// role core no longer has.
///
/// Being a baseline is what keeps this honest: it composes UNDER the settings
/// scope chain via [`AgentEngineConfig::layered_over`], so any of it that a
/// user has an opinion about is theirs, field by field.
pub fn provider_engine_baseline(provider_id: &str) -> Option<AgentEngineConfig> {
    if provider_id != "openrouter" {
        return None;
    }
    Some(AgentEngineConfig {
        agents: Some(AgentEngineAgents {
            // Posture only, no `model`: the driver's model is already
            // answered by the provider row's `default_model`, and naming it
            // again here would outrank an explicit `--model`.
            default: Some(AgentEngineAgent {
                effort: Some(ReasoningEffort::Xhigh),
                reasoning: Some(Toggle::On),
                ..AgentEngineAgent::default()
            }),
        }),
        ..AgentEngineConfig::default()
    })
}

/// The engine settings a session actually runs on: the user's merged
/// `agent_engine_config` layered over whatever posture the resolved provider
/// brings ([`provider_engine_baseline`]).
///
/// Keyed on the provider that resolution actually PICKED, not on which
/// credentials happen to exist. An instance with an OpenRouter key that was
/// nonetheless pointed at `--model anthropic/…` is an Anthropic session, and
/// quietly routing its verifier through a different gateway — and a different
/// bill — would be the config surprising its user.
pub fn engine_with_provider_baseline(
    provider_id: &str,
    configured: &Option<AgentEngineConfig>,
) -> Option<AgentEngineConfig> {
    match (provider_engine_baseline(provider_id), configured) {
        (Some(baseline), Some(user)) => Some(user.layered_over(&baseline)),
        (Some(baseline), None) => Some(baseline),
        (None, configured) => configured.clone(),
    }
}

/// The effort levels that DO something for a model served by `provider_id`.
/// Two constraints intersect here:
/// - the model: a catalog-confirmed non-reasoning model has no levels at
///   all (sending one would either error or silently no-op);
/// - the provider's wire vocabulary: Anthropic/Bedrock budgets give all
///   five tiers distinct meaning; Gemini's `thinkingLevel` is low/high;
///   the OpenAI shapes document low/medium/high (higher tiers collapse to
///   `high` on the wire — offering them would promise a distinction the
///   request can't express); GLM pairs its on/off `thinking` switch with a
///   `reasoning_effort` of the same low/medium/high shape, so it takes the
///   OpenAI-compatible vocabulary like any other member of that dialect.
///
/// Unknown capability (`None`) keeps the provider's full vocabulary: an
/// unknown must never *restrict*, only a confirmed "no" may.
pub fn effort_levels(
    provider_id: &str,
    dialect: crate::config::Dialect,
    supports_reasoning: Option<bool>,
) -> &'static [&'static str] {
    use crate::config::Dialect;
    if supports_reasoning == Some(false) {
        return &[];
    }
    // OpenRouter is OpenAI-compatible on the wire but NOT in this vocabulary:
    // its `reasoning.effort` accepts the full ladder and normalizes each tier
    // onto the routed model, so offering `xhigh`/`max` here promises a
    // distinction the request can genuinely express (see
    // `zai::map_openrouter_effort`).
    if provider_id == "openrouter" {
        return &["low", "medium", "high", "xhigh", "max"];
    }
    match dialect {
        Dialect::Anthropic | Dialect::Bedrock => &["low", "medium", "high", "xhigh", "max"],
        Dialect::Gemini | Dialect::Vertex => &["low", "high"],
        Dialect::OpenaiResponses | Dialect::OpenaiCompatible => &["low", "medium", "high"],
    }
}

/// [`effort_levels`] for a `provider/slug` picker spec, resolving the
/// provider's dialect from the built-in table (settings-defined providers
/// default to the OpenAI-compatible vocabulary, matching their adapter).
pub fn effort_levels_for_spec(provider_id: &str, model: &str) -> &'static [&'static str] {
    let dialect = crate::config::PROVIDERS
        .iter()
        .find(|p| p.id == provider_id)
        .map(|p| p.dialect)
        .unwrap_or(crate::config::Dialect::OpenaiCompatible);
    effort_levels(
        provider_id,
        dialect,
        model_supports_reasoning(provider_id, model),
    )
}

/// The one-line honest-degradation notice for a reasoning effort the selected
/// provider's adapter cannot honor (issue #266), or `None` when the effort is
/// respected — or none is pinned.
///
/// Keyed on the USER's explicit pin, not the auto-resolved effort:
/// `effort_auto` synthesizes a per-agent effort nobody asked for, so firing on
/// the resolved value would spam a boot notice on every `Unsupported` provider
/// under auto mode. The caller passes the settings-pinned value, and the
/// notice only fires when the provider's [`ReasoningPosture`] is `Unsupported`
/// (the shared adapter drops the control) — a `Controllable` provider honors
/// the effort and stays silent.
///
/// [`ReasoningPosture`]: stella_model::provider_parity::ReasoningPosture
pub fn unsupported_effort_notice(
    provider_id: &str,
    provider_label: &str,
    pinned_effort: Option<ReasoningEffort>,
) -> Option<String> {
    use stella_model::provider_parity::{ReasoningPosture, reasoning_posture};
    let effort = pinned_effort?;
    match reasoning_posture(provider_id) {
        Some(ReasoningPosture::Unsupported { note }) => Some(format!(
            "reasoning effort '{}' is pinned, but {provider_label} has no request-level reasoning \
             control on this adapter — the pinned level is ignored ({note}).",
            effort_to_str(effort),
        )),
        _ => None,
    }
}

/// One line when a pinned effort IS accepted but not at the tier asked for, or
/// `None` when the pinned tier reaches the wire as itself (#1499).
///
/// The sibling of [`unsupported_effort_notice`], and the case it could not
/// see. That one fires when the adapter has no reasoning control at all; this
/// one fires when it has one and cannot express the requested depth. Four of
/// the eight `Controllable` providers map the finer tiers onto a coarser set
/// the routed model is guaranteed to accept — the right wire posture, because
/// sending a tier the model rejects is a hard 400, and the wrong thing to stay
/// silent about.
///
/// Worth a boot line rather than a doc note because effort was the single
/// largest measured variable in this repo's own paid arena runs: a run
/// configured at `max` and served `high` is not the run its label says it is,
/// and nothing else in the transcript would ever say so.
///
/// Keyed on the user's explicit pin for the same reason
/// [`unsupported_effort_notice`] is — `effort_auto` synthesizes a per-agent
/// effort nobody asked for, and a notice about a level the user never chose is
/// noise.
pub fn downgraded_effort_notice(
    provider_id: &str,
    provider_label: &str,
    pinned_effort: Option<ReasoningEffort>,
) -> Option<String> {
    let effort = pinned_effort?;
    let requested = effort_to_str(effort);
    let served = stella_model::provider_parity::downgraded_effort(provider_id, requested)?;
    Some(format!(
        "reasoning effort '{requested}' is pinned, but {provider_label}'s adapter cannot send \
         that tier — the request goes out at '{served}'. Pin '{served}' to say what you will \
         actually get, or choose a provider that carries '{requested}'.",
    ))
}

/// Every boot notice owed to a user whose pinned reasoning effort will not be
/// served as pinned.
///
/// Two disjoint postures, one line each and never both: the adapter has no
/// reasoning control ([`unsupported_effort_notice`] — the pin is dropped), or
/// it has one and cannot express that tier ([`downgraded_effort_notice`] — the
/// pin is served coarser). Collected here rather than at the call site because
/// `command_deck.rs` is a god file closed to growth, and because the next
/// posture that owes a notice should be added in the module that owns them.
#[must_use]
pub fn effort_notices(
    provider_id: &str,
    provider_label: &str,
    pinned_effort: Option<ReasoningEffort>,
) -> Vec<String> {
    [
        unsupported_effort_notice(provider_id, provider_label, pinned_effort),
        downgraded_effort_notice(provider_id, provider_label, pinned_effort),
    ]
    .into_iter()
    .flatten()
    .collect()
}

// Enum ↔ string (the TUI edits strings; settings/serde hold enums)

pub fn effort_to_str(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
        ReasoningEffort::Max => "max",
    }
}

pub fn effort_from_str(raw: &str) -> Option<ReasoningEffort> {
    match raw {
        "low" => Some(ReasoningEffort::Low),
        "medium" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        "xhigh" => Some(ReasoningEffort::Xhigh),
        "max" => Some(ReasoningEffort::Max),
        _ => None,
    }
}

pub fn verbosity_to_str(verbosity: Verbosity) -> &'static str {
    match verbosity {
        Verbosity::Low => "low",
        Verbosity::Medium => "medium",
        Verbosity::High => "high",
    }
}

pub fn verbosity_from_str(raw: &str) -> Option<Verbosity> {
    match raw {
        "low" => Some(Verbosity::Low),
        "medium" => Some(Verbosity::Medium),
        "high" => Some(Verbosity::High),
        _ => None,
    }
}

pub fn service_tier_to_str(tier: ServiceTier) -> &'static str {
    match tier {
        ServiceTier::Auto => "auto",
        ServiceTier::Default => "default",
        ServiceTier::Flex => "flex",
        ServiceTier::Priority => "priority",
    }
}

pub fn service_tier_from_str(raw: &str) -> Option<ServiceTier> {
    match raw {
        "auto" => Some(ServiceTier::Auto),
        "default" => Some(ServiceTier::Default),
        "flex" => Some(ServiceTier::Flex),
        "priority" => Some(ServiceTier::Priority),
        _ => None,
    }
}

// Settings ↔ TUI snapshot

/// Build the engine panel's snapshot from the merged settings plus the
/// picker vocabularies the driver knows (provider ids, catalog slugs, and
/// each slug's per-model effort vocabulary).
///
/// `roles` is the resolved per-role wiring the `/models` dialog prints
/// ([`crate::config_wiring::deck_rows`]) — carried on the same snapshot
/// because the deck already holds one from startup, so the dialog renders on
/// the frame it opens rather than after a round-trip.
pub fn state_from_settings(
    engine: &AgentEngineConfig,
    providers: Vec<String>,
    catalog_models: Vec<String>,
    model_efforts: std::collections::HashMap<String, Vec<String>>,
    roles: Vec<stella_tui::envelope::RoleWiringRow>,
) -> EngineConfigState {
    let agents = EngineRole::ALL
        .iter()
        .map(|role| {
            // One role, so one arm. The `match` survives the collapse
            // deliberately: slice 5 (#3909) re-populates this from the live
            // session's plugin seats, and a `match` is where each new row's
            // source lands.
            let agent = match role {
                EngineRole::Default => engine.agent(),
            };
            let params = agent.and_then(|a| a.params.as_ref());
            EngineAgentState {
                // No fallback beyond `default_model` — the snapshot shows what
                // this row SETS, not what it inherits.
                model: agent
                    .and_then(|a| a.model.clone())
                    .or_else(|| engine.default_model.clone()),
                provider: agent.and_then(|a| a.provider.clone()),
                prompt: agent.and_then(|a| a.prompt.clone()),
                effort: agent
                    .and_then(|a| a.effort)
                    .map(|e| effort_to_str(e).to_string()),
                reasoning: agent.and_then(|a| a.reasoning).map(Toggle::is_on),
                temperature: params.and_then(|p| p.temperature),
                top_p: params.and_then(|p| p.top_p),
                top_k: params.and_then(|p| p.top_k),
                frequency_penalty: params.and_then(|p| p.frequency_penalty),
                presence_penalty: params.and_then(|p| p.presence_penalty),
                repetition_penalty: params.and_then(|p| p.repetition_penalty),
                max_tokens: params.and_then(|p| p.max_tokens),
                seed: params.and_then(|p| p.seed),
                verbosity: params
                    .and_then(|p| p.verbosity)
                    .map(|v| verbosity_to_str(v).to_string()),
                service_tier: params
                    .and_then(|p| p.service_tier)
                    .map(|t| service_tier_to_str(t).to_string()),
            }
        })
        .collect();
    EngineConfigState {
        auto_mode: engine.auto_mode_on(),
        effort_auto: engine.effort_auto_on(),
        reasoning_auto: engine.reasoning_auto_on(),
        allowed_models: engine.allowed_models().to_vec(),
        providers,
        catalog_models,
        model_efforts,
        agents,
        roles,
    }
}

/// Rebuild the settings object from an edited snapshot. The model lands on
/// the FLAT `default_model` key — the canonical simple form — with
/// `agents.default.model` cleared, so a TUI save never leaves the two sources
/// disagreeing. Empty agent objects and an empty allowed list are omitted
/// entirely (the file stays minimal).
pub fn settings_from_state(state: &EngineConfigState) -> AgentEngineConfig {
    let mut engine = AgentEngineConfig {
        auto_mode: Some(if state.auto_mode {
            Toggle::On
        } else {
            Toggle::Off
        }),
        effort_auto: Some(if state.effort_auto {
            Toggle::On
        } else {
            Toggle::Off
        }),
        reasoning_auto: Some(if state.reasoning_auto {
            Toggle::On
        } else {
            Toggle::Off
        }),
        allowed_models: (!state.allowed_models.is_empty()).then(|| state.allowed_models.clone()),
        ..AgentEngineConfig::default()
    };
    let mut agents = AgentEngineAgents::default();
    let mut any_agent = false;
    for (index, role) in EngineRole::ALL.iter().enumerate() {
        let Some(edited) = state.agents.get(index) else {
            continue;
        };
        let model = edited
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string);
        match role {
            EngineRole::Default => engine.default_model = model,
        }
        let params = AgentEngineParams {
            temperature: edited.temperature,
            top_p: edited.top_p,
            top_k: edited.top_k,
            frequency_penalty: edited.frequency_penalty,
            presence_penalty: edited.presence_penalty,
            repetition_penalty: edited.repetition_penalty,
            max_tokens: edited.max_tokens,
            seed: edited.seed,
            verbosity: edited.verbosity.as_deref().and_then(verbosity_from_str),
            service_tier: edited
                .service_tier
                .as_deref()
                .and_then(service_tier_from_str),
        };
        let agent = AgentEngineAgent {
            model: None,
            provider: edited
                .provider
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_string),
            prompt: edited.prompt.clone().filter(|p| !p.trim().is_empty()),
            effort: edited.effort.as_deref().and_then(effort_from_str),
            reasoning: edited
                .reasoning
                .map(|on| if on { Toggle::On } else { Toggle::Off }),
            params: (params != AgentEngineParams::default()).then_some(params),
        };
        if agent != AgentEngineAgent::default() {
            match role {
                EngineRole::Default => agents.default = Some(agent),
            }
            any_agent = true;
        }
    }
    engine.agents = any_agent.then_some(agents);
    engine
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_builtin(id: &str) -> bool {
        crate::config::PROVIDERS.iter().any(|p| p.id == id)
    }

    fn engine_from_json(json: &str) -> AgentEngineConfig {
        serde_json::from_str(json).expect("valid engine config json")
    }

    /// The whole point of the baseline is that it resolves through the SAME
    /// path a hand-written config takes — not to a shape that only looks right
    /// in the struct.
    ///
    /// It carried a verifier and a triage row until #3908, and this test
    /// asserted both routed through the gateway. Those rows are gone with the
    /// roles they staffed, so what is left to assert is the half that was
    /// always the subtle one: the baseline pins the driver's POSTURE and
    /// deliberately not its model, because a model here would outrank an
    /// explicit `--model`.
    #[test]
    fn the_openrouter_baseline_pins_posture_and_never_a_model() {
        let engine = provider_engine_baseline("openrouter").expect("openrouter has a baseline");

        let default = tuning_for(&engine);
        assert_eq!(default.effort, Some(ReasoningEffort::Xhigh));
        assert_eq!(default.reasoning, Some(true));
        assert_eq!(engine.model_for(), None);
        assert!(model_spec_for(&engine, &is_builtin).is_none());
    }

    #[test]
    fn no_other_provider_carries_a_baseline() {
        for provider in crate::config::PROVIDERS
            .iter()
            .filter(|p| p.id != "openrouter")
        {
            assert!(
                provider_engine_baseline(provider.id).is_none(),
                "{} unexpectedly has a default posture",
                provider.id
            );
        }
    }

    /// A baseline that could displace a configured value would not be a
    /// default. Each user field must win while what they never mentioned
    /// survives.
    #[test]
    fn configured_settings_beat_the_baseline_field_by_field() {
        let user = engine_from_json(
            r#"{"headless_scope_bypass": "on",
                "agents": {"default": {"effort": "low"}}}"#,
        );
        let merged =
            engine_with_provider_baseline("openrouter", &Some(user)).expect("baseline applies");

        // The user's effort wins over the baseline's xhigh...
        let default = tuning_for(&merged);
        assert_eq!(default.effort, Some(ReasoningEffort::Low));
        // ...the baseline still answers where they said nothing...
        assert_eq!(default.reasoning, Some(true));
        // ...and the field `overlay` used to drop survives the extra layer.
        assert!(merged.headless_scope_bypass_on());
    }

    /// Keyed on the provider that resolution PICKED. An OpenRouter key in the
    /// environment must not quietly re-route an Anthropic session's verifier
    /// through a different gateway — and a different bill.
    #[test]
    fn a_non_openrouter_session_is_left_exactly_as_configured() {
        let user = engine_from_json(r#"{"default_model": "anthropic/claude-fable-5"}"#);
        let merged = engine_with_provider_baseline("anthropic", &Some(user.clone()));
        assert_eq!(merged, Some(user));
        assert_eq!(engine_with_provider_baseline("anthropic", &None), None);
    }

    /// OpenRouter accepts the full effort ladder and normalizes it onto the
    /// routed model, so the picker must offer the tier the default pins.
    #[test]
    fn openrouter_offers_the_effort_tier_its_baseline_pins() {
        let levels = effort_levels_for_spec("openrouter", "moonshotai/kimi-k3");
        assert!(levels.contains(&"xhigh"), "got {levels:?}");
        assert!(levels.contains(&"max"), "got {levels:?}");
        // The direct OpenAI shapes still stop at high — only the gateway
        // normalizes the top tiers.
        assert_eq!(
            effort_levels_for_spec("openai", "gpt-5.5"),
            ["low", "medium", "high"]
        );
    }

    #[test]
    fn parse_model_spec_splits_provider_prefixes_and_resolves_bare_slugs() {
        // provider/slug — including a routed slug that itself contains `/`.
        let spec = parse_model_spec("openrouter/openai/gpt-5.5", &is_builtin).unwrap();
        assert_eq!(spec.provider, "openrouter");
        assert_eq!(spec.model, "openai/gpt-5.5");
        // A bare seeded slug resolves through the catalog.
        let spec = parse_model_spec("glm-5.2", &is_builtin).unwrap();
        assert_eq!(spec.provider, "zai");
        // Unknown either way → None.
        assert!(parse_model_spec("no-such-model", &is_builtin).is_none());
        assert!(parse_model_spec("", &is_builtin).is_none());
    }

    #[test]
    fn model_spec_honors_the_explicit_provider_pin_verbatim() {
        let engine = engine_from_json(
            r#"{"agents": {"default": {"provider": "openrouter", "model": "openai/gpt-5.5"}}}"#,
        );
        let spec = model_spec_for(&engine, &is_builtin).unwrap();
        // No prefix splitting: the slug goes to the pinned provider whole.
        assert_eq!(spec.provider, "openrouter");
        assert_eq!(spec.model, "openai/gpt-5.5");

        // A provider pin with no model → empty slug (provider default).
        let engine = engine_from_json(r#"{"agents": {"default": {"provider": "zai"}}}"#);
        let spec = model_spec_for(&engine, &is_builtin).unwrap();
        assert_eq!(spec.provider, "zai");
        assert!(spec.model.is_empty());
    }

    #[test]
    fn seeded_pin_yields_to_a_qualified_spec_the_catalog_rejects() {
        // `default_model` holds a `provider/slug` spec (the TUI writes it
        // that way), and the precedence chain hands it to a pinned agent
        // too. A stale seeded pin must not swallow one into a phantom slug:
        // `zai` + `openrouter/openrouter/auto` routed verbatim produced the
        // unknown-model error `zai/openrouter/openrouter/auto`.
        let engine = engine_from_json(
            r#"{"default_model": "openrouter/openrouter/auto",
                "agents": {"default": {"provider": "zai"}}}"#,
        );
        let spec = model_spec_for(&engine, &is_builtin).unwrap();
        assert_eq!(spec.provider, "openrouter");
        assert_eq!(spec.model, "openrouter/auto");

        // The TUI-save shape — pin plus a flat spec naming the SAME seeded
        // provider — resolves to the catalog slug, not `provider/provider/…`.
        let engine = engine_from_json(
            r#"{"default_model": "anthropic/claude-fable-5",
                "agents": {"default": {"provider": "anthropic"}}}"#,
        );
        let spec = model_spec_for(&engine, &is_builtin).unwrap();
        assert_eq!(spec.provider, "anthropic");
        assert_eq!(spec.model, "claude-fable-5");

        // A seeded pin with a catalog-valid slug is untouched.
        let engine = engine_from_json(
            r#"{"default_model": "glm-5.2", "agents": {"default": {"provider": "zai"}}}"#,
        );
        let spec = model_spec_for(&engine, &is_builtin).unwrap();
        assert_eq!(spec.provider, "zai");
        assert_eq!(spec.model, "glm-5.2");

        // An UNSEEDED pin keeps verbatim semantics even for a flat-key
        // model whose prefix names another provider — that slug shape is
        // exactly how OpenRouter routes (`openai/gpt-5.5` IS its slug).
        let engine = engine_from_json(
            r#"{"default_model": "openai/gpt-5.5",
                "agents": {"default": {"provider": "openrouter"}}}"#,
        );
        let spec = model_spec_for(&engine, &is_builtin).unwrap();
        assert_eq!(spec.provider, "openrouter");
        assert_eq!(spec.model, "openai/gpt-5.5");
    }

    #[test]
    fn unseeded_pin_strips_its_own_qualified_prefix_but_keeps_verbatim_wire_slugs() {
        // The TUI-save shape for an UNSEEDED provider: pin plus a flat
        // `provider/slug` spec naming the pin itself. Verbatim routing sent
        // the doubled `openrouter/openrouter/auto` to the wire, which
        // OpenRouter 400s on — the qualified reading must win.
        let engine = engine_from_json(
            r#"{"default_model": "openrouter/openrouter/auto",
                "agents": {"default": {"provider": "openrouter"}}}"#,
        );
        let spec = model_spec_for(&engine, &is_builtin).unwrap();
        assert_eq!(spec.provider, "openrouter");
        assert_eq!(spec.model, "openrouter/auto");

        // Same shape over a routed vendor slug (a synced-catalog picker row).
        let engine = engine_from_json(
            r#"{"default_model": "openrouter/anthropic/claude-sonnet-5",
                "agents": {"default": {"provider": "openrouter"}}}"#,
        );
        let spec = model_spec_for(&engine, &is_builtin).unwrap();
        assert_eq!(spec.provider, "openrouter");
        assert_eq!(spec.model, "anthropic/claude-sonnet-5");

        // But a BARE residue keeps verbatim semantics: `openrouter/auto` IS
        // the wire slug (OpenRouter's own vendor namespace) — stripping it
        // would send the unserved `auto`.
        let engine = engine_from_json(
            r#"{"default_model": "openrouter/auto",
                "agents": {"default": {"provider": "openrouter"}}}"#,
        );
        let spec = model_spec_for(&engine, &is_builtin).unwrap();
        assert_eq!(spec.provider, "openrouter");
        assert_eq!(spec.model, "openrouter/auto");
    }

    #[test]
    fn tuning_applies_auto_modes_over_per_agent_settings() {
        let engine = engine_from_json(
            r#"{"effort_auto": "on", "reasoning_auto": "on",
                "agents": {"default": {"effort": "max", "reasoning": "off",
                                       "prompt": "Think hard."}}}"#,
        );
        // Auto overrides the explicit settings — that is its whole contract,
        // and it is the reason `/profile` has to switch it off before pinning
        // an effort of its own.
        let tuning = tuning_for(&engine);
        assert_eq!(tuning.effort, Some(AUTO_EFFORT));
        assert_eq!(tuning.reasoning, Some(AUTO_REASONING));
        // Prompts are never auto-managed.
        assert_eq!(tuning.prompt.as_deref(), Some("Think hard."));

        // Autos off → the per-agent settings answer.
        let engine =
            engine_from_json(r#"{"agents": {"default": {"effort": "max", "reasoning": "off"}}}"#);
        let tuning = tuning_for(&engine);
        assert_eq!(tuning.effort, Some(ReasoningEffort::Max));
        assert_eq!(tuning.reasoning, Some(false));
        // And an untouched config has no opinions at all.
        assert_eq!(
            tuning_for(&AgentEngineConfig::default()),
            AgentTuning::default()
        );
    }

    #[test]
    fn tuning_splits_params_between_direct_fields_and_the_rider() {
        let engine = engine_from_json(
            r#"{"agents": {"default": {"params": {
                "temperature": 0.3, "max_tokens": 9000, "top_p": 0.9, "seed": 42
            }}}}"#,
        );
        let worker = tuning_for(&engine);
        assert_eq!(worker.temperature, Some(0.3));
        assert_eq!(worker.max_output_tokens, Some(9000));
        let rider = worker.params.expect("rider");
        assert_eq!(rider.top_p, Some(0.9));
        assert_eq!(rider.seed, Some(42));
        // temperature/max_tokens alone → no rider at all (byte-stable wire).
        let engine =
            engine_from_json(r#"{"agents": {"worker": {"params": {"temperature": 0.3}}}}"#);
        assert!(tuning_for(&engine).params.is_none());
    }

    #[test]
    fn effort_levels_intersect_model_capability_with_the_provider_vocabulary() {
        use crate::config::Dialect;
        // A confirmed non-reasoning model has no levels anywhere.
        assert!(effort_levels("anthropic", Dialect::Anthropic, Some(false)).is_empty());
        assert!(effort_levels("openrouter", Dialect::OpenaiCompatible, Some(false)).is_empty());
        assert!(effort_levels("zai", Dialect::OpenaiCompatible, Some(false)).is_empty());
        // GLM pairs its on/off `thinking` switch with a `reasoning_effort` of
        // the OpenAI-compatible low/medium/high shape (verified against
        // glm-5.2, 2026-08-04), so a reasoning GLM offers that vocabulary
        // rather than the empty set this used to return — an empty set here
        // made the wire-level effort support unreachable from config.
        assert_eq!(
            effort_levels("zai", Dialect::OpenaiCompatible, Some(true)),
            &["low", "medium", "high"]
        );
        // Provider wire vocabularies.
        assert_eq!(
            effort_levels("anthropic", Dialect::Anthropic, Some(true)),
            &["low", "medium", "high", "xhigh", "max"]
        );
        assert_eq!(
            effort_levels("gemini", Dialect::Gemini, Some(true)),
            &["low", "high"]
        );
        assert_eq!(
            effort_levels("openai", Dialect::OpenaiResponses, Some(true)),
            &["low", "medium", "high"]
        );
        // Unknown capability must not restrict — the provider's own full
        // vocabulary stays offered. For OpenRouter that vocabulary is the
        // whole ladder, not the OpenAI-compatible three: the gateway accepts
        // `xhigh`/`max` and normalizes them onto the routed model, so unlike
        // the direct OpenAI shapes above, offering them promises something
        // the request can actually express.
        assert_eq!(
            effort_levels("openrouter", Dialect::OpenaiCompatible, None),
            &["low", "medium", "high", "xhigh", "max"]
        );

        // The spec form resolves dialect + capability from the tables: the
        // seed marks deepseek-chat non-reasoning, claude-fable-5 reasoning.
        assert!(effort_levels_for_spec("deepseek", "deepseek-chat").is_empty());
        assert_eq!(
            effort_levels_for_spec("anthropic", "claude-fable-5"),
            &["low", "medium", "high", "xhigh", "max"]
        );
    }

    #[test]
    fn unsupported_effort_notice_fires_only_on_pinned_effort_against_unsupported() {
        // No pin → no notice, whatever the posture.
        assert!(unsupported_effort_notice("deepseek", "DeepSeek", None).is_none());
        // Controllable providers honor the effort → silent even when pinned.
        assert!(unsupported_effort_notice("xai", "xAI", Some(ReasoningEffort::High)).is_none());
        assert!(
            unsupported_effort_notice("openrouter", "OpenRouter", Some(ReasoningEffort::Max))
                .is_none()
        );
        // Unsupported provider + a user-pinned effort → one honest line naming
        // the provider and the (ignored) level.
        let notice = unsupported_effort_notice("deepseek", "DeepSeek", Some(ReasoningEffort::High))
            .expect("deepseek is Unsupported");
        assert!(notice.contains("DeepSeek"), "{notice}");
        assert!(notice.contains("high"), "{notice}");
        assert!(notice.contains("ignored"), "{notice}");
        // local is Unsupported too; bedrock became Controllable when the
        // Converse adapter gained the reasoning_config passthrough → silent.
        assert!(unsupported_effort_notice("local", "Local", Some(ReasoningEffort::Low)).is_some());
        assert!(
            unsupported_effort_notice("bedrock", "Bedrock", Some(ReasoningEffort::Medium))
                .is_none()
        );
        // An unknown provider id has no posture row → no notice (the seeded
        // completeness test guarantees real ids always have one).
        assert!(
            unsupported_effort_notice("no-such", "Nope", Some(ReasoningEffort::High)).is_none()
        );
    }

    #[test]
    fn downgraded_effort_notice_fires_only_when_the_tier_is_really_coarsened() {
        // No pin → nothing to be downgraded.
        assert!(downgraded_effort_notice("openai", "OpenAI", None).is_none());

        // The #1499 case: OpenAI folds xhigh/max into "high". A user who
        // pinned max was told nothing and served high.
        let notice = downgraded_effort_notice("openai", "OpenAI", Some(ReasoningEffort::Max))
            .expect("openai collapses max");
        assert!(notice.contains("OpenAI"), "{notice}");
        assert!(notice.contains("max"), "{notice}");
        assert!(notice.contains("high"), "{notice}");

        // Gemini is coarser still — it loses medium too.
        assert!(
            downgraded_effort_notice("gemini", "Gemini", Some(ReasoningEffort::Medium)).is_some()
        );

        // Tiers that survive the mapping stay silent, on the same providers.
        assert!(
            downgraded_effort_notice("openai", "OpenAI", Some(ReasoningEffort::Medium)).is_none()
        );
        assert!(downgraded_effort_notice("gemini", "Gemini", Some(ReasoningEffort::Low)).is_none());

        // Anthropic maps all five tiers onto distinct thinking budgets, and
        // OpenRouter hands the tier to the gateway untouched → silent.
        assert!(
            downgraded_effort_notice("anthropic", "Anthropic", Some(ReasoningEffort::Max))
                .is_none()
        );
        assert!(
            downgraded_effort_notice("openrouter", "OpenRouter", Some(ReasoningEffort::Max))
                .is_none()
        );

        // An Unsupported provider does not *downgrade* the effort, it drops
        // it — that is `unsupported_effort_notice`'s line to print, and this
        // one must not double up on it.
        assert!(
            downgraded_effort_notice("deepseek", "DeepSeek", Some(ReasoningEffort::Max)).is_none()
        );
        assert!(
            unsupported_effort_notice("deepseek", "DeepSeek", Some(ReasoningEffort::Max)).is_some()
        );

        // Unknown id → no row, no claim.
        assert!(downgraded_effort_notice("no-such", "Nope", Some(ReasoningEffort::Max)).is_none());
    }

    #[test]
    fn model_supports_reasoning_reads_the_catalog_and_defaults_to_unknown() {
        assert_eq!(
            model_supports_reasoning("deepseek", "deepseek-chat"),
            Some(false)
        );
        assert_eq!(model_supports_reasoning("zai", "glm-5.2"), Some(true));
        // Unknown slug or provider → unknown, never a hard no.
        assert_eq!(
            model_supports_reasoning("openrouter", "no-such/model"),
            None
        );
    }

    #[test]
    fn snapshot_roundtrips_through_the_tui_state() {
        let engine = engine_from_json(
            r#"{"default_model": "openrouter/openai/gpt-5.5",
                "allowed_models": ["anthropic/claude-fable-5"],
                "auto_mode": "off", "effort_auto": "on", "reasoning_auto": "off",
                "agents": {"default": {"provider": "openrouter", "effort": "high",
                                      "reasoning": "on",
                                      "params": {"temperature": 0.2, "top_k": 40,
                                                  "verbosity": "low",
                                                  "service_tier": "flex"}}}}"#,
        );
        let state = state_from_settings(
            &engine,
            vec!["zai".into()],
            vec!["zai/glm-5.2".into()],
            Default::default(),
            Vec::new(),
        );
        assert!(!state.auto_mode);
        assert!(state.effort_auto);
        let agent = state.agent(EngineRole::Default).expect("default state");
        assert_eq!(agent.model.as_deref(), Some("openrouter/openai/gpt-5.5"));
        assert_eq!(agent.provider.as_deref(), Some("openrouter"));
        assert_eq!(agent.effort.as_deref(), Some("high"));
        assert_eq!(agent.reasoning, Some(true));
        assert_eq!(agent.top_k, Some(40));
        assert_eq!(agent.verbosity.as_deref(), Some("low"));
        assert_eq!(agent.service_tier.as_deref(), Some("flex"));

        let back = settings_from_state(&state);
        // The model lands on the flat key, agents.default.model stays clear.
        assert_eq!(
            back.default_model.as_deref(),
            Some("openrouter/openai/gpt-5.5")
        );
        let agent = back.agent().expect("default agent");
        assert!(agent.model.is_none());
        assert_eq!(agent.provider.as_deref(), Some("openrouter"));
        assert_eq!(agent.effort, Some(ReasoningEffort::High));
        assert_eq!(agent.reasoning, Some(Toggle::On));
        let params = agent.params.expect("params");
        assert_eq!(params.temperature, Some(0.2));
        assert_eq!(params.service_tier, Some(ServiceTier::Flex));
        // The toggles are written explicitly (an "off" the user chose is
        // preserved, not dropped).
        assert_eq!(back.auto_mode, Some(Toggle::Off));
        assert_eq!(back.effort_auto, Some(Toggle::On));
        // The round trip is stable: state → settings → state is identity
        // for the fields the snapshot carries.
        let state2 = state_from_settings(
            &back,
            vec!["zai".into()],
            vec!["zai/glm-5.2".into()],
            Default::default(),
            Vec::new(),
        );
        assert_eq!(state2.agents, state.agents);
        assert_eq!(state2.allowed_models, state.allowed_models);
    }
}
