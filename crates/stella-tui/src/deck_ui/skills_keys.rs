// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The SKILLS tab's dialog and row keys: the scope picker — where a skill an
//! install or a create is about to produce gets written, project or user —
//! the `ctrl+o` preview, and SPEC 9.2's learned-skill verbs `r` and `x`.
//!
//! Also `!`, which opens the rejected-skills review — the reader/undo half
//! `x` never had.
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

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{DeckAction, DeckUi, ScopeAction, SkillPreview, SkillPrompt, create, list_nav};
use crate::envelope::{SkillOp, SkillScope, WorkspaceInput};

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

/// The learned-skill rename dialog's keys: a one-line text input. ⏎ dispatches
/// the rename, esc abandons it, and everything else is swallowed — fully
/// modal, like every other SKILLS prompt, so a name typed here never leaks
/// into the composer behind the overlay.
///
/// An empty or unchanged name is refused *here* rather than sent for the
/// driver to reject: the round-trip would blank the dialog and answer with a
/// status line, and the user is standing in front of the field they need to
/// fix.
pub(super) fn handle_rename_key(
    key: KeyEvent,
    ui: &mut DeckUi,
    scope: SkillScope,
    name: String,
    mut buffer: String,
    was: String,
) -> DeckAction {
    match key.code {
        KeyCode::Esc => {
            ui.skills.prompt = None;
            ui.skills.status = Some("rename cancelled".into());
            DeckAction::Handled
        }
        KeyCode::Enter => {
            let to = buffer.trim().to_string();
            if to.is_empty() {
                ui.skills.status = Some("a skill needs a name".into());
                return DeckAction::Handled;
            }
            if to == name {
                ui.skills.prompt = None;
                ui.skills.status = Some(format!("{name} already has that name"));
                return DeckAction::Handled;
            }
            ui.skills.prompt = None;
            ui.skills.searching = true;
            ui.skills.status = Some(format!("renaming {name} → {to}…"));
            DeckAction::Send(WorkspaceInput::Skill(SkillOp::Rename {
                scope,
                from: name,
                to,
            }))
        }
        KeyCode::Backspace => {
            buffer.pop();
            ui.skills.prompt = Some(SkillPrompt::Rename {
                scope,
                name,
                buffer,
                was,
            });
            DeckAction::Handled
        }
        KeyCode::Char(c)
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            buffer.push(c);
            ui.skills.prompt = Some(SkillPrompt::Rename {
                scope,
                name,
                buffer,
                was,
            });
            DeckAction::Handled
        }
        _ => DeckAction::Handled,
    }
}

/// `r` on the installed pane: open the rename dialog for the highlighted
/// learned skill.
///
/// Learned-only, and the refusal names the reason rather than doing nothing. A
/// skill the user or a registry named already has the name its author chose;
/// the auto-rename exists because a mined `<slug>-<hash8>` has no author to
/// have chosen one.
pub(super) fn begin_rename(ui: &mut DeckUi) -> DeckAction {
    let Some(row) = ui.skills.view.rows.get(ui.skills.sel) else {
        return DeckAction::Handled;
    };
    match &row.learned {
        None => {
            ui.skills.status = Some(format!(
                "{} was not learned from traces — rename it in its own file \
                 with `e`",
                row.name
            ));
        }
        Some(learned) => {
            ui.skills.prompt = Some(SkillPrompt::Rename {
                scope: row.scope,
                name: row.name.clone(),
                // Pre-filled with the current name, so a rename is an edit
                // rather than a retype — the way the edit overlay opens on the
                // current body.
                buffer: row.name.clone(),
                was: learned.was.clone(),
            });
        }
    }
    DeckAction::Handled
}

/// `x` on the installed pane: arm a rejection, or — on the second press —
/// dispatch it.
///
/// Two presses, like uninstall. Rejecting destroys a file *and* writes a
/// durable negative signal into the learner, and neither should happen on a
/// stray key. `armed` is the pane's own flag, read before this key disarmed
/// anything, so the two destructive verbs cannot complete each other.
pub(super) fn reject_press(ui: &mut DeckUi, armed: bool) -> DeckAction {
    let Some(row) = ui.skills.view.rows.get(ui.skills.sel) else {
        return DeckAction::Handled;
    };
    if row.learned.is_none() {
        ui.skills.status = Some(format!(
            "{} was not learned from traces — there is no learner to teach. \
             ctrl+x twice deletes it",
            row.name
        ));
        return DeckAction::Handled;
    }
    if !armed {
        ui.skills.reject_armed = true;
        ui.skills.status = Some(format!(
            "press x again to REJECT {} — it is deleted and the learner stops \
             proposing it",
            row.name
        ));
        return DeckAction::Handled;
    }
    let name = row.name.clone();
    let scope = row.scope;
    ui.skills.searching = true;
    ui.skills.status = Some(format!("rejecting {name}…"));
    DeckAction::Send(WorkspaceInput::Skill(SkillOp::Reject { scope, name }))
}

