//! `stella.toml` — the TOML config document, and how it lowers into
//! [`Settings`].
//!
//! The design this implements is `doc:config-system` — Phases 0 and 1 of it are
//! built (this document and its lowering, comment-preserving writes in
//! [`super::toml_io`], three-scope discovery, and the dual read with TOML
//! winning whole); Phases 2–6 are not. Read §3.1–§3.2 and §6.1 there before
//! changing the shapes below, because the divergences from [`Settings`] are
//! decisions recorded in that spec rather than accidents of this file.
//!
//! # Why a separate document type
//!
//! This is deliberately NOT `#[derive(Deserialize)]` on [`Settings`] itself.
//! The two shapes genuinely differ — the TOML document flattens
//! `agent_engine_config.agents.<name>` to `[agents.<name>]`, moves
//! `allowed_models` to `[models].allowed`, gives the bare `enable_recap` root
//! scalar a table home at `[run].recap`, and reserves a home for
//! `.stella/mcp.toml` under `[mcp.servers]` (parsed but NOT yet consumed —
//! see `mcp_servers_not_yet_read`; `agent::load_mcp_plan` still reads only
//! `.stella/mcp.toml`).
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

use serde::{Deserialize, Serialize};
use stella_core::hooks::Hooks;
use stella_mcp::config::{McpConfig, McpServerEntry};

use super::authority::ManagedAuthoritySettings;
use super::context::ContextSettings;
use super::context_providers::ContextProviderSettings;
use super::{
    AgentEngineAgent, AgentEngineAgents, AgentEngineConfig, McpSettings, PlanReviewSettings,
    ProviderSettings, RewardSettings, Settings, Toggle, ToolsSettings, UiSettings,
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
    /// `always` / `ask` / `never` — whether a run works in a throwaway git
    /// worktree instead of the checkout. Same name in JSON.
    #[serde(default)]
    pub create_worktrees: Option<super::CreateWorktrees>,
    /// `worktree` / `copy-tree` — how a best-of-N fan-out isolates one
    /// candidate from the next. Same name in JSON.
    #[serde(default)]
    pub candidate_isolation: Option<super::CandidateIsolation>,
    /// `on` (the default when absent) = the workspace probe skips paths the
    /// repository's own `.gitignore` excludes. Same name in JSON
    /// (`ignore_gitignore`).
    #[serde(default)]
    pub ignore_gitignore: Option<Toggle>,
}

/// `[models]` — policy over the model catalog, not a model table.
///
/// The catalog (`~/.stella/catalog.db`) is the authority on what models exist
/// and what they cost. This block says only which of them this workspace may
/// use. Keeping the two apart is what stops a hand-written model list from
/// rotting the week a model ships.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ModelsSection {
    /// Was `agent_engine_config.allowed_models`. REPLACES wholesale across
    /// scopes rather than concatenating — it is one vocabulary, and merging
    /// would make it impossible for a project to narrow the user's list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed: Option<Vec<String>>,
    /// `[models.output_caps]` — deliberately spend FEWER output tokens than a
    /// model can write, per model:
    ///
    /// ```toml
    /// [models.output_caps]
    /// "anthropic/claude-sonnet-5" = 64000   # match a comparator that stops here
    /// "deepseek-chat" = 32000               # any route
    /// ```
    ///
    /// Still policy over the catalog rather than a model table, which is what
    /// keeps it inside this section's contract: the catalog remains the
    /// authority on what a model CAN write, and an unlisted model needs no
    /// entry — it simply gets its own maximum. Deleting an entry restores the
    /// correct default rather than leaving a stale number behind, which is
    /// precisely the rotting a hand-written model list does.
    ///
    /// Merges per KEY across scopes, unlike `allowed` above — see
    /// `AgentEngineConfig::overlay`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_caps: Option<BTreeMap<String, u32>>,
}

impl ModelsSection {
    fn is_empty(&self) -> bool {
        self.allowed.is_none() && self.output_caps.is_none()
    }
}

