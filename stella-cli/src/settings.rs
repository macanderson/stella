//! `settings.json` — declarative provider configuration (issue #44).
//!
//! Three scopes, merged per provider id, per field, in ascending precedence:
//!
//! 1. user:        `~/.stella/settings.json`
//! 2. org-managed: `/Library/Application Support/stella/settings.json` on
//!    macOS, `/etc/stella/settings.json` elsewhere (override the path with
//!    `STELLA_MANAGED_SETTINGS` — also how tests point at a fixture)
//! 3. project:     `<workspace>/.stella/settings.json`
//!
//! A trusted benchmark launcher may set `STELLA_NO_SETTINGS=1` to skip all
//! three filesystem scopes and return [`Settings::default`]. The same signal is
//! the process-wide benchmark isolation boundary for other Stella-specific
//! filesystem steering (rules, memories, skills, custom tools, MCP config, and
//! persisted session state). This is stronger than the ordinary project trust
//! boundary: a task image's preinstalled user or managed state is outside the
//! frozen system under test. The launcher-owned `STELLA_ENGINE_CONFIG_JSON`
//! override is applied later by `Config::load_with_settings`, so disabling
//! files does not disable the explicit benchmark engine posture.
//!
//! An entry whose id matches a built-in provider OVERRIDES that provider's
//! defaults (display name, base URL, default model, credential source). An
//! entry with a new id DEFINES a whole new provider — `base_url` becomes
//! required and `dialect` picks the wire adapter (`config.rs` synthesizes
//! the `ProviderConfig`). A malformed file is a hard, named error rather
//! than a silent skip: a typo that quietly reverted someone to a built-in
//! endpoint would be far worse than a loud parse failure.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use stella_core::hooks::Hooks;
use stella_protocol::{ReasoningEffort, ServiceTier, Verbosity};

use crate::config::Dialect;

mod authority;
mod context;
pub(crate) mod context_providers;
mod managed;
mod merge;
pub(crate) mod migrate;
mod private;
#[cfg(test)]
#[path = "settings/private_state_tests.rs"]
mod private_state_tests;
mod toml_config;
mod toml_io;
#[cfg(test)]
#[path = "settings/toml_tests.rs"]
mod toml_tests;
mod unknown;
pub use authority::{AuthorityPolicy, ManagedAuthoritySettings};
pub use toml_config::ConfigScope;
pub(crate) use unknown::{
    ENGINE_AGENT_FIELDS, ENGINE_AGENT_NAMES, ENGINE_PARAM_FIELDS, ENGINE_ROOT_FIELDS,
};
// `ContextSettings`, `RetrievalSettings`, and `InferredDirectivePromotion`
// all have readers now (`memory::tuning` — retrieval budgets, the lifecycle
// switch, promotion thresholds). The remaining nested types (`LearningMode`,
// `GovernanceMode`, …) live in `settings::context` until wired.
pub use context::{ContextSettings, InferredDirectivePromotion, RetrievalSettings};
pub use context_providers::{ContextProviderSettings, ExternalContextProvider, ProviderEndpoint};
pub use merge::ToolScopePolicies;

/// One `providers.<id>` entry. Every field is optional at the schema level;
/// which ones are *required* depends on whether the id names a built-in
/// (override: any subset is fine) or defines a new provider (`base_url`
/// must be present). `config.rs` enforces that split.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ProviderSettings {
    /// Optional restatement of the map key (the issue's examples carry it);
    /// when present it must match the key, so a copy-paste of one entry
    /// under a new key can't silently configure the wrong provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Display name (`ProviderConfig::display_name`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// A literal credential. Sits below env vars and above the interactive
    /// prompt in the chain, mirroring the credentials file. Prefer
    /// `api_key_env` for anything long-lived — settings.json is often
    /// committed, credentials should not be.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Name of an environment variable to read the credential from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Wire dialect for config-defined providers. Defaults to
    /// `openai-compatible`; ignored for built-in overrides (a built-in's
    /// dialect is fixed by its adapter).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dialect: Option<Dialect>,
}

impl ProviderSettings {
    /// Overlay `other` (higher precedence) onto `self`, field by field, so
    /// e.g. an org-managed base URL and a user-scope api_key_env compose
    /// instead of the whole entry being replaced wholesale.
    fn overlay(&mut self, other: &ProviderSettings) {
        macro_rules! take {
            ($field:ident) => {
                if other.$field.is_some() {
                    self.$field = other.$field.clone();
                }
            };
        }
        take!(id);
        take!(name);
        take!(base_url);
        take!(api_key);
        take!(api_key_env);
        take!(default_model);
        take!(dialect);
    }
}

