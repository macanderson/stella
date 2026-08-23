// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The one vocabulary for moving through a list or a scrolling body.
//!
//! Every overlay and tab used to spell its own `↑`/`↓` arms, and they drifted:
//! `j`/`k` worked in five overlays and not the other nine, `Home`/`End` in
//! two, `space` paged one. A reader who learned the keys in one place was
//! wrong everywhere else. These two functions are the keys, and a surface
//! that takes a list or a scroll calls one of them instead of matching
//! `KeyCode` itself.
//!
//! | key | list ([`select`]) | body ([`scroll`]) |
//! |---|---|---|
//! | `↑` `k` | previous item | one line up |
//! | `↓` `j` | next item | one line down |
//! | `⇞` `⇟` | a page of items | a page of lines |
//! | `Home` `End` | first / last item | top / bottom |
//! | `space` | — | a page down |
//!
//! `space` pages a body and does nothing to a list on purpose: on a list it
//! is a toggle (SKILLS enable, ISSUES multiselect) and must stay one.
//!
//! Both return `true` when the key was one of theirs, so the caller's own
//! arms (open, close, verbs) sit after the call and never shadow a
//! navigation key.
//!
//! `letters` is the deck's empty-composer rule applied here: a modal overlay
//! owns the keyboard and passes `true`; a tab passes `composer_empty`, so
//! `j` and `k` build a prompt while one is being typed and move the list
//! only when there is no prompt for them to join. The arrows are never
//! gated — they edit nothing in an empty composer and nothing but the cursor
//! in a full one, and the tab handlers run only once the composer declined.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::scroll::ScrollState;

/// Rows one `⇞`/`⇟` moves a selection when the surface reports no height.
const LIST_PAGE: usize = 8;

/// Move a selection through `count` items. A `count` of zero leaves `sel`
/// at zero; the caller clamps a selection that outlived its list.
pub fn select(key: KeyEvent, sel: &mut usize, count: usize, letters: bool) -> bool {
    if !plain(key) {
        return false;
    }
    let last = count.saturating_sub(1);
    match key.code {
        KeyCode::Up => *sel = sel.saturating_sub(1),
        KeyCode::Down => *sel = (*sel + 1).min(last),
        KeyCode::Char('k') if letters => *sel = sel.saturating_sub(1),
        KeyCode::Char('j') if letters => *sel = (*sel + 1).min(last),
        KeyCode::PageUp => *sel = sel.saturating_sub(LIST_PAGE),
        KeyCode::PageDown => *sel = (*sel + LIST_PAGE).min(last),
        KeyCode::Home => *sel = 0,
        KeyCode::End => *sel = last,
        _ => return false,
    }
    true
}

/// Scroll a body of `total` lines through a viewport `height` lines tall.
pub fn scroll(
    key: KeyEvent,
    state: &mut ScrollState,
    total: usize,
    height: usize,
    letters: bool,
) -> bool {
    if !plain(key) {
        return false;
    }
    match key.code {
        KeyCode::Up => state.scroll_up(1, total, height),
        KeyCode::Down => state.scroll_down(1, total, height),
        KeyCode::Char('k') if letters => state.scroll_up(1, total, height),
        KeyCode::Char('j') if letters => state.scroll_down(1, total, height),
        KeyCode::Char(' ') if letters => state.page_down(total, height),
        KeyCode::PageUp => state.page_up(total, height),
        KeyCode::PageDown => state.page_down(total, height),
        KeyCode::Home => state.to_top(),
        KeyCode::End => state.to_bottom(),
        _ => return false,
    }
    true
}

/// Lines one `⇞`/`⇟` moves an unclamped offset.
const OFFSET_PAGE: usize = 10;

