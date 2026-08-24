//! The `agent_engine_config` block — the one engine agent (model, prompt,
//! effort, params), its JSON schema, and the load/save machinery for the one
//! scope an editor owns.
//!
//! Moved verbatim out of `settings.rs` — no behavior change — when that file
//! crossed the size ratchet (`scripts/check-file-size.sh`): the engine-agent
//! cluster is the largest self-contained concept it carried. Everything here
//! is re-exported by the parent, so every `settings::AgentEngineConfig` path
//! keeps resolving.
//!
//! # One role, named `default`
//!
//! This block used to advertise six model personas — `default`, `worker`,
//! `verifier`, `triage`, `research`, `plan` — for a core loop that has none of
//! them. They were pins on the staged pipeline deleted in #3865, and by the
//! time that crate left the workspace four of the five were **inert**: a user
//! could set `pipeline_verifier_model` and nothing read it, including the one
//! place a second model actually runs (`stella goal`'s cross-family verifier,
//! which selects by family and ignored the pin entirely).
//!
//! A setting that reads like a capability and is not one is worse than no
//! setting, so `doc:roleless-core` §6 slice 4 (#3908, epic #3903) collapsed
//! them to the one role core actually has. What replaces them is
//! [`AgentEngineConfig::seat_models`]: a plugin declares the participants its
//! process needs, and the user assigns a model to each by name. The retired
//! keys are **recognized, ignored and reported by name** rather than dropped
//! silently — see `super::unknown`'s `RETIRED_ENGINE_ROOT` for why a silent
//! removal would leave them reading like capabilities forever.

use super::*;