/// The merged view of every settings.json in scope.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Settings {
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderSettings>,
    /// Lifecycle hooks (`stella_core::hooks`): `SessionStart` context,
    /// `PreToolUse` blocking, `PostToolUse` observation. Scopes CONCATENATE
    /// per event (any scope can add a gate, none can remove another's) —
    /// see [`Settings::load`] for the project-scope trust boundary.
    #[serde(default)]
    pub hooks: Option<Hooks>,
    /// MCP settings — currently just the server registry URL the MCP tab
    /// searches. Optional; the default registry is applied at the read site
    /// ([`Settings::mcp_registry_url`]).
    #[serde(default)]
    pub mcp: Option<McpSettings>,
    /// Agent-engine configuration: per-role models, prompts, effort, and
    /// sampling parameters for the four engine agents (default / worker /
    /// judge / triage), plus the allowed-model vocabulary and the auto
    /// modes. Scopes overlay per field like `providers`. Deliberately kept
    /// mostly OUTSIDE the project trust boundary: model and sampling fields
    /// carry no credential routing (an agent's `provider` names an id whose
    /// endpoint and credential fields are gated above). Per-agent replacement
    /// prompts are privileged and restored from trusted scopes unless the
    /// effective authority explicitly permits project prompts.
    #[serde(default)]
    pub agent_engine_config: Option<AgentEngineConfig>,
    /// Built-in tool switches ([`ToolsSettings`]). Scopes overlay per field,
    /// subject to repository trust and the non-overridable managed ceiling.
    #[serde(default)]
    pub tools: Option<ToolsSettings>,
    /// `on` = print an end-of-run recap in text mode: a deterministic
    /// synthesis of the outcome (completed/verified/failed/aborted) and what
    /// changed, beside the file and cost panels. No model call. Default off.
    #[serde(default)]
    pub enable_recap: Option<Toggle>,
    /// `on` = assemble a trajectory trace after each finished execution
    /// (#1042): the exact model inputs, staged path, tool activity, and cost,
    /// appended to `.stella/private/traces.jsonl` and pointed to from the
    /// run's episode. Local-only by construction. Default off.
    #[serde(default)]
    pub trace_capture: Option<Toggle>,
    /// `always` / `ask` / `never` — whether a run does its work in a throwaway
    /// git worktree instead of the working tree. Absent, null, or empty means
    /// `ask`, and the question is put once, at triage, only when the run is
    /// actually going to change files. See [`CreateWorktrees`].
    #[serde(default)]
    pub create_worktrees: Option<CreateWorktrees>,
    /// Appearance preferences — currently just the TUI colour theme
    /// (`/theme`). Whole-block last-wins across scopes; carries no authority.
    #[serde(default)]
    pub ui: Option<UiSettings>,
    /// Adaptive-context lifecycle configuration (the `context` block).
    /// `context.lifecycle.enabled` defaults **`true`** — the lifecycle ships
    /// on, and setting it `false` restores every pre-adaptive behavior.
    /// `None` = the block was absent, which means the defaults apply, not that
    /// the lifecycle is off. Whole-block
    /// last-wins across scopes (see [`Settings::overlay_scope`]). See
    /// [`ContextSettings`].
    #[serde(default)]
    pub context: Option<ContextSettings>,
    /// `context_providers.<id>` — third-party CGP context sources reached over
    /// stdio/HTTP (#453). Merged per-entry across scopes like `providers`, so
    /// a project may enable a provider the user scope declared without
    /// restating its transport. Empty (the shipping default) registers
    /// nothing and leaves recall exactly as it is today.
    ///
    /// Inside the project trust boundary: an entry spawns a command or opens
    /// an egress-consented connection, so an untrusted repo's entries are
    /// dropped in favour of the trusted scopes' (see [`Settings::load`]).
    #[serde(default)]
    pub context_providers: ContextProviderSettings,
    /// Authority ceilings are honored only from the org-managed settings
    /// file. The serde name is intentionally short because the containing
    /// file is already the policy source.
    #[serde(default, rename = "authority")]
    managed_authority: Option<ManagedAuthoritySettings>,
    /// Raw enrollment captured only from managed scope; validated fail-open by its adapter.
    #[serde(default)]
    enterprise_telemetry: Option<serde_json::Value>,
    /// Effective authority computed by [`Settings::load`]. This is skipped
    /// when parsing individual scopes so repository text cannot supply it.
    #[serde(skip)]
    pub authority_policy: AuthorityPolicy,
}

