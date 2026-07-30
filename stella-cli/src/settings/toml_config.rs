//! `stella.toml` — the TOML config document, and how it lowers into
//! [`Settings`].
//!
//! # Why a separate document type
//!
//! This is deliberately NOT `#[derive(Deserialize)]` on [`Settings`] itself.
//! The two shapes genuinely differ — the TOML document flattens
//! `agent_engine_config.agents.<name>` to `[agents.<name>]`, moves
//! `allowed_models` to `[models].allowed`, gives the bare `enable_recap` root
//! scalar a table home at `[run].recap`, and folds the whole of
//! `.stella/mcp.toml` in under `[mcp.servers]`.
//!
//! Teaching one type to deserialize from both shapes would mean serde
//! attributes that are correct for one format and misleading for the other.
//! Instead this module owns the TOML shape, lowers it into the existing
//! `Settings` (which the whole merge/trust/authority chain already consumes
//! unchanged), and leaves the JSON path completely untouched — so the port
//! carries no regression risk for anyone still on `settings.json`.
//!
//! # What is NOT re-declared here
//!
//! Every block whose shape is identical across the two formats reuses the
//! existing type verbatim: [`ProviderSettings`], [`ToolsSettings`], [`Hooks`],
//! [`ContextSettings`], [`ContextProviderSettings`], [`ManagedAuthoritySettings`],
//! [`UiSettings`], and [`AgentEngineAgent`]. Their serde renames (`type`,
//! `timeoutMs`, kebab-case dialects, lowercase toggles) are format-agnostic and
//! work in TOML as written.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use stella_core::hooks::Hooks;
use stella_mcp::config::{McpConfig, McpServerEntry};

use super::authority::ManagedAuthoritySettings;
use super::context::ContextSettings;
use super::context_providers::ContextProviderSettings;
use super::{
    AgentEngineAgent, AgentEngineAgents, AgentEngineConfig, McpSettings, ProviderSettings,
    Settings, Toggle, ToolsSettings, UiSettings,
};

/// The schema version this build writes and understands.
///
/// A file with no `[meta]` block is version 1 by definition — that is what
/// makes it safe to eventually stop reading `settings.json`, because a file
/// predating the field is unambiguously the original shape.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Which tier a config file was loaded from. The file's LOCATION decides this;
/// `[meta].scope` only restates it, and a mismatch is an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigScope {
    User,
    Managed,
    Project,
}

impl ConfigScope {
    pub fn label(self) -> &'static str {
        match self {
            ConfigScope::User => "user",
            ConfigScope::Managed => "managed",
            ConfigScope::Project => "project",
        }
    }
}

/// `[meta]` — the migration seam.
///
/// `settings.json` has no version field, so a typo and a key from a future
/// release are indistinguishable to serde; the only defense is the advisory
/// warning in [`super::unknown`]. This block turns forward compatibility from a
/// convention into a mechanism.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct Meta {
    #[serde(default)]
    pub schema_version: Option<u32>,
    /// Optional self-declaration, checked against the file's location.
    #[serde(default)]
    pub scope: Option<ConfigScope>,
}

/// `[run]` — root-level run behavior.
///
/// Exists so `enable_recap` has a table to live in. A bare key at the document
/// root is a TOML footgun: it attaches to whatever `[table]` precedes it, so
/// moving one line in the file silently reassigns it. Drafting the reference
/// config, `enable_recap` written beside `[ui]` landed inside `[authority]`
/// and was caught only because that struct happens to be the one
/// `deny_unknown_fields` type in the schema.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct RunSection {
    /// `on` = print an end-of-run recap in text mode. Was `enable_recap`.
    #[serde(default)]
    pub recap: Option<Toggle>,
}

/// `[models]` — policy over the model catalog, not a model table.
///
/// The catalog (`~/.stella/catalog.db`) is the authority on what models exist
/// and what they cost. This block says only which of them this workspace may
/// use. Keeping the two apart is what stops a hand-written model list from
/// rotting the week a model ships.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct ModelsSection {
    /// Was `agent_engine_config.allowed_models`. REPLACES wholesale across
    /// scopes rather than concatenating — it is one vocabulary, and merging
    /// would make it impossible for a project to narrow the user's list.
    #[serde(default)]
    pub allowed: Option<Vec<String>>,
}