/// The `agent_engine_config` root object — issue-style schema:
///
/// ```jsonc
/// {
///   "agent_engine_config": {
///     "default_model": "anthropic/claude-fable-5",
///     "allowed_models": ["anthropic/claude-fable-5", "zai/glm-5.2"],
///     "seat_models": {"planner": "openrouter/openai/gpt-5.5"},
///     "auto_mode": "off",
///     "effort_auto": "on",
///     "reasoning_auto": "on",
///     "agents": {
///       "default": {
///         "provider": "openrouter",
///         "model": "openai/gpt-5.5",
///         "prompt": "You are a terse, test-first engineer…",
///         "effort": "high",
///         "reasoning": "on",
///         "params": {"temperature": 0.2, "top_p": 0.9}
///       }
///     }
///   }
/// }
/// ```
///
/// Model precedence: `agents.default.model` > `default_model` > the
/// provider's own default. A model string is either `provider/slug`
/// (`--model` semantics) or, when the agent's `provider` field is set, a bare
/// slug sent verbatim to THAT provider — which is how an OpenRouter key
/// routes `openai/gpt-5.5` while an Anthropic key serves the session.
///
/// A model for anything *other* than the session's own agent is a
/// [`seat_models`](Self::seat_models) entry, keyed by a name the plugin that
/// declared the participant chose. Core ships exactly one model setting; see
/// this module's header for why the five that used to sit beside it are gone.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentEngineConfig {
    /// The session's model — the one model setting core ships.
    ///
    /// Every other model a session spends on is one a human assigned to a
    /// name an installed plugin declared, via
    /// [`seat_models`](Self::seat_models).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// The model vocabulary the TUI pickers offer and `auto_mode` selects
    /// from. Entries are `provider/slug` strings. Empty/absent = no
    /// restriction (pickers fall back to the seed catalog).
    ///
    /// It is the ceiling on **seat assignments** too, and not only on what a
    /// picker will offer: a seat naming a model outside a non-empty list is
    /// refused with a notice and falls back to the session's model, the same
    /// answer an unassigned seat gets (see [`crate::agent::seats`]). Without
    /// that, an operator who narrowed their vocabulary to two models could
    /// still be billed for a third by one line in a plugin's seat map — which
    /// is precisely the restriction this list exists to express.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    /// Which model each **plugin-declared seat** runs on.
    ///
    /// ```jsonc
    /// "seat_models": {
    ///   "vera/verifier": "anthropic/claude-opus-5",
    ///   "stella-plan/planner": "deepseek/deepseek-chat"
    /// }
    /// ```
    ///
    /// Keys are `<plugin-id>/<role>`, where the role is a name the plugin chose
    /// for a participant in its process; values are model strings in `--model`
    /// spelling. Nothing in this workspace matches a key against a literal, and
    /// nothing validates one against a list of known roles — there is no such
    /// list, deliberately. A plugin describes the process it needs; this map is
    /// where the user decides whether a participant gets a model of its own.
    ///
    /// **The plugin declares only the bare role name; the host applies the
    /// prefix.** That is what makes the namespace non-forgeable: a plugin never
    /// writes `vera/…`, so it cannot declare a role under another plugin's name
    /// and capture the assignment meant for it. There is deliberately no bare
    /// form and no precedence ladder — resolution is one lookup, and a miss is
    /// the default model (`doc:roleless-core` §8.4).
    ///
    /// Absent, or absent for a given seat, means that seat runs on the
    /// session's model — so installing a plugin with five participants costs
    /// exactly what a single-model session costs until someone assigns
    /// otherwise. See [`crate::agent::seats`] for the resolution and for why
    /// core never substitutes a default of its own here.
    ///
    /// This is the replacement for the `pipeline_<role>_model` keys, which
    /// named roles a core loop no longer has: those were pins on a staged
    /// pipeline deleted in #3865, and #3908 retired them. Retiring the
    /// four-language contract that pins the same words is #3910, and it is
    /// deliberately a separate ticket because the guard may only go once role
    /// names travel as trace data (#3906). Epic #3903.
    ///
    /// Assignments are bounded by [`allowed_models`](Self::allowed_models)
    /// when that list is non-empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seat_models: Option<std::collections::BTreeMap<String, String>>,
    /// Per-model output ceilings, overriding what the catalog knows the model
    /// can write. `[models.output_caps]` in TOML; keys are `provider/slug` or
    /// a bare slug, values are token counts.
    ///
    /// The default is, and stays, the model's OWN maximum — the catalog
    /// learns it from the provider and the engine asks for all of it, because
    /// a cap below the model's ceiling decides where work stops rather than
    /// the model doing so. This map exists for the one direction that is a
    /// real request: deliberately spending LESS than the model allows, to
    /// bound cost or latency, or to match a comparator that caps itself
    /// lower. Raising a model above its own ceiling is not a thing this can
    /// do — the provider rejects it — so an entry above the catalog's number
    /// is refused rather than sent (#1290).
    ///
    /// An override, never a definition. Absence means "the catalog wins",
    /// which is what keeps this from becoming the hand-written model table
    /// `[models]` exists to avoid: a model that ships tomorrow needs no entry
    /// here, and one listed here that later changes its real ceiling still
    /// gets a correct default the moment the entry is removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_output_caps: Option<std::collections::BTreeMap<String, u32>>,
    /// `on` = leave model selection to Stella rather than to a pin.
    ///
    /// **It no longer selects a model, and that is a declared gap rather than
    /// an oversight (#3936).** It used to pick the verifier out of
    /// `allowed_models`, preferring a different family than the worker's and
    /// ranking by catalog list price; #3908 retired the role that pin
    /// staffed, and the selector (`engine_config::auto_verifier_spec`) went
    /// with it rather than being left as code nothing calls. The live
    /// second-model path — `stella goal`'s `resolve_cross_family_verifier` —
    /// groups by family at the point of use and has never read this key.
    ///
    /// What it still does is real but narrow: it is one of the three switches
    /// [`crate::profile`] reads and writes, so it participates in "is this
    /// config in the auto state, or has a profile claimed it?"
    /// (`profile::is_auto`, `restore_auto`, `detect`). That is why it is a
    /// *diminished* key rather than a retired one — and why retiring it is a
    /// decision rather than a cleanup: `bench/harbor_adapter`'s registered
    /// postures write it, so removing it re-hashes published benchmark
    /// digests (see `super::unknown`'s `RETIRED_ENGINE_ROOT`). Wiring it to
    /// choose models for unassigned seats, or retiring it, is #3936.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_mode: Option<Toggle>,
    /// `on` = the agent's reasoning effort is chosen automatically, overriding
    /// any `agents.default.effort`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_auto: Option<Toggle>,
    /// `on` = thinking mode is chosen automatically, overriding any
    /// `agents.default.reasoning`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_auto: Option<Toggle>,
    /// `on` = a headless `stella run` proceeds past scope review instead of
    /// stopping at `ScopeReviewRequiredHeadless`. Only `stella run` reads it:
    /// `stella goal` and fleet workers keep the hard-off constant.
    ///
    /// Off by default, and deliberately so: scope review is the gate that
    /// keeps a large blast radius from landing unattended, and a headless
    /// surface has nobody to ask. Turn it on only where the working tree is
    /// disposable and the budget cap is the real guard — a benchmark
    /// container, CI on a scratch checkout. Without it, any plan over the
    /// scope thresholds (more than 5 steps by default) ends the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headless_scope_bypass: Option<Toggle>,
    /// Seconds of provider silence that end a single generation
    /// (`stella_core::EngineConfig::model_timeout`). Absent keeps the engine's
    /// own default, which is what every run used before this key existed.
    ///
    /// This is the third of the three coupled ceilings, and the only one that
    /// used to be reachable from nothing: the output cap lives in
    /// `agents.<role>.params.max_tokens` and the turn budget is a CLI flag, but
    /// `model_timeout` was an engine constant, so moving it meant recompiling.
    /// For a benchmark arm that made a timeout change a *system-under-test*
    /// change — a re-freeze of the registered commit rather than a line in the
    /// posture (#1211 §6.2).
    ///
    /// The three move together or not at all. Raising the output cap alone
    /// relocates the cliff rather than removing it: a step allowed 128k output
    /// tokens against a timeout sized for 64k stops on the timeout instead, and
    /// the run reports a capability difference that was really a ceiling
    /// nobody scaled. The rule that sets all three is the same — never be the
    /// side that stops first.
    ///
    /// It bounds *idle silence between stream fragments*, not elapsed time, so
    /// a generation that keeps streaming is never cut by it. Size it as a
    /// margin against a provider that stopped answering, above what the output
    /// cap can take to produce at the model's observed throughput.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_timeout_secs: Option<u64>,
    /// Conversation-size ceiling that triggers compaction, in estimated
    /// tokens (`stella_core::EngineConfig::compaction_budget_tokens`). Absent
    /// keeps the engine default, which is what every run used before this key
    /// existed. Whatever is set here is still clamped to 3/4 of the resolved
    /// model's context window — a budget above the window is a provider-side
    /// overflow scheduled in advance, never a legal configuration.
    ///
    /// This existed only as an engine constant (and a `stella-serve`
    /// override): a benchmark arm could not move it without a rebuild, which
    /// made context-handling experiments system-under-test changes rather
    /// than posture keys (#1285).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_budget_tokens: Option<u64>,
    /// Age-based tool-result retention horizon, in tool-bearing steps
    /// (`stella_core::EngineConfig::tool_result_horizon_steps`): results
    /// older than this many steps are middle-out aged on every step,
    /// independent of the compaction budget. Absent keeps the engine
    /// default; `0` disables the pass entirely (the same "0 opts out"
    /// convention as `model_timeout_secs`), restoring pure budget-triggered
    /// compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result_horizon_steps: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<AgentEngineAgents>,
}