/// `[seats]` — which model each plugin-declared seat runs on.
///
/// ```toml
/// [seats]
/// "stella-plan/planner" = "openrouter/openai/gpt-5.5"
/// "vera/verifier" = "anthropic/claude-opus-5"
/// ```
///
/// Keys are `<plugin-id>/<role>` (`doc:roleless-core` §8.4). The plugin
/// declares only the bare role name and the host applies the prefix, so the
/// namespace cannot be forged by a plugin claiming another's.
///
/// Its own top-level table, and **not** a flattened part of `[agents]`, for the
/// exact reason [`AgentsSection`]'s doc gives for why it may flatten: that set
/// is closed, because its agent names are struct fields that cannot collide
/// with a root key. Seats are the opposite — the names are chosen by whatever
/// plugin the user installed, so a seat called `auto_mode` or `default_model`
/// is a thing a plugin author can write tomorrow. Flattening an open set into a
/// table that also holds root keys is a collision waiting for its first
/// unlucky name; a table of its own cannot collide with anything.
///
/// A map, not fields, for the same reason: core does not know what seats
/// exist, must not have a list of the ones it will accept, and must round-trip
/// a name it has never seen without altering it.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct SeatsSection {
    /// Seat name → model string, both opaque to this layer.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, String>,
}

impl SeatsSection {
    fn is_empty(&self) -> bool {
        self.models.is_empty()
    }
}

/// `[agents]` — the flattened engine config.
///
/// The JSON nests per-agent config one level below the flat model fields
/// (`agent_engine_config.agents.default`); rendered literally that is
/// `[agents.agents.default]`. Flattening is safe because the agent set is
/// CLOSED — `default` is a struct field, not a map key, so the per-agent table
/// can never collide with a root field.
///
/// **This is exactly why plugin seats went to their own `[seats]` table** and
/// not into this one: seat names are chosen by whatever the operator
/// installed, so folding them here would put user-chosen keys in the same
/// namespace as `auto_mode` and `default_model`. The closed set is what keeps
/// the flattening safe, and #3908 shrank it to one rather than opening it.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct AgentsSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_mode: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort_auto: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_auto: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimal_prompt: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headless_scope_bypass: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_budget_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result_horizon_steps: Option<u64>,

    // The built-in agent, as a table. Serialized LAST so the flat scalars
    // above are not stranded after a table header — TOML would then read them
    // as keys of that table, which is the same hazard `[run]` exists to avoid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<AgentEngineAgent>,
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

/// `[workspace]` — facts about the tree this config belongs to.
///
/// Its own section rather than a key under `[run]`: `allowed_dirs` states what
/// the *workspace* is, not how one run behaves, and the distinction is what
/// keeps a future `[workspace]` key from having to justify itself against a
/// section named for something else.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct WorkspaceSection {
    /// Directories outside the root that write tools may touch. Relative
    /// entries resolve against the project root; see
    /// [`Settings::allowed_write_dirs`].
    #[serde(default)]
    pub allowed_dirs: Option<Vec<String>>,
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
    pub workspace: WorkspaceSection,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderSettings>,
    #[serde(default)]
    pub models: ModelsSection,
    #[serde(default)]
    pub agents: AgentsSection,
    #[serde(default)]
    pub seats: SeatsSection,
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
    /// `[plugins]` — the per-plugin retraction switches. Same shape in JSON
    /// and TOML (a table of `name = "on"|"off"`), so no lowering beyond the
    /// move. Carried here rather than left out because a TOML-migrated scope
    /// that silently ignored `plugins.<name> = "off"` would keep running a
    /// plugin its operator switched off.
    #[serde(default)]
    pub plugins: BTreeMap<String, super::Toggle>,
    /// `[reward]` — what a turn's verdict is worth as a training label (#1043).
    /// Same shape in JSON and TOML, so no lowering beyond the move.
    #[serde(default)]
    pub reward: Option<RewardSettings>,
    /// `[plan_review]` — whether the deck's plan gate is installed and how big
    /// a plan has to be before it fires (#4611). Same shape in JSON and TOML,
    /// so no lowering beyond the move.
    ///
    /// A top-level section rather than a key under `[run]` because it is a
    /// block of two, and `[run]`'s contract is scalars that used to be bare
    /// root keys.
    #[serde(default)]
    pub plan_review: Option<PlanReviewSettings>,
    /// Honored only from the managed tier; see [`Settings::managed_authority`].
    #[serde(default)]
    pub authority: Option<ManagedAuthoritySettings>,
    #[serde(default)]
    pub enterprise_telemetry: Option<toml::Value>,
    /// `[self_driving]` — how the autonomous loop identifies itself.
    ///
    /// Here rather than in a file of its own because everything configurable
    /// about stella comes from `stella.toml`: a second config system is a
    /// second place to look, and the one thing worse than a setting nobody can
    /// find is two settings that disagree.
    #[serde(default)]
    pub self_driving: SelfDrivingSection,
    /// `[issues]` — which tracker is active, and where its vocabulary lives.
    #[serde(default)]
    pub issues: IssuesSection,
}

