//! The queue editor's modal keys. Everything the design doc promises for the
//! queue-as-a-list: per-item delete, pull-back-to-edit, and an explicit
//! two-press clear-all. Split out of `deck_ui.rs` beside `nav`/`gates` under
//! the god-file rule.

use super::*;

/// One key while the editor popup is open. Modal: the popup owns the keyboard
/// until Esc (or emptiness) closes it.
pub(super) fn handle_queue_key(
    key: KeyEvent,
    model: &WorkspaceModel,
    ui: &mut DeckUi,
) -> DeckAction {
    let count = model.queue.pending();
    if count == 0 {
        // Nothing left to edit — any key just closes the popup.
        ui.queue_open = false;
        ui.queue_confirm_clear = false;
        return DeckAction::Handled;
    }
    ui.queue_sel = ui.queue_sel.min(count - 1);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('d') if ctrl => {
            if ui.queue_confirm_clear {
                ui.queue_confirm_clear = false;
                ui.queue_open = false;
                return DeckAction::Send(WorkspaceInput::QueueClear);
            }
            ui.queue_confirm_clear = true;
            return DeckAction::Handled;
        }
        _ => ui.queue_confirm_clear = false,
    }
    if super::list_nav::select(key, &mut ui.queue_sel, count, true) {
        return DeckAction::Handled;
    }
    match key.code {
        KeyCode::Char('x') if ctrl => DeckAction::Send(WorkspaceInput::QueueRemove {
            index: ui.queue_sel,
        }),
        KeyCode::Enter => {
            // Pull the prompt out of the queue and into the composer to edit —
            // it is *removed*, not duplicated; re-submitting re-enqueues it.
            let index = ui.queue_sel;
            if let Some(item) = model.queue.items.get(index) {
                ui.composer.load(item.text.clone());
            }
            ui.queue_open = false;
            DeckAction::Send(WorkspaceInput::QueueRemove { index })
        }
        KeyCode::Esc | KeyCode::Char('q') => {
            ui.queue_open = false;
            DeckAction::Handled
        }
        _ => DeckAction::Ignored,
    }
}
