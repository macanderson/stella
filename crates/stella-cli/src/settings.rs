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

use crate::reward::{OutcomeWeights, RewardPolicy, RewardShaping};
use serde::{Deserialize, Serialize};
use stella_core::hooks::Hooks;
use stella_protocol::{ReasoningEffort, ServiceTier, Verbosity};

use crate::config::Dialect;

mod authority;
mod context;
pub(crate) mod context_providers;
// The `agent_engine_config` cluster, split out when this file crossed the
// size ratchet; its items are re-exported below so callers' paths are
// unchanged.
mod engine;
mod managed;
mod merge;
pub(crate) mod migrate;
mod private;
mod steering;
pub(crate) mod toml_config;
pub(crate) mod toml_io;
mod unknown;
mod withheld;
/// Persist a rendered config file through this module's own write discipline —
/// the one door for a writer that lives outside it.
///
/// `plugin_cmd::configure` writes `stella.toml` when a package's `[[configure]]`
/// table is applied or reverted (#3999). It could not simply call
/// `std::fs::write`: the symlink refusal and the atomic, mode-preserving write
/// are the properties that make a settings write safe, and a second writer that
/// re-derived them would be free to get one wrong. Exposed as one function
/// rather than by widening [`private`]'s two, so the pair cannot be used apart.
pub(crate) fn write_config_file(
    path: &std::path::Path,
    bytes: &[u8],
    user_private: bool,
) -> Result<(), String> {
    private::reject_symlink(path)?;
    private::write_settings(path, bytes, user_private)
}

