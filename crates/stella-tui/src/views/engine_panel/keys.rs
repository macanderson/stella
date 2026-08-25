// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The engine panel's modal key map, and the writes it makes into the working
//! copy.
//!
//! Precedence within the panel: the model picker owns the keyboard when open,
//! then an active inline edit, then navigation — the same
//! innermost-context-first ladder the deck itself uses.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::tabs::{EngineTab, GLOBAL_ROWS, GlobalRow};
use super::{
    AgentField, EngineEdit, NO_SNAPSHOT_HINT, SERVICE_TIER_VALUES, VERBOSITY_VALUES,
    effort_values_for, open_picker, picker_matches,
};
use crate::deck_ui::{DeckAction, DeckUi, list_nav};
use crate::envelope::{
    AgentScope, EngineAgentState, EngineConfigState, EngineRole, WorkspaceInput,
};

/// The panel's modal key map, dispatched by [`crate::deck_ui::handle_deck_key`]
/// while `ui.engine.focused`.
pub fn handle_engine_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    if ui.engine.picker.is_some() {
        return handle_picker_key(key, ui);
    }
    if ui.engine.edit.is_some() {
        return handle_inline_edit_key(key, ui);
    }
    handle_nav_key(key, ui)
}

/// The model picker's keys: type to filter, ↑/↓ walk the matches, ⏎ applies
/// the picked slug to the current agent's `model`, Esc closes the picker
/// only (the panel stays focused).
fn handle_picker_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    // Snapshot the filtered matches up front so bounds and the picked slug
    // can never disagree with what render showed.
    let matches: Vec<String> = match (&ui.engine.state, &ui.engine.picker) {
        (Some(state), Some(picker)) => picker_matches(state, &picker.query),
        _ => Vec::new(),
    };
    let count = matches.len();
    match key.code {
        KeyCode::Esc => {
            ui.engine.picker = None;
            DeckAction::Handled
        }
        KeyCode::Up => {
            if let Some(p) = ui.engine.picker.as_mut() {
                p.sel = p.sel.saturating_sub(1);
            }
            DeckAction::Handled
        }
        KeyCode::Down => {
            if let Some(p) = ui.engine.picker.as_mut()
                && count > 0
            {
                p.sel = (p.sel + 1).min(count - 1);
            }
            DeckAction::Handled
        }
        KeyCode::Enter => {
            let sel = ui.engine.picker.as_ref().map(|p| p.sel).unwrap_or(0);
            let picked = matches.get(sel.min(count.saturating_sub(1))).cloned();
            ui.engine.picker = None;
            // The filter matched nothing → just close, like the graph picker.
            if let Some(slug) = picked
                && let EngineTab::Agent(role) = ui.engine.tab
                && let Some(state) = ui.engine.state.as_mut()
                && let Some(agent) = agent_mut(state, role)
            {
                agent.model = Some(slug);
            }
            DeckAction::Handled
        }
        KeyCode::Backspace => {
            if let Some(p) = ui.engine.picker.as_mut() {
                p.query.pop();
                p.sel = 0; // the match set changed — re-anchor
            }
            DeckAction::Handled
        }
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META) =>
        {
            if let Some(p) = ui.engine.picker.as_mut() {
                p.query.push(c);
                p.sel = 0; // the match set changed — re-anchor
            }
            DeckAction::Handled
        }
        // Modal: swallow everything else so nothing leaks behind the popup.
        _ => DeckAction::Handled,
    }
}

/// The inline edit's keys: printable/backspace edit the buffer, ⏎ commits
/// (a parse failure keeps the edit alive with a hint), Esc cancels without
/// touching the field.
fn handle_inline_edit_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    match key.code {
        KeyCode::Esc => {
            ui.engine.edit = None;
            DeckAction::Handled
        }
        KeyCode::Enter => commit_inline(ui),
        KeyCode::Backspace => {
            if let Some(e) = ui.engine.edit.as_mut() {
                e.buffer.pop();
            }
            DeckAction::Handled
        }
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META) =>
        {
            if let Some(e) = ui.engine.edit.as_mut() {
                e.buffer.push(c);
            }
            DeckAction::Handled
        }
        _ => DeckAction::Handled,
    }
}

