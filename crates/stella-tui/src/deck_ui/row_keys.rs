// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The bare letters a highlighted transcript row lends the keyboard.
//!
//! Four of them today: `u` on a delete event (SPEC 6.3's `· git-backed ·
//! u undo`, SPEC 11), `l` / `r` on a gate board carrying a failure (SPEC
//! 8.1's `^N jump · l full log · r rerun gate`), and `x` on a logged memory
//! (SPEC 6.3's `· x reject`). One module rather than four arms in
//! `deck_ui.rs`, which is a god file closed to growth — and one *shape*,
//! because all four answer the same two questions: does this keystroke belong
//! to the keyboard or to the composer, and does the highlight actually offer
//! this verb?
//!
//! # The rule every letter here follows
//!
//! A bare letter is only a hotkey when the composer is blank and the highlight
//! offers the verb. Otherwise it is a character, and [`act`] answers `None` so
//! the keystroke reaches the composer intact — which is why a prompt containing
//! the word `run` does not lose its `r` because a gate happened to fail three
//! screens up.
//!
//! Grown out of `undo.rs`, which answered exactly one of these questions for
//! exactly one letter.

use crossterm::event::{KeyEvent, KeyModifiers};

use crate::deck::WorkspaceModel;
use crate::deck_ui::{DeckAction, DeckUi};
use crate::envelope::WorkspaceInput;
use crate::model::TranscriptEntry;

/// Whether this keystroke is a bare letter the transcript may claim — no
/// modifier, and a composer with nothing in it.
///
/// Not a check on the *highlight*: that question is [`act`]'s, and splitting
/// them keeps this half cheap enough to run on every letter typed into the
/// composer.
pub(super) fn is_bare_row_key(c: char, key: KeyEvent, ui: &DeckUi) -> bool {
    matches!(c, 'u' | 'l' | 'r' | 'x')
        && !key.modifiers.intersects(
            KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META | KeyModifiers::ALT,
        )
        && ui.composer.is_blank()
}

/// What the highlighted row does with `c`, or `None` to let it be typed.
pub(super) fn act(c: char, model: &WorkspaceModel, ui: &mut DeckUi) -> Option<DeckAction> {
    match c {
        'u' => selected_delete_paths(model, ui)
            .map(|paths| DeckAction::Send(WorkspaceInput::UndoDelete { paths })),
        // `l` opens the failure block to the whole log, and closes it again.
        // It is the deck's existing per-entry expansion rather than a second
        // mechanism beside it, which is what lets `ctrl+o` and the expand-all
        // overlay open a gate board too without either one learning about
        // gates (`deck_ui::is_expandable`).
        'l' => {
            let idx = selected_failed_board(model, ui)?.0;
            toggle_entry_expansion(model, ui, idx);
            Some(DeckAction::Handled)
        }
        // The first failed gate in the rule's order, because the selection is
        // the *board* — the transcript highlights entries, and a board is one
        // entry however many rows it draws. A board with several failures is
        // the case this loses to: a key labelled `rerun gate` re-requests one
        // gate, and a second press re-requests it again rather than moving on.
        'r' => {
            let gate = selected_failed_board(model, ui)?.1;
            Some(DeckAction::Send(WorkspaceInput::RerunGate { gate }))
        }
        // The text travels with the id because the rejection is a tombstone
        // and an id alone would not hold it: the reflection loop re-mines
        // paraphrases, so the same lesson returns tomorrow under a fresh
        // `nod_…`. `stella-store`'s `forget` compares candidates against the
        // content it copied in, which is what catches the restatement.
        'x' => {
            let (memory_id, text) = super::memory::selected_memory(model, ui)?;
            Some(DeckAction::Send(WorkspaceInput::RejectMemory {
                memory_id,
                text,
            }))
        }
        _ => None,
    }
}