/// `create_worktrees`: whether a run does its work in a throwaway git worktree
/// instead of the user's checkout.
///
/// `"always"`, `"ask"`, `"never"` — and `""`, null, or the key being absent all
/// mean `"ask"`, so a scope that wants to say "no opinion" can write the key
/// empty rather than having to delete it. Anything else is a loud parse error;
/// a typo here would otherwise silently pick a side on where somebody's work
/// happens.
///
/// The policy is consumed at triage (`Pipeline::isolate_in_worktree`), which is
/// the first moment it is both answerable and worth asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CreateWorktrees {
    Always,
    #[default]
    Ask,
    Never,
}

impl CreateWorktrees {
    /// The pipeline's spelling of the same decision.
    pub fn policy(self) -> stella_pipeline::ports::WorktreePolicy {
        match self {
            Self::Always => stella_pipeline::ports::WorktreePolicy::Always,
            Self::Ask => stella_pipeline::ports::WorktreePolicy::Ask,
            Self::Never => stella_pipeline::ports::WorktreePolicy::Never,
        }
    }
}

impl<'de> Deserialize<'de> for CreateWorktrees {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Through `Option<String>` so an explicit `null` lands here rather than
        // being rejected before this function sees it.
        let raw = Option::<String>::deserialize(deserializer)?;
        match raw.as_deref().map(str::trim).unwrap_or("") {
            "" | "ask" => Ok(Self::Ask),
            "always" => Ok(Self::Always),
            "never" => Ok(Self::Never),
            other => Err(serde::de::Error::custom(format!(
                "\"create_worktrees\": {other:?} is not one of \"always\", \"ask\", \"never\" \
                 (empty or absent means \"ask\")"
            ))),
        }
    }
}

impl Serialize for CreateWorktrees {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::Always => "always",
            Self::Ask => "ask",
            Self::Never => "never",
        })
    }
}

/// An `on`/`off` switch. A dedicated enum rather than `bool` because the
/// JSON reads as configuration prose (`"auto_mode": "on"`) and because a
/// typo'd value must be a loud parse error, not a silently-false bool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Toggle {
    On,
    Off,
}

impl Toggle {
    pub fn is_on(self) -> bool {
        matches!(self, Toggle::On)
    }
}

/// The four configurable engine agents. `Default` is the interactive /
/// step-loop agent; the other three are the staged pipeline's roles
/// (`stella_protocol::Role::{Worker, Judge, Triage}` — `Plan` rides the
/// worker's settings, matching the router's tiering).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineAgentKind {
    Default,
    Worker,
    Judge,
    Triage,
}

impl EngineAgentKind {
    pub const ALL: [EngineAgentKind; 4] = [
        EngineAgentKind::Default,
        EngineAgentKind::Worker,
        EngineAgentKind::Judge,
        EngineAgentKind::Triage,
    ];
}

