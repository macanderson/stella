//! The create-from-prompt flow for agents and skills: its key table and the
//! two snapshot transitions that settle it.
//!
//! Split from `deck_ui` because "describe → pick scope → watch it draft → read
//! what landed" is one coherent four-step conversation rather than more of the
//! deck's key table, and because both halves of it — the AGENTS tab's
//! installed-agents pane and the SKILLS tab — run the *same* shape against
//! different driver ops. Keeping them adjacent is what makes that symmetry
//! checkable.
//!
//! The recurring rule here: **a create never settles by vanishing.** The
//! dialog that dispatched the op is the dialog that reports its outcome — the
//! created definition on success, the driver's error on failure — because the
//! op is slow (a model call, sometimes parked behind a running turn) and a
//! dialog that closed on dispatch left the reader with no idea whether
//! anything happened. That is the usability bug this module exists to fix.
//!
//! Two consequences worth stating, since both are load-bearing:
//!
//! - **The completion signal is a driver snapshot, not the keypress.** The
//!   in-flight states ([`super::InstalledMode::Creating`],
//!   [`super::SkillPrompt::Creating`]) are left by
//!   [`settle_agents_snapshot`] / [`settle_skills_snapshot`], never by a key.
//!   An interim snapshot for a *parked* create carries `creating: true`
//!   precisely so it cannot be mistaken for the real thing.
//! - **Esc hides, it does not cancel.** The driver-side draft keeps running
//!   and its snapshot still folds in; the reader has only opted out of
//!   watching. Nothing here can abort a model call already in flight, so
//!   nothing here pretends to.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{DeckAction, DeckUi, InstalledMode, ScopeAction, SkillPreview, SkillPrompt};
use crate::envelope::{InstalledAgentEntry, SkillOp, SkillScope, SkillsView, WorkspaceInput};

// ── The agent create flow (the AGENTS tab's INSTALLED AGENTS pane) ──────────

/// Step 1: a one-line description buffer. ⏎ advances to the scope picker;
/// Esc cancels the flow.
pub(super) fn handle_describe_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    match key.code {
        KeyCode::Esc => {
            ui.installed.mode = InstalledMode::Browse;
            ui.installed.status = None;
            DeckAction::Handled
        }
        KeyCode::Enter => {
            if ui.installed.create_desc.trim().is_empty() {
                ui.installed.status = Some("describe the agent first".into());
            } else {
                ui.installed.mode = InstalledMode::CreateScope;
                ui.installed.status = None;
            }
            DeckAction::Handled
        }
        KeyCode::Backspace => {
            ui.installed.create_desc.pop();
            DeckAction::Handled
        }
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META) =>
        {
            ui.installed.create_desc.push(c);
            DeckAction::Handled
        }
        _ => DeckAction::Handled,
    }
}

/// Step 2: the install-scope picker (project / user, mirroring the skills
/// install flow's scope question). ⏎ dispatches the LLM-assisted creation;
/// Esc steps back to the description.
pub(super) fn handle_scope_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    match key.code {
        KeyCode::Esc => {
            ui.installed.mode = InstalledMode::CreateDescribe;
            DeckAction::Handled
        }
        KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {
            ui.installed.scope_sel = 1 - ui.installed.scope_sel.min(1);
            DeckAction::Handled
        }
        KeyCode::Enter => {
            let description = ui.installed.create_desc.trim().to_string();
            let scope = ui.installed.create_scope();
            // The dialog STAYS open: the creating state keeps an animated
            // spinner up until an [`crate::envelope::Inbound::AgentsList`]
            // with `creating: false` (the completion signal) lands.
            ui.installed.mode = InstalledMode::Creating;
            ui.installed.busy = true;
            ui.installed.status = Some(format!(
                "drafting the agent with the session model ({} scope)…",
                scope.label()
            ));
            DeckAction::Send(WorkspaceInput::AgentCreate { description, scope })
        }
        _ => DeckAction::Handled,
    }
}

/// Step 3: the in-flight creation dialog. Fully modal — Esc hides it and
/// everything else is swallowed.
///
/// Hiding deliberately clears no progress state: the driver-side draft keeps
/// running, `busy` stays set (so an empty list still reads "loading" rather
/// than "no agents installed"), and `status` keeps the drafting line in the
/// browse footer. The reader loses the spinner, not the fact of the op.
pub(super) fn handle_creating_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    if matches!(key.code, KeyCode::Esc) {
        ui.installed.mode = InstalledMode::Browse;
    }
    DeckAction::Handled
}

/// Step 4: the settled dialog — the created agent's detail view (or the
/// failure notice). ↑/↓ and PageUp/Down scroll the definition (clamped at
/// render time, like the skills preview); esc / ⏎ / q close it.
pub(super) fn handle_done_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    match key.code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {
            ui.installed.mode = InstalledMode::Browse;
            ui.installed.created_name = None;
            ui.installed.create_error = None;
            ui.installed.created_scroll = 0;
        }
        KeyCode::Up | KeyCode::Char('k') => {
            ui.installed.created_scroll = ui.installed.created_scroll.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            ui.installed.created_scroll = ui.installed.created_scroll.saturating_add(1);
        }
        KeyCode::PageUp => {
            ui.installed.created_scroll = ui.installed.created_scroll.saturating_sub(10);
        }
        KeyCode::PageDown | KeyCode::Char(' ') => {
            ui.installed.created_scroll = ui.installed.created_scroll.saturating_add(10);
        }
        KeyCode::Home => ui.installed.created_scroll = 0,
        KeyCode::End => ui.installed.created_scroll = u16::MAX,
        _ => {}
    }
    DeckAction::Handled
}

