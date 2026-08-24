// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The AGENTS pane of the SETTINGS tab — the `agent_engine_config` editor, on
//! the SPEC 5 frame:
//!
//! ```text
//!   GLOBAL     default                                             modified
//!
//! ▸ auto_mode            on
//!   effort_auto          off
//!   allowed_models       anthropic/claude-fable-5, openai/gpt-6
//!
//!   saved to user settings
//!   ⇥ agent · ⏎ edit · space toggle · x clear · s save user · esc done
//! ```
//!
//! It edits `settings.json` → `agent_engine_config`: the global toggles (the
//! auto modes, the minimal-prompt switch, the allowed-model list) plus the
//! `default` agent's model / prompt / sampling overrides — one agent page,
//! because core has one role (#3908).
//!
//! Ownership mirrors the MCP and SKILLS surfaces: the **driver** owns the
//! settings files on disk and pushes [`crate::envelope::Inbound::EngineConfig`]
//! snapshots (at startup, after every save, and on request); the deck edits a
//! **working copy** in memory and sends it back whole via
//! [`WorkspaceInput::EngineConfigSave`] — the driver merges the
//! `agent_engine_config` object into the chosen scope's `settings.json`
//! (preserving every other key) and answers with a fresh snapshot whose
//! `status` carries the outcome. A `pristine` twin of the last adopted
//! snapshot is what the "modified" marker compares against, and it lets
//! [`ingest_config`] tell a benign refresh (safe to adopt) from one that
//! would clobber unsaved edits (kept until saved — or until focus leaves
//! the panel and the next snapshot arrives, which is the deliberate discard
//! path).
//!
//! Interaction follows the queue-editor contract: **modal while focused**
//! (`e` on the SETTINGS tab focuses; Esc hands the keyboard back to the
//! tab). Every key is claimed by [`keys::handle_engine_key`] while focused,
//! so the letter verbs (`s`/`S`/`x`/`r`), the inline edit buffer, and the
//! model-picker filter can never leak a keystroke into the global composer.
//!
//! Three modules under one panel, each with a boundary the others do not
//! cross: [`tabs`] is the strip's vocabulary and cycle order, [`keys`] is the
//! modal key map and the writes it makes into the working copy, and
//! [`paint`] draws. The split is what let the panel leave `views/engine.rs`,
//! a grandfathered god file, without any of the three landing over the
//! 1500-line ceiling (AGENTS.md § "God files").

use crate::deck::DeckTab;
use crate::deck_ui::{DeckAction, DeckUi};
use crate::envelope::{EngineAgentState, EngineConfigState, EngineRole, WorkspaceInput};
use crate::views::settings::SettingsPane;

pub mod keys;
pub mod paint;
pub mod tabs;

pub use keys::handle_engine_key;
pub use paint::render;
pub use tabs::EngineTab;

/// The legal `effort` values, in cycle order (⏎ walks them, then wraps to
/// "provider default"). This is the FULL vocabulary — the fallback when
/// the selected model's own levels are unknown; [`effort_values_for`]
/// narrows it to what the model/provider pair actually supports.
const EFFORT_VALUES: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// The legal `verbosity` values.
pub(crate) const VERBOSITY_VALUES: [&str; 3] = ["low", "medium", "high"];
/// The legal `service_tier` values.
pub(crate) const SERVICE_TIER_VALUES: [&str; 4] = ["auto", "default", "flex", "priority"];

/// Hint shown when an action needs the config snapshot the driver has not
/// delivered yet (a race right after startup, or a driver error).
pub(crate) const NO_SNAPSHOT_HINT: &str = "waiting for the engine config snapshot — r to reload";