pub use authority::{AuthorityPolicy, ManagedAuthoritySettings};
pub use toml_config::ConfigScope;
pub(crate) use unknown::{
    ENGINE_AGENT_FIELDS, ENGINE_AGENT_NAMES, ENGINE_PARAM_FIELDS, ENGINE_ROOT_FIELDS,
    RETIRED_ENGINE_AGENT_NAMES, RETIRED_ENGINE_ROOT,
};
// `ContextSettings`, `RetrievalSettings`, and `InferredDirectivePromotion`
// all have readers now (`memory::tuning` — retrieval budgets, the lifecycle
// switch, promotion thresholds). The remaining nested types (`LearningMode`,
// `GovernanceMode`, …) live in `settings::context` until wired.
pub use context::{ContextSettings, InferredDirectivePromotion, RetrievalSettings};
pub use context_providers::{ContextProviderSettings, ExternalContextProvider, ProviderEndpoint};
pub use engine::*;
pub use merge::ToolScopePolicies;
pub(crate) use steering::SteeringCeiling;
pub(crate) use withheld::WithheldNotice;

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
    /// Pin a *gateway* provider to these upstreams, in preference order, and
    /// forbid it from falling back to any other (OpenRouter
    /// `provider.order` + `allow_fallbacks: false`). Honored today by the
    /// OpenAI-compatible arm when it addresses OpenRouter; inert elsewhere,
    /// because a direct endpoint has no upstream to choose.
    ///
    /// Absent — the normal case — lets the gateway route wherever it likes.
    /// Set it when two runs must be comparable: the gateway picks per app
    /// identity and can serve the same slug from a different vendor between
    /// runs, which silently varies the model provider a head-to-head claims
    /// to hold fixed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_pin: Option<Vec<String>>,
    /// Wire dialect for config-defined providers. Defaults to
    /// `openai-compatible`; ignored for built-in overrides (a built-in's
    /// dialect is fixed by its adapter).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dialect: Option<Dialect>,
    /// Prompt-cache window pin (`"5m"` / `"1h"`, #1839). Absent — the normal
    /// case — lets the session surface decide: the interactive deck/REPL asks
    /// for the 1-hour window (human-paced gaps outlive 5 minutes; losing the
    /// prefix re-bills it whole), headless runs keep the 5-minute provider
    /// default (seconds-long gaps would pay the 2x write premium for
    /// nothing). Honored today by the `anthropic` provider only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_ttl: Option<stella_model::CacheTtl>,
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
        take!(upstream_pin);
        take!(dialect);
        take!(cache_ttl);
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
    /// verifier / triage), plus the allowed-model vocabulary and the auto
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
    /// `on` (the default when the key is absent) = the workspace probe skips
    /// paths the repository's own `.gitignore` excludes, so build churn is
    /// never walked, recorded as file touches, or fed to the diff surfaces.
    /// `off` restores the unfiltered walk. Outside a git repository the
    /// switch is inert — nothing is ignored, so the probe stays fully
    /// sighted. Last-wins across scopes, like `enable_recap`.
    #[serde(default)]
    pub ignore_gitignore: Option<Toggle>,
    /// `always` / `ask` / `never` — whether a run does its work in a throwaway
    /// git worktree instead of the working tree. Absent, null, or empty means
    /// `ask`, and the question is put once, at triage, only when the run is
    /// actually going to change files. See [`CreateWorktrees`].
    #[serde(default)]
    pub create_worktrees: Option<CreateWorktrees>,
    /// Directories OUTSIDE the workspace root that write tools may touch
    /// (`stella.toml`'s `[workspace] allowed_dirs`). Relative entries resolve
    /// against the project root at the read site
    /// ([`Settings::allowed_write_dirs`]) rather than at parse time: a scope
    /// is parsed before the run knows which workspace it is for, so resolving
    /// here would bake the wrong root into a user-scope entry.
    ///
    /// Last-wins per scope like every other field — a project that states its
    /// own list replaces the user's rather than inheriting half of it, because
    /// a write permission nobody wrote in full is exactly the grant an
    /// operator must not be surprised by. The CLI's `--allow-dir` is additive
    /// on top of whatever this resolves to; see `crate::write_dirs`.
    #[serde(default)]
    pub allowed_dirs: Option<Vec<String>>,
    /// Appearance preferences — currently just the TUI colour theme
    /// (`/theme`). Whole-block last-wins across scopes; carries no authority.
    #[serde(default)]
    pub ui: Option<UiSettings>,
    /// What a turn's verdict is worth as a training label (#1043). Whole-block
    /// last-wins across scopes; carries no credential or egress authority, so
    /// no trust restoration — a project setting a weight is expressing an
    /// opinion about its own verifier, not borrowing permission.
    #[serde(default)]
    pub reward: Option<RewardSettings>,
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
    /// `plugins.<name>` — the per-plugin switch a scope uses to **retract**
    /// an installed plugin (`doc:pipeline-as-plugins` §A4). Merged per key,
    /// and `off` is sticky: once any scope retracts a plugin, no later scope
    /// restores it. A name absent from every scope means the plugin runs if
    /// it is installed.
    ///
    /// **This is the property `hooks` deliberately does not have**, and the
    /// asymmetry is the point. Hook matchers concatenate across scopes because
    /// an operator gate must not be removable by a lower-precedence file. A
    /// plugin is not an operator gate: it is a third party's process, it is
    /// *owned*, and "uninstalled" has to mean uninstalled. Deleting the
    /// directory retracts a plugin within its own scope; this switch is how a
    /// project retracts one the user scope installed, which is the case no
    /// deletion can reach without editing someone else's home.
    ///
    /// Sticky-`off` rather than last-wins is what keeps this **outside** the
    /// project trust boundary safely: the only thing a cloned repository can
    /// do with this key is stop a plugin from running on the machine that
    /// cloned it. It can never re-enable one the user switched off, so there
    /// is no direction in which an untrusted file grants anything. To restore
    /// a retracted plugin, delete the `off` line — which is an edit to a file
    /// you own, deliberately.
    #[serde(default)]
    pub plugins: BTreeMap<String, Toggle>,
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
    /// Whether the ORG's managed scope permits steering at all, captured from
    /// the managed snapshot alone by [`Settings::merge_captured_scopes`].
    ///
    /// Separate from `context.steering.enabled` because the two answer
    /// different questions and compose in one direction only: the block is
    /// the ordinary last-wins chain that any scope — and the environment —
    /// may move, while this is the ceiling over it. Folding the ceiling into
    /// the block would make them indistinguishable, and then an env var that
    /// is *supposed* to re-enable steering after a project turned it off
    /// could not tell that case apart from the org having forbidden it.
    #[serde(skip)]
    steering_ceiling: SteeringCeiling,
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
/// The policy used to be consumed at triage (`Pipeline::isolate_in_worktree`)
/// by the staged pipeline's candidate-workspace isolation — removed along
/// with that crate (#3865). The setting itself is left in place (still
/// parses, still validates) since retiring a settings.toml key is a schema
/// decision of its own; see the filed issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CreateWorktrees {
    Always,
    #[default]
    Ask,
    Never,
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

