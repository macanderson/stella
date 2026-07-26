//! The deck's half of transcript navigation: the search bar's state, the keys
//! that drive it, and the fold bookkeeping behind `ctrl+z`.
//!
//! Split from `deck_ui` because these are a coherent feature rather than more
//! of the deck's key table, and because the pure answers they ask for live in
//! [`crate::transcript_nav`] — keeping the glue in one file makes the seam
//! between "what the reader pressed" and "where that lands" legible.
//!
//! The recurring rule here: both search and failure-seek finish by setting
//! `session_selected` and `session_pending_scroll`, which the session view
//! resolves where visual row ranges are known. Nothing in this module needs to
//! know how tall a row is.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{DeckAction, DeckUi};
use crate::deck::WorkspaceModel;

/// The transcript search bar's state.
///
/// `hits` are entry indices, not visual rows: rows are rebuilt every keystroke
/// and are private to the fold, while an entry index is stable and plugs
/// straight into the selection machinery `↑`/`↓` already drives — so scrolling
/// a match into view costs nothing new.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TranscriptSearch {
    /// Whether the bar is up and claiming keys.
    pub open: bool,
    /// What the reader has typed.
    pub query: String,
    /// Entry indices matching `query`, ascending.
    pub hits: Vec<usize>,
    /// Index into `hits` of the match currently selected.
    pub cursor: usize,
}

impl TranscriptSearch {
    /// Recompute `hits` for the current query, keeping the reader as close as
    /// possible to where they were. Re-anchoring to the nearest surviving hit
    /// (rather than resetting to 0) is what makes typing another character
    /// feel like *narrowing* rather than starting over.
    pub fn refresh(&mut self, transcript: &[crate::model::TranscriptEntry]) {
        let anchor = self.hits.get(self.cursor).copied();
        self.hits = crate::transcript_nav::search_hits(transcript, &self.query);
        self.cursor = match anchor {
            Some(a) => self
                .hits
                .iter()
                .position(|&h| h >= a)
                .unwrap_or(self.hits.len().saturating_sub(1)),
            None => 0,
        };
    }

    /// The currently selected match, if any.
    #[must_use]
    pub fn current(&self) -> Option<usize> {
        self.hits.get(self.cursor).copied()
    }

    /// Step to the next (or previous) match, wrapping.
    pub fn step(&mut self, forward: bool) -> Option<usize> {
        if self.hits.is_empty() {
            return None;
        }
        let n = self.hits.len();
        self.cursor = if forward {
            (self.cursor + 1) % n
        } else {
            (self.cursor + n - 1) % n
        };
        self.current()
    }
}

/// Transcript search, modal while the bar is up.
///
/// Modelled on find-in-page rather than on a filter box: the transcript never
/// stops showing everything, matches are *visited* rather than isolated, and
/// the reader keeps the surrounding context that makes a hit interpretable.
/// A filter would answer "which rows mention this"; the question a reader
/// actually has is "take me to where this happened".
///
/// Searching is incremental — every keystroke re-anchors the selection onto
/// the nearest surviving hit, so narrowing feels continuous rather than like
/// starting over.
pub(super) fn handle_search_key(
    key: KeyEvent,
    model: &WorkspaceModel,
    ui: &mut DeckUi,
) -> DeckAction {
    let Some(agent) = model.agents.get(ui.focused) else {
        ui.search.open = false;
        return DeckAction::Handled;
    };
    let transcript = &agent.model.transcript;
    let modified = key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META);
    match key.code {
        // Esc leaves the transcript where the reader was left looking, rather
        // than snapping back — they searched their way here on purpose.
        KeyCode::Esc => {
            ui.search.open = false;
            DeckAction::Handled
        }
        KeyCode::Enter | KeyCode::Down => {
            step_search(model, ui, true);
            DeckAction::Handled
        }
        KeyCode::Up => {
            step_search(model, ui, false);
            DeckAction::Handled
        }
        KeyCode::Char('n') if modified => {
            step_search(model, ui, true);
            DeckAction::Handled
        }
        KeyCode::Char('p') if modified => {
            step_search(model, ui, false);
            DeckAction::Handled
        }
        KeyCode::Backspace => {
            ui.search.query.pop();
            ui.search.refresh(transcript);
            reveal_current_match(model, ui);
            DeckAction::Handled
        }
        KeyCode::Char(c) if !modified => {
            ui.search.query.push(c);
            ui.search.refresh(transcript);
            reveal_current_match(model, ui);
            DeckAction::Handled
        }
        _ => DeckAction::Ignored,
    }
}