/// `[self_driving]` — the autonomous loop's own configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SelfDrivingSection {
    /// `[self_driving.attribution]` — what the loop appends to what it writes,
    /// and how it names branches.
    pub attribution: stella_autonomy::Attribution,
    /// `[self_driving.merge]` — which checks are allowed to block a merge.
    #[serde(default)]
    pub merge: MergeSection,
    /// `[self_driving.verify]` — how to prove a change when CI cannot.
    #[serde(default)]
    pub verify: VerifySection,
    /// `[self_driving.doctrine]` — how the loop decides, where two operators
    /// would decide differently.
    ///
    /// Whether to fix a base somebody else broke, whether to defer when
    /// another actor looks busy on it, and whether an escalation parks the
    /// loop. These are judgement calls about *this* team's norms, not facts
    /// about the code, which is why they are declared rather than compiled in.
    #[serde(default)]
    pub doctrine: stella_autonomy::Doctrine,
    /// `[self_driving.triage]` — the vocabulary the loop places issues in.
    ///
    /// Here rather than in the tracker manifest because these are *this
    /// operator's* judgements about their own backlog — which labels mean
    /// urgent, which mean "not ours" — not how the tracker spells a concept
    /// every tracker has. Two teams on one GitHub can want different ladders.
    #[serde(default)]
    pub triage: TriageSection,
}

/// `[self_driving.merge]` — which checks may block a merge.
///
/// Filtering to a repository's *required* contexts is necessary and not
/// sufficient: a required check fails identically whether the code is wrong or
/// the billing account is suspended, and only one of those is fixable by a
/// pull request. See `stella_autonomy::gate`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MergeSection {
    /// Checks that never block, whatever their history.
    ///
    /// The operator's escape hatch. Automatic detection needs several commits
    /// of evidence; somebody who already knows their deploy provider is
    /// suspended should not have to wait for the loop to infer it.
    #[serde(default)]
    pub ignore_checks: Vec<String>,
    /// Consecutive failing base commits after which a check is treated as
    /// unwinnable. `0` disables the inference, leaving only `ignore_checks`.
    #[serde(default)]
    pub stuck_after: Option<usize>,
}

impl MergeSection {
    /// Resolve to the policy the gate reads.
    #[must_use]
    pub fn policy(&self) -> stella_autonomy::BlockingPolicy {
        stella_autonomy::BlockingPolicy {
            ignore: self.ignore_checks.clone(),
            stuck_after: self
                .stuck_after
                .unwrap_or(stella_autonomy::DEFAULT_STUCK_AFTER),
        }
    }
}

/// `[self_driving.verify]` — how to prove a change when CI cannot.
///
/// A repository whose remote checks cannot go green — a suspended account, an
/// exhausted quota — still has a test suite. Running it on this machine is a
/// weaker signal than a clean CI run and a far stronger one than nothing, and
/// it is the difference between a loop that ships and a loop that waits for a
/// billing problem to resolve itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifySection {
    /// The command that proves a change, run in the work worktree.
    ///
    /// Absent means auto-detect from the project's own tooling.
    #[serde(default)]
    pub command: Option<String>,
    /// Seconds before the command is abandoned. Defaults to 30 minutes — long
    /// enough for a monorepo suite, short enough that a wedged process does
    /// not hold the loop overnight.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

/// `[self_driving.triage]` — how the loop places an issue.
///
/// Every field defaults, so an operator who writes none of it gets a working
/// loop on GitHub's common conventions. Declaring any one of them replaces
/// that list wholesale rather than adding to it: a partial override would
/// leave the operator unable to *remove* a built-in they disagree with.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageSection {
    /// Priority labels, most urgent first. Empty means "use the default".
    #[serde(default)]
    pub priorities: Vec<String>,
    /// Labels meaning "this loop works this".
    #[serde(default)]
    pub defect_kinds: Vec<String>,
    /// Labels meaning "this loop does not work this".
    #[serde(default)]
    pub excluded_kinds: Vec<String>,
}

impl TriageSection {
    /// Resolve to the policy the ranker reads, filling each unset list from
    /// the default independently.
    #[must_use]
    pub fn policy(&self) -> stella_autonomy::priority::TriagePolicy {
        let mut policy = stella_autonomy::priority::TriagePolicy::default();
        if !self.priorities.is_empty() {
            policy.ladder = stella_autonomy::priority::PriorityLadder::new(self.priorities.clone());
        }
        if !self.defect_kinds.is_empty() {
            policy.defect_kinds = self.defect_kinds.clone();
        }
        if !self.excluded_kinds.is_empty() {
            policy.excluded_kinds = self.excluded_kinds.clone();
        }
        policy
    }
}