/// Open the ctrl+o preview for the highlighted installed skill — its body is
/// already in hand (`SkillRow::body`), so no driver round-trip.
///
/// A **learned** skill opens on its source traces instead (SPEC 9.2's
/// `ctrl+o show source traces`): the traces come first, then the body under
/// its own heading. One overlay rather than two, because "what is this skill"
/// and "why does it exist" are the same question for a skill nobody wrote —
/// and answering only the second would take the body away from the one class
/// of skill whose body was never reviewed by a human.
pub(super) fn open_installed_preview(ui: &mut DeckUi) -> Option<DeckAction> {
    let row = ui.skills.view.rows.get(ui.skills.sel)?;
    let body = if row.body.trim().is_empty() {
        "*(this skill has an empty body)*".to_string()
    } else {
        row.body.clone()
    };
    let (subtitle, body) = match &row.learned {
        Some(learned) => (
            crate::views::skills::provenance(row)
                .unwrap_or_else(|| format!("{} · learned", row.scope.label())),
            format!("{}\n## The skill\n\n{body}", source_trace_markdown(learned)),
        ),
        None => (
            format!("{} · {} · v{}", row.scope.label(), row.origin, row.version),
            body,
        ),
    };
    ui.skills.preview = Some(SkillPreview {
        title: row.name.clone(),
        subtitle,
        pending: None,
        body: Some(body),
        scroll: 0,
    });
    Some(DeckAction::Handled)
}

/// The source traces behind a learned skill, as the markdown the preview
/// overlay renders: one bullet per trace, its reference and instant in front
/// of the observation that was actually recorded.
///
/// The empty case says so out loud. A learned skill whose file carries no
/// `## Evidence` section — one mined before the section existed, or a
/// hand-edited file whose appendix was cut — is a real thing to find in a
/// workspace, and a heading with nothing under it reads as a rendering bug.
fn source_trace_markdown(learned: &crate::envelope::LearnedProvenance) -> String {
    let mut out = String::from("## Source traces\n\n");
    if learned.sources.is_empty() {
        out.push_str(
            "*(this skill's file records no traces — it was mined before its \
             evidence was kept, or the section was edited away)*\n\n",
        );
        return out;
    }
    for source in &learned.sources {
        out.push_str(&format!(
            "- `{}` (observed at {}) — {}\n",
            source.reference, source.observed_at, source.snippet
        ));
    }
    out.push('\n');
    out
}

/// The rejected-skills review's keys: ↑/↓ choose, ⏎ / `u` reverses the
/// highlighted rejection, esc closes.
///
/// One press, unlike `x reject` and `ctrl+x uninstall`. Reversing a
/// rejection destroys nothing — it drops one entry, and the miner starts
/// proposing the skill again on its next pass. A confirm step here would
/// just be friction, with no destructive act to guard against.
pub(super) fn handle_rejected_key(key: KeyEvent, ui: &mut DeckUi, sel: usize) -> DeckAction {
    let count = ui.skills.view.rejections.len();
    if count == 0 {
        // The list emptied under an open dialog. Nothing left to choose, so
        // close rather than show an empty picker.
        ui.skills.prompt = None;
        return DeckAction::Handled;
    }
    let sel = sel.min(count - 1);
    match key.code {
        KeyCode::Esc => {
            ui.skills.prompt = None;
            DeckAction::Handled
        }
        KeyCode::Up => {
            ui.skills.prompt = Some(SkillPrompt::Rejected {
                sel: sel.saturating_sub(1),
            });
            DeckAction::Handled
        }
        KeyCode::Down => {
            ui.skills.prompt = Some(SkillPrompt::Rejected {
                sel: (sel + 1).min(count - 1),
            });
            DeckAction::Handled
        }
        KeyCode::Enter | KeyCode::Char('u') => {
            let row = ui.skills.view.rejections[sel].clone();
            ui.skills.prompt = None;
            ui.skills.searching = true;
            ui.skills.status = Some(format!("un-rejecting {}…", row.name));
            DeckAction::Send(WorkspaceInput::Skill(SkillOp::Unreject {
                scope: row.scope,
                mined_as: row.mined_as,
            }))
        }
        _ => DeckAction::Handled,
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