/// Commit the inline buffer into the working copy. Empty clears the field to
/// "provider default" (`None`); numeric fields must parse or the edit stays
/// open with a hint — a half-applied number is worse than a visible error.
fn commit_inline(ui: &mut DeckUi) -> DeckAction {
    let Some(edit) = ui.engine.edit.clone() else {
        return DeckAction::Handled;
    };
    let tab = ui.engine.tab;
    let Some(state) = ui.engine.state.as_mut() else {
        ui.engine.edit = None;
        return DeckAction::Handled;
    };
    let result = match tab {
        // `allowed_models` is the only text-editable GLOBAL row: parse the
        // comma-joined display form back into slugs (empty → no restriction,
        // so the pickers fall back to the catalog).
        EngineTab::Global => {
            state.allowed_models = edit
                .buffer
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            Ok(())
        }
        EngineTab::Agent(role) => {
            let field = AgentField::ALL[edit.row.min(AgentField::ALL.len() - 1)];
            match agent_mut(state, role) {
                Some(agent) => field.set_from_text(agent, &edit.buffer),
                None => Ok(()),
            }
        }
    };
    match result {
        Ok(()) => ui.engine.edit = None,
        // Keep editing so the user can fix the buffer (or Esc out).
        Err(hint) => ui.engine.status = Some(hint),
    }
    DeckAction::Handled
}

/// The panel's navigation/verb keys (no picker, no edit active).
fn handle_nav_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    let plain = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META);
    // Row movement is the deck's one vocabulary, not this panel's: `↑`/`↓`,
    // `j`/`k`, `⇞`/`⇟` and `Home`/`End` all move the selection. `letters` is
    // true because the panel is modal while focused — nothing here is
    // composing a prompt for `j` to join. Tab/`←`/`→` are the pane step and
    // stay below (#4370).
    let count = ui.engine.row_count();
    if list_nav::select(key, &mut ui.engine.row, count, true) {
        return DeckAction::Handled;
    }
    match key.code {
        // Return focus to the tab's left column. The working copy stays in
        // memory (refocusing resumes the edits) until the next driver
        // snapshot replaces an unfocused panel's state — see
        // [`super::ingest_config`].
        KeyCode::Esc => {
            ui.engine.focused = false;
            DeckAction::Handled
        }
        KeyCode::Tab | KeyCode::Right => switch_tab(ui, true),
        KeyCode::BackTab | KeyCode::Left => switch_tab(ui, false),
        KeyCode::Enter => activate_row(ui, false),
        // Space is the toggle chord only: it flips the on/off rows exactly
        // like ⏎ but does NOT open pickers/edits — a stray
        // space must not drop the user into an input they didn't ask for.
        KeyCode::Char(' ') if plain => activate_row(ui, true),
        KeyCode::Char('x') if plain => clear_row(ui),
        KeyCode::Char('s') if plain => save(ui, AgentScope::User),
        KeyCode::Char('S') if plain => save(ui, AgentScope::Project),
        KeyCode::Char('r') if plain => refresh(ui),
        // Modal: swallow everything else.
        _ => DeckAction::Handled,
    }
}

/// Cycle to the neighboring tab, keeping the row selection where possible
/// (clamped — the GLOBAL tab is shorter than the agent tabs).
fn switch_tab(ui: &mut DeckUi, forward: bool) -> DeckAction {
    let e = &mut ui.engine;
    e.tab = if forward { e.tab.next() } else { e.tab.prev() };
    e.row = e.row.min(e.row_count().saturating_sub(1));
    DeckAction::Handled
}

