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
mod private;
#[cfg(test)]
#[path = "settings/private_state_tests.rs"]
mod private_state_tests;
pub use authority::{AuthorityPolicy, ManagedAuthoritySettings};
// Only `ContextSettings` is consumed today (the inert `Settings::context`
// field). The nested types (`LearningMode`, `GovernanceMode`, …) live in
// `settings::context`; a later phase re-exports them here as it wires them in.
pub use context::ContextSettings;
pub use context_providers::{ContextProviderSettings, ExternalContextProvider, ProviderEndpoint};
pub use merge::ToolScopePolicies;

/// One `providers.<id>` entry. Every field is optional at the schema level;
/// which ones are *required* depends on whether the id names a built-in
/// (override: any subset is fine) or defines a new provider (`base_url`
/// must be present). `config.rs` enforces that split.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct ProviderSettings {
    /// Optional restatement of the map key (the issue's examples carry it);
    /// when present it must match the key, so a copy-paste of one entry
    /// under a new key can't silently configure the wrong provider.
    pub id: Option<String>,
    /// Display name (`ProviderConfig::display_name`).
    pub name: Option<String>,
    pub base_url: Option<String>,
    /// A literal credential. Sits below env vars and above the interactive
    /// prompt in the chain, mirroring the credentials file. Prefer
    /// `api_key_env` for anything long-lived — settings.json is often
    /// committed, credentials should not be.
    pub api_key: Option<String>,
    /// Name of an environment variable to read the credential from.
    pub api_key_env: Option<String>,
    pub default_model: Option<String>,
    /// Wire dialect for config-defined providers. Defaults to
    /// `openai-compatible`; ignored for built-in overrides (a built-in's
    /// dialect is fixed by its adapter).
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
    /// Adaptive-context lifecycle configuration (the `context` block). INERT
    /// in Phase 0: deserialized, round-tripped, and merged, but read by no
    /// code yet — `context.lifecycle.enabled` defaults `false`, which is what
    /// preserves current behavior. `None` = the block was absent. Whole-block
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
    /// `on` = a headless run proceeds past scope review instead of stopping
    /// at `ScopeReviewRequiredHeadless`.
    ///
    /// Off by default, and deliberately so: scope review is the gate that
    /// keeps a large blast radius from landing unattended, and a headless
    /// surface has nobody to ask. Turn it on only where the working tree is
    /// disposable and the budget cap is the real guard — a benchmark
    /// container, CI on a scratch checkout. Without it, any plan over the
    /// scope thresholds (more than 5 steps by default) ends the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headless_scope_bypass: Option<Toggle>,
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

/// The `mcp` section of settings.json. All fields optional so an absent
/// section behaves exactly as the defaults.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct McpSettings {
    /// Base URL of an MCP Server Registry API (the frozen `GET /v0.1/servers`
    /// contract). Any registry serving that shape works; unset means the
    /// official registry ([`stella_mcp::DEFAULT_REGISTRY_URL`]).
    #[serde(default)]
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
/// legacy hooks-only flag kept working for back-compat. A value of `0` or
/// empty does not count as set.
#[derive(Clone, Copy)]
struct ProjectTrust {
    hooks: bool,
    credentials: bool,
}

fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty() && v != "0")
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
