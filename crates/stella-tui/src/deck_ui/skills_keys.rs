// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The SKILLS scope picker's keys — where a skill an install or a create is
//! about to produce gets written, project or user.
//!
//! Split from `deck_ui.rs` (#629's 1500-line ratchet) when the picker's
//! movement was routed through [`list_nav`](crate::deck_ui::list_nav), the way
//! `mcp_keys` and `issues_keys` were split before it.
//!
//! Two options, and the picker draws them **side by side**, which is why it
//! keeps `←`/`→` of its own: `list_nav` moves a vertical list and has no
//! horizontal axis. Everything else — `↑`/`↓`, `j`/`k`, `⇞`/`⇟`, `Home`/`End`
//! — is the deck's one vocabulary, so the picker no longer answers the arrows
//! differently from every list behind it (#4370). `p`/`u` stay as the
//! mnemonics that name the two scopes rather than a direction.

use crossterm::event::{KeyCode, KeyEvent};

use super::{DeckAction, DeckUi, ScopeAction, SkillPrompt, create, list_nav};
use crate::envelope::SkillScope;

/// The two scopes in the order the picker draws them: project first, because
/// a skill that travels with the repository is the one a teammate also gets.
const PROJECT: usize = 0;
const USER: usize = 1;

/// The scope picker's keys. Fully modal: every key is swallowed so nothing
/// leaks into the composer behind the overlay.
pub(super) fn handle_scope_key(
    key: KeyEvent,
    ui: &mut DeckUi,
    action: ScopeAction,
    user: bool,
) -> DeckAction {
    match key.code {
        KeyCode::Esc => {
            ui.skills.prompt = None;
            ui.skills.status = Some("cancelled".into());
            DeckAction::Handled
        }
        KeyCode::Enter => {
            let scope = if user {
                SkillScope::User
            } else {
                SkillScope::Project
            };
            ui.skills.searching = true;
            create::dispatch_skills_scope(ui, action, scope)
        }
        _ => {
            let mut sel = if user { USER } else { PROJECT };
            let moved = list_nav::select(key, &mut sel, 2, true) || horizontal(key, &mut sel);
            if moved {
                ui.skills.prompt = Some(SkillPrompt::Scope {
                    action,
                    user: sel == USER,
                });
            }
            // Modal: swallow everything else.
            DeckAction::Handled
        }
    }
}

/// The picker's own axis, which `list_nav` does not have: the two options sit
/// left and right, and `p`/`u` name them.
fn horizontal(key: KeyEvent, sel: &mut usize) -> bool {
    match key.code {
        KeyCode::Left | KeyCode::Char('p' | 'P') => {
            *sel = PROJECT;
            true
        }
        KeyCode::Right | KeyCode::Char('u' | 'U') => {
            *sel = USER;
            true
        }
        _ => false,
    }
}