/// ⏎ (or, for toggles, space) on the selected row: flip toggles, cycle
/// enums, open the model picker, or start an inline edit — per the row's
/// nature.
fn activate_row(ui: &mut DeckUi, via_space: bool) -> DeckAction {
    if ui.engine.state.is_none() {
        ui.engine.status = Some(NO_SNAPSHOT_HINT.into());
        return DeckAction::Handled;
    }
    let row = ui.engine.row;
    match ui.engine.tab {
        EngineTab::Global => {
            let state = ui.engine.state.as_mut().expect("guarded above");
            match GLOBAL_ROWS[row.min(GLOBAL_ROWS.len() - 1)] {
                GlobalRow::AllowedModels => {
                    if via_space {
                        return DeckAction::Handled;
                    }
                    let buffer = state.allowed_models.join(", ");
                    ui.engine.edit = Some(EngineEdit { row, buffer });
                }
                toggle => toggle.flip(state),
            }
        }
        EngineTab::Agent(role) => {
            let field = AgentField::ALL[row.min(AgentField::ALL.len() - 1)];
            match field {
                AgentField::Model => {
                    if via_space {
                        return DeckAction::Handled;
                    }
                    open_picker(&mut ui.engine, role);
                }
                // Reasoning is the tri-state toggle: provider default → on →
                // off → default. Space works here too — it's a toggle.
                AgentField::Reasoning => {
                    let state = ui.engine.state.as_mut().expect("guarded above");
                    if let Some(agent) = agent_mut(state, role) {
                        agent.reasoning = match agent.reasoning {
                            None => Some(true),
                            Some(true) => Some(false),
                            Some(false) => None,
                        };
                    }
                }
                AgentField::Effort | AgentField::Verbosity | AgentField::ServiceTier => {
                    if via_space {
                        return DeckAction::Handled;
                    }
                    let owned_values: Vec<String> = match field {
                        // Model-aware: only the levels this agent's selected
                        // model (as served by its provider) can act on.
                        AgentField::Effort => {
                            let state = ui.engine.state.as_ref().expect("guarded above");
                            effort_values_for(state, role)
                        }
                        AgentField::Verbosity => {
                            VERBOSITY_VALUES.iter().map(|s| s.to_string()).collect()
                        }
                        _ => SERVICE_TIER_VALUES.iter().map(|s| s.to_string()).collect(),
                    };
                    if owned_values.is_empty() {
                        // Effort on a model with no reasoning: explain
                        // instead of a keypress that visibly does nothing,
                        // and drop any stale level so a save can't carry it.
                        let state = ui.engine.state.as_mut().expect("guarded above");
                        if let Some(agent) = agent_mut(state, role) {
                            agent.effort = None;
                        }
                        ui.engine.status = Some(
                            "this model does not support reasoning — effort does not apply".into(),
                        );
                        return DeckAction::Handled;
                    }
                    let values: Vec<&str> = owned_values.iter().map(String::as_str).collect();
                    let state = ui.engine.state.as_mut().expect("guarded above");
                    if let Some(agent) = agent_mut(state, role) {
                        let slot = match field {
                            AgentField::Effort => &mut agent.effort,
                            AgentField::Verbosity => &mut agent.verbosity,
                            _ => &mut agent.service_tier,
                        };
                        cycle_enum(slot, &values);
                    }
                }
                AgentField::Provider => {
                    if via_space {
                        return DeckAction::Handled;
                    }
                    let providers = ui
                        .engine
                        .state
                        .as_ref()
                        .expect("guarded above")
                        .providers
                        .clone();
                    if providers.is_empty() {
                        // Nothing to cycle through — explain instead of a
                        // keypress that visibly does nothing.
                        ui.engine.status = Some(
                            "no providers configured — the driver's settings define them".into(),
                        );
                        return DeckAction::Handled;
                    }
                    let refs: Vec<&str> = providers.iter().map(String::as_str).collect();
                    let state = ui.engine.state.as_mut().expect("guarded above");
                    if let Some(agent) = agent_mut(state, role) {
                        cycle_enum(&mut agent.provider, &refs);
                    }
                }
                // Free-text / numeric rows: start the inline edit seeded
                // with the current value (None seeds empty — committing an
                // untouched empty buffer round-trips back to None).
                _ => {
                    if via_space {
                        return DeckAction::Handled;
                    }
                    let state = ui.engine.state.as_ref().expect("guarded above");
                    let buffer = state
                        .agent(role)
                        .and_then(|a| field.value(a))
                        .unwrap_or_default();
                    ui.engine.edit = Some(EngineEdit { row, buffer });
                }
            }
        }
    }
    DeckAction::Handled
}