/// The `agents` map — one named key rather than a `BTreeMap` so access is
/// typed instead of stringly (a misspelled agent key in JSON is simply
/// ignored, the same tolerance every other settings object has for unknown
/// fields).
///
/// It carries exactly one entry, and its name is the one role core has.
/// `worker`, `verifier`, `triage`, `research` and `plan` were siblings here
/// until #3908; a settings file still naming one is reported by
/// `super::unknown` rather than silently ignored.
///
/// Still a struct rather than a bare `Option<AgentEngineAgent>` on the parent
/// because `agents.default` is the shipped spelling in every settings file,
/// TOML `[agents.default]` section and benchmark posture in the wild —
/// flattening it would be a second breaking rename for no gain.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentEngineAgents {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<AgentEngineAgent>,
}

/// One agent's engine overrides. Every field optional — an absent field
/// means "no opinion", falling through to the flat model fields / engine
/// defaults / the provider's own defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentEngineAgent {
    /// Model slug — `provider/slug`, or a bare slug when `provider` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Gateway/provider id this agent's requests go through (a built-in id
    /// or a settings-defined provider). When set, `model` is sent verbatim
    /// as the slug to this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Custom system prompt replacing the built-in one for this agent.
    /// Workspace memories/rules still append to it (they are additive
    /// context, not part of the base instruction set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Reasoning effort (`low`/`medium`/`high`/`xhigh`/`max`), forwarded
    /// as `CompletionRequest::effort`. Overridden by `effort_auto: on`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    /// Thinking mode on/off, forwarded as `CompletionRequest::reasoning`.
    /// Absent = the provider's default. Overridden by `reasoning_auto: on`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<AgentEngineParams>,
}