/// The effort levels `role`'s currently-selected model can act on, from
/// the driver-computed `model_efforts` map. Lookup tries the model string
/// verbatim (the picker writes `provider/slug`), then provider-qualified,
/// then any provider serving that bare slug. Unknown models keep the full
/// vocabulary — unknown must never restrict. `Some(vec![])` means effort
/// is genuinely not a knob for this model.
pub(crate) fn effort_values_for(state: &EngineConfigState, role: EngineRole) -> Vec<String> {
    let full = || EFFORT_VALUES.iter().map(|s| s.to_string()).collect();
    let Some(agent) = state.agent(role) else {
        return full();
    };
    let Some(model) = agent
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
    else {
        return full();
    };
    let qualified = agent
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| format!("{p}/{model}"));
    let suffix = format!("/{model}");
    let hit = state
        .model_efforts
        .get(model)
        .or_else(|| qualified.as_ref().and_then(|q| state.model_efforts.get(q)))
        .or_else(|| {
            // `model_efforts` is a `HashMap`, whose iteration order is
            // unspecified — if two providers happen to serve the same bare
            // slug (e.g. `openrouter/gpt-4` and `azure/gpt-4`), picking the
            // first match `.iter()` happens to yield would make the shown
            // effort vocabulary flicker between runs for the exact same
            // config. Sorting by spec first makes the choice a pure function
            // of `model_efforts`' contents, not its hash layout.
            state
                .model_efforts
                .iter()
                .filter(|(spec, _)| spec.ends_with(&suffix))
                .min_by_key(|(spec, _)| spec.as_str())
                .map(|(_, levels)| levels)
        });
    match hit {
        Some(levels) => levels.clone(),
        None => full(),
    }
}

/// One editable field of [`EngineAgentState`], in the struct's declaration
/// order — the agent tabs render exactly one row per field, so the screen
/// order and the settings order can never drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentField {
    Model,
    Provider,
    Prompt,
    Effort,
    Reasoning,
    Temperature,
    TopP,
    TopK,
    FrequencyPenalty,
    PresencePenalty,
    RepetitionPenalty,
    MaxTokens,
    Seed,
    Verbosity,
    ServiceTier,
}

impl AgentField {
    pub const ALL: [AgentField; 15] = [
        AgentField::Model,
        AgentField::Provider,
        AgentField::Prompt,
        AgentField::Effort,
        AgentField::Reasoning,
        AgentField::Temperature,
        AgentField::TopP,
        AgentField::TopK,
        AgentField::FrequencyPenalty,
        AgentField::PresencePenalty,
        AgentField::RepetitionPenalty,
        AgentField::MaxTokens,
        AgentField::Seed,
        AgentField::Verbosity,
        AgentField::ServiceTier,
    ];