/// `x`: clear the selected row back to "provider default" (`None`). The
/// GLOBAL booleans have no `None` — clearing means "off" — and clearing
/// `allowed_models` lifts the restriction (pickers fall back to the catalog).
fn clear_row(ui: &mut DeckUi) -> DeckAction {
    let row = ui.engine.row;
    let tab = ui.engine.tab;
    let Some(state) = ui.engine.state.as_mut() else {
        ui.engine.status = Some(NO_SNAPSHOT_HINT.into());
        return DeckAction::Handled;
    };
    match tab {
        EngineTab::Global => match GLOBAL_ROWS[row.min(GLOBAL_ROWS.len() - 1)] {
            GlobalRow::AllowedModels => state.allowed_models.clear(),
            toggle => toggle.switch_off(state),
        },
        EngineTab::Agent(role) => {
            let field = AgentField::ALL[row.min(AgentField::ALL.len() - 1)];
            if let Some(agent) = agent_mut(state, role) {
                field.clear(agent);
            }
        }
    }
    DeckAction::Handled
}

/// `s`/`S`: send the whole working copy to the driver for persistence at
/// `scope`. The request rides `pending_inputs` (drained by the shell after
/// this key) and the reply — a fresh snapshot with the outcome in `status` —
/// clears `busy` and re-baselines the modified marker via
/// [`super::ingest_config`].
fn save(ui: &mut DeckUi, scope: AgentScope) -> DeckAction {
    let Some(state) = ui.engine.state.clone() else {
        ui.engine.status = Some(NO_SNAPSHOT_HINT.into());
        return DeckAction::Handled;
    };
    ui.engine.busy = true;
    ui.engine.status = Some(format!("saving to {} settings…", scope.label()));
    ui.pending_inputs
        .push(WorkspaceInput::EngineConfigSave { state, scope });
    DeckAction::Handled
}

/// `r`: ask the driver to re-read the settings chain. The reply is
/// dirty-guarded ([`super::ingest_config`]), so a reload can never eat unsaved
/// edits — save or close first to adopt disk truth over them.
fn refresh(ui: &mut DeckUi) -> DeckAction {
    ui.engine.busy = true;
    ui.engine.status = Some("reloading engine config…".into());
    ui.pending_inputs.push(WorkspaceInput::EngineConfigRefresh);
    DeckAction::Handled
}

/// Mutable access to `role`'s slot in `state.agents`. The driver always
/// sends all four ([`EngineRole::ALL`] order), but a short vector (a hand-
/// built scenario, a driver bug) must not silently drop an edit — grow it
/// with defaults instead.
fn agent_mut(state: &mut EngineConfigState, role: EngineRole) -> Option<&mut EngineAgentState> {
    let idx = EngineRole::ALL.iter().position(|r| *r == role)?;
    while state.agents.len() <= idx {
        state.agents.push(EngineAgentState::default());
    }
    state.agents.get_mut(idx)
}

