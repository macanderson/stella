// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The SKILLS tab's own view state: which pane has the keyboard, the open
//! overlay, and everything `SkillsPanel` holds beside the driver's
//! `SkillsView` snapshot.
//!
//! `deck_ui.rs` is closed to growth (AGENTS.md's "God files"). So the
//! rejected-skills review overlay lands here instead of there.

use crate::envelope::{SkillScope, SkillSearchHit, SkillsView};

/// Which pane of the SKILLS tab has the keyboard: the installed list, or the
/// registry search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillsFocus {
    #[default]
    Installed,
    Search,
}

/// An open SKILLS-tab overlay. It captures keys ahead of the list and
/// search panes: the scope picker, the create-description input, the edit
/// buffer, the version pin picker, or the rejected-skills review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillPrompt {
    /// Choose install/create scope (project or user) before dispatching.
    Scope {
        action: ScopeAction,
        /// Highlighted choice: `false` = project, `true` = user.
        user: bool,
    },
    /// Type a short description for LLM-assisted creation. The scope
    /// picker follows.
    CreateDescription { buffer: String },
    /// LLM-assisted creation is running on the driver side. The dialog
    /// shows a spinner until a fresh `Inbound::Skills` snapshot lands. Esc
    /// hides it (creation keeps going); every other key is swallowed.
    Creating {
        description: String,
        scope: SkillScope,
    },
    /// Creation failed. The dialog stays open and shows the driver's
    /// error. Esc, ⏎, or `q` closes it.
    CreateFailed { error: String },
    /// Edit a skill's body. Saving bumps its version and pins the new one.
    Edit {
        scope: SkillScope,
        name: String,
        buffer: String,
    },
    /// Pick a version to pin. No edit, no version bump.
    Pin {
        scope: SkillScope,
        name: String,
        latest: u32,
        sel: u32,
    },
    /// Give a learned skill a human name (SPEC 9.2's `r rename`).
    ///
    /// `was` is the mined hash the rename keeps. It rides in the dialog so
    /// the screen can promise that up front. A silent rename that dropped
    /// the hash, and one that kept it, would look the same at the prompt —
    /// keeping it is the whole point of this verb.
    Rename {
        scope: SkillScope,
        name: String,
        buffer: String,
        was: String,
    },
    /// Review the skills rejected in this workspace, and reverse one.
    ///
    /// `x` only ever deleted a skill; it never showed the rejection back or
    /// let anyone undo it. A rejection is durable and otherwise invisible
    /// outside `.stella-skills.json`. This is the one screen that answers
    /// "what have I rejected here?" without an editor.
    ///
    /// `sel` picks a row in `SkillsView::rejections`. `↑`/`↓` move it; `⏎`
    /// or `u` sends `SkillOp::Unreject` for that row and closes the dialog.
    Rejected { sel: usize },
}

/// The action a [`SkillPrompt::Scope`] picker runs once the user picks a
/// scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScopeAction {
    Install { id: String },
    Create { description: String },
}

/// The `ctrl+o` preview: a scrollable, read-only render of a skill's
/// `SKILL.md`. Either pane can open it. For an installed skill the body is
/// already in hand (`SkillRow::body`), so `body` starts `Some`. For a
/// registry hit it is fetched (`body` starts `None`, filled in by
/// `Inbound::SkillPreview` once its `id` matches `pending`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SkillPreview {
    /// Heading shown in the popup border: the skill's id or name.
    pub title: String,
    /// A dim sub-line under the title: the `skills.sh` url, or the scope
    /// and origin.
    pub subtitle: String,
    /// The hit `id` this preview is waiting on, while `body` is `None`.
    /// `None` once the body is local.
    pub pending: Option<String>,
    /// The markdown body, once it has one. `None` shows a loading state.
    pub body: Option<String>,
    /// Scroll offset in lines, clamped to the content at render time.
    pub scroll: u16,
}

/// All SKILLS-tab view state. The installed list, plus `busy` and
/// `status`, come from an `Inbound::Skills` snapshot the driver owns. The
/// rest — selection, the live search query, transient arming, the open
/// overlay — is local to this panel.
#[derive(Debug, Clone, Default)]
pub struct SkillsPanel {
    /// The installed-skills read-model, from `Inbound::Skills`.
    pub view: SkillsView,
    pub focus: SkillsFocus,
    /// Selected row in the installed list.
    pub sel: usize,
    /// The live search-query buffer.
    pub query: String,
    /// Last search results, from `Inbound::SkillSearch`.
    pub hits: Vec<SkillSearchHit>,
    pub search_sel: usize,
    /// True once the query changed since the last search. Then `⏎` searches
    /// again instead of installing.
    pub query_dirty: bool,
    /// True while an npx search or install is in flight.
    pub searching: bool,
    /// A one-line hint: the last op's outcome, or a small affordance.
    pub status: Option<String>,
    /// First `ctrl+x` arms delete; the second one runs it.
    pub uninstall_armed: bool,
    /// First `x` arms rejection; the second one runs it (SPEC 9.2).
    ///
    /// Armed apart from [`Self::uninstall_armed`], because the two verbs
    /// say different things. `ctrl+x` then `x` must not complete either one
    /// — it just means the user changed their mind.
    pub reject_armed: bool,
    /// An open overlay, capturing keys ahead of the panes.
    pub prompt: Option<SkillPrompt>,
    /// The `ctrl+o` preview overlay: modal, scrolls, closes on Esc. `None`
    /// when closed.
    pub preview: Option<SkillPreview>,
}

impl SkillsPanel {
    /// Open the rejected-skills review, on `!` from the installed pane. If
    /// there is nothing to review, say so instead.
    ///
    /// An empty list is refused rather than shown as an empty picker.
    /// `SkillOp::Unreject` needs a `mined_as` to name, and a picker over
    /// zero rows has none to offer.
    pub fn open_rejected(&mut self) {
        if self.view.rejections.is_empty() {
            self.status = Some("nothing rejected in this workspace".to_string());
            return;
        }
        self.prompt = Some(SkillPrompt::Rejected { sel: 0 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal names the reason instead of opening an empty picker.
    #[test]
    fn open_rejected_refuses_on_an_empty_list() {
        let mut panel = SkillsPanel::default();
        panel.open_rejected();
        assert!(panel.prompt.is_none());
        assert_eq!(
            panel.status.as_deref(),
            Some("nothing rejected in this workspace")
        );
    }

    /// A non-empty list opens the overlay at row 0.
    #[test]
    fn open_rejected_opens_at_the_first_row() {
        let mut panel = SkillsPanel::default();
        panel
            .view
            .rejections
            .push(crate::envelope::RejectedSkillRow {
                scope: SkillScope::Project,
                name: "bench-rig-access".to_string(),
                mined_as: "bench-rig-access-a1b2c3d4".to_string(),
                rejected_at: 1_700_000_000,
            });
        panel.open_rejected();
        assert_eq!(panel.prompt, Some(SkillPrompt::Rejected { sel: 0 }));
    }
}