    /// The settings key — doubles as the row label so what the user reads is
    /// exactly what lands in `settings.json`.
    pub fn label(self) -> &'static str {
        match self {
            AgentField::Model => "model",
            AgentField::Provider => "provider",
            AgentField::Prompt => "prompt",
            AgentField::Effort => "effort",
            AgentField::Reasoning => "reasoning",
            AgentField::Temperature => "temperature",
            AgentField::TopP => "top_p",
            AgentField::TopK => "top_k",
            AgentField::FrequencyPenalty => "frequency_penalty",
            AgentField::PresencePenalty => "presence_penalty",
            AgentField::RepetitionPenalty => "repetition_penalty",
            AgentField::MaxTokens => "max_tokens",
            AgentField::Seed => "seed",
            AgentField::Verbosity => "verbosity",
            AgentField::ServiceTier => "service_tier",
        }
    }

    /// The display/edit-seed form of this field on `agent`. `None` = unset
    /// (renders dimmed as "(provider default)"; seeds an empty edit buffer).
    pub(crate) fn value(self, agent: &EngineAgentState) -> Option<String> {
        match self {
            AgentField::Model => agent.model.clone(),
            AgentField::Provider => agent.provider.clone(),
            AgentField::Prompt => agent.prompt.clone(),
            AgentField::Effort => agent.effort.clone(),
            AgentField::Reasoning => agent
                .reasoning
                .map(|on| (if on { "on" } else { "off" }).to_string()),
            AgentField::Temperature => agent.temperature.map(|v| v.to_string()),
            AgentField::TopP => agent.top_p.map(|v| v.to_string()),
            AgentField::TopK => agent.top_k.map(|v| v.to_string()),
            AgentField::FrequencyPenalty => agent.frequency_penalty.map(|v| v.to_string()),
            AgentField::PresencePenalty => agent.presence_penalty.map(|v| v.to_string()),
            AgentField::RepetitionPenalty => agent.repetition_penalty.map(|v| v.to_string()),
            AgentField::MaxTokens => agent.max_tokens.map(|v| v.to_string()),
            AgentField::Seed => agent.seed.map(|v| v.to_string()),
            AgentField::Verbosity => agent.verbosity.clone(),
            AgentField::ServiceTier => agent.service_tier.clone(),
        }
    }

    /// Reset this field to "provider default".
    pub(crate) fn clear(self, agent: &mut EngineAgentState) {
        match self {
            AgentField::Model => agent.model = None,
            AgentField::Provider => agent.provider = None,
            AgentField::Prompt => agent.prompt = None,
            AgentField::Effort => agent.effort = None,
            AgentField::Reasoning => agent.reasoning = None,
            AgentField::Temperature => agent.temperature = None,
            AgentField::TopP => agent.top_p = None,
            AgentField::TopK => agent.top_k = None,
            AgentField::FrequencyPenalty => agent.frequency_penalty = None,
            AgentField::PresencePenalty => agent.presence_penalty = None,
            AgentField::RepetitionPenalty => agent.repetition_penalty = None,
            AgentField::MaxTokens => agent.max_tokens = None,
            AgentField::Seed => agent.seed = None,
            AgentField::Verbosity => agent.verbosity = None,
            AgentField::ServiceTier => agent.service_tier = None,
        }
    }

    /// Apply a committed inline buffer to this field. Empty (after trimming)
    /// clears to `None`; numeric fields must parse into their exact type.
    pub(crate) fn set_from_text(
        self,
        agent: &mut EngineAgentState,
        raw: &str,
    ) -> Result<(), String> {
        let t = raw.trim();
        let text = || {
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        };
        match self {
            AgentField::Model => agent.model = text(),
            AgentField::Provider => agent.provider = text(),
            // The prompt keeps the buffer verbatim (minus a wholly-blank one):
            // its inline edit is a single line, and whether whitespace matters
            // inside a system prompt is not this layer's call.
            AgentField::Prompt => {
                agent.prompt = if t.is_empty() {
                    None
                } else {
                    Some(raw.to_string())
                }
            }
            AgentField::Effort => agent.effort = text(),
            AgentField::Verbosity => agent.verbosity = text(),
            AgentField::ServiceTier => agent.service_tier = text(),
            // ⏎ cycles reasoning in place — it never inline-edits, so a text
            // commit reaching here can only mean "leave it alone".
            AgentField::Reasoning => {}
            AgentField::Temperature => agent.temperature = parse_num::<f32>(t, "temperature")?,
            AgentField::TopP => agent.top_p = parse_num::<f32>(t, "top_p")?,
            AgentField::TopK => agent.top_k = parse_num::<u32>(t, "top_k")?,
            AgentField::FrequencyPenalty => {
                agent.frequency_penalty = parse_num::<f32>(t, "frequency_penalty")?
            }
            AgentField::PresencePenalty => {
                agent.presence_penalty = parse_num::<f32>(t, "presence_penalty")?
            }
            AgentField::RepetitionPenalty => {
                agent.repetition_penalty = parse_num::<f32>(t, "repetition_penalty")?
            }
            AgentField::MaxTokens => agent.max_tokens = parse_num::<u32>(t, "max_tokens")?,
            AgentField::Seed => agent.seed = parse_num::<u64>(t, "seed")?,
        }
        Ok(())
    }
}

/// Parse one numeric buffer: empty → `None` (provider default), otherwise
/// the value or a keep-editing hint.
fn parse_num<T: std::str::FromStr>(t: &str, label: &str) -> Result<Option<T>, String> {
    if t.is_empty() {
        return Ok(None);
    }
    t.parse::<T>()
        .map(Some)
        .map_err(|_| format!("{label}: “{t}” does not parse — fix it or Esc to cancel"))
}