/// The user-scope settings path (`~/.stella/settings.json`), when
/// `HOME` is known — the TUI save target for user-scope edits and the
/// first file of [`Settings::load`]'s chain.
pub fn user_settings_path() -> Option<PathBuf> {
    crate::paths::stella_root().map(|root| root.join("settings.json"))
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

/// The `tools` section of settings.json — the one place a tool is switched
/// off, whether it is a built-in, an MCP server's, or one the customer wrote.
///
/// An **open map**, not a fixed set of fields: a fixed field set could
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

/// The `reward` section of settings.json — what a turn's verdict is worth when
/// it becomes a training label (#1043).
///
/// Every field optional, and an absent field means the stated default rather
/// than zero: a workspace that only wants to price steps differently writes
/// `per_step` alone and inherits the rest.
///
/// There used to be a `verifier_weight` here, priced against
/// `deterministic_weight`, because the ladder had a tier whose magnitude came
/// from a model verifier's opinion. That call is gone and so is the tier, so
/// the key is retired rather than kept as a no-op: an accepted key that steers
/// nothing is the settings failure mode this surface exists to avoid, and a
/// retired one is at least reported by name by the unrecognized-key pass
/// (#2616). See [`crate::reward`] for the full argument.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct RewardSettings {
    /// Magnitude of a deterministic pass or fail. Default `1.0`, and the unit
    /// the shaping prices are measured against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deterministic_weight: Option<f64>,
    /// Reward subtracted per model call. Default `0.02`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_step: Option<f64>,
    /// Reward subtracted per USD spent. Default `0.5`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_usd: Option<f64>,
    /// Reward subtracted per verification round beyond the first. Default
    /// `0.1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_revision: Option<f64>,
}

impl RewardSettings {
    /// Fill the absent fields from the defaults and validate the result.
    ///
    /// The error is the message a person sees at launch, so it names the key
    /// they have to change and what the rule is — never just "invalid".
    pub fn resolve(&self) -> Result<RewardPolicy, String> {
        let defaults = RewardPolicy::default();
        let policy = RewardPolicy {
            outcome: OutcomeWeights {
                deterministic: self
                    .deterministic_weight
                    .unwrap_or(defaults.outcome.deterministic),
            },
            shaping: RewardShaping {
                per_step: self.per_step.unwrap_or(defaults.shaping.per_step),
                per_usd: self.per_usd.unwrap_or(defaults.shaping.per_usd),
                per_revision: self.per_revision.unwrap_or(defaults.shaping.per_revision),
            },
        };
        policy
            .validate()
            .map_err(|error| format!("settings `reward`: {}", error.explain()))?;
        Ok(policy)
    }
}

/// The `ui` section of settings.json — appearance preferences. All fields
/// optional so an absent section behaves exactly as the defaults.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct UiSettings {
    /// The TUI colour theme slug (`stella-dark` | `stella-light`). Unset — or
    /// unrecognised — falls back to the default (`stella-dark`, gold on the
    /// cool near-black ramp; the product palette at
    /// `crates/stella-tui/src/palette.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// What a plain prompt typed at a running agent does: `queue` (the
    /// default — wait as the lead's next turn, so Esc can steer the backlog
    /// into the running turn), `ask` (raise the routing card), or `spawn`
    /// (fork a sidecar lane, the original behaviour). Unset or unrecognised
    /// keeps the default; the reader validates
    /// ([`stella_tui::deck_ui::MidTurnPrompt::parse`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mid_turn_prompt: Option<String>,
}