/// Settle an in-flight agent create against a fresh driver snapshot.
///
/// A no-op unless the creating dialog is actually up and the snapshot is the
/// completion (`creating: false`) — an interim snapshot for a create parked
/// behind a running turn must keep the spinner going. On completion the dialog
/// transitions **in place**: the created agent's detail view on success, the
/// driver's `status` as the error on failure. Never silently back to Browse.
pub(super) fn settle_agents_snapshot(ui: &mut DeckUi, snap: AgentsSnapshot<'_>) {
    if ui.installed.mode != InstalledMode::Creating || snap.creating {
        return;
    }
    ui.installed.created_name = snap.created.cloned();
    ui.installed.create_error = if snap.created.is_some() {
        None
    } else {
        Some(
            snap.status
                .cloned()
                .unwrap_or_else(|| "agent creation failed".to_string()),
        )
    };
    ui.installed.created_scroll = 0;
    ui.installed.mode = InstalledMode::CreateDone;
    // Select the new agent so the list lands on it after close.
    if let Some(name) = snap.created
        && let Some(idx) = snap.entries.iter().position(|e| &e.name == name)
    {
        ui.installed.sel = idx;
    }
}

/// The borrowed fields of an [`crate::envelope::Inbound::AgentsList`] that
/// [`settle_agents_snapshot`] needs. A struct rather than four positional
/// arguments because `creating` and `created` are easy to transpose and the
/// whole point of them is that they mean different things.
pub(super) struct AgentsSnapshot<'a> {
    pub entries: &'a [InstalledAgentEntry],
    pub status: Option<&'a String>,
    /// True while the create is still in flight driver-side — this snapshot is
    /// interim and must NOT settle the dialog.
    pub creating: bool,
    /// The installed agent's name when this snapshot is a create's completion.
    pub created: Option<&'a String>,
}

// ── The skill create flow (the SKILLS tab) ─────────────────────────────────

/// Answer the SKILLS scope picker's ⏎ for whichever op it was asked about.
///
/// The asymmetry is the point: an **install** closes the dialog (the npx op is
/// quick and the refreshed list is its own receipt), while a **create** leaves
/// the dialog open in [`SkillPrompt::Creating`] so an animated spinner holds
/// the reader's place until the refreshed snapshot settles it.
pub(super) fn dispatch_skills_scope(
    ui: &mut DeckUi,
    action: ScopeAction,
    scope: SkillScope,
) -> DeckAction {
    match action {
        ScopeAction::Install { id } => {
            ui.skills.prompt = None;
            ui.skills.status = Some(format!("installing {id} for {}…", scope.label()));
            DeckAction::Send(WorkspaceInput::Skill(SkillOp::Install { scope, id }))
        }
        ScopeAction::Create { description } => {
            ui.skills.prompt = Some(SkillPrompt::Creating {
                description: description.clone(),
                scope,
            });
            ui.skills.status = Some(format!("assembling a skill for {}…", scope.label()));
            DeckAction::Send(WorkspaceInput::Skill(SkillOp::Create {
                scope,
                description,
            }))
        }
    }
}

/// The skills creation-in-flight dialog: fully modal. Esc hides it (the
/// driver-side creation keeps running and the refreshed snapshot still folds
/// in); everything else is swallowed so keys never leak to the composer.
pub(super) fn handle_skills_creating_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    if matches!(key.code, KeyCode::Esc) {
        ui.skills.prompt = None;
    }
    DeckAction::Handled
}

/// The skills creation-failed dialog: esc / ⏎ / q acknowledge and close.
pub(super) fn handle_skills_failed_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q')) {
        ui.skills.prompt = None;
    }
    DeckAction::Handled
}

/// Settle an in-flight skill create against a fresh driver snapshot.
///
/// A no-op unless the creating dialog is up. On success the dialog becomes the
/// created skill's `ctrl+o` preview (title, scope subtitle, rendered
/// `SKILL.md`) so the reader reads what actually landed; on failure it shows
/// the driver's error — either way the outcome can't be missed.
pub(super) fn settle_skills_snapshot(ui: &mut DeckUi, view: &SkillsView) {
    if !matches!(ui.skills.prompt, Some(SkillPrompt::Creating { .. })) {
        return;
    }
    let created = view
        .created
        .as_ref()
        .and_then(|name| view.rows.iter().find(|r| &r.name == name));
    let Some(row) = created else {
        ui.skills.prompt = Some(SkillPrompt::CreateFailed {
            error: view
                .status
                .clone()
                .unwrap_or_else(|| "skill creation failed".to_string()),
        });
        return;
    };
    ui.skills.prompt = None;
    ui.skills.preview = Some(SkillPreview {
        title: row.name.clone(),
        subtitle: format!("{} · {} · v{}", row.scope.label(), row.origin, row.version),
        pending: None,
        body: Some(if row.body.trim().is_empty() {
            "*(this skill has an empty body)*".to_string()
        } else {
            row.body.clone()
        }),
        scroll: 0,
    });
    // Select the new skill so the list lands on it after close.
    if let Some(idx) = view.rows.iter().position(|r| r.name == row.name) {
        ui.skills.sel = idx;
    }
}
