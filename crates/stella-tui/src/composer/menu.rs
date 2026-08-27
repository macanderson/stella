// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The slash menu itself: what a query reaches, how the result is sectioned,
//! and what the popup's keys do — SPEC 10, rendering `08-command-palette`.
//!
//! Split out of `composer.rs` when that file crossed the 1500-line guard
//! (#5048), the way `render/tests` split by topic. The line falls where the
//! concerns already did: `composer.rs` is the *input model* — the buffer, the
//! cursor, paste chips, soft-wrap layout — and this is the *menu* built over
//! a caller-supplied vocabulary. [`Composer::slash_menu`](super::Composer)
//! stays there, because deciding whether the buffer is a slash query at all
//! is a fact about the buffer.
//!
//! The menu filters a caller-supplied command list, so `/help /clear
//! /models /diff /files` are an *input*, not a hard-coded set — the CLI owns
//! the real vocabulary, and a second surface with a different one costs this
//! module nothing. What ranks the result
//! is [`super::palette`] (what the session is doing, what was run here
//! before); what decides the letters is [`super::fuzzy`].
//!
//! [`handle_slash_popup_key`] is the one implementation of slash-popup key
//! handling, shared by every composer-driven surface (the deck's
//! [`crate::deck_ui`]) so a future fix to selection clamping, Esc semantics,
//! or completion behavior can't land on one surface and drift from the other.

use crossterm::event::{KeyCode, KeyEvent};

use super::{Composer, NameMatch, PaletteState, SlashCommand, Tier, fuzzy, palette};

/// One row of the palette: a command the query reached, and how it reached
/// it.
///
/// The match travels *with* the command rather than in a list beside it,
/// because the two are read together on every frame — the renderer asks
/// "which letters of this name do I light" for each visible row — and a
/// parallel `Vec<Vec<usize>>` is a pair of collections a future sort could
/// silently take out of step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashMatch<'a> {
    /// The command itself, borrowed from the caller's vocabulary.
    pub command: &'a SlashCommand,
    /// Which tier the query matched at, and which of
    /// [`SlashCommand::name`]'s bytes it lit (SPEC 10: "matched letters
    /// render gold inside each command name").
    pub matched: NameMatch,
}

impl SlashMatch<'_> {
    /// The command's name, slash included — the string the offsets in
    /// [`Self::matched`] index into.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.command.name
    }
}

/// The filtered slash-command list for the current query. Borrows the
/// caller's command vocabulary — the menu owns no command list of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashMenu<'a> {
    pub query: String,
    pub matches: Vec<SlashMatch<'a>>,
    /// Headings the palette draws above [`Self::matches`], as
    /// `(index of the first match under it, heading)`. Ascending, and only
    /// ever populated for the browse list — see [`Self::filter_with`].
    pub sections: Vec<(usize, String)>,
}

impl<'a> SlashMenu<'a> {
    /// [`Self::filter_with`] against a session with nothing to say.
    ///
    /// A composer with no session behind it has no plan, no lanes and no
    /// inbox to read,
    /// so it gets the ranking it always had rather than a relevance block
    /// derived from zeroes.
    pub fn filter(commands: &'a [SlashCommand], query: &str) -> Self {
        Self::filter_with(commands, query, &PaletteState::default())
    }