/// The `agent_engine_config` root object — issue-style schema:
///
/// ```jsonc
/// {
///   "agent_engine_config": {
///     "default_model": "anthropic/claude-fable-5",
///     "pipeline_worker_model": "zai/glm-5.2",
///     "pipeline_judge_model": "openrouter/openai/gpt-5.5",
///     "pipeline_triage_model": "deepseek/deepseek-chat",
///     "allowed_models": ["anthropic/claude-fable-5", "zai/glm-5.2"],
///     "auto_mode": "off",
///     "effort_auto": "on",
///     "reasoning_auto": "on",
///     "agents": {
///       "judge": {
///         "provider": "openrouter",
///         "model": "openai/gpt-5.5",
///         "prompt": "You are a strict code-review judge…",
///         "effort": "high",
///         "reasoning": "on",
///         "params": {"temperature": 0.2, "top_p": 0.9}
///       }
///     }
///   }
/// }
/// ```
///
/// Model precedence per agent: `agents.<agent>.model` >
/// `pipeline_<agent>_model` (or `default_model` for the default agent) >
/// `default_model` > the provider's own default. A model string is either
/// `provider/slug` (`--model` semantics) or, when the agent's `provider`
/// field is set, a bare slug sent verbatim to THAT provider — which is how
/// an OpenRouter key routes `openai/gpt-5.5` while an Anthropic key serves
/// the worker.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentEngineConfig {
    /// Model for the default (interactive/step-loop) agent, and the base
    /// for every pipeline role that has no more specific setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_judge_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_worker_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_triage_model: Option<String>,
    /// The model vocabulary the TUI pickers offer and `auto_mode` selects
    /// from. Entries are `provider/slug` strings. Empty/absent = no
    /// restriction (pickers fall back to the seed catalog).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_models: Option<Vec<String>>,
    /// `on` = pick the judge model automatically from `allowed_models`,
    /// preferring a different model family than the worker's and ranking
    /// by catalog list price (the closest objective proxy for capability
    /// tier the seed catalog carries). You never worry about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_mode: Option<Toggle>,
    /// `on` = per-agent reasoning effort is chosen automatically (judge
    /// high, worker medium, triage low, default medium), overriding any
    /// per-agent `effort`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_auto: Option<Toggle>,
    /// `on` = thinking mode is chosen automatically (on for judge/worker/
    /// default, off for triage), overriding any per-agent `reasoning`.
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
    /// Revision turns the pipeline may spend per candidate when verification
    /// fails (`stella_pipeline::PipelineConfig::max_revisions`). Absent keeps
    /// the pipeline's own default, which is what every run used before this
    /// key existed.
    ///
    /// Raising it buys near-misses another attempt and nothing else: a
    /// revision only happens on a *failed* verification, so a run that passes
    /// first time costs the same at 2 as at 4. The ceiling that actually
    /// bounds spend is the budget cap, not this number — but each revision is
    /// a full execute turn, so on a task that cannot be fixed it is the
    /// difference between failing cheaply and failing expensively.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_max_revisions: Option<u32>,
    /// Best-of-N: how many candidate executions the pipeline generates before
    /// selecting one (`stella_pipeline::PipelineConfig::candidates`). Absent
    /// or `1` is single-shot, the default.
    ///
    /// Unlike `pipeline_max_revisions` this is paid *unconditionally* — n
    /// candidates run whether or not the first one would have passed, so `2`
    /// is a straight doubling of execution cost. Opt in where the tail matters
    /// more than the bill.
    ///
    /// Wants candidate isolation to be meaningful: with a
    /// `CandidateWorkspacePort` wired, each candidate runs in its own snapshot
    /// and only the winner is adopted. Without one the pipeline warns and runs
    /// them sequentially in the shared tree, where losing candidates' edits
    /// stay behind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline_candidates: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<AgentEngineAgents>,
}

/// The `agents` map — fixed keys rather than a `BTreeMap` so per-role
/// access is exhaustive and typed instead of stringly (a misspelled agent
/// key in JSON is simply ignored, the same tolerance every other settings
/// object has for unknown fields).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentEngineAgents {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<AgentEngineAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<AgentEngineAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge: Option<AgentEngineAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage: Option<AgentEngineAgent>,
}

impl AgentEngineAgents {
    pub fn get(&self, kind: EngineAgentKind) -> Option<&AgentEngineAgent> {
        match kind {
            EngineAgentKind::Default => self.default.as_ref(),
            EngineAgentKind::Worker => self.worker.as_ref(),
            EngineAgentKind::Judge => self.judge.as_ref(),
            EngineAgentKind::Triage => self.triage.as_ref(),
        }
    }