/// `[agents]` — the flattened engine config.
///
/// The JSON nests per-agent config one level below the flat model fields
/// (`agent_engine_config.agents.judge`); rendered literally that is
/// `[agents.agents.judge]`. Flattening is safe because the agent set is CLOSED
/// — the four names are struct fields, not map keys, so a per-agent table can
/// never collide with a root field. Opening that set (a later phase) means
/// either restoring the nesting or reserving these names explicitly.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct AgentsSection {
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub pipeline_judge_model: Option<String>,
    #[serde(default)]
    pub pipeline_worker_model: Option<String>,
    #[serde(default)]
    pub pipeline_triage_model: Option<String>,
    #[serde(default)]
    pub auto_mode: Option<Toggle>,
    #[serde(default)]
    pub effort_auto: Option<Toggle>,
    #[serde(default)]
    pub reasoning_auto: Option<Toggle>,
    #[serde(default)]
    pub headless_scope_bypass: Option<Toggle>,

    // The four built-in agents, as tables.
    #[serde(default)]
    pub default: Option<AgentEngineAgent>,
    #[serde(default)]
    pub worker: Option<AgentEngineAgent>,
    #[serde(default)]
    pub judge: Option<AgentEngineAgent>,
    #[serde(default)]
    pub triage: Option<AgentEngineAgent>,
}

impl AgentsSection {
    /// `true` when nothing in this section was set — used to avoid
    /// materializing an all-`None` `agent_engine_config`, which would look
    /// like "the user configured an empty engine" to the merge.
    fn is_empty(&self) -> bool {
        *self == AgentsSection::default()
    }
}

/// `[mcp]` — the registry URL plus the whole of `.stella/mcp.toml`.
///
/// `mcp.toml` is already TOML with a `[servers.<name>]` table, so it folds in
/// under a prefix with no shape change at all. This is the cheapest
/// consolidation in the migration.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct McpSection {
    #[serde(default)]
    pub registry_url: Option<String>,
    #[serde(default)]
    pub servers: BTreeMap<String, McpServerEntry>,
}

/// A whole `stella.toml` document.
///
/// Every field optional so an empty file is exactly the defaults, and unknown
/// keys are tolerated (forward compatibility) with [`super::unknown`] supplying
/// the warning that a typo would otherwise get for free.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct TomlConfig {
    #[serde(default)]
    pub meta: Meta,
    #[serde(default)]
    pub run: RunSection,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderSettings>,
    #[serde(default)]
    pub models: ModelsSection,
    #[serde(default)]
    pub agents: AgentsSection,
    #[serde(default)]
    pub tools: Option<ToolsSettings>,
    #[serde(default)]
    pub hooks: Option<Hooks>,
    #[serde(default)]
    pub mcp: Option<McpSection>,
    #[serde(default)]
    pub context: Option<ContextSettings>,
    #[serde(default)]
    pub context_providers: ContextProviderSettings,
    #[serde(default)]
    pub ui: Option<UiSettings>,
    /// Honored only from the managed tier; see [`Settings::managed_authority`].
    #[serde(default)]
    pub authority: Option<ManagedAuthoritySettings>,
    #[serde(default)]
    pub enterprise_telemetry: Option<toml::Value>,
}

impl TomlConfig {
    /// Parse a document, mapping a syntax error to a named, path-bearing
    /// message. A malformed config is a hard error rather than a silent skip:
    /// a typo that quietly reverted someone to a built-in endpoint would be far
    /// worse than a loud parse failure.
    pub fn parse(contents: &str, path: &Path) -> Result<Self, String> {
        toml::from_str(contents)
            .map_err(|e| format!("invalid config file {}: {e}", path.display()))
    }

    /// Validate `[meta]` against the file's actual location.
    ///
    /// Two checks, both fail-closed:
    ///
    /// * an unknown `schema_version` — a file written by a newer stella may use
    ///   keys this build would silently ignore, so refusing is the only honest
    ///   answer;
    /// * a `scope` that disagrees with where the file was found — a project
    ///   file claiming `scope = "managed"` is either a mistake or an attempt to
    ///   borrow authority it does not have.
    pub fn validate_meta(&self, path: &Path, actual: ConfigScope) -> Result<(), String> {
        if let Some(version) = self.meta.schema_version
            && version != CURRENT_SCHEMA_VERSION
        {
            return Err(format!(
                "{}: schema_version {version} is not supported by this stella \
                 (this build reads version {CURRENT_SCHEMA_VERSION}) — upgrade stella, \
                 or pin the file to {CURRENT_SCHEMA_VERSION} if it really is that shape",
                path.display()
            ));
        }
        if let Some(declared) = self.meta.scope
            && declared != actual
        {
            return Err(format!(
                "{}: [meta] scope = \"{}\" but this file was loaded as the {} config. \
                 The file's location decides its scope; remove the field or correct it.",
                path.display(),
                declared.label(),
                actual.label(),
            ));
        }
        Ok(())
    }