/// Per-agent generation parameters — the "Include" checkbox model: a
/// `Some` value goes on the wire (where the provider's dialect supports
/// it), `None` leaves the provider default untouched. `temperature` and
/// `max_tokens` land on `CompletionRequest` directly; the rest ride
/// `CompletionRequest::params`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentEngineParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repetition_penalty: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<Verbosity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
}

impl AgentEngineParams {
    /// Overlay `other` (higher precedence) onto `self`, per field.
    pub(super) fn overlay(&mut self, other: &AgentEngineParams) {
        macro_rules! take {
            ($field:ident) => {
                if other.$field.is_some() {
                    self.$field = other.$field;
                }
            };
        }
        take!(temperature);
        take!(top_p);
        take!(top_k);
        take!(frequency_penalty);
        take!(presence_penalty);
        take!(repetition_penalty);
        take!(max_tokens);
        take!(seed);
        take!(verbosity);
        take!(service_tier);
    }
}

impl AgentEngineAgent {
    /// Overlay `other` (higher precedence) onto `self`, per field —
    /// `params` composes recursively so a project scope can set one knob
    /// without clobbering the user scope's others.
    pub(super) fn overlay(&mut self, other: &AgentEngineAgent) {
        macro_rules! take {
            ($field:ident) => {
                if other.$field.is_some() {
                    self.$field = other.$field.clone();
                }
            };
        }
        take!(model);
        take!(provider);
        take!(prompt);
        take!(effort);
        take!(reasoning);
        if let Some(params) = &other.params {
            self.params
                .get_or_insert_with(AgentEngineParams::default)
                .overlay(params);
        }
    }
}

impl AgentEngineConfig {
    /// Overlay `other` (higher precedence) onto `self`, per field, per
    /// agent — the same composition rule as `ProviderSettings::overlay`.
    /// `allowed_models` REPLACES wholesale (it is one vocabulary, not a
    /// set of independent knobs — concatenating scopes would make it
    /// impossible for a project to narrow the user's list).
    pub(super) fn overlay(&mut self, other: &AgentEngineConfig) {
        macro_rules! take {
            ($field:ident) => {
                if other.$field.is_some() {
                    self.$field = other.$field.clone();
                }
            };
        }
        take!(default_model);
        take!(allowed_models);
        // Per KEY, for `model_output_caps`'s reason and one of its own: a
        // project pinning its plan plugin's `planner` seat has said nothing
        // about the `reviewer` seat the user assigned in their own file, and
        // wholesale replacement would drop it. Seats are independent
        // assignments, never one vocabulary.
        if let Some(seats) = &other.seat_models {
            let target = self.seat_models.get_or_insert_with(Default::default);
            for (seat, model) in seats {
                target.insert(seat.clone(), model.clone());
            }
        }
        // Per KEY, not wholesale — the deliberate opposite of
        // `allowed_models` two lines up, and the contrast is the reason
        // either rule is right. `allowed_models` is one vocabulary, so a
        // project narrowing it must be able to replace the user's list
        // entirely. These are independent per-model knobs: a project pinning
        // one model's ceiling has said nothing about any other model, and
        // replacing the map wholesale would silently drop the user's pins on
        // models the project never mentioned.
        if let Some(caps) = &other.model_output_caps {
            let target = self.model_output_caps.get_or_insert_with(Default::default);
            for (model, cap) in caps {
                target.insert(model.clone(), *cap);
            }
        }
        take!(auto_mode);
        take!(effort_auto);
        take!(reasoning_auto);
        // Was omitted from this list: a higher-precedence scope setting it
        // had its value discarded, so the LOWER scope silently won the one
        // knob deciding whether an unattended run may proceed past scope
        // review. Latent while nothing layered underneath the user's file;
        // required now that `provider_engine_baseline` does.
        take!(headless_scope_bypass);
        take!(model_timeout_secs);
        take!(compaction_budget_tokens);
        take!(tool_result_horizon_steps);
        if let Some(agents) = &other.agents
            && let Some(agent) = &agents.default
        {
            self.agents
                .get_or_insert_with(AgentEngineAgents::default)
                .default
                .get_or_insert_with(AgentEngineAgent::default)
                .overlay(agent);
        }
    }