    pub fn get_mut(&mut self, kind: EngineAgentKind) -> &mut Option<AgentEngineAgent> {
        match kind {
            EngineAgentKind::Default => &mut self.default,
            EngineAgentKind::Worker => &mut self.worker,
            EngineAgentKind::Judge => &mut self.judge,
            EngineAgentKind::Triage => &mut self.triage,
        }
    }
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
    fn overlay(&mut self, other: &AgentEngineParams) {
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
    fn overlay(&mut self, other: &AgentEngineAgent) {
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
    fn overlay(&mut self, other: &AgentEngineConfig) {
        macro_rules! take {
            ($field:ident) => {
                if other.$field.is_some() {
                    self.$field = other.$field.clone();
                }
            };
        }
        take!(default_model);
        take!(pipeline_judge_model);
        take!(pipeline_worker_model);
        take!(pipeline_triage_model);
        take!(allowed_models);
        take!(auto_mode);
        take!(effort_auto);
        take!(reasoning_auto);
        // Was omitted from this list: a higher-precedence scope setting it
        // had its value discarded, so the LOWER scope silently won the one
        // knob deciding whether an unattended run may proceed past scope
        // review. Latent while nothing layered underneath the user's file;
        // load-bearing now that `provider_engine_baseline` does.
        take!(headless_scope_bypass);
        take!(pipeline_max_revisions);
        take!(pipeline_candidates);
        if let Some(agents) = &other.agents {
            let target = self.agents.get_or_insert_with(AgentEngineAgents::default);
            for kind in EngineAgentKind::ALL {
                if let Some(agent) = agents.get(kind) {
                    target
                        .get_mut(kind)
                        .get_or_insert_with(AgentEngineAgent::default)
                        .overlay(agent);
                }
            }
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

    /// The per-agent overrides for `kind`, if any.
    pub fn agent(&self, kind: EngineAgentKind) -> Option<&AgentEngineAgent> {
        self.agents.as_ref().and_then(|a| a.get(kind))
    }

    /// The effective model STRING for `kind` (not yet resolved to a
    /// provider): `agents.<kind>.model` > the flat per-role field >
    /// `default_model`. `None` = no opinion (auto-detect / `--model` /
    /// provider default decide, exactly as before this config existed).
    pub fn model_for(&self, kind: EngineAgentKind) -> Option<&str> {
        if let Some(model) = self.agent(kind).and_then(|a| a.model.as_deref()) {
            return Some(model);
        }
        let flat = match kind {
            EngineAgentKind::Default => None,
            EngineAgentKind::Worker => self.pipeline_worker_model.as_deref(),
            EngineAgentKind::Judge => self.pipeline_judge_model.as_deref(),
            EngineAgentKind::Triage => self.pipeline_triage_model.as_deref(),
        };
        flat.or(self.default_model.as_deref())
    }

    /// The allowed-model vocabulary, empty when unrestricted.
    pub fn allowed_models(&self) -> &[String] {
        self.allowed_models.as_deref().unwrap_or(&[])
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

/// The user-scope settings path (`~/.stella/settings.json`), when
/// `HOME` is known — the TUI save target for user-scope edits and the
/// first file of [`Settings::load`]'s chain.
pub fn user_settings_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".stella").join("settings.json"))
}

/// The project-scope settings path (`<workspace>/.stella/settings.json`).
pub fn project_settings_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".stella").join("settings.json")
}

/// The user-scope file an EDIT should target: `~/.stella/stella.toml` when it
/// exists, otherwise `~/.stella/settings.json`.
///
/// Separate from [`user_settings_path`] on purpose. That function names the
/// JSON file specifically and is still what the JSON loader and the
/// owner-only-write check compare against; this one answers a different
/// question — "where does a `/theme` save go?" — and its answer changes once a
/// user migrates. Collapsing the two would silently redirect the JSON reader.
///
/// It never invents a TOML file: a user who has not migrated keeps writing
/// JSON, so the editor cannot half-migrate someone behind their back.
pub fn user_config_path() -> Option<PathBuf> {
    if let Some(toml) = toml_config::user_toml_path()
        && toml.exists()
    {
        return Some(toml);
    }
    user_settings_path()
}

/// The project-scope file an EDIT should target — `<workspace>/stella.toml`
/// when it exists, otherwise `<workspace>/.stella/settings.json`. See
/// [`user_config_path`] for why this is not the same as
/// [`project_settings_path`].
pub fn project_config_path(workspace_root: &Path) -> PathBuf {
    let toml = toml_config::project_toml_path(workspace_root);
    if toml.exists() {
        return toml;
    }
    project_settings_path(workspace_root)
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

/// The `tools` section of settings.json — the one place a tool is switched
/// off, whether it is a built-in, an MCP server's, or one the customer wrote.
///
/// An **open map**, not a fixed set of fields. It used to be
/// `{ bash: Option<Toggle>, web: Option<Toggle> }`, which made every new
/// "should this be on?" question cost another field here, another
/// `RegistryOptions` boolean, and another hand-written branch — and could
/// never address an MCP or custom tool at all, because those names are not
/// known at compile time. A key is a tool name, a group name, or `"*"`; see
/// [`stella_tools::policy::ToolPolicy`] for the precedence rules.
///
/// Values stay [`Toggle`] rather than `bool` so a typo'd value is a loud
/// parse error, not a silent state. Absent means **on**: Stella ships with
/// every tool available, and this section is a deny list.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ToolsSettings {
    /// Every `"<key>": "on"|"off"` pair in the section, verbatim. Flattened,
    /// so the JSON is just the pairs — `{"bash": "off", "process": "off"}` —
    /// exactly as the two-field version read.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub entries: BTreeMap<String, Toggle>,
}

impl ToolsSettings {
    /// The `tools` table of one `stella.toml`. Same contract as the JSON
    /// [`ToolsSettings::read_from`]: a missing file is an empty section, and a
    /// `tools` value that is not a map of `on`/`off` is a named error rather
    /// than a silent reset.
    fn read_from_toml(path: &Path) -> Result<Self, String> {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let root: toml::Value = toml::from_str(&contents)
            .map_err(|e| format!("invalid config file {}: {e}", path.display()))?;
        match root.get("tools") {
            None => Ok(Self::default()),
            Some(value) => value
                .clone()
                .try_into()
                .map_err(|e| format!("invalid config file {}: tools: {e}", path.display())),
        }
    }

