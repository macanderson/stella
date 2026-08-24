// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The session-override pickers: `/model` (switch this session's model) and
//! `/agent` (run as an installed agent this session).
//!
//! One state machine ([`ListPicker`]) serves both overlays — a modal
//! scrollable list, `↑`/`↓` (the deck's one list vocabulary,
//! [`crate::deck_ui::list_nav`]) to move, `⏎` to choose, Esc to cancel —
//! because the two differ only in what their rows are and what a choice
//! sends. The rows are read LIVE at key/render time from state the deck
//! already holds (the driver's [`EngineConfigState`] snapshot for models,
//! the INSTALLED AGENTS entries for agents), never copied into the picker:
//! both snapshots can arrive *after* the picker opens (opening sends the
//! refresh), and a copy taken at open time would pin the overlay to an
//! empty list.
//!
//! In a file of its own under the god-file rule — `deck_ui.rs` pays only
//! the two state fields; key routing is `deck_ui/pickers.rs`.
//!
//! [`EngineConfigState`]: crate::envelope::EngineConfigState

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::deck::{PipelineRole, WorkspaceModel};
use crate::deck_ui::DeckUi;
use crate::theme;
use crate::views::cards;

/// Both pickers' card width. Wider than `cards::CARD_MAX_W` (not a link:
/// it is `pub(crate)`) for the same reason the `/info` dialog runs wide: a
/// `provider/vendor/slug` spec may not elide.
const PICKER_CARD_W: u16 = 64;

/// Rows shown at once; longer lists scroll under the selection.
const VISIBLE_ROWS: usize = 12;

/// What a keystroke did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    /// Not the picker's key (it is closed, or the key is Ctrl-C — declined
    /// so the deck's quit branch still fires).
    Ignored,
    /// Consumed; the picker stays up.
    Handled,
    /// `⏎` on row `i` of the caller's candidate list.
    Choose(usize),
    /// Esc/`q` — cancel, choosing nothing.
    Close,
}

/// The modal list state both pickers share. Rows live with the caller;
/// this holds only whether the picker is up and where the highlight is.
#[derive(Debug, Clone, Default)]
pub struct ListPicker {
    pub open: bool,
    sel: usize,
}

impl ListPicker {
    /// Raise the picker with the highlight on the first row.
    pub fn raise(&mut self) {
        self.open = true;
        self.sel = 0;
    }

    /// Take the picker down.
    pub fn close(&mut self) {
        self.open = false;
        self.sel = 0;
    }

    /// The highlighted row.
    #[must_use]
    pub fn selected(&self) -> usize {
        self.sel
    }

    /// Fold one keystroke against a candidate list `count` rows long.
    pub fn key(&mut self, key: KeyEvent, count: usize) -> PickerAction {
        if !self.open {
            return PickerAction::Ignored;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return PickerAction::Ignored;
        }
        // The list can shrink between frames (a fresh snapshot landed) —
        // clamp before navigating so the highlight never points past it.
        self.sel = self.sel.min(count.saturating_sub(1));
        if crate::deck_ui::list_nav::closes(key) {
            return PickerAction::Close;
        }
        // Modal, so `letters` is true (#4370).
        if crate::deck_ui::list_nav::select(key, &mut self.sel, count, true) {
            return PickerAction::Handled;
        }
        match key.code {
            KeyCode::Enter if count > 0 => PickerAction::Choose(self.sel),
            _ => PickerAction::Handled,
        }
    }
}

/// The `/model` picker's vocabulary: what the SETTINGS tab's model picker
/// offers — `allowed_models` when a restriction is configured, else the
/// catalog scoped to credentialed providers. Empty until the driver's first
/// [`EngineConfigState`] snapshot lands (opening the picker requests one).
///
/// [`EngineConfigState`]: crate::envelope::EngineConfigState
pub(crate) fn model_candidates(ui: &DeckUi) -> &[String] {
    ui.engine
        .state
        .as_ref()
        .map(crate::views::engine::picker_candidates)
        .unwrap_or(&[])
}