/// Cycle an enum-valued field through `values` and back to `None` (provider
/// default) past the end. An unrecognized stored value (hand-edited
/// settings) also wraps to `None` rather than guessing a position.
fn cycle_enum(current: &mut Option<String>, values: &[&str]) {
    let next = match current.as_deref() {
        None => values.first().map(|v| v.to_string()),
        Some(cur) => match values.iter().position(|v| v.eq_ignore_ascii_case(cur)) {
            Some(i) if i + 1 < values.len() => Some(values[i + 1].to_string()),
            _ => None,
        },
    };
    *current = next;
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{open_ui, ready_ui};
    use super::*;
    use crate::deck::{DeckTab, WorkspaceModel};
    use crate::deck_ui::handle_deck_key;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ch(c: char) -> KeyEvent {
        key(KeyCode::Char(c))
    }

    #[test]
    fn cycling_effort_on_a_non_reasoning_model_clears_and_explains() {
        let (_model, mut ui) = open_ui();
        let mut state = super::super::fixtures::sample_state();
        state
            .model_efforts
            .insert("openrouter/mistralai/mistral-7b-instruct".into(), vec![]);
        state.agents[0].model = Some("openrouter/mistralai/mistral-7b-instruct".into());
        state.agents[0].effort = Some("xhigh".into()); // stale — model can't
        ui.engine.state = Some(state.clone());
        ui.engine.pristine = Some(state);
        ui.engine.tab = EngineTab::Agent(EngineRole::Default);
        // Row index of the Effort field on the agent tab.
        ui.engine.row = AgentField::ALL
            .iter()
            .position(|f| *f == AgentField::Effort)
            .expect("effort row exists");

        let action = handle_engine_key(key(KeyCode::Enter), &mut ui);
        assert_eq!(action, DeckAction::Handled);
        let agent = &ui.engine.state.as_ref().unwrap().agents[0];
        assert_eq!(agent.effort, None, "stale effort was dropped, not cycled");
        assert!(
            ui.engine
                .status
                .as_deref()
                .is_some_and(|s| s.contains("does not support reasoning")),
            "status explains why nothing cycled: {:?}",
            ui.engine.status
        );
    }

    #[test]
    fn e_on_the_settings_tab_focuses_the_panel_and_esc_unfocuses() {
        let model = WorkspaceModel::new();
        let mut ui = ready_ui();
        ui.set_tab(DeckTab::Settings);
        let action = handle_deck_key(ch('e'), &model, &mut ui);
        assert_eq!(
            action,
            DeckAction::Send(WorkspaceInput::EngineConfigRefresh),
            "focusing the panel asks the driver for a fresh snapshot"
        );
        assert!(ui.engine.focused);
        assert_eq!(ui.engine.tab, EngineTab::Global);
        assert!(ui.engine.picker.is_none());

        // Esc hands the keyboard back to the tab.
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
        assert!(!ui.engine.focused, "esc hands the keyboard back to the tab");
    }

    #[test]
    fn tabs_cycle_and_rows_clamp() {
        let (model, mut ui) = open_ui();
        assert_eq!(ui.engine.tab, EngineTab::Global);

        // ↓ past the last GLOBAL row clamps; ↑ past the first clamps at 0.
        for _ in 0..10 {
            handle_deck_key(key(KeyCode::Down), &model, &mut ui);
        }
        assert_eq!(ui.engine.row, GLOBAL_ROWS.len() - 1);
        for _ in 0..10 {
            handle_deck_key(key(KeyCode::Up), &model, &mut ui);
        }
        assert_eq!(ui.engine.row, 0);

        // Tab walks GLOBAL → every agent → wraps back to GLOBAL. Counted off
        // `EngineTab::ALL`, which is itself derived from `EngineRole::ALL`, so
        // adding a configurable role does not silently un-test its page.
        let mut seen = vec![ui.engine.tab];
        for _ in 0..EngineTab::ALL.len() {
            handle_deck_key(key(KeyCode::Tab), &model, &mut ui);
            seen.push(ui.engine.tab);
        }
        assert_eq!(seen.first(), seen.last(), "one press per tab wraps around");
        assert_eq!(ui.engine.tab, EngineTab::Global);

        // A deep agent-row selection clamps when returning to GLOBAL.
        handle_deck_key(key(KeyCode::Tab), &model, &mut ui); // → default
        for _ in 0..20 {
            handle_deck_key(key(KeyCode::Down), &model, &mut ui);
        }
        assert_eq!(ui.engine.row, AgentField::ALL.len() - 1);
        handle_deck_key(key(KeyCode::BackTab), &model, &mut ui); // → GLOBAL
        assert_eq!(ui.engine.tab, EngineTab::Global);
        assert_eq!(
            ui.engine.row,
            GLOBAL_ROWS.len() - 1,
            "row clamped into the shorter tab"
        );
    }

    #[test]
    fn enter_cycles_reasoning_none_on_off() {
        let (model, mut ui) = open_ui();
        ui.engine.tab = EngineTab::Agent(EngineRole::Default);
        ui.engine.row = 4; // reasoning (AgentField::ALL[4])

        let reasoning = |ui: &DeckUi| ui.engine.state.as_ref().unwrap().agents[0].reasoning;
        assert_eq!(reasoning(&ui), None);
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        assert_eq!(reasoning(&ui), Some(true));
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        assert_eq!(reasoning(&ui), Some(false));
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        assert_eq!(reasoning(&ui), None, "the cycle wraps back to default");
        // Space is the same toggle.
        handle_deck_key(ch(' '), &model, &mut ui);
        assert_eq!(reasoning(&ui), Some(true));
        // `x` clears outright.
        handle_deck_key(ch('x'), &model, &mut ui);
        assert_eq!(reasoning(&ui), None);
    }

    #[test]
    fn inline_temperature_edit_commits_clears_and_rejects() {
        let (model, mut ui) = open_ui();
        ui.engine.tab = EngineTab::Agent(EngineRole::Default);
        ui.engine.row = 5; // temperature (AgentField::ALL[5])

        let temp = |ui: &DeckUi| ui.engine.state.as_ref().unwrap().agents[0].temperature;

        // ⏎ starts the edit seeded empty (the field is unset)…
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        assert_eq!(
            ui.engine.edit,
            Some(EngineEdit {
                row: 5,
                buffer: String::new()
            })
        );
        // …"0.7" ⏎ commits Some(0.7).
        for c in "0.7".chars() {
            handle_deck_key(ch(c), &model, &mut ui);
        }
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        assert_eq!(ui.engine.edit, None, "a clean parse ends the edit");
        assert_eq!(temp(&ui), Some(0.7));

        // Re-entering seeds the current value; emptying it clears to None.
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        assert_eq!(ui.engine.edit.as_ref().unwrap().buffer, "0.7");
        for _ in 0..3 {
            handle_deck_key(key(KeyCode::Backspace), &model, &mut ui);
        }
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        assert_eq!(temp(&ui), None, "an empty commit means provider default");

        // Garbage sets a hint and keeps the edit alive — never half-applies.
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        for c in "abc".chars() {
            handle_deck_key(ch(c), &model, &mut ui);
        }
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        assert!(ui.engine.edit.is_some(), "still editing after a bad parse");
        assert!(
            ui.engine
                .status
                .as_deref()
                .is_some_and(|s| s.contains("temperature")),
            "the hint names the field: {:?}",
            ui.engine.status
        );
        assert_eq!(temp(&ui), None, "the field is untouched");
        // Esc abandons the bad buffer.
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
        assert_eq!(ui.engine.edit, None);
        assert!(ui.engine.focused, "esc closed the edit, not the panel");
    }

    #[test]
    fn s_saves_the_working_copy_at_user_scope() {
        let (model, mut ui) = open_ui();
        ui.engine.tab = EngineTab::Agent(EngineRole::Default);
        ui.engine.row = 5; // temperature
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        for c in "0.7".chars() {
            handle_deck_key(ch(c), &model, &mut ui);
        }
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui);

        let action = handle_deck_key(ch('s'), &model, &mut ui);
        assert_eq!(action, DeckAction::Handled, "the save rides pending_inputs");
        let mut expected = super::super::fixtures::sample_state();
        expected.agents[0].temperature = Some(0.7);
        assert_eq!(
            ui.pending_inputs,
            vec![WorkspaceInput::EngineConfigSave {
                state: expected.clone(),
                scope: AgentScope::User,
            }],
            "the edited working copy goes out whole, at user scope"
        );
        assert!(ui.engine.busy);

        // `S` targets the project scope with the same working copy.
        ui.pending_inputs.clear();
        handle_deck_key(ch('S'), &model, &mut ui);
        assert_eq!(
            ui.pending_inputs,
            vec![WorkspaceInput::EngineConfigSave {
                state: expected,
                scope: AgentScope::Project,
            }]
        );
    }

    #[test]
    fn picker_filters_by_substring_and_enter_sets_the_model() {
        let (model, mut ui) = open_ui();
        ui.engine.tab = EngineTab::Agent(EngineRole::Default);
        ui.engine.row = 0; // model

        handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        assert!(ui.engine.picker.is_some(), "⏎ on the model row opens it");
        for c in "gpt".chars() {
            handle_deck_key(ch(c), &model, &mut ui);
        }
        let state = ui.engine.state.as_ref().unwrap();
        assert_eq!(
            picker_matches(state, "gpt"),
            vec!["openai/gpt-6".to_string()],
            "substring filter narrows the allowed models"
        );
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        assert_eq!(ui.engine.picker, None, "⏎ closes the picker");
        assert_eq!(
            ui.engine.state.as_ref().unwrap().agents[0].model.as_deref(),
            Some("openai/gpt-6")
        );
        assert!(ui.engine.dirty(), "the pick is a local edit until saved");
    }
}