    /// Lower this document into the [`Settings`] the merge chain consumes, plus
    /// the MCP servers it declared (empty when the document has no
    /// `[mcp.servers]`, which is how a workspace keeps using `.stella/mcp.toml`
    /// unchanged).
    pub fn into_settings(self) -> (Settings, McpConfig) {
        let TomlConfig {
            meta: _,
            run,
            providers,
            models,
            agents,
            tools,
            hooks,
            mcp,
            context,
            context_providers,
            ui,
            authority,
            enterprise_telemetry,
        } = self;

        let (registry_url, servers) = match mcp {
            Some(section) => (section.registry_url, section.servers),
            None => (None, BTreeMap::new()),
        };

        // `[mcp]` with only servers must not synthesize an `McpSettings` whose
        // `registry_url` is None — that is indistinguishable from the block
        // being absent, and the read site already applies the default. Keep it
        // None so scope merging cannot have an empty block win over a real one.
        let mcp_settings = registry_url.map(|url| McpSettings {
            registry_url: Some(url),
        });

        let agent_engine_config = lower_agents(agents, models);

        let settings = Settings {
            providers,
            hooks,
            mcp: mcp_settings,
            agent_engine_config,
            tools,
            enable_recap: run.recap,
            ui,
            context,
            context_providers,
            managed_authority: authority,
            // Round-trip through JSON: the field is stored untyped and
            // validated fail-open by its own adapter, which speaks
            // `serde_json::Value`. TOML has no null, so this cannot lose
            // information in the direction that matters.
            enterprise_telemetry: enterprise_telemetry
                .and_then(|value| serde_json::to_value(value).ok()),
            authority_policy: Default::default(),
        };

        (settings, McpConfig { servers })
    }
}

/// Read, parse, validate, and lower one scope's `stella.toml`.
///
/// `read` is injected so the managed tier can supply its hardened
/// `O_NOFOLLOW`, ownership- and mode-checked reader instead of a plain
/// `read_to_string`. That check is the whole reason managed settings can carry
/// an authority ceiling, and a TOML path that bypassed it would hand any local
/// user the ability to raise the org's ceiling by dropping a file.
///
/// A missing file is an empty scope, never an error.
pub fn load_toml_scope(
    path: &Path,
    scope: ConfigScope,
    read: impl FnOnce(&Path) -> std::io::Result<String>,
) -> Result<LoadedToml, String> {
    let contents = match read(path) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoadedToml {
                settings: Settings::default(),
                mcp: McpConfig::default(),
                warnings: Vec::new(),
            });
        }
        Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
    };

    let parsed = TomlConfig::parse(&contents, path)?;
    parsed.validate_meta(path, scope)?;

    // The entry's `id` may restate its key, but must then agree with it — so a
    // copy-pasted block under a new key cannot silently configure a different
    // provider. Same rule the JSON loader enforces.
    for (id, entry) in &parsed.providers {
        if let Some(stated) = &entry.id
            && stated != id
        {
            return Err(format!(
                "config file {}: providers.{id} declares id `{stated}` — the \
                 entry's id must match its key",
                path.display()
            ));
        }
    }

    // A root `stella.toml` is committed far more often than `.stella/settings.json`
    // ever was, so the one field that was tolerable purely through obscurity
    // stops being tolerable. Env-var indirection and the 0600 credentials file
    // both already exist; this is a refusal with two named alternatives, not a
    // capability being removed.
    if scope == ConfigScope::Project {
        for (id, entry) in &parsed.providers {
            if entry.api_key.is_some() {
                return Err(format!(
                    "config file {}: providers.{id}.api_key is a literal secret in a \
                     file that belongs in version control. Use `api_key_env = \"...\"` \
                     to name an environment variable, or run \
                     `stella auth set {id}` to store the key in \
                     ~/.stella/credentials.toml (owner-only).",
                    path.display()
                ));
            }
        }
    }

    let mut warnings = Vec::new();
    let server_count = parsed
        .mcp
        .as_ref()
        .map(|mcp| mcp.servers.len())
        .unwrap_or(0);
    if server_count > 0 {
        warnings.push(mcp_servers_not_yet_read(path, server_count));
    }

    let (settings, mcp) = parsed.into_settings();
    Ok(LoadedToml {
        settings,
        mcp,
        warnings,
    })
}

