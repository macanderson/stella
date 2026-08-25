// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The engine panel's tab strip: GLOBAL plus one page per configurable agent.
//!
//! Everything here is the strip's *vocabulary and cycle order*; none of it
//! touches the panel's state or draws anything.
//!
//! [`EngineTab::ALL`] is **derived** from [`EngineRole::ALL`] rather than
//! written out. It used to be a hand-typed copy of the role list, which is the
//! shape this workspace has been bitten by before — `ModelCallRole::ALL` is
//! macro-derived for the same reason. A copy costs nothing until the day a role
//! is added, and then it silently renders one tab fewer than the settings file
//! has pages: the role is configurable, the deck simply never offers it.

use crate::envelope::{EngineConfigState, EngineRole};

/// Which tab of the panel has the keyboard: the GLOBAL toggles or one of
/// the per-agent override pages. GLOBAL comes first — the cross-agent
/// switches are what the panel usually opens on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EngineTab {
    #[default]
    Global,
    Agent(EngineRole),
}

impl EngineTab {
    /// Display/cycle order: GLOBAL, then the agents in [`EngineRole::ALL`]
    /// order.
    ///
    /// A `const` block rather than a literal so the two can never disagree:
    /// the length is `EngineRole::ALL`'s own, so a role added there adds a tab
    /// here with no edit at all.
    pub const ALL: [EngineTab; EngineRole::ALL.len() + 1] = {
        let mut tabs = [EngineTab::Global; EngineRole::ALL.len() + 1];
        let mut i = 0;
        while i < EngineRole::ALL.len() {
            tabs[i + 1] = EngineTab::Agent(EngineRole::ALL[i]);
            i += 1;
        }
        tabs
    };

    fn index(self) -> usize {
        EngineTab::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    /// The next tab, wrapping past the last back to GLOBAL.
    pub fn next(self) -> Self {
        EngineTab::ALL[(self.index() + 1) % EngineTab::ALL.len()]
    }

    /// The previous tab, wrapping before GLOBAL to the last agent.
    pub fn prev(self) -> Self {
        let n = EngineTab::ALL.len();
        EngineTab::ALL[(self.index() + n - 1) % n]
    }

    /// The header label — the settings key for agents, `GLOBAL` otherwise.
    pub fn label(self) -> &'static str {
        match self {
            EngineTab::Global => "GLOBAL",
            EngineTab::Agent(role) => role.key(),
        }
    }
}

/// The GLOBAL tab's rows, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GlobalRow {
    AutoMode,
    EffortAuto,
    ReasoningAuto,
    MinimalPrompt,
    AllowedModels,
}

pub(super) const GLOBAL_ROWS: [GlobalRow; 5] = [
    GlobalRow::AutoMode,
    GlobalRow::EffortAuto,
    GlobalRow::ReasoningAuto,
    GlobalRow::MinimalPrompt,
    GlobalRow::AllowedModels,
];

impl GlobalRow {
    pub(super) fn label(self) -> &'static str {
        match self {
            GlobalRow::AutoMode => "auto_mode",
            GlobalRow::EffortAuto => "effort_auto",
            GlobalRow::ReasoningAuto => "reasoning_auto",
            GlobalRow::MinimalPrompt => "minimal_prompt",
            GlobalRow::AllowedModels => "allowed_models",
        }
    }

    /// This row's on/off flag in `state` — `None` for `AllowedModels`, which
    /// is a list, not a switch. The one place the row→field mapping lives for
    /// the panel's flip/clear/render sites, so a new toggle row is an arm
    /// here (plus [`Self::flag_mut`]'s) rather than three edits spread across
    /// the panel.
    pub(super) fn flag(self, state: &EngineConfigState) -> Option<bool> {
        match self {
            GlobalRow::AutoMode => Some(state.auto_mode),
            GlobalRow::EffortAuto => Some(state.effort_auto),
            GlobalRow::ReasoningAuto => Some(state.reasoning_auto),
            GlobalRow::MinimalPrompt => Some(state.minimal_prompt),
            GlobalRow::AllowedModels => None,
        }
    }

    /// [`Self::flag`]'s mutable twin. A second match over the same mapping
    /// because a shared projection cannot serve both borrow shapes; the two
    /// sit adjacent so an arm added to one is visibly missing from the other.
    fn flag_mut(self, state: &mut EngineConfigState) -> Option<&mut bool> {
        match self {
            GlobalRow::AutoMode => Some(&mut state.auto_mode),
            GlobalRow::EffortAuto => Some(&mut state.effort_auto),
            GlobalRow::ReasoningAuto => Some(&mut state.reasoning_auto),
            GlobalRow::MinimalPrompt => Some(&mut state.minimal_prompt),
            GlobalRow::AllowedModels => None,
        }
    }

    /// ⏎/space on a toggle row: flip it. A no-op for `AllowedModels`, whose
    /// activation opens the inline edit and is handled by the caller.
    pub(super) fn flip(self, state: &mut EngineConfigState) {
        if let Some(flag) = self.flag_mut(state) {
            *flag = !*flag;
        }
    }

    /// `x` on a toggle row: back to off, the shipped default.
    pub(super) fn switch_off(self, state: &mut EngineConfigState) {
        if let Some(flag) = self.flag_mut(state) {
            *flag = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property the derivation buys, and the one a literal could not
    /// state: every configurable agent has a page, and GLOBAL is the first.
    #[test]
    fn every_engine_role_has_a_tab_after_global() {
        assert_eq!(EngineTab::ALL.len(), EngineRole::ALL.len() + 1);
        assert_eq!(EngineTab::ALL[0], EngineTab::Global);
        for role in EngineRole::ALL {
            assert!(
                EngineTab::ALL.contains(&EngineTab::Agent(role)),
                "role {} has no tab — the deck cannot configure what it cannot show",
                role.key()
            );
        }
    }

    /// Cycling walks every tab once and returns to GLOBAL, in both
    /// directions — the wrap arithmetic is the one thing the derivation
    /// changed the shape of.
    #[test]
    fn next_and_prev_walk_the_whole_strip_and_wrap() {
        let mut tab = EngineTab::Global;
        for expected in EngineTab::ALL.iter().skip(1) {
            tab = tab.next();
            assert_eq!(&tab, expected);
        }
        assert_eq!(tab.next(), EngineTab::Global, "wraps back to GLOBAL");
        assert_eq!(
            EngineTab::Global.prev(),
            *EngineTab::ALL.last().expect("the strip is never empty"),
            "and backwards to the last agent"
        );
    }
}