/// Flip entry `idx`'s expanded state for the focused agent — what `ctrl+o`
/// does with a selection, and what `l` does to a gate board's failure block.
///
/// Only a genuinely expandable entry toggles: a no-op press must not bump
/// `expanded_rev` and invalidate the settled fold cache.
///
/// The overlay case is why this is one function rather than a call to
/// [`super::toggle_expanded`] at each key. Collapsing ONE row out of the
/// everything-open `ctrl+o` overlay has to materialize that overlay into the
/// per-entry set first, so the toggle closes the highlighted row and leaves the
/// rest open — and a second key reaching the same state without doing it would
/// close every open row on the screen instead of one.
pub(super) fn toggle_entry_expansion(model: &WorkspaceModel, ui: &mut DeckUi, idx: usize) {
    let Some(agent) = model.agents.get(ui.focused) else {
        return;
    };
    if !agent
        .model
        .transcript
        .get(idx)
        .is_some_and(super::is_expandable)
    {
        return;
    }
    let id = agent.meta.id.clone();
    if ui.transcript_expand_all {
        let all: std::collections::HashSet<usize> = agent
            .model
            .transcript
            .iter()
            .enumerate()
            .filter(|(_, e)| super::is_expandable(e))
            .map(|(i, _)| i)
            .collect();
        ui.expanded.insert(id.clone(), all);
        ui.transcript_expand_all = false;
    }
    super::toggle_expanded(ui, &id, idx);
}

/// The highlighted gate board that carries a failure, as `(entry index, the
/// name of its first failed gate)`.
///
/// `None` for any other selection — including a board whose gates all held, and
/// one whose gates went undecided. Neither has a log to open or a failure to
/// re-request, and lending them the keys would take `l` and `r` away from the
/// composer on a row that cannot use them.
fn selected_failed_board(model: &WorkspaceModel, ui: &DeckUi) -> Option<(usize, String)> {
    let idx = ui.session_selected?;
    let transcript = &model.agents.get(ui.focused)?.model.transcript;
    let TranscriptEntry::GateBoard { board } = transcript.get(idx)? else {
        return None;
    };
    let failed = board.gates.iter().find(|gate| gate.failed())?;
    Some((idx, failed.name.clone()))
}

/// The delete event under the transcript highlight, as the paths that one
/// `delete_file` call removed — `None` for any other selection, which leaves
/// `u` to the composer.
///
/// Both rows of the visual block answer: the call head (which carries the
/// `· git-backed · u undo` label) and its paired result, resolved back to the
/// head by `call_id` — a reader's ↑ from the bottom lands on the result
/// first, and the affordance must not depend on knowing which of the two is
/// highlighted. A batch delete carries its targets in the call's raw argument
/// object rather than the head's `path`.
fn selected_delete_paths(model: &WorkspaceModel, ui: &DeckUi) -> Option<Vec<String>> {
    let idx = ui.session_selected?;
    let transcript = &model.agents.get(ui.focused)?.model.transcript;
    let (name, path, raw) =
        match transcript.get(idx)? {
            TranscriptEntry::ToolStart {
                name, path, raw, ..
            } => (name, path, raw),
            TranscriptEntry::ToolResult { call_id, .. } => transcript
                .iter()
                .take(idx)
                .rev()
                .find_map(|entry| match entry {
                    TranscriptEntry::ToolStart {
                        call_id: start_id,
                        name,
                        path,
                        raw,
                        ..
                    } if start_id == call_id => Some((name, path, raw)),
                    _ => None,
                })?,
            _ => return None,
        };
    if name != "delete_file" {
        return None;
    }
    if let Some(path) = path {
        return Some(vec![path.clone()]);
    }
    let parsed: serde_json::Value = serde_json::from_str(raw).ok()?;
    let paths: Vec<String> = parsed
        .get("files")?
        .as_array()?
        .iter()
        .filter_map(|f| Some(f.get("path")?.as_str()?.to_string()))
        .collect();
    (!paths.is_empty()).then_some(paths)
}