/// Move to the next/previous match and bring it into view.
pub(super) fn step_search(model: &WorkspaceModel, ui: &mut DeckUi, forward: bool) {
    ui.search.step(forward);
    reveal_current_match(model, ui);
}

/// Select the current match and queue the scroll that reveals it.
///
/// A hit inside a folded turn would otherwise be scrolled to and then not be
/// there, so finding one unfolds its turn: search outranks folding, because a
/// reader who asked for a thing by name has said they want to see it.
pub(super) fn reveal_current_match(model: &WorkspaceModel, ui: &mut DeckUi) {
    let Some(idx) = ui.search.current() else {
        return;
    };
    ui.session_selected = Some(idx);
    ui.session_pending_scroll = Some(idx);
    unfold_containing(model, ui, idx);
}

/// Make sure entry `idx` is actually on screen by unfolding the turn holding
/// it, if that turn is folded.
///
/// Search and failure-seek both outrank folding: a reader who asked for a
/// thing by name, or asked to be taken to what broke, has said they want to
/// see it — scrolling to a row that is folded away would be a lie.
pub(super) fn unfold_containing(model: &WorkspaceModel, ui: &mut DeckUi, idx: usize) {
    let Some(agent) = model.agents.get(ui.focused) else {
        return;
    };
    let turns = crate::transcript_nav::turns(&agent.model.transcript);
    let Some(turn) = crate::transcript_nav::turn_at(&turns, idx) else {
        return;
    };
    let start = turn.rows.start;
    if !is_folded(ui, &agent.meta.id, turn) {
        return;
    }
    // `folded_turns` is a set of *exceptions* to `fold_all_turns`, so the same
    // toggle unfolds under the overlay and folds without it.
    let set = ui.folded_turns.entry(agent.meta.id.clone()).or_default();
    if !set.remove(&start) {
        set.insert(start);
    }
    ui.fold_rev += 1;
}

/// Whether a turn currently renders folded.
///
/// The per-turn set is an *exception list* against the `ctrl+z` overlay rather
/// than an independent state, which is what lets one key both "fold this" and
/// "unfold this out of the fold-all" without the reader learning two verbs.
/// An unfinished turn never folds, whatever the sets say.
pub(crate) fn is_folded(ui: &DeckUi, agent: &str, turn: &crate::transcript_nav::Turn) -> bool {
    if !turn.finished {
        return false;
    }
    let listed = ui
        .folded_turns
        .get(agent)
        .is_some_and(|s| s.contains(&turn.rows.start));
    ui.fold_all_turns != listed
}

/// Jump the selection to the next (or previous) failure and reveal it.
pub(super) fn seek_failure(model: &WorkspaceModel, ui: &mut DeckUi, forward: bool) -> DeckAction {
    let Some(agent) = model.agents.get(ui.focused) else {
        return DeckAction::Handled;
    };
    // A `None` is left deliberately unhandled: nothing went wrong in this
    // session, and leaving the selection alone says that more honestly than
    // moving it somewhere arbitrary would.
    if let Some(idx) =
        crate::transcript_nav::seek_failure(&agent.model.transcript, ui.session_selected, forward)
    {
        ui.session_selected = Some(idx);
        ui.session_pending_scroll = Some(idx);
        unfold_containing(model, ui, idx);
    }
    DeckAction::Handled
}

/// `ctrl+z`: fold the selected turn, or with no selection every finished one.
///
/// The mirror of `ctrl+o`'s two modes, deliberately: one overlay key that
/// works on the selection when there is one and on the whole transcript when
/// there is not is a rule worth having twice rather than two different rules.
pub(super) fn toggle_fold(model: &WorkspaceModel, ui: &mut DeckUi) -> DeckAction {
    let Some(agent) = model.agents.get(ui.focused) else {
        return DeckAction::Handled;
    };
    let turns = crate::transcript_nav::turns(&agent.model.transcript);
    match ui.session_selected {
        Some(sel) => {
            // Only a settled turn folds. Collapsing work that is still
            // arriving would hide the very thing the reader is watching.
            let Some(turn) = crate::transcript_nav::turn_at(&turns, sel).filter(|t| t.finished)
            else {
                return DeckAction::Handled;
            };
            let start = turn.rows.start;
            let set = ui.folded_turns.entry(agent.meta.id.clone()).or_default();
            if !set.remove(&start) {
                set.insert(start);
            }
            // A row about to be folded away would leave the highlight pointing
            // at nothing; move it to the digest line that replaces it.
            ui.session_selected = Some(start);
            ui.session_pending_scroll = Some(start);
            ui.fold_rev += 1;
        }
        None => {
            ui.fold_all_turns = !ui.fold_all_turns;
            ui.fold_rev += 1;
        }
    }
    DeckAction::Handled
}