/// The user-scope config (`~/.stella/stella.toml`).
pub fn user_toml_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".stella").join("stella.toml"))
}

/// The project-scope config — `<workspace>/stella.toml`, at the REPOSITORY
/// ROOT rather than under `.stella/`.
///
/// This is a committed, human-edited, reviewable file, and the ecosystem
/// convention for exactly that is the root (`Cargo.toml`, `pyproject.toml`).
/// Burying it three directories deep would make the one config with real
/// authority implications the least likely to be read in a PR diff.
///
/// The consequence is deliberate and handled elsewhere: a file next to
/// `README.md` gets committed far more readily, which is why an inline
/// `api_key` is rejected at this scope (see `Settings::load`).
pub fn project_toml_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join("stella.toml")
}

/// The administrator-managed config.
///
/// `STELLA_MANAGED_SETTINGS` names ONE file whatever its format, so an
/// operator who points it at a `.toml` gets the TOML reader and one who points
/// it at a `.json` keeps the JSON reader — the extension decides, and there is
/// never a second managed file competing with the one they named.
pub fn managed_toml_path() -> PathBuf {
    if let Some(path) = std::env::var_os("STELLA_MANAGED_SETTINGS") {
        return PathBuf::from(path);
    }
    if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/stella/stella.toml")
    } else {
        PathBuf::from("/etc/stella/stella.toml")
    }
}

/// Whether a path names a TOML file.
///
/// The format, not the scope, decides which reader and which key vocabulary
/// apply — so this is used both by the managed reader (which must pick a format
/// rather than probe for two files) and by the unrecognized-key pass.
pub fn path_is_toml(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
}

/// What a scope's TOML file contributed, plus anything worth telling the user.
pub struct LoadedToml {
    pub settings: Settings,
    /// Servers declared under `[mcp.servers]`. **Parsed but not yet consumed**
    /// — see [`mcp_servers_not_yet_read`].
    pub mcp: McpConfig,
    /// Advisory lines the caller prints once. Never a reason to fail a load.
    pub warnings: Vec<String>,
}

/// The Phase-1 boundary, stated out loud rather than silently dropped.
///
/// Folding `[mcp.servers]` into this file touches three call sites with a
/// code-execution trust gate between them (`agent::load_mcp_plan`, the
/// `stella mcp` command surface, and the deck's MCP tab). Getting that wrong
/// re-opens the `git clone && stella` RCE hole the gate exists to close, so it
/// is its own change rather than a rider on the format port.
///
/// Until then a `[mcp.servers]` block parses (so a migrated file is never
/// rejected) and says clearly that it is inert. A silently-ignored server block
/// would be exactly the invisible misconfiguration this config is supposed to
/// make impossible.
pub fn mcp_servers_not_yet_read(path: &Path, count: usize) -> String {
    format!(
        "  ! {}: [mcp.servers] declares {count} server{} that stella does not read yet — \
         MCP servers still load from .stella/mcp.toml. Move them back there until \
         the fold lands, or they will not start.",
        path.display(),
        if count == 1 { "" } else { "s" },
    )
}

/// Fold `[agents]` + `[models]` into the one `AgentEngineConfig` the engine
/// reads. Returns `None` when neither section said anything, so an absent block
/// stays absent rather than becoming an all-`None` object that the scope merge
/// would treat as a real (empty) opinion.
fn lower_agents(agents: AgentsSection, models: ModelsSection) -> Option<AgentEngineConfig> {
    if agents.is_empty() && models.allowed.is_none() {
        return None;
    }

    let per_agent = AgentEngineAgents {
        default: agents.default,
        worker: agents.worker,
        judge: agents.judge,
        triage: agents.triage,
    };
    let agents_field = (per_agent != AgentEngineAgents::default()).then_some(per_agent);

    Some(AgentEngineConfig {
        default_model: agents.default_model,
        pipeline_judge_model: agents.pipeline_judge_model,
        pipeline_worker_model: agents.pipeline_worker_model,
        pipeline_triage_model: agents.pipeline_triage_model,
        allowed_models: models.allowed,
        auto_mode: agents.auto_mode,
        effort_auto: agents.effort_auto,
        reasoning_auto: agents.reasoning_auto,
        headless_scope_bypass: agents.headless_scope_bypass,
        agents: agents_field,
    })
}
