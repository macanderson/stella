// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The command palette's taxonomy and its relevance rule — SPEC 10,
//! rendering `08-command-palette`.
//!
//! The palette used to open on a flat fuzzy ranking of every command, in
//! whatever order the vocabulary happened to be written in. Thirty rows with
//! no shape is a list you read rather than a menu you use, and the one row
//! that matters — the plan, mid-turn — sat wherever the alphabet put it.
//!
//! Two facts fix that, and both live here so they are testable without a
//! terminal: a [`SlashDomain`] on every command, which groups the browse
//! list; and [`relevant_now`], a pure rule over [`PaletteState`] that names
//! the handful of commands the session's own state makes worth reaching for.
//!
//! [`PaletteState::default`] is deliberately inert — every field is the
//! quiet value — so a surface with no session state to offer (the plain
//! REPL) gets exactly the ordering it had before, rather than a relevance
//! block computed from zeroes.

/// What a slash command is about. The palette's browse list groups by this,
/// in [`SlashDomain::ALL`] order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SlashDomain {
    /// The conversation itself: reset it, export it, read the inbox.
    #[default]
    Session,
    /// The turn in flight: the plan, the lanes, the spend, the context sent.
    Plan,
    /// The workspace: the index, the graph, the files, the diff.
    Code,
    /// What the agent can reach: skills, MCP servers, agent definitions.
    Extend,
    /// Configuration: models, profile, theme, settings, directories.
    Config,
    /// User-authored — the ⚡ rows, whatever they do.
    Custom,
}

impl SlashDomain {
    /// Every domain, in the order the palette prints its groups: the
    /// conversation first, then the turn, then the workspace, then what the
    /// agent can reach, then config, then whatever the user wrote.
    pub const ALL: [SlashDomain; 6] = [
        SlashDomain::Session,
        SlashDomain::Plan,
        SlashDomain::Code,
        SlashDomain::Extend,
        SlashDomain::Config,
        SlashDomain::Custom,
    ];

    /// The group heading.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            SlashDomain::Session => "session",
            SlashDomain::Plan => "turn",
            SlashDomain::Code => "workspace",
            SlashDomain::Extend => "extend",
            SlashDomain::Config => "config",
            SlashDomain::Custom => "custom",
        }
    }

    /// Position in [`Self::ALL`] — the group sort key.
    #[must_use]
    pub fn order(self) -> usize {
        Self::ALL.iter().position(|d| *d == self).unwrap_or(0)
    }
}

/// What the session is doing and what has been run here before, as the
/// palette needs to see it.
///
/// A flat copy of the facts rather than a borrow of the deck: the rule below
/// is a pure function over owned data, which is what lets it be a table of
/// cases in a test instead of a screenshot. The deck fills it at render time
/// (`deck_render::palette_state`); nothing is cached.
///
/// The default is the quiet session — no turn, no plan, no lanes, nothing
/// unread, nothing changed, an index that is present, and no history. Every
/// field is phrased so that its zero value fires no rule and adds no section,
/// so `relevant_now(&PaletteState::default())` is `None` and a browse list
/// built from it is the domain groups alone.
///
/// The six session fields were `Copy` until [`Self::recent`] joined them
/// (#5048). The type is passed by reference everywhere it is read, so the
/// clone is paid once per frame in `palette_state` and nowhere else.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaletteState {
    /// A turn is in flight on the focused agent.
    pub turn_running: bool,
    /// Steps in the focused agent's plan.
    pub plan_steps: usize,
    /// Sub-agent lanes this session dispatched.
    pub subagents: usize,
    /// Unread notifications in the inbox.
    pub unread: usize,
    /// Files this session changed.
    pub changed_files: usize,
    /// The code graph has not been built — `stella init` has not run here.
    /// Phrased as the *absence* so a default state claims nothing.
    pub graph_missing: bool,
    /// Commands run from this palette in this workspace, **newest first** and
    /// already deduplicated — the `recent` section SPEC 10 asks for, and the
    /// one field here that outlives the session (#5048; the remainder of
    /// #4338, whose doc comment said the deck had no store for it).
    ///
    /// The deck does not read or write the file: the driver owns the
    /// workspace's private state directory and pushes this list in, exactly
    /// as it pushes the command vocabulary itself
    /// (`Inbound::PaletteRecents`). Names, not commands — a history entry for
    /// a command the vocabulary no longer offers simply matches nothing and
    /// disappears from the menu, which is the right behaviour for a plugin
    /// that was uninstalled.
    pub recent: Vec<String>,
}