    /// Fuzzy filter over `commands`, ordered by what the session is doing and
    /// by what has been run in this workspace before.
    ///
    /// Matching decides *what appears* and lives in [`fuzzy`]: a name-prefix
    /// match ranks first, a name-substring match second, a **scattered**
    /// name match third (`ga` reaching `/graph query` — #5048), and a
    /// description-substring match last. Every one of them reports which of
    /// the name's characters it lit, which is what lets SPEC 10's gold
    /// letters land inside the name instead of only on a typed prefix. An
    /// empty query (just `/`) matches everything and lights nothing.
    ///
    /// `state` decides *the order*, and the two cases are different
    /// surfaces rather than one compromise (#4338):
    ///
    /// - **The browse list** (an empty query — the palette just opened) is
    ///   sectioned: [`palette::relevant_now`]'s commands first under one
    ///   heading that says why, then a group per [`super::SlashDomain`], then
    ///   `recent` — the last commands run in this workspace, newest first
    ///   ([`PaletteState::recent`]). Thirty rows in vocabulary order is a
    ///   list you read; groups are a menu you use. The three blocks are
    ///   disjoint: a command promoted into `relevant now` or into `recent` is
    ///   *moved* out of its domain group rather than printed twice, and
    ///   relevance wins when a command qualifies for both — the reason it is
    ///   on top is the more urgent of the two.
    /// - **A typed query** stays one flat ranked list with no headings —
    ///   grouping a three-row result buries the rows under their own
    ///   captions — but a relevant command still leads *within its tier*, so
    ///   `/pl` mid-turn opens on `/plan`, and a recently-run one breaks the
    ///   tie after that.
    pub fn filter_with(commands: &'a [SlashCommand], query: &str, state: &PaletteState) -> Self {
        let needle = query.trim_start_matches('/').to_ascii_lowercase();
        // The name first, at whichever tier it reaches; failing that, the
        // one-line description, which lights nothing because nothing in the
        // *name* was matched.
        let matched = |c: &SlashCommand| -> Option<NameMatch> {
            fuzzy::match_name(&needle, &c.name).or_else(|| {
                c.description
                    .to_ascii_lowercase()
                    .contains(&needle)
                    .then(|| NameMatch {
                        tier: Tier::Description,
                        lit: Vec::new(),
                    })
            })
        };
        let relevant = palette::relevant_now(state);
        // Where a command sits in the relevance block, or past every one of
        // them. `usize::MAX` rather than an `Option` so it sorts last with
        // no second comparison. `recency` is the same shape over the
        // workspace's run history — position 0 is the most recent.
        let relevance = |c: &SlashCommand| -> usize {
            relevant
                .as_ref()
                .and_then(|r| r.commands.iter().position(|n| *n == c.name))
                .unwrap_or(usize::MAX)
        };
        let recency = |c: &SlashCommand| -> usize {
            state
                .recent
                .iter()
                .position(|name| *name == c.name)
                .unwrap_or(usize::MAX)
        };

        let mut ranked: Vec<SlashMatch<'a>> = commands
            .iter()
            .filter_map(|command| matched(command).map(|matched| SlashMatch { command, matched }))
            .collect();

        if !needle.is_empty() {
            // Stable within a key, so the vocabulary order survives among
            // commands the session says nothing about.
            ranked.sort_by_key(|m| {
                (
                    m.matched.tier.rank(),
                    relevance(m.command),
                    recency(m.command),
                )
            });
            return Self {
                query: query.to_string(),
                matches: ranked,
                sections: Vec::new(),
            };
        }

        // The browse list: relevance block, then a group per domain, then
        // recent. `recent_only` is what keeps the blocks disjoint — a
        // command the session already promoted is not also a `recent` row.
        let recent_only = |c: &SlashCommand| relevance(c) == usize::MAX && recency(c) != usize::MAX;
        ranked.sort_by_key(|m| {
            let c = m.command;
            (
                relevance(c),
                usize::from(recent_only(c)),
                // Only the recent block orders by recency; every other row
                // sorts past it and falls through to its domain.
                if recent_only(c) {
                    recency(c)
                } else {
                    usize::MAX
                },
                c.domain.order(),
            )
        });
        let matches = ranked;

        let promoted = matches
            .iter()
            .filter(|m| relevance(m.command) != usize::MAX)
            .count();
        let recent_rows = matches.iter().filter(|m| recent_only(m.command)).count();
        // The recent block is a suffix by construction (the sort key's second
        // slot), so one subtraction locates it.
        let recent_start = matches.len() - recent_rows;

        let mut sections = Vec::new();
        if let Some(relevant) = relevant.as_ref()
            && promoted > 0
        {
            sections.push((0, format!("relevant now · {}", relevant.reason)));
        }
        let mut group = None;
        for (i, m) in matches.iter().enumerate().take(recent_start).skip(promoted) {
            if group != Some(m.command.domain) {
                group = Some(m.command.domain);
                sections.push((i, m.command.domain.label().to_string()));
            }
        }
        if recent_rows > 0 {
            sections.push((recent_start, "recent".to_string()));
        }
        Self {
            query: query.to_string(),
            matches,
            sections,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }
}

/// The names of the slash commands currently matching the composer, or empty
/// when the popup should be inactive. Owned strings so a caller can keep
/// mutating its own UI state while acting on them.
///
/// `state` must be the same one the frame is drawn with: this list is what
/// the selection index means, so a key handler ordering it differently from
/// the renderer would run the row *above* the one highlighted.
pub fn slash_popup_matches(
    composer: &Composer,
    slash_commands: &[SlashCommand],
    state: &PaletteState,
) -> Vec<String> {
    composer
        .slash_menu(slash_commands, state)
        .map(|m| m.matches.iter().map(|m| m.name().to_string()).collect())
        .unwrap_or_default()
}

/// What a slash-popup key press should do, abstracted over the caller's own
/// action type — a single-session `Prompt` and a deck `Enqueue` both start
/// from the same submitted text.
pub enum SlashPopupOutcome {
    /// Navigation, completion, or dismiss — fully handled here.
    Handled,
    /// Enter: dispatch this text as a prompt.
    Submit(String),
}

/// Slash-popup navigation shared by every composer-driven surface: ↑/↓
/// choose, Tab completes into the buffer, Enter dispatches the selection,
/// Esc dismisses. Returns `None` for a key the popup doesn't claim, so the
/// caller can fall through to normal composer editing. `matches` must be
/// non-empty — callers only reach this once the popup is confirmed active; an
/// empty slice trips a `debug_assert!` and then claims nothing, so a caller
/// bug degrades to "the popup isn't open" rather than a panic mid-keystroke.
pub fn handle_slash_popup_key(
    key: KeyEvent,
    matches: &[String],
    composer: &mut Composer,
    slash_selected: &mut usize,
) -> Option<SlashPopupOutcome> {
    // The `- 1`s below (and the `matches[selected]` indexing) rest on this.
    // Every caller gates on `slash_popup_matches(..)` being non-empty, so a
    // violation is a caller bug worth surfacing in dev rather than a release
    // panic inside the key handler.
    debug_assert!(
        !matches.is_empty(),
        "handle_slash_popup_key called with no matches — the popup is inactive"
    );
    if matches.is_empty() {
        return None;
    }
    let selected = (*slash_selected).min(matches.len() - 1);
    match key.code {
        KeyCode::Up => {
            *slash_selected = selected.saturating_sub(1);
            Some(SlashPopupOutcome::Handled)
        }
        KeyCode::Down => {
            *slash_selected = (selected + 1).min(matches.len() - 1);
            Some(SlashPopupOutcome::Handled)
        }
        KeyCode::Tab => {
            composer.load(matches[selected].clone());
            *slash_selected = 0;
            Some(SlashPopupOutcome::Handled)
        }
        KeyCode::Enter => {
            composer.clear();
            *slash_selected = 0;
            Some(SlashPopupOutcome::Submit(matches[selected].clone()))
        }
        KeyCode::Esc => {
            composer.clear();
            *slash_selected = 0;
            Some(SlashPopupOutcome::Handled)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer::{SlashDomain, SlashKind};

    fn commands() -> Vec<SlashCommand> {
        vec![
            SlashCommand::new("/help", "show help"),
            SlashCommand::new("/clear", "clear the transcript"),
            SlashCommand::new("/models", "list models"),
            SlashCommand::new("/diff", "open the diff viewer"),
            SlashCommand::new("/files", "focus the files panel"),
        ]
    }

    /// The vocabulary with domains, for the palette tests.
    fn classified_commands() -> Vec<SlashCommand> {
        vec![
            SlashCommand::new("/help", "show help").in_domain(SlashDomain::Session),
            SlashCommand::new("/clear", "clear the transcript").in_domain(SlashDomain::Session),
            SlashCommand::new("/plan", "the plan").in_domain(SlashDomain::Plan),
            SlashCommand::new("/budget", "set the spend cap").in_domain(SlashDomain::Plan),
            SlashCommand::new("/diff", "open the diff viewer").in_domain(SlashDomain::Code),
            SlashCommand::custom("/fix-bug", "fix a bug end to end"),
        ]
    }

    /// **The witness (#4338).** The browse list opens on what the session
    /// makes relevant, under a heading that says why, then one group per
    /// domain — not thirty rows in vocabulary order.
    #[test]
    fn the_browse_list_leads_with_relevance_then_groups_by_domain() {
        let cmds = classified_commands();
        let state = PaletteState {
            turn_running: true,
            ..PaletteState::default()
        };
        let mut c = Composer::new();
        c.insert_char('/');
        let menu = c.slash_menu(&cmds, &state).expect("slash menu active");

        let names: Vec<&str> = menu.matches.iter().map(|m| m.name()).collect();
        assert_eq!(
            &names[..2],
            &["/plan", "/budget"],
            "the running turn's commands lead: {names:?}"
        );
        assert_eq!(
            menu.sections.first(),
            Some(&(0, "relevant now · a turn is running".to_string())),
            "the heading says why: {:?}",
            menu.sections
        );
        assert_eq!(
            menu.sections[1..].to_vec(),
            vec![
                (2, "session".to_string()),
                (4, "workspace".to_string()),
                (5, "custom".to_string()),
            ],
            "one heading per remaining group, in domain order"
        );
    }

    /// A quiet session has no relevance block at all — the list is the domain
    /// groups alone, with no heading claiming a reason that does not exist.
    #[test]
    fn a_quiet_browse_list_is_groups_only() {
        let cmds = classified_commands();
        let mut c = Composer::new();
        c.insert_char('/');
        let menu = c
            .slash_menu(&cmds, &PaletteState::default())
            .expect("slash menu active");
        assert!(
            !menu.sections.iter().any(|(_, h)| h.starts_with("relevant")),
            "nothing to be relevant about: {:?}",
            menu.sections
        );
        assert_eq!(menu.sections.first().map(|(at, _)| *at), Some(0));
        let names: Vec<&str> = menu.matches.iter().map(|m| m.name()).collect();
        assert_eq!(
            names,
            vec!["/help", "/clear", "/plan", "/budget", "/diff", "/fix-bug"],
            "domain order, vocabulary order within a group"
        );
    }

    /// A typed query keeps one flat ranked list — but a relevant command
    /// leads its rank, so `/b` mid-turn opens on `/budget` rather than on
    /// whatever the vocabulary happened to list first.
    #[test]
    fn a_typed_query_promotes_the_relevant_match_without_headings() {
        let cmds = classified_commands();
        let state = PaletteState {
            turn_running: true,
            ..PaletteState::default()
        };
        let mut c = Composer::new();
        for ch in "/p".chars() {
            c.insert_char(ch);
        }
        let menu = c.slash_menu(&cmds, &state).expect("slash menu active");
        assert!(menu.sections.is_empty(), "no headings under a query");
        let names: Vec<&str> = menu.matches.iter().map(|m| m.name()).collect();
        assert_eq!(
            names.first(),
            Some(&"/plan"),
            "the prefix match still leads: {names:?}"
        );
        assert_eq!(
            names.iter().position(|n| *n == "/budget"),
            Some(2),
            "and the relevant one leads its own (weaker) rank: {names:?}"
        );

        // Idle, the same query is the plain fuzzy ranking: `/budget` sits
        // where the vocabulary put it, behind the two rows above it.
        let idle = c
            .slash_menu(&cmds, &PaletteState::default())
            .expect("slash menu active");
        let idle_names: Vec<&str> = idle.matches.iter().map(|m| m.name()).collect();
        assert_eq!(idle_names.first(), Some(&"/plan"));
        assert_eq!(
            idle_names.iter().position(|n| *n == "/budget"),
            Some(3),
            "relevance is what moved it: {idle_names:?}"
        );
    }

    /// **The `recent` section (#5048).** The browse list ends with what this
    /// workspace ran before, newest first, under its own heading — and a
    /// command that got there is *moved* out of its domain group rather than
    /// printed twice.
    #[test]
    fn the_browse_list_ends_with_the_workspaces_recent_commands() {
        let cmds = classified_commands();
        let state = PaletteState {
            recent: vec!["/diff".to_string(), "/help".to_string()],
            ..PaletteState::default()
        };
        let mut c = Composer::new();
        c.insert_char('/');
        let menu = c.slash_menu(&cmds, &state).expect("slash menu active");

        let names: Vec<&str> = menu.matches.iter().map(|m| m.name()).collect();
        assert_eq!(
            names,
            vec!["/clear", "/plan", "/budget", "/fix-bug", "/diff", "/help"],
            "the un-run commands keep their domain order; the run ones become \
             the tail, newest first: {names:?}"
        );
        assert_eq!(
            menu.sections.last(),
            Some(&(4, "recent".to_string())),
            "the last heading opens the recent block: {:?}",
            menu.sections
        );
        assert!(
            !menu.sections.iter().any(|(at, h)| *at > 4 && h != "recent"),
            "nothing is filed after `recent`: {:?}",
            menu.sections
        );
    }

    /// Relevance outranks recency: a command that is both is printed once, in
    /// the `relevant now` block, because the reason it is on top is the more
    /// urgent of the two — and no `recent` heading claims a row that is not
    /// under it.
    #[test]
    fn a_command_both_relevant_and_recent_is_promoted_once_by_relevance() {
        let cmds = classified_commands();
        let state = PaletteState {
            turn_running: true,
            recent: vec!["/plan".to_string()],
            ..PaletteState::default()
        };
        let mut c = Composer::new();
        c.insert_char('/');
        let menu = c.slash_menu(&cmds, &state).expect("slash menu active");

        let names: Vec<&str> = menu.matches.iter().map(|m| m.name()).collect();
        assert_eq!(
            names.iter().filter(|n| **n == "/plan").count(),
            1,
            "printed once: {names:?}"
        );
        assert_eq!(names.first(), Some(&"/plan"), "in the relevance block");
        assert!(
            !menu.sections.iter().any(|(_, h)| h == "recent"),
            "no recent block, because its only member was promoted: {:?}",
            menu.sections
        );
    }

    /// A typed query still draws no headings, but recency breaks the tie
    /// after relevance — the same "leads within its tier" rule the relevance
    /// block follows.
    #[test]
    fn a_typed_query_breaks_ties_on_recency_without_headings() {
        let cmds = vec![
            SlashCommand::new("/plan", "the plan"),
            SlashCommand::new("/profile", "retune the roles"),
            SlashCommand::new("/proposals", "review proposals"),
        ];
        let mut c = Composer::new();
        for ch in "/pro".chars() {
            c.insert_char(ch);
        }
        let idle = c
            .slash_menu(&cmds, &PaletteState::default())
            .expect("slash menu active");
        let idle_names: Vec<&str> = idle.matches.iter().map(|m| m.name()).collect();
        assert_eq!(
            idle_names,
            vec!["/profile", "/proposals"],
            "vocabulary order among equals: {idle_names:?}"
        );

        let state = PaletteState {
            recent: vec!["/proposals".to_string()],
            ..PaletteState::default()
        };
        let menu = c.slash_menu(&cmds, &state).expect("slash menu active");
        assert!(menu.sections.is_empty(), "still no headings under a query");
        let names: Vec<&str> = menu.matches.iter().map(|m| m.name()).collect();
        assert_eq!(
            names,
            vec!["/proposals", "/profile"],
            "the one run here leads its tier: {names:?}"
        );
    }

    /// A history naming a command the vocabulary no longer offers adds no
    /// row and no heading — an uninstalled skill must not leave a ghost in
    /// the menu.
    #[test]
    fn a_recent_command_that_no_longer_exists_adds_nothing() {
        let cmds = commands();
        let state = PaletteState {
            recent: vec!["/uninstalled-skill".to_string()],
            ..PaletteState::default()
        };
        let mut c = Composer::new();
        c.insert_char('/');
        let menu = c.slash_menu(&cmds, &state).expect("slash menu active");
        assert_eq!(menu.matches.len(), cmds.len());
        assert!(
            !menu.sections.iter().any(|(_, h)| h == "recent"),
            "no heading over an empty block: {:?}",
            menu.sections
        );
    }

    #[test]
    fn slash_menu_fuzzy_ranks_name_prefix_over_substring_over_description() {
        let cmds = commands();
        let mut c = Composer::new();
        for ch in "/f".chars() {
            c.insert_char(ch);
        }
        let menu = c
            .slash_menu(&cmds, &PaletteState::default())
            .expect("slash menu active");
        let names: Vec<&str> = menu.matches.iter().map(|m| m.name()).collect();
        // `/files` starts with the query; `/diff` merely contains it — the
        // prefix match must lead.
        assert_eq!(names, vec!["/files", "/diff"]);
    }

    #[test]
    fn slash_menu_falls_back_to_description_matches() {
        let cmds = commands();
        let mut c = Composer::new();
        for ch in "/transcript".chars() {
            c.insert_char(ch);
        }
        let menu = c
            .slash_menu(&cmds, &PaletteState::default())
            .expect("slash menu active");
        let names: Vec<&str> = menu.matches.iter().map(|m| m.name()).collect();
        // No name contains "transcript"; `/clear`'s description does.
        assert_eq!(names, vec!["/clear"]);
    }

    #[test]
    fn bare_slash_lists_every_command() {
        let cmds = commands();
        let mut c = Composer::new();
        c.insert_char('/');
        let menu = c.slash_menu(&cmds, &PaletteState::default()).unwrap();
        assert_eq!(menu.matches.len(), cmds.len());
    }

    #[test]
    fn slash_menu_is_inactive_once_a_space_is_typed() {
        let cmds = commands();
        let mut c = Composer::new();
        for ch in "/models ".chars() {
            c.insert_char(ch);
        }
        assert!(c.slash_menu(&cmds, &PaletteState::default()).is_none());
    }

    #[test]
    fn slash_command_constructors_set_the_kind() {
        assert_eq!(SlashCommand::new("/help", "d").kind, SlashKind::Builtin);
        assert_eq!(SlashCommand::custom("/x", "d").kind, SlashKind::Custom);
    }

    #[test]
    fn slash_menu_is_inactive_when_chips_are_present() {
        let cmds = commands();
        let mut c = Composer::with_paste_threshold(2);
        c.paste("a\nb\nc");
        c.insert_char('/');
        assert!(c.slash_menu(&cmds, &PaletteState::default()).is_none());
    }
}