    /// Resolve this section into the policy the runtime enforces.
    pub fn policy(&self) -> stella_tools::policy::ToolPolicy {
        stella_tools::policy::ToolPolicy::from_switches(
            self.entries
                .iter()
                .map(|(key, toggle)| (key.clone(), toggle.is_on())),
        )
    }

    /// The inverse of [`ToolsSettings::policy`] — how a computed policy (the
    /// managed ceiling folded in, say) is written back into the settings
    /// shape that scopes merge and the TUI round-trips.
    pub fn from_policy(policy: &stella_tools::policy::ToolPolicy) -> Self {
        Self {
            entries: policy
                .switches()
                .iter()
                .map(|(key, &enabled)| {
                    (key.clone(), if enabled { Toggle::On } else { Toggle::Off })
                })
                .collect(),
        }
    }

    /// The `"tools"` section of ONE settings file — what that scope says, not
    /// what the merged chain resolved to.
    ///
    /// The distinction is the whole reason this exists rather than reading
    /// [`Settings::load`]'s answer: the settings editor read-modify-writes a
    /// single scope, and writing a *merged* map back would copy the other two
    /// scopes' switches into this file and freeze them there — a project's
    /// `{"bash": "off"}` would silently become the user's, and would survive
    /// the project removing it.
    ///
    /// A missing file is an empty section (the shipped default); a file whose
    /// `tools` value is not a map of `on`/`off` is a named error, never a
    /// silent reset — an editor that quietly discarded switches it could not
    /// parse would be worse than one that refuses to save.
    pub fn read_from(path: &Path) -> Result<Self, String> {
        if toml_config::path_is_toml(path) {
            return Self::read_from_toml(path);
        }
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        let root: serde_json::Value = serde_json::from_str(&contents)
            .map_err(|e| format!("invalid settings file {}: {e}", path.display()))?;
        match root.get("tools") {
            None => Ok(Self::default()),
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|e| format!("invalid settings file {}: tools: {e}", path.display())),
        }
    }

    /// Persist THIS section as the `"tools"` key of the settings file at
    /// `path`, preserving every other key in the file byte-for-byte at the
    /// value level — the exact contract (and the exact shape) of
    /// [`AgentEngineConfig::save_to`], because a settings editor that rewrote
    /// the whole file would silently drop `providers`, `hooks`, `mcp`, and
    /// every forward-compat key it has never heard of.
    ///
    /// An EMPTY section removes the key rather than writing `{}`: "no switches"
    /// is the shipped posture, and the file should read as though the editor
    /// had never been opened.
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if toml_config::path_is_toml(path) {
            let user_private = user_config_path().as_deref() == Some(path);
            let value = (!self.entries.is_empty()).then_some(self);
            return toml_io::save_section(path, "tools", value, user_private);
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
        if self.entries.is_empty() {
            object.remove("tools");
        } else {
            let value =
                serde_json::to_value(self).map_err(|e| format!("cannot serialize tools: {e}"))?;
            object.insert("tools".to_string(), value);
        }
        let mut rendered = serde_json::to_string_pretty(&root)
            .map_err(|e| format!("cannot render settings: {e}"))?;
        rendered.push('\n');
        let user_private = user_settings_path().as_deref() == Some(path);
        // Same split as `AgentEngineConfig::save_to`: owner-only atomic writes
        // are reserved for the user scope, which can hold credentials.
        private::write_settings(path, rendered.as_bytes(), user_private)
    }
}