/// An in-progress inline edit: which row it belongs to and the live buffer.
/// The buffer is committed on ⏎ (parsed per field type; a parse failure
/// keeps the edit alive with a hint rather than half-applying) and dropped
/// on Esc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineEdit {
    /// The row index (within the current tab) the buffer edits.
    pub row: usize,
    pub buffer: String,
}

/// The model-picker sub-overlay's state: a filter-as-you-type query over the
/// allowed models (falling back to the seed catalog when no restriction is
/// configured) — the graph tab's file-picker idiom.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelPicker {
    pub query: String,
    /// Selected row, indexing the *filtered* match list. Reset to 0 on every
    /// query edit (the match set changed under it).
    pub sel: usize,
}

/// All engine-panel view state (a field on [`DeckUi`]). The config itself
/// is driver-owned — `state` is the working copy being edited and `pristine`
/// the last snapshot adopted from the driver; everything else is ephemeral
/// interaction state. The panel is the full-width body of the SETTINGS tab
/// (no popup, no `/engine` command): `focused` is what routes the keyboard
/// to it.
#[derive(Debug, Clone, Default)]
pub struct EngineOverlay {
    /// Whether the panel owns the keyboard (modal while set, on the AGENT
    /// ENGINE tab only — `e` focuses, Esc returns focus to the left column).
    pub focused: bool,
    /// The working copy being edited. `None` until the first snapshot lands.
    pub state: Option<EngineConfigState>,
    /// The last driver snapshot adopted verbatim — `state != pristine` is
    /// the "modified" marker and the unsaved-edits guard in [`ingest_config`].
    pub pristine: Option<EngineConfigState>,
    pub tab: EngineTab,
    /// Selected row within the current tab, clamped on tab switches.
    pub row: usize,
    /// The active inline edit, if any (claims keys ahead of navigation).
    pub edit: Option<EngineEdit>,
    /// The model-picker sub-overlay, if open (claims keys ahead of the edit).
    pub picker: Option<ModelPicker>,
    /// One-line hint: driver save/refresh outcomes, local parse errors.
    pub status: Option<String>,
    /// A save/refresh is in flight driver-side — cleared when the next
    /// [`crate::envelope::Inbound::EngineConfig`] folds back.
    pub busy: bool,
}

impl EngineOverlay {
    /// Rows on the current tab (the ↑/↓ clamp bound).
    pub fn row_count(&self) -> usize {
        match self.tab {
            EngineTab::Global => tabs::GLOBAL_ROWS.len(),
            EngineTab::Agent(_) => AgentField::ALL.len(),
        }
    }

    /// Whether the working copy has unsaved local edits.
    pub fn dirty(&self) -> bool {
        self.state != self.pristine
    }

    /// The agent the current tab edits, if it is an agent tab.
    pub fn role(&self) -> Option<EngineRole> {
        match self.tab {
            EngineTab::Agent(role) => Some(role),
            EngineTab::Global => None,
        }
    }
}

// ── driver snapshot ingest ──────────────────────────────────────────────────

/// Fold one [`crate::envelope::Inbound::EngineConfig`] snapshot. Adopted as
/// both `pristine` + working copy unless the overlay is open with unsaved
/// edits — a background refresh must never eat what the user typed. The one
/// exception inside a dirty overlay is a snapshot that **equals** the working
/// copy (the echo of our own save): adopting it re-baselines `pristine`, so
/// the "modified" marker clears the moment the driver confirms the write.
/// `status` (save outcomes, errors) always lands, and `busy` always clears —
/// a snapshot is the completion signal for whatever op was in flight.
pub fn ingest_config(ui: &mut DeckUi, state: &EngineConfigState, status: &Option<String>) {
    let e = &mut ui.engine;
    let echoes_working = e.state.as_ref() == Some(state);
    if !e.focused || !e.dirty() || echoes_working {
        e.state = Some(state.clone());
        e.pristine = Some(state.clone());
    }
    if let Some(status) = status {
        e.status = Some(status.clone());
    }
    e.busy = false;
}

// ── focusers (`e` on the SETTINGS tab) ─────────────────────────────────────

