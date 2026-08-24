// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The deck-local slash commands: the ones that change view state the driver
//! has no say over — a tab switch, an overlay, a floating card.
//!
//! Split out of [`super`] (a god file, closed to growth) and, more to the
//! point, out of the slash popup's key handler. They used to be matched only
//! on the popup's `Submit`, which is the selection a reader confirms with ⏎
//! while the menu is up. Typing `/inspect` in full — with the menu dismissed,
//! or with a trailing space, or pasted — went down the ordinary submit path
//! instead, reached the driver as a prompt, and the driver's command table
//! swallowed it as "handled TUI-side". The overlay never opened and nothing
//! said why. Both paths now ask this one function first.

use super::*;

/// The view action a deck-local command performs, or `None` for anything the
/// driver owns (`/clear`, `/help`, `/init`, custom commands, prose).
pub(super) fn deck_local_command(text: &str, ui: &mut DeckUi) -> Option<DeckAction> {
    Some(match text {
        // Only the tab-switch commands are deck-local (they change view
        // state the driver has no say over). `/diff` opens the diff
        // viewer; `/files` shows the file tree, so it must also *close* a
        // diff left open from a prior view.
        "/files" | "/diff" => {
            ui.set_tab(DeckTab::Files);
            ui.files_diff_open = text == "/diff";
            DeckAction::Handled
        }
        "/graph" => {
            ui.set_tab(DeckTab::Graph);
            DeckAction::Handled
        }
        // `/agents` opens the AGENTS tab directly, on the INSTALLED
        // AGENTS pane (the configured-on-disk view the command has
        // always been about) — and asks the driver, which owns the
        // definitions on disk, for a fresh list.
        "/agents" => {
            ui.set_tab(DeckTab::Agents);
            ui.installed.busy = true;
            DeckAction::Send(WorkspaceInput::AgentsRefresh)
        }
        // `/skills` opens the SKILLS tab directly and asks the driver —
        // which owns the skills on disk — for a fresh installed list.
        "/skills" => {
            ui.set_tab(DeckTab::Skills);
            ui.skills.status = Some("loading skills…".to_string());
            DeckAction::Send(WorkspaceInput::Skill(SkillOp::List))
        }
        "/mcp" => {
            ui.set_tab(DeckTab::Mcp);
            DeckAction::Handled
        }
        // `/settings` opens the SETTINGS tab — the home of all config
        // (deck-local view state, like the tab switches above).
        "/settings" => {
            ui.set_tab(DeckTab::Settings);
            DeckAction::Handled
        }
        // The three transcript-page overlays are deck-local view state,
        // exactly like the tab switches above (their keyboard shortcuts:
        // `ctrl-e` / `ctrl-k`, and the footer's ✉ badge for the inbox).
        "/sessions" => open_sessions_overlay(ui),
        "/subagents" => crate::v2::subagents::open(ui),
        "/context" => open_context_overlay(ui),
        "/inspect" => open_inspect_overlay(ui),
        "/inbox" => open_inbox_overlay(ui),
        // `/mcp-search` jumps straight into the MCP tab's registry
        // search — THE way to begin looking for a server from anywhere
        // (the old `/`-on-the-MCP-tab trigger collided with the command
        // menu and is gone; `s` on the tab is the local equivalent).
        "/mcp-search" => {
            ui.set_tab(DeckTab::Mcp);
            ui.mcp.mode = McpMode::Search;
            ui.mcp.status = None;
            DeckAction::Handled
        }
        // The floating cards: pure view state, so the driver has no say.
        // `/budget` only *renders* locally — the edit it takes leaves as
        // `WorkspaceInput::SetBudget` from the card's own key handler.
        // `/plan` is one card where `/tasks`, `/scope` and `/witness`
        // used to be three. The old names still route here rather than
        // erroring: a user who learned them should land on the surface
        // that answers what they were asking, not on "unknown command".
        "/plan" | "/tasks" | "/scope" | "/witness" => {
            ui.cards.raise(cards::Card::Plan);
            DeckAction::Handled
        }
        "/models" => {
            ui.cards.raise(cards::Card::Models);
            DeckAction::Handled
        }
        "/budget" => {
            ui.cards.raise(cards::Card::Budget);
            DeckAction::Handled
        }
        // Everything else — including `/help` — is enqueued for the
        // driver, which owns the session vocabulary and answers into the
        // transcript (a transient overlay would leave no record).
        _ => return None,
    })
}

/// The `/model` argument menu's keys for the deck composer (see
/// [`crate::composer::args`]): active only while the buffer is `/model` plus
/// an in-progress argument and something matches. Shares the slash popup's
/// selection index — the two menus are never up at once (the slash menu
/// closes at the whitespace that opens this one).
pub(super) fn model_arg_key(
    key: KeyEvent,
    model: &WorkspaceModel,
    ui: &mut DeckUi,
) -> Option<DeckAction> {
    let matches = crate::composer::args::arg_matches(&ui.composer, "/model", &ui.model_candidates);
    if matches.is_empty() {
        return None;
    }
    let outcome = crate::composer::args::handle_arg_popup_key(
        key,
        "/model",
        &matches,
        &mut ui.composer,
        &mut ui.slash_selected,
    )?;
    match outcome {
        SlashPopupOutcome::Handled => Some(DeckAction::Handled),
        SlashPopupOutcome::Submit(text) => Some(super::submit_prompt(ui, model, text)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The witness.** `/inspect` submitted from the composer — not chosen
    /// off the popup — opens the overlay and asks the driver for the index,
    /// instead of leaving as a prompt the driver swallows.
    #[test]
    fn a_typed_deck_command_acts_on_the_deck() {
        let model = WorkspaceModel::new();
        let mut ui = DeckUi::default();
        let action = super::super::submit_prompt(&mut ui, &model, "/inspect ".to_string());
        assert!(ui.inspect_open, "the overlay opens");
        assert!(
            matches!(action, DeckAction::Send(WorkspaceInput::InspectRefresh)),
            "and the index is requested: {action:?}"
        );
        let mut ui = DeckUi::default();
        super::super::submit_prompt(&mut ui, &model, "/graph".to_string());
        assert_eq!(ui.tab, DeckTab::Graph);
    }

    /// Anything else still goes where it went.
    #[test]
    fn a_prompt_is_not_a_deck_command() {
        let mut ui = DeckUi::default();
        assert!(deck_local_command("fix the tests", &mut ui).is_none());
        assert!(deck_local_command("/help", &mut ui).is_none());
    }
}
