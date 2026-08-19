//! The ENGINE overlay's read models: the per-agent override editor's state,
//! and the resolved role wiring the `/models` dialog reports.
//!
//! Split out of `envelope.rs` (#3493) under #629's 1500-line ratchet. Like
//! every other type in this module tree these are *display* shapes: the
//! driver resolves the settings precedence chain beside the request path in
//! `stella-cli` and hands the answers over pre-rendered, so the deck never
//! grows a second opinion about what a role will run.

/// Which agent a per-agent engine override applies to — the configurable
/// "agents" of `agent_engine_config`.
///
/// One variant, because core has one role. `Worker`, `Verifier`, `Triage`,
/// `Research` and `Plan` were siblings here until #3908: they mirrored
/// `stella-cli`'s `EngineAgentKind`, which mirrored the staged pipeline
/// deleted in #3865, and each rendered an editor for a settings key that had
/// stopped steering anything. An overlay tab that writes a dead key is worse
/// than a missing tab — it reports success.
///
/// It stays an enum rather than collapsing into a bare struct because the
/// pane's replacement is a **list**, not a single row: slice 5 (#3909) turns
/// this into rows from the live session — the installed plugins' declared
/// seats, each with its assigned model or `default` — the way
/// `views/tools.rs` already sources MCP and custom tools. Keeping the
/// role-indexed shape is what lets that land without re-plumbing
/// `EngineConfigState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineRole {
    Default,
}

impl EngineRole {
    /// Stable settings-key / display order.
    pub const ALL: [EngineRole; 1] = [EngineRole::Default];

    /// The `agent_engine_config.agents.<key>` settings key (and display
    /// label) for this agent.
    pub fn key(self) -> &'static str {
        match self {
            EngineRole::Default => "default",
        }
    }
}

/// One agent's engine overrides as the ENGINE overlay edits them — a
/// TUI-local mirror of `stella-cli`'s `agent_engine_config` per-agent
/// settings, decoupled (like [`InstalledAgentEntry`](super::InstalledAgentEntry))
/// so the TUI crate stays independent of the settings engine; the driver maps
/// one to the other. Every field is optional — `None` renders as "provider default"
/// and is omitted from the saved JSON (the screenshot's unchecked
/// "Include" box).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EngineAgentState {
    /// Model slug — `"provider/slug"` or a bare slug.
    pub model: Option<String>,
    /// Gateway/provider id override (`"anthropic"`, `"openrouter"`,
    /// `"zai"`, or a settings-defined provider). When set, `model` is sent
    /// verbatim as the slug to THIS provider — how an OpenRouter key routes
    /// an `openai/...` slug.
    pub provider: Option<String>,
    /// Custom system prompt replacing the built-in one for this agent.
    pub prompt: Option<String>,
    /// Reasoning effort: `low|medium|high|xhigh|max`.
    pub effort: Option<String>,
    /// Thinking mode on/off (`None` = provider default).
    pub reasoning: Option<bool>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<u32>,
    pub frequency_penalty: Option<f32>,
    pub presence_penalty: Option<f32>,
    pub repetition_penalty: Option<f32>,
    pub max_tokens: Option<u32>,
    pub seed: Option<u64>,
    /// `low|medium|high`.
    pub verbosity: Option<String>,
    /// `auto|default|flex|priority`.
    pub service_tier: Option<String>,
}