/// The `ui` section of settings.json — appearance preferences. All fields
/// optional so an absent section behaves exactly as the defaults.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct UiSettings {
    /// The TUI colour theme slug (`stella-dark` | `stella-light`). Unset — or
    /// unrecognised — falls back to the default (`stella-dark`, Phosphor Gold
    /// on Ink; the comet brand kit at `docs/brand/BRAND.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
}

impl UiSettings {
    /// Persist THIS section as the `"ui"` key of the settings file at `path`,
    /// preserving every other key byte-for-byte at the value level — the same
    /// read-modify-write contract as [`ToolsSettings::save_to`] and
    /// [`AgentEngineConfig::save_to`], so a theme write never drops
    /// `providers`, `hooks`, or any forward-compat key. An empty section
    /// removes the key rather than writing `{}`.
    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if toml_config::path_is_toml(path) {
            let user_private = user_config_path().as_deref() == Some(path);
            let value = self.theme.is_some().then_some(self);
            return toml_io::save_section(path, "ui", value, user_private);
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
        if self.theme.is_none() {
            object.remove("ui");
        } else {
            let value =
                serde_json::to_value(self).map_err(|e| format!("cannot serialize ui: {e}"))?;
            object.insert("ui".to_string(), value);
        }
        let mut rendered = serde_json::to_string_pretty(&root)
            .map_err(|e| format!("cannot render settings: {e}"))?;
        rendered.push('\n');
        let user_private = user_settings_path().as_deref() == Some(path);
        private::write_settings(path, rendered.as_bytes(), user_private)
    }
}

/// The `mcp` section of settings.json. All fields optional so an absent
/// section behaves exactly as the defaults.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct McpSettings {
    /// Base URL of an MCP Server Registry API (the frozen `GET /v0.1/servers`
    /// contract). Any registry serving that shape works; unset means the
    /// official registry ([`stella_mcp::DEFAULT_REGISTRY_URL`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_url: Option<String>,
}

impl Settings {
    /// The merged `tools` section as the policy the runtime enforces. The
    /// managed ceiling is already folded in by [`Settings::load`], so this is
    /// the whole answer — no caller needs to re-apply authority.
    pub fn tool_policy(&self) -> stella_tools::policy::ToolPolicy {
        self.tools
            .as_ref()
            .map(ToolsSettings::policy)
            .unwrap_or_default()
    }

    /// Whether the end-of-run recap is enabled for this workspace. Default
    /// off; only an explicit `"enable_recap": "on"` in the scope chain turns
    /// it on (a later `"off"` turns it back off — project wins per field).
    pub fn recap_enabled(&self) -> bool {
        self.enable_recap.is_some_and(Toggle::is_on)
    }

    /// Whether trajectory trace capture (#1042) is enabled. Default off;
    /// only an explicit `"trace_capture": "on"` in the scope chain turns it
    /// on (a later `"off"` turns it back off — project wins per field).
    pub fn trace_capture_enabled(&self) -> bool {
        self.trace_capture.is_some_and(Toggle::is_on)
    }

    /// The resolved `create_worktrees` policy. An absent key means `ask`, the
    /// same as an empty or null one — see [`CreateWorktrees`].
    pub fn create_worktrees(&self) -> CreateWorktrees {
        self.create_worktrees.unwrap_or_default()
    }

    /// The persisted TUI colour-theme slug (`ui.theme`), if any. `None` means
    /// no preference was saved — the caller applies the default. The value is
    /// the raw slug; validation is the reader's job ([`stella_tui::theme`]).
    pub fn ui_theme(&self) -> Option<&str> {
        self.ui.as_ref().and_then(|u| u.theme.as_deref())
    }

    /// The configured MCP registry URL, or the official default. Applied at the
    /// read site (the house convention) rather than baked into serde.
    pub fn mcp_registry_url(&self) -> String {
        self.mcp
            .as_ref()
            .and_then(|m| m.registry_url.as_deref())
            .filter(|u| !u.trim().is_empty())
            .unwrap_or(stella_mcp::DEFAULT_REGISTRY_URL)
            .to_string()
    }
}

