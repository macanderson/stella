// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Argument completion for a slash command — the `/model <fragment>` popup.
//!
//! The slash menu ([`crate::composer::SlashMenu`]) deliberately closes at the
//! first whitespace: the command has been chosen, and what follows is the
//! argument. For a command whose argument comes from a known vocabulary —
//! `/model`'s is the active provider's model list — that is exactly when a
//! second menu earns its place: type `/model gl` and the candidates narrow
//! as you type, Tab completes, ⏎ submits the completed command.
//!
//! Same shape as the slash menu: the caller owns the candidate vocabulary.
//! The deck's caller is `v2::picker`'s `typeahead_candidates`, which
//! narrows the `/model` picker's own list — `allowed_models` when one is
//! configured, else the credentialed catalog — to the session's active
//! provider; this module only filters and navigates. One argument, no
//! quoting: a second whitespace-separated word closes the menu and the text
//! submits as typed.

use crossterm::event::{KeyCode, KeyEvent};

use super::{Composer, SlashPopupOutcome};

/// The filtered candidate list for `command`'s argument, or empty when the
/// popup should be inactive — the composer is not `command` plus a single
/// in-progress argument word, or nothing matches.
///
/// Ranking mirrors the slash menu's: prefix matches first, substring matches
/// second, stable within a rank so the caller's vocabulary order survives.
/// The match is case-insensitive on the ASCII fold; model slugs are ASCII.
/// A bare `"{command} "` (argument not started) offers everything.
pub fn arg_matches(composer: &Composer, command: &str, candidates: &[String]) -> Vec<String> {
    let Some(fragment) = active_fragment(composer, command) else {
        return Vec::new();
    };
    let needle = fragment.to_ascii_lowercase();
    let mut ranked: Vec<(u8, &String)> = candidates
        .iter()
        .filter_map(|c| {
            let hay = c.to_ascii_lowercase();
            if hay.starts_with(&needle) {
                Some((0, c))
            } else if hay.contains(&needle) {
                Some((1, c))
            } else {
                None
            }
        })
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, c)| c.clone()).collect()
}

/// The in-progress argument word, or `None` when the composer is not in
/// `command`-argument position: chips present, a different head, no space
/// typed yet (the slash menu still owns the popup), or a second word begun.
fn active_fragment<'a>(composer: &'a Composer, command: &str) -> Option<&'a str> {
    if !composer.chips().is_empty() {
        return None;
    }
    let rest = composer.buffer().strip_prefix(command)?;
    let rest = rest.strip_prefix(' ')?;
    if rest.contains(char::is_whitespace) {
        return None;
    }
    Some(rest)
}

/// Argument-popup navigation, mirroring
/// [`super::handle_slash_popup_key`]: ↑/↓ choose, Tab completes the argument
/// into the buffer, ⏎ submits `"{command} {selection}"`, Esc clears the
/// composer. `None` for keys the popup does not claim, so typing keeps
/// narrowing the fragment.
pub fn handle_arg_popup_key(
    key: KeyEvent,
    command: &str,
    matches: &[String],
    composer: &mut Composer,
    selected: &mut usize,
) -> Option<SlashPopupOutcome> {
    debug_assert!(
        !matches.is_empty(),
        "handle_arg_popup_key called with no matches — the popup is inactive"
    );
    if matches.is_empty() {
        return None;
    }
    let sel = (*selected).min(matches.len() - 1);
    match key.code {
        KeyCode::Up => {
            *selected = sel.saturating_sub(1);
            Some(SlashPopupOutcome::Handled)
        }
        KeyCode::Down => {
            *selected = (sel + 1).min(matches.len() - 1);
            Some(SlashPopupOutcome::Handled)
        }
        KeyCode::Tab => {
            composer.load(format!("{command} {}", matches[sel]));
            *selected = 0;
            Some(SlashPopupOutcome::Handled)
        }
        KeyCode::Enter => {
            composer.clear();
            *selected = 0;
            Some(SlashPopupOutcome::Submit(format!(
                "{command} {}",
                matches[sel]
            )))
        }
        KeyCode::Esc => {
            composer.clear();
            *selected = 0;
            Some(SlashPopupOutcome::Handled)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn candidates() -> Vec<String> {
        vec![
            "zai/glm-5.2".to_string(),
            "zai/glm-5.1".to_string(),
            "zai/glm-4.5-air".to_string(),
        ]
    }

    fn composer_with(text: &str) -> Composer {
        let mut c = Composer::default();
        c.load(text);
        c
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// **The witness.** `/model ` with an argument fragment filters the
    /// candidate vocabulary as you type — prefix matches lead — where the
    /// slash menu has already closed (it ends at the first whitespace).
    #[test]
    fn the_argument_menu_opens_where_the_slash_menu_closed() {
        let c = composer_with("/model zai/glm-5");
        assert!(
            c.slash_menu(&[], &crate::composer::PaletteState::default())
                .is_none(),
            "the slash menu is closed once the argument begins"
        );
        let matches = arg_matches(&c, "/model", &candidates());
        assert_eq!(matches, vec!["zai/glm-5.2", "zai/glm-5.1"]);
    }

    #[test]
    fn a_bare_command_with_a_space_offers_every_candidate() {
        let c = composer_with("/model ");
        assert_eq!(arg_matches(&c, "/model", &candidates()).len(), 3);
    }

    #[test]
    fn a_substring_match_ranks_after_a_prefix_match() {
        let c = composer_with("/model glm");
        let matches = arg_matches(&c, "/model", &candidates());
        // No candidate starts with "glm" (all carry the provider prefix), so
        // all three are substring matches in vocabulary order.
        assert_eq!(matches.len(), 3);
        let c = composer_with("/model zai/glm-4");
        assert_eq!(
            arg_matches(&c, "/model", &candidates()),
            vec!["zai/glm-4.5-air"]
        );
    }

    #[test]
    fn the_menu_stays_shut_without_the_command_or_past_one_argument() {
        for text in ["/models zai", "/model zai x", "prose", "/model"] {
            let c = composer_with(text);
            assert!(
                arg_matches(&c, "/model", &candidates()).is_empty(),
                "{text:?} must not open the argument menu"
            );
        }
    }

    #[test]
    fn tab_completes_and_enter_submits_the_completed_command() {
        let mut c = composer_with("/model zai/glm-5");
        let mut sel = 1usize;
        let matches = arg_matches(&c, "/model", &candidates());
        let out = handle_arg_popup_key(key(KeyCode::Tab), "/model", &matches, &mut c, &mut sel);
        assert!(matches!(out, Some(SlashPopupOutcome::Handled)));
        assert_eq!(c.buffer(), "/model zai/glm-5.1");

        let mut c = composer_with("/model zai/glm-5");
        let mut sel = 0usize;
        let out = handle_arg_popup_key(key(KeyCode::Enter), "/model", &matches, &mut c, &mut sel);
        match out {
            Some(SlashPopupOutcome::Submit(text)) => assert_eq!(text, "/model zai/glm-5.2"),
            other => panic!("expected a submit, got {:?}", other.is_some()),
        }
        assert_eq!(c.buffer(), "", "submit clears the composer");
    }
}
