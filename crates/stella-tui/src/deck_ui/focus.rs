// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The deck's one navigation vocabulary — the focus tree.
//!
//! Everything on the deck sits in a tree, and four keys walk it the same way
//! at every level:
//!
//! ```text
//! tabs ───── SESSION ── AGENTS ── TRACES ── GRAPH ── FILES ── SKILLS ── …
//!               │                                       │        │
//!               ├ lead ─ message ─ message ─ …          │     installed ─ search
//!               │   └ sub:2 ─ message ─ …               │
//!               └ queue                              file ─ diff
//! ```
//!
//! | key | meaning |
//! |---|---|
//! | `←` `→` | the previous / next **sibling**: the pane beside this one, or — when the tab has no pane that way — the tab beside this one |
//! | `↑` `↓` | the previous / next **item** in the list under the cursor |
//! | `⏎` | **open** what is under the cursor: a file's diff, a lane's transcript, a message's full text |
//! | `⌫` | **back** up one level, and nothing else — it never reaches a running turn |
//!
//! `Esc` is a fifth key with two halves: the first is `⌫`'s (peel one layer)
//! and the second, once there is nothing left to peel, is the turn interrupt
//! (`deck_ui`'s Esc precedence list). `⌫` exists so the reader who only wants
//! *out* has a key that cannot also mean *stop*.
//!
//! ## Bubbling
//!
//! A key is offered to the deepest focused node first and rises to its parent
//! when that node declines it — the DOM's event model, and the reason one
//! vocabulary can serve every tab. `→` on SETTINGS's AGENTS pane moves to the
//! TOOLS pane; `→` on TOOLS, the last pane, has no sibling there, so the key
//! rises to the tab strip and moves to the next tab. Nothing in this module
//! knows the panes: each tab's handler claims what it can and returns `None`
//! for what it cannot, and [`handle_deck_key`](super::handle_deck_key) calls
//! [`step_tab`] only once every handler below has declined.
//!
//! ## The empty-composer rule
//!
//! Every arrow here fires only from an **empty** composer. With text in it
//! `←`/`→` are cursor motion and `⌫` deletes a character, exactly as a reader
//! typing a prompt expects; the tree is what the keys mean when there is no
//! prompt for them to edit. This is the deck's standing rule for bare-letter
//! hotkeys (`crates/stella-tui/README.md`, "Keys are a precedence chain"),
//! applied to the arrows.
//!
//! Tab / Shift-Tab keep cycling tabs beside `←`/`→`: the chord was taught
//! and costs nothing to keep. #4341 asked which of the two the tui-v2
//! renderings' `tab` should be; the answer this module gives is that Tab
//! stays a tab key, the plan panel stays on `⌃S`, and the breadcrumb prints
//! the key that is real.

use crossterm::event::KeyCode;

use super::{DeckAction, DeckUi, queue_issues_first_load};
use crate::deck::{DeckTab, WorkspaceModel};

/// Which way a sibling step goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Prev,
    Next,
}

impl Dir {
    /// The sibling direction an arrow key names, if it names one.
    pub fn of(code: KeyCode) -> Option<Self> {
        match code {
            KeyCode::Left => Some(Dir::Prev),
            KeyCode::Right => Some(Dir::Next),
            _ => None,
        }
    }
}

/// Move to the sibling tab. The last resort of a `←`/`→` nothing deeper
/// claimed — see the module doc on bubbling. `DeckTab::next`/`prev` wrap,
/// so the strip is a ring and the far end is one press away.
pub fn step_tab(dir: Dir, ui: &mut DeckUi) -> DeckAction {
    let tab = match dir {
        Dir::Prev => ui.tab.prev(),
        Dir::Next => ui.tab.next(),
    };
    ui.set_tab(tab);
    queue_issues_first_load(ui);
    DeckAction::Handled
}

/// Back up one level from wherever the reader is, or `None` when there is
/// nothing above. Checked innermost first, so each press peels exactly one
/// layer and every way in has the same way out:
///
/// 1. the FILES diff — back to the file list;
/// 2. a highlighted SESSION message — back to the unhighlighted tail;
/// 3. the expand-all / fold-all transcript overlays — back to the plain fold;
/// 4. an opened lane — back to the agent that dispatched it.
///
/// Deliberately **not** here: stopping anything. A reader who opened a lane
/// to look at it and pressed `⌫` to leave must get the dispatcher back, never
/// a cancelled worker — which is what the first Esc at a running lane used
/// to be.
pub fn back(model: &WorkspaceModel, ui: &mut DeckUi) -> Option<DeckAction> {
    if ui.tab == DeckTab::Files && ui.files_diff_open {
        ui.files_diff_open = false;
        return Some(DeckAction::Handled);
    }
    if ui.tab == DeckTab::Session {
        if ui.session_selected.take().is_some() {
            return Some(DeckAction::Handled);
        }
        if ui.transcript_expand_all {
            ui.transcript_expand_all = false;
            return Some(DeckAction::Handled);
        }
        if ui.fold_all_turns {
            ui.fold_all_turns = false;
            ui.fold_rev += 1;
            return Some(DeckAction::Handled);
        }
        return back_to_dispatcher(model, ui);
    }
    None
}

/// From an opened lane, back to the agent that dispatched it. `None` at a
/// root or when the parent is not on the deck.
pub fn back_to_dispatcher(model: &WorkspaceModel, ui: &mut DeckUi) -> Option<DeckAction> {
    let parent = model.parent_of(ui.focused)?;
    ui.focus_agent(parent);
    Some(DeckAction::Handled)
}

/// `⏎` on the SESSION tab from an empty composer: open the highlighted
/// message. `None` when nothing is highlighted (the blank-composer `⏎`
/// submits nothing and stays ignored) or the message has nothing folded.
pub fn open_selected(model: &WorkspaceModel, ui: &mut DeckUi) -> Option<DeckAction> {
    let sel = ui.session_selected?;
    let agent = model.agents.get(ui.focused)?;
    if !agent
        .model
        .transcript
        .get(sel)
        .is_some_and(super::is_expandable)
    {
        return None;
    }
    super::toggle_expanded(ui, &agent.meta.id.clone(), sel);
    Some(DeckAction::Handled)
}