    /// Layer `self` OVER `baseline`, field by field and agent by agent —
    /// `self` (the user's merged settings) wins every field it sets, and
    /// `baseline` answers only where the user had no opinion.
    ///
    /// This is the same composition rule the settings scope chain already
    /// uses; the difference is only what sits underneath. It exists so a
    /// gateway can ship a default posture (see
    /// [`crate::engine_config::provider_engine_baseline`]) that is a genuine
    /// *default* — visible in `stella config`, overridable at any scope, and
    /// never able to displace something the user actually wrote down.
    pub fn layered_over(&self, baseline: &AgentEngineConfig) -> AgentEngineConfig {
        let mut merged = baseline.clone();
        merged.overlay(self);
        merged
    }

    /// The agent's overrides, if any — `agents.default`.
    pub fn agent(&self) -> Option<&AgentEngineAgent> {
        self.agents.as_ref().and_then(|a| a.default.as_ref())
    }

    /// The effective model STRING (not yet resolved to a provider):
    /// `agents.default.model` > `default_model`. `None` = no opinion
    /// (auto-detect / `--model` / provider default decide, exactly as before
    /// this config existed).
    pub fn model_for(&self) -> Option<&str> {
        self.agent()
            .and_then(|a| a.model.as_deref())
            .or(self.default_model.as_deref())
    }

    /// The allowed-model vocabulary, empty when unrestricted.
    pub fn allowed_models(&self) -> &[String] {
        self.allowed_models.as_deref().unwrap_or(&[])
    }

    /// The operator's deliberate output ceiling for one model, if they set
    /// one. `None` means what it should mean everywhere in this feature: use
    /// the model's own maximum.
    ///
    /// Both spellings resolve, qualified first: `anthropic/claude-sonnet-5`
    /// pins that model on that provider, while a bare `claude-sonnet-5` pins
    /// it wherever it is reached from. Both are useful and they are not the
    /// same request — capping a model only on the gateway you pay per-token
    /// for is a real thing to want — so the qualified form wins when both are
    /// present rather than one silently shadowing the other.
    ///
    /// `0` is not admissible and is treated as unset. Elsewhere in this
    /// engine `0` spells "no ceiling" (`model_timeout_secs`), but here that
    /// reading is unavailable: the field's absence already means "the model's
    /// own maximum", so a literal zero can only be a mistake — and honoring
    /// it would ask the provider for an empty completion on every step.
    pub fn model_output_cap(&self, provider: &str, slug: &str) -> Option<u32> {
        let caps = self.model_output_caps.as_ref()?;
        caps.get(&format!("{provider}/{slug}"))
            .or_else(|| caps.get(slug))
            .copied()
            .filter(|cap| *cap > 0)
    }

    pub fn auto_mode_on(&self) -> bool {
        self.auto_mode.is_some_and(Toggle::is_on)
    }

    pub fn effort_auto_on(&self) -> bool {
        self.effort_auto.is_some_and(Toggle::is_on)
    }

    pub fn reasoning_auto_on(&self) -> bool {
        self.reasoning_auto.is_some_and(Toggle::is_on)
    }