/// Focus the engine panel (switching to the SETTINGS tab if needed) on
/// the GLOBAL tab, and ask the driver to re-read the settings chain so the
/// panel reflects disk truth (the reply is dirty-guarded by
/// [`ingest_config`], so refocusing over unsaved edits is safe).
pub fn focus_panel(ui: &mut DeckUi) -> DeckAction {
    ui.set_tab(DeckTab::Settings);
    // The SETTINGS tab hosts two modal editors; exactly one owns the keyboard.
    ui.tools.focused = false;
    // …and only one is on screen. Move the tab's secondary nav with the focus
    // so focusing from anywhere (a command, another pane) can never leave the
    // keyboard in an editor the user cannot see.
    ui.settings_pane = SettingsPane::Agents;
    let e = &mut ui.engine;
    e.focused = true;
    e.tab = EngineTab::Global;
    e.row = 0;
    e.edit = None;
    e.picker = None;
    e.busy = true;
    DeckAction::Send(WorkspaceInput::EngineConfigRefresh)
}

/// Open the model picker for `role`, pre-selecting the agent's current model
/// among the candidates (the graph picker's "start where you already are").
pub(crate) fn open_picker(e: &mut EngineOverlay, role: EngineRole) {
    let sel = e
        .state
        .as_ref()
        .and_then(|state| {
            let current = state.agent(role).and_then(|a| a.model.as_deref())?;
            picker_candidates(state)
                .iter()
                .position(|c| c.as_str() == current)
        })
        .unwrap_or(0);
    e.picker = Some(ModelPicker {
        query: String::new(),
        sel,
    });
}

/// The picker's vocabulary: `allowed_models` when restricted, else the
/// catalog — one derivation, shared with the `/model` session picker
/// ([`crate::views::picker`]), so the two can never offer different lists.
pub fn picker_candidates(state: &EngineConfigState) -> &[String] {
    if state.allowed_models.is_empty() {
        &state.catalog_models
    } else {
        &state.allowed_models
    }
}