/// Which project-scope trust boundaries are open this process.
///
/// `STELLA_TRUST_PROJECT=1` opens both; `STELLA_PROJECT_HOOKS=1` is the
/// legacy hooks-only flag kept working for back-compat. Only a truthy value
/// (`1`/`true`/`yes`/`on`, case-insensitive) counts as set — an explicit
/// `false`/`no`/`off`/`0` means what it says. Trust gates must not open on a
/// value spelled to close them.
#[derive(Clone, Copy)]
struct ProjectTrust {
    hooks: bool,
    credentials: bool,
}

fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| truthy_flag(&v))
}

/// The pure predicate behind [`env_flag`], split out so it is testable
/// without mutating the process environment (POSIX setenv/getenv races are
/// undefined under the concurrent test runner). Same truthy vocabulary as
/// `agent::output::is_truthy_env_value`: before this existed, `env_flag`
/// treated any non-`"0"` value as set, so `STELLA_TRUST_PROJECT=false`
/// opened the project-trust boundary it was written to keep closed.
fn truthy_flag(value: &std::ffi::OsStr) -> bool {
    value.to_str().is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Whether the trusted launcher enabled the benchmark's filesystem-isolation
/// boundary. Settings/config use it to disable filesystem configuration and
/// credentials; session assembly also uses it to exclude Stella-specific
/// prompt steering, executable extensions, and persisted learning state.
pub(crate) fn filesystem_settings_disabled() -> bool {
    #[cfg(test)]
    {
        TEST_FILESYSTEM_ISOLATION.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        env_flag("STELLA_NO_SETTINGS")
    }
}

// Unit tests exercise many prompt/rule/memory loaders concurrently. A process
// environment toggle would make unrelated tests observe claim mode (and POSIX
// setenv/getenv races are undefined), so tests use a thread-scoped equivalent
// of the production launcher signal.
#[cfg(test)]
std::thread_local! {
    static TEST_FILESYSTEM_ISOLATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) struct TestFilesystemIsolationGuard {
    previous: bool,
}

#[cfg(test)]
pub(crate) fn test_filesystem_isolation(enabled: bool) -> TestFilesystemIsolationGuard {
    let previous = TEST_FILESYSTEM_ISOLATION.replace(enabled);
    TestFilesystemIsolationGuard { previous }
}

#[cfg(test)]
impl Drop for TestFilesystemIsolationGuard {
    fn drop(&mut self) {
        TEST_FILESYSTEM_ISOLATION.set(self.previous);
    }
}

/// Home directory used by Stella-specific user-scope extension loaders.
/// Centralizing it keeps claim isolation and test injection consistent across
/// rules, skills, and custom tools.
pub(crate) fn user_home_dir() -> Option<PathBuf> {
    #[cfg(test)]
    {
        TEST_USER_HOME.with(|home| home.borrow().clone())
    }
    #[cfg(not(test))]
    {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

#[cfg(test)]
std::thread_local! {
    static TEST_USER_HOME: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct TestUserHomeGuard {
    previous: Option<PathBuf>,
}

#[cfg(test)]
pub(crate) fn test_user_home(path: PathBuf) -> TestUserHomeGuard {
    let previous = TEST_USER_HOME.with(|home| home.replace(Some(path)));
    TestUserHomeGuard { previous }
}

#[cfg(test)]
impl Drop for TestUserHomeGuard {
    fn drop(&mut self) {
        TEST_USER_HOME.with(|home| {
            home.replace(self.previous.take());
        });
    }
}

fn project_trust() -> ProjectTrust {
    let all = env_flag("STELLA_TRUST_PROJECT");
    ProjectTrust {
        hooks: all || env_flag("STELLA_PROJECT_HOOKS"),
        credentials: all,
    }
}

/// Whether this process trusts the current project to run code it configures.
/// Governs project-scope lifecycle hooks and the MCP servers declared in
/// `.stella/mcp.toml` — both spawn processes / open connections that a cloned
/// repo must not be able to start silently. Same gate as project hooks:
/// `STELLA_TRUST_PROJECT=1` (or the legacy hooks-only `STELLA_PROJECT_HOOKS=1`).
pub(crate) fn project_code_execution_trusted() -> bool {
    project_trust().hooks
}

#[cfg(test)]
mod tests;
