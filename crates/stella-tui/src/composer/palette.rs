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

/// What the session is doing, as the palette needs to see it.
///
/// A flat copy of six facts rather than a borrow of the deck: the rule below
/// is a pure function over owned data, which is what lets it be a table of
/// cases in a test instead of a screenshot. The deck fills it at render time
/// (`deck_render::palette_state`); nothing is cached.
///
/// The default is the quiet session — no turn, no plan, no lanes, nothing
/// unread, nothing changed, and an index that is present. Every field is
/// phrased so that its zero value fires no rule, so
/// `relevant_now(&PaletteState::default())` is `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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

    #[test]
    fn the_domains_order_and_label_themselves_from_one_list() {
        for (i, domain) in SlashDomain::ALL.iter().enumerate() {
            assert_eq!(domain.order(), i);
            assert!(!domain.label().is_empty());
        }
    }
}