/// The commands the session's state makes worth reaching for, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelevantNow {
    /// The half-sentence the palette prints beside the heading — the state
    /// that put these rows on top, so the ranking explains itself instead of
    /// looking arbitrary.
    pub reason: &'static str,
    /// Command names, most relevant first. Never empty.
    pub commands: Vec<&'static str>,
}

/// One relevance rule: when it fires, what it says, and what it offers.
///
/// A table rather than a chain of `if`s so the cases are enumerable — both
/// by a reader and by [`rule_command_names`], which the CLI's
/// `every_command_a_relevance_rule_can_name_is_a_real_one` walks to keep
/// these names from drifting away from the vocabulary that defines them.
struct Rule {
    fires: fn(&PaletteState) -> bool,
    reason: &'static str,
    commands: &'static [&'static str],
}

/// The rules, in precedence order. Every rule that fires contributes its
/// commands; the **first** one to fire also supplies the reason, because the
/// heading is one line and the most urgent state is the one a reader needs
/// named.
const RULES: &[Rule] = &[
    Rule {
        fires: |s| s.turn_running,
        reason: "a turn is running",
        commands: &["/plan", "/inspect", "/budget"],
    },
    Rule {
        fires: |s| s.subagents > 0,
        reason: "sub-agents are running",
        commands: &["/subagents"],
    },
    Rule {
        fires: |s| s.plan_steps > 0,
        reason: "a plan is open",
        commands: &["/plan", "/budget"],
    },
    Rule {
        fires: |s| s.changed_files > 0,
        reason: "this session changed files",
        commands: &["/diff", "/files", "/export"],
    },
    Rule {
        fires: |s| s.unread > 0,
        reason: "you have unread notifications",
        commands: &["/inbox"],
    },
    Rule {
        fires: |s| s.graph_missing,
        reason: "this workspace is not indexed",
        commands: &["/init", "/graph"],
    },
];

/// How many commands the `recent` section remembers.
///
/// Small on purpose. `recent` is a shortcut back to what you were just doing,
/// not a log: the browse list already offers the whole vocabulary a few rows
/// below, and a `recent` block long enough to need scrolling would push the
/// domain groups off the popup it is supposed to shorten. Five is also what
/// fits above the slash popup's visible-row cap (`SLASH_POPUP_MAX_ROWS` in
/// `crate::render`) without the section alone filling the window.
pub const RECENT_LIMIT: usize = 5;

/// Record `name` as the most recently run command, newest first.
///
/// Move-to-front rather than append: running a command you ran before should
/// move it up, not leave a stale copy behind it and a duplicate row in the
/// menu. Capped at [`RECENT_LIMIT`].
///
/// Pure, and shared by both ends on purpose — the deck calls it so the
/// section reorders on the keystroke rather than a round-trip later, and the
/// driver calls it on the list it is about to persist. One rule, so the two
/// cannot disagree about what "recent" means.
pub fn remember(recent: &mut Vec<String>, name: &str) {
    recent.retain(|existing| existing != name);
    recent.insert(0, name.to_string());
    recent.truncate(RECENT_LIMIT);
}