/// One engine role's **resolved** wiring — what that role will actually send,
/// and the setting that decided it. The `/models` dialog's row type.
///
/// Every cell arrives pre-rendered from the driver, and deliberately so. The
/// resolution is a precedence chain (`agents.<role>.model` over the flat
/// `pipeline_<role>_model` over `default_model` over the session pin, with
/// `--model` owning the worker and `effort_auto`/`reasoning_auto` able to
/// replace a pin outright), it lives beside the request path in
/// `stella-cli`'s `config_wiring`, and `stella config` already prints these
/// exact cells. Re-deriving any of it here would give the deck a second
/// answer that can drift from the engine's — and a routing report that can
/// disagree with what runs is worse than no report at all.
///
/// This is why the fields are strings and not a typed mirror of the settings:
/// the type carries a *rendering*, not a model to reason over.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoleWiringRow {
    /// The settings key this role is spelled as (`default` / `worker` /
    /// `verifier` / `triage`) — the word a user would type to change it.
    pub role: String,
    /// `provider/slug` exactly as it goes on the wire.
    pub model: String,
    /// The resolved reasoning effort, or `provider default`. Names the pinned
    /// value `effort_auto` discarded when it discarded one.
    pub effort: String,
    /// `thinking on` / `thinking off` / `thinking default`, with the same
    /// `reasoning_auto` disclosure.
    pub thinking: String,
    /// The settings key that decided `model`, as a path a user can edit:
    /// `agents.verifier.model`, `pipeline_triage_model`, `default_model`,
    /// `--model (this invocation)`, or `session default`.
    pub source: String,
    /// What a session started **now** would resolve for this role, when a
    /// saved settings edit makes that differ from the four cells above —
    /// `None` when the saved settings agree with what is running, which is the
    /// ordinary case.
    ///
    /// The cells above stay the session-start resolution, because "what is
    /// running" is the question this dialog exists to answer and showing a
    /// mid-session edit as though it were in force would misreport exactly
    /// that. But saying nothing about the edit was its own lie: a user who
    /// changed `pipeline_verifier_model` and saved saw their old pin with no
    /// explanation, and read the dialog as having ignored the save (#1521).
    /// So this is strictly *additional* information, pre-rendered driver-side
    /// like every other cell, holding only the parts that differ.
    pub next_session: Option<String>,
}

/// The full agent-engine configuration snapshot the ENGINE overlay renders
/// and edits ([`Inbound::EngineConfig`](super::Inbound::EngineConfig) /
/// [`WorkspaceInput::EngineConfigSave`](super::WorkspaceInput::EngineConfigSave)).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EngineConfigState {
    /// Auto verifier-model selection: pick the best allowed model for the
    /// verifier (preferring a different family than the worker's).
    pub auto_mode: bool,
    /// Auto per-agent effort (verifier high, worker medium, triage low).
    pub effort_auto: bool,
    /// Auto per-agent reasoning (on for verifier/worker, off for triage).
    pub reasoning_auto: bool,
    /// The model slugs the model pickers offer (`allowed_models`). Empty
    /// means "no restriction" — pickers fall back to `catalog_models`.
    pub allowed_models: Vec<String>,
    /// Every configured provider id, for the provider picker.
    pub providers: Vec<String>,
    /// `provider/slug` strings from the catalog, scoped by the driver to
    /// providers with a usable credential — the picker's fallback
    /// vocabulary when `allowed_models` is empty.
    pub catalog_models: Vec<String>,
    /// Per-model effort vocabularies, keyed by the same `provider/slug`
    /// strings (plus any `allowed_models` spec): the effort levels this
    /// model, as served by this provider, can actually act on. An empty
    /// list means effort is not a knob for that model (no reasoning
    /// support, or an on/off-only thinking switch); a model absent from
    /// the map is unknown and keeps the full vocabulary.
    pub model_efforts: std::collections::HashMap<String, Vec<String>>,
    /// Exactly one entry per [`EngineRole::ALL`] slot, in that order.
    pub agents: Vec<EngineAgentState>,
    /// What each role **resolves to**, which is a different question from
    /// `agents` above: those are the raw overrides the ENGINE overlay edits,
    /// where `None` means "inherit", and these are the answers after the
    /// precedence chain and the auto-modes have run. The `/models` dialog
    /// renders these; the overlay's editors never touch them.
    ///
    /// **An open vocabulary** (#3472), unlike `agents` above: the roles the
    /// deck knows, then any role the host contributed, in the order it sent
    /// them — see [`roles`](super::roles) for the fold and what stays closed.
    /// Empty on a driver that sent no wiring; the dialog says so rather than
    /// inventing rows.
    pub roles: Vec<RoleWiringRow>,
}

impl EngineConfigState {
    /// The state for `role`, if present (the driver always sends all four).
    pub fn agent(&self, role: EngineRole) -> Option<&EngineAgentState> {
        EngineRole::ALL
            .iter()
            .position(|r| *r == role)
            .and_then(|i| self.agents.get(i))
    }
}
