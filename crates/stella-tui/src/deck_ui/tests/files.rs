//! **The witness (#4368).** The FILES tab's one row of [`crate::keymap`] —
//! `⏎`, "open / close the diff" — pressed through [`super::handle_deck_key`].
//!
//! `v2::files_tab`'s render tests set `files_diff_open` by hand, so the key
//! that flips it had no witness at all: the row was a claim about an arm of
//! `handle_files_key` nothing had pressed.

use super::*;
use stella_protocol::{AgentEvent, FileChangeKind};

fn model_with_one_change() -> WorkspaceModel {
    let mut m = model_with(&["lead"]);
    m.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::FileChange {
            path: "src/existing.rs".into(),
            kind: FileChangeKind::Modified,
            added: 2,
            removed: 1,
            diff: Some("@@ -1,2 +1,3 @@\n context\n-old\n+new\n".into()),
        },
    });
    m
}

/// ⏎ on a ledger row opens the diff and ⏎ again closes it — the same key
/// both ways, so there is nothing to remember about getting back out.
#[test]
fn files_enter_opens_and_closes_the_diff() {
    let model = model_with_one_change();
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Files);
    assert!(!ui.files_diff_open);

    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert!(ui.files_diff_open, "⏎ opened the diff");
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert!(!ui.files_diff_open, "⏎ closed it again");
}

/// With a prompt in progress ⏎ submits it, and an empty ledger has no diff to
/// open — neither case may leave the tab in a state the row cannot explain.
#[test]
fn files_enter_yields_to_a_prompt_and_to_an_empty_ledger() {
    let model = model_with_one_change();
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Files);
    handle_deck_key(ch('g'), &model, &mut ui);
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert!(!ui.files_diff_open, "⏎ queued the prompt instead");

    let empty = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Files);
    handle_deck_key(key(KeyCode::Enter), &empty, &mut ui);
    assert!(!ui.files_diff_open, "no rows, no diff to open");
}