/// Case-insensitive substring filter over the candidates — the exact
/// semantics of [`crate::graph::GraphSnapshot::matching_files`], so both
/// pickers feel identical.
pub(crate) fn picker_matches(state: &EngineConfigState, query: &str) -> Vec<String> {
    let needle = query.trim().to_lowercase();
    picker_candidates(state)
        .iter()
        .filter(|m| needle.is_empty() || m.to_lowercase().contains(&needle))
        .cloned()
        .collect()
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use crate::deck::{DeckTab, WorkspaceModel};

    pub(crate) fn ready_ui() -> DeckUi {
        let mut ui = DeckUi::default();
        ui.splash.skip(); // past the splash for interaction tests
        ui
    }

    pub(crate) fn sample_state() -> EngineConfigState {
        EngineConfigState {
            allowed_models: vec!["anthropic/claude-fable-5".into(), "openai/gpt-6".into()],
            providers: vec!["anthropic".into(), "openrouter".into()],
            catalog_models: vec!["zai/glm-5".into()],
            agents: vec![EngineAgentState::default(); 4],
            // The rest defaults empty: the three auto toggles, the per-model
            // effort vocabularies, and the resolved `roles` that ride this
            // snapshot for `/models` but that nothing this panel edits reads.
            ..Default::default()
        }
    }

    /// A deck on the SETTINGS tab with the panel already focused over a
    /// loaded snapshot — the state most key tests start from.
    pub(crate) fn open_ui() -> (WorkspaceModel, DeckUi) {
        let model = WorkspaceModel::new();
        let mut ui = ready_ui();
        ui.set_tab(DeckTab::Settings);
        ui.engine.focused = true;
        ui.engine.state = Some(sample_state());
        ui.engine.pristine = Some(sample_state());
        (model, ui)
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::deck::WorkspaceModel;
    use crate::deck_ui::ingest_inbound;
    use crate::envelope::Inbound;

    #[test]
    fn effort_vocabulary_follows_the_selected_model() {
        let mut state = sample_state();
        state.model_efforts.insert(
            "anthropic/claude-fable-5".into(),
            vec![
                "low".into(),
                "medium".into(),
                "high".into(),
                "xhigh".into(),
                "max".into(),
            ],
        );
        state
            .model_efforts
            .insert("openrouter/mistralai/mistral-7b-instruct".into(), vec![]);
        state.model_efforts.insert(
            "gemini/gemini-3-pro".into(),
            vec!["low".into(), "high".into()],
        );

        // Picker-written qualified spec → exact hit.
        state.agents[0].model = Some("anthropic/claude-fable-5".into());
        assert_eq!(effort_values_for(&state, EngineRole::Default).len(), 5);

        // A confirmed no-reasoning model → no levels at all.
        state.agents[0].model = Some("openrouter/mistralai/mistral-7b-instruct".into());
        assert!(effort_values_for(&state, EngineRole::Default).is_empty());

        // Provider pin + bare slug → provider-qualified lookup.
        state.agents[0].provider = Some("gemini".into());
        state.agents[0].model = Some("gemini-3-pro".into());
        assert_eq!(
            effort_values_for(&state, EngineRole::Default),
            vec!["low".to_string(), "high".to_string()]
        );

        // Unknown model (or no model at all) keeps the full vocabulary —
        // unknown never restricts.
        state.agents[0].provider = None;
        state.agents[0].model = Some("something-new".into());
        assert_eq!(effort_values_for(&state, EngineRole::Default).len(), 5);
        state.agents[0].model = None;
        assert_eq!(effort_values_for(&state, EngineRole::Default).len(), 5);
    }

    #[test]
    fn picker_falls_back_to_the_catalog_when_nothing_is_allowed() {
        let mut state = sample_state();
        state.allowed_models.clear();
        assert_eq!(
            picker_matches(&state, ""),
            vec!["zai/glm-5".to_string()],
            "an empty allow-list means the catalog vocabulary"
        );
    }

    #[test]
    fn ingest_applies_snapshot_and_status_without_clobbering_edits() {
        let mut model = WorkspaceModel::new();
        let mut ui = ready_ui();
        let snap = sample_state();

        // A first snapshot lands verbatim (working + pristine), with status.
        ingest_inbound(
            &Inbound::EngineConfig {
                state: snap.clone(),
                status: Some("loaded".into()),
            },
            &mut model,
            &mut ui,
        );
        assert_eq!(ui.engine.state.as_ref(), Some(&snap));
        assert_eq!(ui.engine.pristine.as_ref(), Some(&snap));
        assert_eq!(ui.engine.status.as_deref(), Some("loaded"));
        assert!(!ui.engine.busy);
        assert!(!ui.engine.dirty());

        // A refresh over an OPEN, dirty overlay must not eat local edits…
        ui.engine.focused = true;
        ui.engine.state.as_mut().unwrap().auto_mode = true;
        let mut other = sample_state();
        other.effort_auto = true;
        ingest_inbound(
            &Inbound::EngineConfig {
                state: other,
                status: None,
            },
            &mut model,
            &mut ui,
        );
        let state = ui.engine.state.as_ref().unwrap();
        assert!(state.auto_mode, "the local edit survives the refresh");
        assert!(!state.effort_auto, "the conflicting snapshot was held off");
        assert!(ui.engine.dirty());

        // …but the echo of our own save re-baselines pristine: modified clears.
        let echo = ui.engine.state.clone().unwrap();
        ingest_inbound(
            &Inbound::EngineConfig {
                state: echo,
                status: Some("saved to user settings".into()),
            },
            &mut model,
            &mut ui,
        );
        assert!(!ui.engine.dirty(), "the save echo clears the marker");
        assert_eq!(ui.engine.status.as_deref(), Some("saved to user settings"));

        // A CLOSED overlay always adopts the next snapshot (the deliberate
        // discard path for edits abandoned by closing).
        ui.engine.focused = false;
        ui.engine.state.as_mut().unwrap().auto_mode = false; // stale local edit
        let mut newest = sample_state();
        newest.reasoning_auto = true;
        ingest_inbound(
            &Inbound::EngineConfig {
                state: newest.clone(),
                status: None,
            },
            &mut model,
            &mut ui,
        );
        assert_eq!(ui.engine.state.as_ref(), Some(&newest));
    }
}