    /// Whether a headless run skips the (now-removed) staged pipeline's
    /// scope-review gate rather than refusing outright. The gate is gone
    /// (#3865, and the deck's interactive half with it in #3861), so no
    /// production code consults the answer any more.
    ///
    /// **It is nevertheless not retired, and the reason is a measurement one
    /// rather than an oversight (#3870).** The key is inert in the engine but
    /// required in the benchmark contract:
    /// `bench/harbor_adapter/stella_harbor/posture.py` writes
    /// `headless_scope_bypass: "on"` into the claim-path Terminal-Bench
    /// posture, whose digest `6c7fc70c` is registered in
    /// `bench/READINESS.md` §8.4.5 and asserted by
    /// `test_the_registered_sonnet_digest_is_unchanged`. Moving it to
    /// `settings::unknown`'s `RETIRED` list would remove it from `ENGINE_ROOT_FIELDS`,
    /// which `config::trusted_engine_config_shape_is_strict` shares and which
    /// fails **closed** — so retiring it either refuses every benchmark launch
    /// or forces the posture to drop the key and re-hash every registered arm.
    /// Choosing between those is a maintainer's call about published numbers,
    /// not a cleanup; #3870 carries the analysis.
    ///
    /// That gap is about the **field**, which must keep parsing and merging.
    /// The *accessor* is a separate question with a plain answer: its only
    /// callers are the settings-merge tests that exercise the scope-chain
    /// precedence rule through this name. So it is `cfg(test)` rather than
    /// `#[allow(dead_code)]` — the field stays in `ENGINE_ROOT_FIELDS` and the
    /// registered posture digest is untouched, while nothing claims the
    /// dead-code lint was wrong about a function no shipped path calls.
    #[cfg(test)]
    pub fn headless_scope_bypass_on(&self) -> bool {
        self.headless_scope_bypass.is_some_and(Toggle::is_on)
    }

    /// Persist THIS config as the `agent_engine_config` key of the
    /// settings file at `path`, preserving every other key in the file
    /// byte-for-byte at the value level (providers, hooks, mcp, and any
    /// forward-compat keys survive a TUI save untouched). Creates the file
    /// (and parent directories) when absent.
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        // The FORMAT is a property of the target file, not of the caller, so
        // every existing call site keeps working unchanged once its path
        // resolver starts returning a `.toml`.
        if toml_config::path_is_toml(path) {
            let sections = toml_config::agent_sections(self)?;
            let user_private = user_config_path().as_deref() == Some(path);
            return toml_io::save_sections(path, &sections, user_private);
        }
        private::reject_symlink(path)?;
        let mut root: serde_json::Value = match std::fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents)
                .map_err(|e| format!("invalid settings file {}: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let object = root
            .as_object_mut()
            .ok_or_else(|| format!("settings file {} is not a JSON object", path.display()))?;
        let value = serde_json::to_value(self)
            .map_err(|e| format!("cannot serialize agent_engine_config: {e}"))?;
        object.insert("agent_engine_config".to_string(), value);
        let mut rendered = serde_json::to_string_pretty(&root)
            .map_err(|e| format!("cannot render settings: {e}"))?;
        rendered.push('\n');
        let user_private = user_settings_path().as_deref() == Some(path);
        // Project settings are canonical committable configuration: the
        // persistence layer reserves owner-only atomic writes for user scope.
        private::write_settings(path, rendered.as_bytes(), user_private)
    }
}
/// The `agent_engine_config` recorded in ONE user-scope file, ignoring every
/// other scope.
///
/// [`Settings::load`] returns the MERGED view of user + managed + project. That
/// is the right input for *running* the engine and the wrong input for
/// *editing* one file: [`AgentEngineConfig::save_to`] replaces the whole block,
/// so a caller that loads the merged config, changes one field, and saves to
/// user scope also copies the project's and the org's opinions into the user
/// file — silently promoting a per-repo model pin to a machine-wide default.
/// An editor reads the scope it is about to write.
///
/// A **missing** file is an empty config — there is nothing to preserve, and
/// the caller is about to create it. A file that exists but cannot be parsed is
/// an **error**, and deliberately not a fallback to empty: because the caller
/// then writes the whole block back, degrading here would let one malformed key
/// somewhere else in the file destroy an engine config this editor never owned.
/// A named error the user can act on beats silently discarding their settings.
///
/// User scope specifically, because it is the only scope a slash command
/// writes; a project-scope sibling would need to pass its own
/// [`toml_config::ConfigScope`] so a TOML file's declared scope still validates.
pub fn user_engine_config_at(path: &Path) -> Result<AgentEngineConfig, String> {
    let settings = if toml_config::path_is_toml(path) {
        // A closure, not `std::fs::read_to_string` by name: the free function
        // is generic over `AsRef<Path>`, so inference pins it to one concrete
        // lifetime and it stops satisfying the reader's for-any-lifetime bound.
        toml_config::load_toml_scope(path, toml_config::ConfigScope::User, |p: &Path| {
            std::fs::read_to_string(p)
        })?
        .settings
    } else {
        Settings::load_scope(path)?
    };
    Ok(settings.agent_engine_config.unwrap_or_default())
}