/// The `/model` **argument menu's** vocabulary ([`crate::composer::args`]):
/// [`model_candidates`] narrowed to the session's active provider.
///
/// The picker and the typeahead read one list on purpose — a second source
/// would let the two disagree about what is pickable — but they scope it
/// differently, because they answer different questions. The picker is a
/// menu of everywhere this workspace can go, so it offers every credentialed
/// provider. The typeahead completes a spec the reader is already typing at
/// *this* session, so it offers this session's provider.
///
/// Falls back to the full list when the active provider contributes nothing
/// to it (a gateway spec whose prefix is the gateway, not the vendor), since
/// an empty menu would read as "no models" rather than "none here".
pub(crate) fn typeahead_candidates(model: &WorkspaceModel, ui: &DeckUi) -> Vec<String> {
    let all = model_candidates(ui);
    let Some(provider) = model
        .role_pins
        .get(&crate::deck::PipelineRole::Worker)
        .map(|pin| pin.provider.clone())
    else {
        return all.to_vec();
    };
    let scoped: Vec<String> = all
        .iter()
        .filter(|spec| {
            spec.strip_prefix(provider.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
        })
        .cloned()
        .collect();
    if scoped.is_empty() {
        all.to_vec()
    } else {
        scoped
    }
}

/// The window of `count` rows that keeps `sel` visible: at most
/// [`VISIBLE_ROWS`], slid so the highlight never leaves it.
fn window(sel: usize, count: usize) -> std::ops::Range<usize> {
    let visible = count.min(VISIBLE_ROWS);
    let start = (sel + 1)
        .saturating_sub(visible)
        .min(count.saturating_sub(visible));
    start..start + visible
}

/// Paint the `/model` picker. A no-op while closed.
pub fn render_model(model: &WorkspaceModel, ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if !ui.model_picker.open {
        return;
    }
    let candidates = model_candidates(ui);
    let current = model
        .role_pins
        .get(&PipelineRole::Worker)
        .map(crate::deck::RolePin::slug);
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let sel = ui
        .model_picker
        .selected()
        .min(candidates.len().saturating_sub(1));
    let inner_w = usize::from(PICKER_CARD_W) - 2;

    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut selected_row = None;
    if candidates.is_empty() {
        rows.push(Line::from(Span::styled(
            "no models to offer yet — waiting for the provider snapshot",
            dim,
        )));
        rows.push(Line::from(Span::styled(
            "(`/info` lists providers; `/info refresh` re-syncs the catalog)",
            dim,
        )));
    } else {
        let window = window(sel, candidates.len());
        for (i, spec) in candidates
            .iter()
            .enumerate()
            .skip(window.start)
            .take(window.len())
        {
            let is_sel = i == sel;
            let mut spans = vec![
                cards::marker(is_sel),
                Span::styled(
                    cards::truncate_cols(spec, inner_w.saturating_sub(12)),
                    if is_sel {
                        theme::accent()
                    } else {
                        Style::new().fg(theme::TEXT_PRIMARY)
                    },
                ),
            ];
            // The session's live pin, as a WORD — the golden suite strips
            // style, and this is the row a reader orients on.
            if current.as_deref() == Some(spec.as_str()) {
                spans.push(Span::styled("  · current", dim));
            }
            if is_sel {
                selected_row = Some(i - window.start);
            }
            rows.push(Line::from(spans));
        }
    }

    let context = if candidates.is_empty() {
        Vec::new()
    } else {
        vec![Span::styled(
            format!("{}/{} · this session only", sel + 1, candidates.len()),
            dim,
        )]
    };
    let height = u16::try_from(rows.len()).unwrap_or(u16::MAX);
    let card = cards::card_area(area, height, PICKER_CARD_W, ui.accessible);
    let inner = cards::card_frame(card, "model", context, "↑↓ move · ⏎ use · esc", buf);
    cards::render_body(rows, selected_row, inner, buf);
}

/// Paint the `/agent` picker. A no-op while closed.
pub fn render_agent(ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if !ui.agent_picker.open {
        return;
    }
    let entries = &ui.installed.entries;
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let sel = ui
        .agent_picker
        .selected()
        .min(entries.len().saturating_sub(1));
    let inner_w = usize::from(PICKER_CARD_W) - 2;

    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut selected_row = None;
    if entries.is_empty() {
        rows.push(Line::from(Span::styled(
            if ui.installed.busy {
                "loading installed agents…"
            } else {
                "no installed agents — create one on the AGENTS tab (`/agents`)"
            },
            dim,
        )));
    } else {
        let window = window(sel, entries.len());
        for (i, entry) in entries
            .iter()
            .enumerate()
            .skip(window.start)
            .take(window.len())
        {
            let is_sel = i == sel;
            let name_style = if is_sel {
                theme::accent()
            } else {
                Style::new()
                    .fg(theme::TEXT_PRIMARY)
                    .add_modifier(Modifier::BOLD)
            };
            let used = 2 + entry.name.chars().count() + 2 + entry.scope.label().len() + 2;
            let spans = vec![
                cards::marker(is_sel),
                Span::styled(entry.name.clone(), name_style),
                Span::styled(format!("  {}", entry.scope.label()), dim),
                Span::styled(
                    format!(
                        "  {}",
                        cards::truncate_cols(&entry.description, inner_w.saturating_sub(used))
                    ),
                    dim,
                ),
            ];
            if is_sel {
                selected_row = Some(i - window.start);
            }
            rows.push(Line::from(spans));
        }
    }

    let context = if entries.is_empty() {
        Vec::new()
    } else {
        vec![Span::styled(
            format!("{}/{} · this session only", sel + 1, entries.len()),
            dim,
        )]
    };
    let height = u16::try_from(rows.len()).unwrap_or(u16::MAX);
    let card = cards::card_area(area, height, PICKER_CARD_W, ui.accessible);
    let inner = cards::card_frame(card, "agent", context, "↑↓ move · ⏎ assume · esc", buf);
    cards::render_body(rows, selected_row, inner, buf);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// The shared vocabulary: arrows and `j`/`k` move, Esc/`q` cancel, `⏎`
    /// chooses the highlighted row, and a closed picker claims nothing.
    #[test]
    fn the_picker_moves_chooses_and_cancels() {
        let mut picker = ListPicker::default();
        assert_eq!(picker.key(key(KeyCode::Enter), 3), PickerAction::Ignored);

        picker.raise();
        assert_eq!(picker.key(key(KeyCode::Down), 3), PickerAction::Handled);
        assert_eq!(
            picker.key(key(KeyCode::Char('j')), 3),
            PickerAction::Handled
        );
        assert_eq!(picker.key(key(KeyCode::Enter), 3), PickerAction::Choose(2));
        // `Home`/`End` come with the shared vocabulary (#4370), not a
        // hand-rolled arrow pair.
        assert_eq!(picker.key(key(KeyCode::Home), 3), PickerAction::Handled);
        assert_eq!(picker.key(key(KeyCode::Enter), 3), PickerAction::Choose(0));
        assert_eq!(picker.key(key(KeyCode::End), 3), PickerAction::Handled);
        assert_eq!(picker.key(key(KeyCode::Enter), 3), PickerAction::Choose(2));
        assert_eq!(picker.key(key(KeyCode::Esc), 3), PickerAction::Close);
        assert_eq!(
            picker.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), 3),
            PickerAction::Ignored,
            "a picker must not be the one state you cannot quit from"
        );
    }

    /// A list that shrank between frames clamps the highlight, and `⏎` on
    /// an empty list chooses nothing.
    #[test]
    fn a_shrunken_or_empty_list_never_chooses_past_the_end() {
        let mut picker = ListPicker::default();
        picker.raise();
        for _ in 0..5 {
            picker.key(key(KeyCode::Down), 6);
        }
        assert_eq!(picker.key(key(KeyCode::Enter), 2), PickerAction::Choose(1));
        assert_eq!(picker.key(key(KeyCode::Enter), 0), PickerAction::Handled);
    }

    /// The window slides so the highlight stays visible at either end.
    #[test]
    fn the_window_keeps_the_selection_visible() {
        assert_eq!(window(0, 5), 0..5, "a short list shows whole");
        assert_eq!(window(0, 40), 0..VISIBLE_ROWS);
        let end = window(39, 40);
        assert!(end.contains(&39), "the last row is reachable");
        assert_eq!(end.len(), VISIBLE_ROWS);
        let mid = window(20, 40);
        assert!(mid.contains(&20));
    }
}