impl UiSettings {
    /// Whether every field is unset — the section would serialize to `{}`,
    /// and the writers drop the key instead.
    pub fn is_empty(&self) -> bool {
        self.theme.is_none() && self.mid_turn_prompt.is_none()
    }
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
            let value = (!self.is_empty()).then_some(self);
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
        if self.is_empty() {
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

    /// Whether the workspace probe skips what the repository's own ignore
    /// rules exclude. **Defaults on** — an absent key means the filter runs;
    /// only an explicit `"ignore_gitignore": "off"` in the scope chain
    /// restores the unfiltered walk.
    pub fn ignore_gitignore(&self) -> bool {
        self.ignore_gitignore.is_none_or(Toggle::is_on)
    }

    /// The resolved reward policy for this workspace (#1043). An absent
    /// `reward` block is exactly the defaults.
    ///
    /// Returns `Err` rather than clamping. A weight the module refuses is a
    /// mistake someone has to see: silently substituting a legal value would
    /// produce correctly-shaped labels that mean something the operator never
    /// asked for, which is worse than not starting.
    pub fn reward_policy(&self) -> Result<RewardPolicy, String> {
        self.reward.clone().unwrap_or_default().resolve()
    }

    /// The resolved `create_worktrees` policy. An absent key means `ask`, the
    /// same as an empty or null one — see [`CreateWorktrees`].
    pub fn create_worktrees(&self) -> CreateWorktrees {
        self.create_worktrees.unwrap_or_default()
    }

    /// The extra write directories this run is granted: the configured list
    /// UNIONED with `extra` (the invocation's `--allow-dir`), each resolved to
    /// an absolute path against `workspace_root`. An absent key and an empty
    /// `extra` give an empty list — the workspace root alone, the default
    /// scope.
    ///
    /// Resolution happens here rather than at parse time because a scope file
    /// is read before the run's root is known (see the field's docs), and the
    /// union happens here rather than at the call site so no consumer can
    /// apply one source without the other.
    pub fn allowed_write_dirs(&self, workspace_root: &Path, extra: &[String]) -> Vec<PathBuf> {
        crate::write_dirs::resolve(
            workspace_root,
            self.allowed_dirs.as_deref().unwrap_or(&[]),
            extra,
        )
    }

    /// The persisted TUI colour-theme slug (`ui.theme`), if any. `None` means
    /// no preference was saved — the caller applies the default. The value is
    /// the raw slug; validation is the reader's job ([`stella_tui::theme`]).
    pub fn ui_theme(&self) -> Option<&str> {
        self.ui.as_ref().and_then(|u| u.theme.as_deref())
    }

    /// The persisted mid-turn prompt policy slug (`ui.mid_turn_prompt`), if
    /// any. Raw like [`Settings::ui_theme`]: the deck parses and applies its
    /// default ([`stella_tui::deck_ui::MidTurnPrompt`]).
    pub fn ui_mid_turn_prompt(&self) -> Option<&str> {
        self.ui.as_ref().and_then(|u| u.mid_turn_prompt.as_deref())
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

pub(crate) fn env_flag(name: &str) -> bool {
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
    crate::paths::filesystem_settings_disabled()
}

fn project_trust() -> ProjectTrust {
    let all = env_flag("STELLA_TRUST_PROJECT");
    ProjectTrust {
        hooks: all || env_flag("STELLA_PROJECT_HOOKS"),
        credentials: all,
    }
}

/// Whether this process trusts the current project to run code it configures.
///
/// **The normative enumeration of the code-execution boundary.** Every surface
/// a cloned repository could use to start a process on the reader's machine is
/// listed here with the call site that gates it; the README cites this comment
/// rather than restating the list, so there is one copy to keep correct. A PR
/// that adds a gated surface adds its row here in the same change.
///
/// | Surface | Gated at |
/// |---|---|
/// | Project-scope lifecycle hooks — an untrusted scope keeps only what the user/managed scopes declared | `settings::merge::merge_captured_scopes` |
/// | Project-scope `context_providers` — a stdio entry spawns its `command` at admission, an http entry supplies its own `egress_consent` for a payload carrying workspace content | the same function, immediately below hooks |
/// | The MCP servers declared in `.stella/mcp.toml` — an arbitrary stdio `command` run at session start, or an attacker-chosen http endpoint | `agent::load_mcp_plan` |
/// | `<workspace>/.stella/plugins/` — a plugin declares a program the host spawns and can arbitrate the agent loop | `plugin_cmd::roster::read_project_tier` |
/// | `stella self-driving`'s issue work — refused outright when the workspace declares context records that would not reach the turn, because unsteered work is indistinguishable from steered work in the pull request it produces | `self_driving_cmd::work::refuse_if_unsteered` |
///
/// `stella plugin install --scope project` consults it once more, to *warn*
/// rather than gate: the copy onto disk is the operator's own act, but the
/// loader will refuse to read the tier back, and "installed and inert" must
/// not look like "installed and running" (`plugin_cmd::install`).
///
/// Opened by `STELLA_TRUST_PROJECT=1`, or by the legacy hooks-only
/// `STELLA_PROJECT_HOOKS=1`.
///
/// **Not this gate:** credential routing (`base_url`, `api_key`,
/// `api_key_env`, `mcp.registry_url`), project custom tools and project system
/// prompts ride `ProjectTrust::credentials`, which `STELLA_PROJECT_HOOKS`
/// deliberately does not open — a user who asked only for hooks did not ask to
/// have their API key re-pointed. Reading this predicate as "the project trust
/// flag" would grade those as trusted one flag too early.
pub(crate) fn project_code_execution_trusted() -> bool {
    project_trust().hooks
}

// `cfg(doc)` keeps this module visible to `cargo doc`: merge.rs and
// unknown.rs carry intra-doc links to `super::completeness`, and rustdoc
// does not set `cfg(test)` while building docs, so a plain `cfg(test)`
// gate would leave those links broken outside a test build.
#[cfg(any(test, doc))]
mod completeness;
#[cfg(test)]
mod tests;