/// Scroll a body whose height only its renderer knows: the offset grows
/// unbounded here and the draw clamps it. `Home` is zero; `End` is
/// `usize::MAX`, which the same clamp turns into the bottom.
pub fn offset(key: KeyEvent, top: &mut usize, letters: bool) -> bool {
    if !plain(key) {
        return false;
    }
    match key.code {
        KeyCode::Up => *top = top.saturating_sub(1),
        KeyCode::Down => *top = top.saturating_add(1),
        KeyCode::Char('k') if letters => *top = top.saturating_sub(1),
        KeyCode::Char('j') if letters => *top = top.saturating_add(1),
        KeyCode::Char(' ') if letters => *top = top.saturating_add(OFFSET_PAGE),
        KeyCode::PageUp => *top = top.saturating_sub(OFFSET_PAGE),
        KeyCode::PageDown => *top = top.saturating_add(OFFSET_PAGE),
        KeyCode::Home => *top = 0,
        KeyCode::End => *top = usize::MAX,
        _ => return false,
    }
    true
}

/// Whether `key` closes a modal surface: `Esc` everywhere, and `q` as the
/// hand-on-the-home-row synonym. Every overlay answers this the same way so
/// the way out never has to be looked up.
pub fn closes(key: KeyEvent) -> bool {
    plain(key) && matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
}

/// A key with no Ctrl/Cmd/Alt on it — Shift is allowed, so `End` on a
/// keyboard that reports it shifted still counts.
fn plain(key: KeyEvent) -> bool {
    !key.modifiers.intersects(
        KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META | KeyModifiers::ALT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// `j`/`k` and the arrows are the same key on a list, and the ends clamp.
    #[test]
    fn a_list_takes_arrows_and_jk_alike_and_clamps() {
        let mut sel = 0;
        assert!(select(key(KeyCode::Char('j')), &mut sel, 3, true));
        assert_eq!(sel, 1);
        assert!(select(key(KeyCode::Down), &mut sel, 3, true));
        assert!(select(key(KeyCode::Down), &mut sel, 3, true));
        assert_eq!(sel, 2, "clamped at the last item");
        assert!(select(key(KeyCode::Char('k')), &mut sel, 3, true));
        assert_eq!(sel, 1);
        assert!(select(key(KeyCode::Home), &mut sel, 3, true));
        assert_eq!(sel, 0);
        assert!(select(key(KeyCode::End), &mut sel, 3, true));
        assert_eq!(sel, 2);
        assert!(
            !select(key(KeyCode::Char(' ')), &mut sel, 3, true),
            "space is not a list key"
        );
        assert!(!select(key(KeyCode::Enter), &mut sel, 3, true));
        let mut empty = 0;
        assert!(select(key(KeyCode::Down), &mut empty, 0, true));
        assert_eq!(empty, 0, "an empty list stays at zero");
        let mut typing = 1;
        assert!(
            !select(key(KeyCode::Char('j')), &mut typing, 3, false),
            "on a tab with a prompt in progress `j` is a letter"
        );
        assert!(select(key(KeyCode::Down), &mut typing, 3, false));
        assert_eq!(typing, 2, "the arrow still moves it");
    }

    /// A ctrl chord is never a list key, so `ctrl-k`/`ctrl-j` stay free for
    /// the deck.
    #[test]
    fn a_modified_key_is_not_navigation() {
        let mut sel = 1;
        let ctrl_k = KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL);
        assert!(!select(ctrl_k, &mut sel, 3, true));
        assert_eq!(sel, 1);
        let mut state = ScrollState::default();
        assert!(!scroll(ctrl_k, &mut state, 10, 3, true));
        assert!(!closes(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL
        )));
        assert!(closes(key(KeyCode::Char('q'))));
        assert!(closes(key(KeyCode::Esc)));
    }

    /// A body pages on space and jumps on Home/End.
    #[test]
    fn a_body_scrolls_pages_and_jumps() {
        let mut state = ScrollState::default();
        state.to_top();
        assert!(scroll(key(KeyCode::Char('j')), &mut state, 20, 5, true));
        assert_eq!(state.window(20, 5).start, 1);
        assert!(scroll(key(KeyCode::Char(' ')), &mut state, 20, 5, true));
        assert_eq!(state.window(20, 5).start, 6);
        assert!(scroll(key(KeyCode::End), &mut state, 20, 5, true));
        assert!(state.follow);
        assert!(scroll(key(KeyCode::Home), &mut state, 20, 5, true));
        assert_eq!(state.window(20, 5).start, 0);
    }
}