/// `[issues]` — the active tracker.
///
/// Two fields and no more, deliberately. *Which* tracker is a stella-level
/// decision and belongs here; *how that tracker spells things* is the
/// tracker's own vocabulary and belongs in its manifest, which this points at.
/// Putting the spellings here too would mean a customer with two trackers had
/// nowhere to put the second one's words.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct IssuesSection {
    /// The provider id — `"github"` today.
    pub provider: String,
    /// Path to the provider's manifest, relative to the workspace root.
    ///
    /// `None` resolves to `.stella/issues/<provider>.toml`, and an absent file
    /// yields the built-in defaults for that provider — so a workspace that
    /// has configured nothing still works, which is the property that keeps
    /// "everything is configurable" from becoming "everything must be
    /// configured".
    pub manifest: Option<String>,
}

impl Default for IssuesSection {
    fn default() -> Self {
        Self {
            provider: "github".to_owned(),
            manifest: None,
        }
    }
}

impl TomlConfig {
    /// Parse a document, mapping a syntax error to a named, path-bearing
    /// message. A malformed config is a hard error rather than a silent skip:
    /// a typo that quietly reverted someone to a built-in endpoint would be far
    /// worse than a loud parse failure.
    pub fn parse(contents: &str, path: &Path) -> Result<Self, String> {
        toml::from_str(contents).map_err(|e| format!("invalid config file {}: {e}", path.display()))
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

    /// Refuse an inline `providers.<id>.api_key` at the project scope.
    ///
    /// `<repo>/stella.toml` lives at the repository root and is meant to be
    /// committed and reviewed like source, so a literal secret in it leaks into
    /// version control. The rule is enforced on BOTH the load path and the
    /// migration path from one definition here — when only the loader had it,
    /// `stella migrate config` would happily copy a key out of
    /// `.stella/settings.json` into the committed file and then produce a
    /// config stella refuses to read on the next launch.
    ///
    /// The offending value is never echoed: the message names the provider and
    /// the file, so it stays safe to paste into an issue.
    pub fn validate_project_secrets(&self, path: &Path, scope: ConfigScope) -> Result<(), String> {
        if scope != ConfigScope::Project {
            return Ok(());
        }
        for (id, entry) in &self.providers {
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
            workspace,
            providers,
            models,
            agents,
            seats,
            tools,
            hooks,
            mcp,
            context,
            context_providers,
            ui,
            plugins,
            reward,
            plan_review,
            authority,
            enterprise_telemetry,
            // Deliberately not lowered into `Settings`. Both are read straight
            // off the parsed document by the self-driving verbs
            // (`self_driving_cmd::config`), because `Settings` is the engine's
            // merged view and neither of these steers a turn — folding them in
            // would put loop bookkeeping and tracker vocabulary on the type
            // every session-building code path already carries.
            //
            // Named rather than `..` so this stays an exhaustive destructure:
            // the next section added here must still be consciously placed.
            self_driving: _,
            issues: _,
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

        let agent_engine_config = lower_agents(agents, models, seats);

        let settings = Settings {
            providers,
            hooks,
            mcp: mcp_settings,
            agent_engine_config,
            tools,
            ignore_gitignore: run.ignore_gitignore,
            create_worktrees: run.create_worktrees,
            candidate_isolation: run.candidate_isolation,
            allowed_dirs: workspace.allowed_dirs,
            ui,
            reward,
            plan_review,
            context,
            context_providers,
            plugins,
            managed_authority: authority,
            // Round-trip through JSON: the field is stored untyped and
            // validated fail-open by its own adapter, which speaks
            // `serde_json::Value`. TOML has no null, so this cannot lose
            // information in the direction that matters.
            enterprise_telemetry: enterprise_telemetry
                .and_then(|value| serde_json::to_value(value).ok()),
            authority_policy: Default::default(),
            // Both computed after the merge, never carried by a document —
            // this is one scope's parse, and the ceiling is a fact about the
            // managed scope alone.
            steering_ceiling: Default::default(),
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
    // capability being removed. `stella migrate config` applies the same rule
    // before it writes, so the two paths cannot disagree about what is legal.
    parsed.validate_project_secrets(path, scope)?;

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
    crate::paths::stella_root().map(|root| root.join("stella.toml"))
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
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
}

/// What a scope's TOML file contributed, plus anything worth telling the user.
pub struct LoadedToml {
    pub settings: Settings,
    /// Servers declared under `[mcp.servers]`. **Parsed but not yet consumed**
    /// — see [`mcp_servers_not_yet_read`].
    ///
    /// Carried rather than dropped so Phase 1b has a seam to attach to and so
    /// the tests can prove the block parses today. `dead_code` is expected and
    /// accurate until `agent::load_mcp_plan` reads it; silencing it here is
    /// preferable to deleting the field and reconstructing it later, because
    /// the parse is what the user-facing warning is claiming works.
    #[allow(
        dead_code,
        reason = "Phase 1b seam; the warning below is its only reader today"
    )]
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

/// Split an [`AgentEngineConfig`] back into the two TOML sections it lowers
/// from — the exact inverse of [`lower_agents`].
///
/// The split is why saving the engine config cannot be one `save_section`
/// call: `allowed_models` belongs to `[models]`, everything else to
/// `[agents]`. Writing them separately would parse and re-render the file
/// twice, doubling the window in which a crash leaves a half-written config.
pub fn raise_agents(cfg: &AgentEngineConfig) -> (AgentsSection, ModelsSection, SeatsSection) {
    let per_agent = cfg.agents.clone().unwrap_or_default();
    let agents = AgentsSection {
        default_model: cfg.default_model.clone(),
        auto_mode: cfg.auto_mode,
        effort_auto: cfg.effort_auto,
        reasoning_auto: cfg.reasoning_auto,
        minimal_prompt: cfg.minimal_prompt,
        headless_scope_bypass: cfg.headless_scope_bypass,
        model_timeout_secs: cfg.model_timeout_secs,
        compaction_budget_tokens: cfg.compaction_budget_tokens,
        tool_result_horizon_steps: cfg.tool_result_horizon_steps,
        default: per_agent.default,
    };
    let models = ModelsSection {
        allowed: cfg.allowed_models.clone(),
        output_caps: cfg.model_output_caps.clone(),
    };
    let seats = SeatsSection {
        models: cfg.seat_models.clone().unwrap_or_default(),
    };
    (agents, models, seats)
}

/// The `[agents]` / `[models]` items for a write, with an EMPTY section
/// rendered as a removal rather than a bare header.
///
/// `[models]` with nothing under it is not harmless: on the next read it is an
/// empty-but-present block, and the very next thing after it in the file — a
/// bare key, a comment the user meant for something else — reads as its
/// content. Removing it keeps the file honest.
pub fn agent_sections(
    cfg: &AgentEngineConfig,
) -> Result<Vec<(&'static str, Option<toml_edit::Item>)>, String> {
    let (agents, models, seats) = raise_agents(cfg);
    let agents_item = if agents.is_empty() {
        None
    } else {
        Some(super::toml_io::item_for(&agents, "agents")?)
    };
    let models_item = if models.is_empty() {
        None
    } else {
        Some(super::toml_io::item_for(&models, "models")?)
    };
    let seats_item = if seats.is_empty() {
        None
    } else {
        Some(super::toml_io::item_for(&seats, "seats")?)
    };
    Ok(vec![
        ("agents", agents_item),
        ("models", models_item),
        ("seats", seats_item),
    ])
}

/// Fold `[agents]` + `[models]` into the one `AgentEngineConfig` the engine
/// reads. Returns `None` when neither section said anything, so an absent block
/// stays absent rather than becoming an all-`None` object that the scope merge
/// would treat as a real (empty) opinion.
fn lower_agents(
    agents: AgentsSection,
    models: ModelsSection,
    seats: SeatsSection,
) -> Option<AgentEngineConfig> {
    if agents.is_empty() && models.allowed.is_none() && seats.is_empty() {
        return None;
    }

    let per_agent = AgentEngineAgents {
        default: agents.default,
    };
    let agents_field = (per_agent != AgentEngineAgents::default()).then_some(per_agent);

    Some(AgentEngineConfig {
        default_model: agents.default_model,
        allowed_models: models.allowed,
        // Absent stays absent: an empty `[seats]` table must not become an
        // empty-but-present map, which the scope merge would read as a real
        // opinion and let a lower scope's assignments survive a higher scope
        // that meant to say nothing.
        seat_models: (!seats.is_empty()).then_some(seats.models),
        model_output_caps: models.output_caps,
        auto_mode: agents.auto_mode,
        effort_auto: agents.effort_auto,
        reasoning_auto: agents.reasoning_auto,
        minimal_prompt: agents.minimal_prompt,
        headless_scope_bypass: agents.headless_scope_bypass,
        model_timeout_secs: agents.model_timeout_secs,
        compaction_budget_tokens: agents.compaction_budget_tokens,
        tool_result_horizon_steps: agents.tool_result_horizon_steps,
        agents: agents_field,
    })
}