/// Every command name any rule can name — the input to the guard that keeps
/// this module honest about the vocabulary. A name here that no command
/// answers to is a row the palette would silently never promote.
#[must_use]
pub fn rule_command_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = RULES
        .iter()
        .flat_map(|r| r.commands.iter().copied())
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// The commands `state` makes relevant, most relevant first, or `None` for a
/// session with nothing to say.
///
/// Deduplicated across rules while keeping each command's earliest position:
/// `/plan` is named by both the running-turn rule and the open-plan rule,
/// and it must appear once, at the top, rather than twice.
#[must_use]
pub fn relevant_now(state: &PaletteState) -> Option<RelevantNow> {
    let mut reason = None;
    let mut commands: Vec<&'static str> = Vec::new();
    for rule in RULES {
        if !(rule.fires)(state) {
            continue;
        }
        reason.get_or_insert(rule.reason);
        for name in rule.commands {
            if !commands.contains(name) {
                commands.push(name);
            }
        }
    }
    reason.map(|reason| RelevantNow { reason, commands })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quiet_session_promotes_nothing() {
        assert_eq!(relevant_now(&PaletteState::default()), None);
    }

    /// The running turn is the loudest state, so it names the reason even
    /// when other rules fire underneath it — and its commands lead.
    #[test]
    fn a_running_turn_leads_with_the_plan_and_names_itself() {
        let state = PaletteState {
            turn_running: true,
            plan_steps: 4,
            subagents: 2,
            unread: 3,
            ..PaletteState::default()
        };
        let relevant = relevant_now(&state).expect("a running turn is relevant");
        assert_eq!(relevant.reason, "a turn is running");
        assert_eq!(relevant.commands.first(), Some(&"/plan"));
        assert!(
            relevant.commands.contains(&"/inbox"),
            "the quieter rules still contribute"
        );
    }

    /// `/plan` is named by two rules; the palette lists it once, where the
    /// first rule put it.
    #[test]
    fn a_command_two_rules_name_appears_once_at_its_earliest_place() {
        let state = PaletteState {
            turn_running: true,
            plan_steps: 4,
            ..PaletteState::default()
        };
        let relevant = relevant_now(&state).expect("relevant");
        assert_eq!(
            relevant.commands.iter().filter(|c| **c == "/plan").count(),
            1
        );
        assert_eq!(relevant.commands[0], "/plan");
    }

    /// Each remaining rule fires on its own field alone, so no rule is
    /// reachable only in company with another.
    #[test]
    fn every_rule_fires_on_its_own_state() {
        let cases = [
            (
                PaletteState {
                    subagents: 1,
                    ..PaletteState::default()
                },
                "sub-agents are running",
            ),
            (
                PaletteState {
                    plan_steps: 1,
                    ..PaletteState::default()
                },
                "a plan is open",
            ),
            (
                PaletteState {
                    changed_files: 1,
                    ..PaletteState::default()
                },
                "this session changed files",
            ),
            (
                PaletteState {
                    unread: 1,
                    ..PaletteState::default()
                },
                "you have unread notifications",
            ),
            (
                PaletteState {
                    graph_missing: true,
                    ..PaletteState::default()
                },
                "this workspace is not indexed",
            ),
        ];
        for (state, reason) in cases {
            let relevant =
                relevant_now(&state).unwrap_or_else(|| panic!("{state:?} fires nothing"));
            assert_eq!(relevant.reason, reason, "{state:?}");
        }
    }

    /// Move-to-front, deduplicated, capped — the three properties the
    /// `recent` section rests on, asserted together because a regression in
    /// any one of them prints a duplicate or a stale row.
    #[test]
    fn remembering_a_command_moves_it_to_the_front_without_duplicating_it() {
        let mut recent = Vec::new();
        for name in ["/plan", "/diff", "/plan"] {
            remember(&mut recent, name);
        }
        assert_eq!(recent, vec!["/plan", "/diff"], "one entry, at the front");

        // Past the cap, the oldest falls off the end rather than the newest
        // failing to land.
        let mut recent = Vec::new();
        for i in 0..(RECENT_LIMIT + 3) {
            remember(&mut recent, &format!("/c{i}"));
        }
        assert_eq!(recent.len(), RECENT_LIMIT);
        assert_eq!(recent[0], format!("/c{}", RECENT_LIMIT + 2), "newest first");
        assert!(!recent.contains(&"/c0".to_string()), "oldest dropped");
    }

    #[test]
    fn the_domains_order_and_label_themselves_from_one_list() {
        for (i, domain) in SlashDomain::ALL.iter().enumerate() {
            assert_eq!(domain.order(), i);
            assert!(!domain.label().is_empty());
        }
    }
}
