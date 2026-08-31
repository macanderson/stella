//! The card overlays — the plan, model routing, the budget editor and the
//! task zoom — their shared view state and their modal key handlers. Split out
//! of `deck_ui.rs` (already the crate's largest file) so the file-size guard
//! holds; the rendering lives in
//! `crate::views::{plan_card, models_card, budget_card, task_zoom}` over the
//! shared chrome in `crate::views::cards`.
//!
//! `/plan` is one card where there used to be three. `/tasks` showed a board
//! nothing ever populated, `/scope` showed the same plan's envelope without
//! its steps, and `/witness` showed the verification records. Three cards, one
//! subject, and no single one of them could answer "what is step 3".
//!
//! ## Interaction contract
//!
//! Cards are modal exactly like the queue editor: while one is up it owns
//! the keyboard, and Esc closes it before any other Esc meaning fires (the
//! "topmost card first" rule in [`super::handle_deck_key`]'s Esc precedence
//! list). At most one card is up at a time — raising one lowers the rest —
//! so "topmost" is unambiguous. Everything a card *does* leaves as a
//! [`WorkspaceInput`]; the deck renders only folded state back (skipping a
//! task, proposing a scope edit, and setting the budget cap all round-trip
//! through the driver).

use crossterm::event::{KeyCode, KeyEvent};

use crate::deck::WorkspaceModel;
use crate::deck_ui::{DeckAction, DeckUi};
use crate::envelope::WorkspaceInput;

/// Which card is up. At most one — see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Card {
    /// The plan card (`/plan`): every plan step with its full text, over the
    /// plan's operating envelope.
    Plan,
    /// The model-routing card (`/models`): think · work · verify slots.
    Models,
    /// The spend-cap editor (`/budget`).
    Budget,
    /// The task zoom (SPEC 7.5): the plan card's selected step at full size —
    /// its contract, its evidence, the planned and actual lanes, and what it
    /// spent. Raised by `⏎` on the plan card, and the one card that takes the
    /// whole content band instead of floating over it.
    TaskZoom,
}

/// The card overlays' ephemeral view state.
#[derive(Debug, Clone, Default)]
pub struct CardState {
    /// The raised card, if any.
    pub open: Option<Card>,
    /// Plan-card step selection, clamped to the plan at render time. The task
    /// zoom reads the same field, so zooming and returning keep one selection
    /// rather than two that can disagree.
    pub plan_sel: usize,
    /// The budget editor's input buffer — digits and at most one `.`.
    pub budget_input: String,
    /// The task zoom's body scroll. The zoom's selection is the *task*, not
    /// a row, so this offset drives the window. `↑`/`↓`/`⇞`/`⇟` move it.
    pub zoom_scroll: crate::scroll::ScrollState,
}

impl CardState {
    /// Whether any card is up (the statline's collapse trigger).
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Raise `card`, lowering whichever was up. Re-raising resets the card's
    /// own transient state (selection, the budget draft) so `/plan` always
    /// opens at the first step and `/budget` opens with a clean input.
    pub fn raise(&mut self, card: Card) {
        self.open = Some(card);
        self.plan_sel = 0;
        self.budget_input.clear();
        self.zoom_scroll = crate::scroll::ScrollState {
            top: 0,
            follow: false,
        };
    }

    /// Zoom the plan card's selected step (SPEC 7.5), and come back out of it.
    ///
    /// Neither touches [`Self::plan_sel`], which is what makes `⏎` then `esc`
    /// land the reader back on the step they zoomed rather than at the top of
    /// the plan — [`Self::raise`] resets, and that is the wrong verb for a
    /// move between two views of one selection.
    pub fn zoom_selected_step(&mut self) {
        self.open = Some(Card::TaskZoom);
        // Open at the top. Tail-follow would hide the contract ⏎ asked for.
        self.zoom_scroll = crate::scroll::ScrollState {
            top: 0,
            follow: false,
        };
    }

    /// Leave the zoom for the plan card it was opened from.
    pub fn unzoom(&mut self) {
        self.open = Some(Card::Plan);
    }

    /// Lower whatever is up.
    pub fn close(&mut self) {
        self.open = None;
    }
}

/// The modal key map while a card is up. Returns `None` when no card is
/// open, so the caller falls through to the rest of the precedence chain.
pub fn handle_card_key(
    key: KeyEvent,
    model: &WorkspaceModel,
    ui: &mut DeckUi,
) -> Option<DeckAction> {
    let card = ui.cards.open?;
    // Esc closes any card, except the zoom — which is one level *inside* the
    // plan card, so Esc there is a step back rather than a way out (SPEC 7.5
    // spells the affordance `esc back`). ctrl+s closes both, because that
    // chord *raised* the plan in the first place — a toggle that only opens is
    // a trap, and this handler runs before the deck-level chord, so swallowing
    // it here would make the second press do nothing.
    let ctrl_s = key
        .modifiers
        .contains(crossterm::event::KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('s'));
    if matches!(key.code, KeyCode::Esc) && card == Card::TaskZoom {
        ui.cards.unzoom();
        return Some(DeckAction::Handled);
    }
    if matches!(key.code, KeyCode::Esc) || (ctrl_s && matches!(card, Card::Plan | Card::TaskZoom)) {
        ui.cards.close();
        return Some(DeckAction::Handled);
    }
    Some(match card {
        Card::Plan => handle_plan_key(key, model, ui),
        Card::Budget => handle_budget_key(key, ui),
        Card::TaskZoom => handle_zoom_key(key, ui),
        // Read-only surface: every key is swallowed so a stray letter never
        // reaches the composer behind a card the user is looking at.
        Card::Models => DeckAction::Handled,
    })
}

/// The task zoom's action row (SPEC 7.5): `r re-run checks · s split task ·
/// b hand to worker · i promote to issue · ⌥ diff plan`.
///
/// **Every one of the five is drawn and inert**, and each names the issue that
/// wires it. This repository's rule is that an unwired affordance is either
/// tracked or absent, so the row ships with its tracking rather than with four
/// of the five deleted — the reader of the zoom needs to know what the surface
/// will do, and a verb that silently does nothing is the failure this comment
/// exists to prevent.
///
/// - `r` re-run checks — #5149. Needs a runner that can re-execute one task's
///   `Check` list and fold the outcomes back; nothing today re-runs a check.
/// - `s` split task — #5150. Needs plan-revision authoring (#5037): splitting
///   a task is `r{n+1}` with the prior plan retained.
/// - `b` hand to worker — #5151. Needs a dispatch path that hands one task,
///   not a whole turn, to a lane.
/// - `i` promote to issue — #5152. Needs the tracker MCP write path the ISSUES
///   tab's `n` uses, addressed at a task rather than a prompt.
/// - `⌥` diff plan — #5153. Needs the `[:NEXT]`/`[:THEN]` edges (#5037) to
///   have two revisions to diff.
///
/// The body scrolls. The zoom has no row cursor to follow, so
/// [`super::list_nav::scroll`]'s vocabulary drives an offset instead. It scrolls
/// against the viewport the last render recorded in
/// [`crate::deck_ui::DeckMetrics`] — the `files_diff` contract.
///
/// Every other key is swallowed regardless, so a stray letter never reaches
/// the composer behind a surface the reader is looking at.
fn handle_zoom_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    let (total, height) = (ui.metrics.zoom_total, ui.metrics.zoom_height);
    let _ = super::list_nav::scroll(key, &mut ui.cards.zoom_scroll, total, height, true);
    DeckAction::Handled
}

/// Plan card: ↑/↓ select a step, ⏎ zooms it (SPEC 7.5), `x` asks the driver to
/// skip the selected still-open step, and `e` proposes a change to the plan's
/// envelope once it is approved.
///
/// ⏎ used to toggle a `plan_expanded` flag that no renderer ever read, so the
/// key did nothing at all; it now raises [`Card::TaskZoom`] on the selected
/// step, which is what SPEC 11 has always said `↵` means — *open or zoom the
/// selected object*.
///
/// Both writes leave as a [`WorkspaceInput`] — the card never edits locally,
/// so what it shows is always the plan actually in force, and a step's state
/// changes only when the driver's next snapshot folds back.
fn handle_plan_key(key: KeyEvent, model: &WorkspaceModel, ui: &mut DeckUi) -> DeckAction {
    let Some(agent) = model.agents.get(ui.focused) else {
        return DeckAction::Handled;
    };
    let steps = agent.model.plan.steps();
    let approved =
        agent.model.pending_scope_review.is_none() && agent.model.approved_scope.is_some();
    if matches!(key.code, KeyCode::Char('e')) && approved {
        return DeckAction::Send(WorkspaceInput::ScopeChangeRequest {
            agent: agent.meta.id.clone(),
        });
    }
    let count = steps.len();
    if count == 0 {
        return DeckAction::Handled;
    }
    ui.cards.plan_sel = ui.cards.plan_sel.min(count - 1);
    match key.code {
        KeyCode::Up => {
            ui.cards.plan_sel = ui.cards.plan_sel.saturating_sub(1);
            DeckAction::Handled
        }
        KeyCode::Down => {
            ui.cards.plan_sel = (ui.cards.plan_sel + 1).min(count - 1);
            DeckAction::Handled
        }
        KeyCode::Enter => {
            ui.cards.zoom_selected_step();
            DeckAction::Handled
        }
        KeyCode::Char('x') => match steps.get(ui.cards.plan_sel) {
            // Only a still-open step can be skipped; `x` on a settled row is
            // a no-op rather than a stray request.
            Some(step) if step.state.is_open() => DeckAction::Send(WorkspaceInput::TaskSkip {
                agent: agent.meta.id.clone(),
                id: step.id.clone(),
            }),
            _ => DeckAction::Handled,
        },
        _ => DeckAction::Handled,
    }
}

/// Budget editor: digits and one `.` build the cap, ⌫ deletes, ⏎ sends the
/// parsed cap as [`WorkspaceInput::SetBudget`] and closes — with an empty
/// input it clears the cap instead. The card renders the cap the model
/// currently holds; the new one appears only when the driver's budget stream
/// folds it back.
fn handle_budget_key(key: KeyEvent, ui: &mut DeckUi) -> DeckAction {
    match key.code {
        KeyCode::Char(c @ '0'..='9') => {
            ui.cards.budget_input.push(c);
            DeckAction::Handled
        }
        KeyCode::Char('.') if !ui.cards.budget_input.contains('.') => {
            ui.cards.budget_input.push('.');
            DeckAction::Handled
        }
        KeyCode::Backspace => {
            ui.cards.budget_input.pop();
            DeckAction::Handled
        }
        KeyCode::Enter => {
            let input = ui.cards.budget_input.trim().to_string();
            ui.cards.close();
            if input.is_empty() {
                return DeckAction::Send(WorkspaceInput::SetBudget { limit_usd: None });
            }
            match input.parse::<f64>() {
                Ok(cap) if cap > 0.0 => DeckAction::Send(WorkspaceInput::SetBudget {
                    limit_usd: Some(cap),
                }),
                // Unparseable or non-positive: nothing is sent — the card
                // closes and the folded cap stands.
                _ => DeckAction::Handled,
            }
        }
        _ => DeckAction::Handled,
    }
}

/// True when the selected plan step can still be skipped — the render side
/// uses this to decide whether to advertise `x skip` in the hints.
pub fn selected_step_skippable(model: &WorkspaceModel, ui: &DeckUi) -> bool {
    model
        .agents
        .get(ui.focused)
        .and_then(|a| a.model.plan.steps().get(ui.cards.plan_sel).cloned())
        .is_some_and(|s| s.state.is_open())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use stella_protocol::{AgentEvent, TaskItem, TaskStatus};

    use crate::envelope::{AgentMeta, Inbound};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn board_model() -> WorkspaceModel {
        let mut m = WorkspaceModel::new();
        m.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        m.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::TaskUpdate {
                tasks: vec![
                    TaskItem {
                        id: "1".into(),
                        subject: "done one".into(),
                        description: None,
                        status: TaskStatus::Completed,
                        owner: Some("lead".into()),
                        contract: None,
                    },
                    TaskItem {
                        id: "2".into(),
                        subject: "doing two".into(),
                        description: Some("the long form".into()),
                        status: TaskStatus::InProgress,
                        owner: Some("lead".into()),
                        contract: None,
                    },
                    TaskItem {
                        id: "3".into(),
                        subject: "queued three".into(),
                        description: None,
                        status: TaskStatus::Pending,
                        owner: None,
                        contract: None,
                    },
                ],
            },
        });
        m
    }

    #[test]
    fn esc_closes_the_topmost_card_and_claims_the_key() {
        let model = board_model();
        let mut ui = DeckUi::default();
        ui.cards.raise(Card::Plan);
        let action = handle_card_key(key(KeyCode::Esc), &model, &mut ui);
        assert_eq!(action, Some(DeckAction::Handled));
        assert!(!ui.cards.is_open(), "Esc lowers the card");
    }

    #[test]
    fn raising_a_card_lowers_the_previous_one() {
        let mut cards = CardState::default();
        cards.raise(Card::Plan);
        cards.raise(Card::Models);
        assert_eq!(cards.open, Some(Card::Models), "at most one card is up");
    }

    #[test]
    fn skip_sends_a_skip_for_the_selected_open_step() {
        let model = board_model();
        let mut ui = DeckUi::default();
        ui.cards.raise(Card::Plan);
        ui.cards.plan_sel = 1; // "doing two" — open
        let action = handle_card_key(key(KeyCode::Char('x')), &model, &mut ui);
        assert_eq!(
            action,
            Some(DeckAction::Send(WorkspaceInput::TaskSkip {
                agent: "lead".into(),
                id: "2".into(),
            }))
        );
    }

    #[test]
    fn skip_on_a_settled_step_sends_nothing() {
        let model = board_model();
        let mut ui = DeckUi::default();
        ui.cards.raise(Card::Plan);
        ui.cards.plan_sel = 0; // "done one" — completed, terminal
        let action = handle_card_key(key(KeyCode::Char('x')), &model, &mut ui);
        assert_eq!(action, Some(DeckAction::Handled));
    }

    #[test]
    fn budget_enter_parses_and_sends_the_cap() {
        let model = board_model();
        let mut ui = DeckUi::default();
        ui.cards.raise(Card::Budget);
        for c in ['2', '.', '5'] {
            handle_card_key(key(KeyCode::Char(c)), &model, &mut ui);
        }
        assert_eq!(ui.cards.budget_input, "2.5");
        let action = handle_card_key(key(KeyCode::Enter), &model, &mut ui);
        assert_eq!(
            action,
            Some(DeckAction::Send(WorkspaceInput::SetBudget {
                limit_usd: Some(2.5)
            }))
        );
        assert!(!ui.cards.is_open(), "sending closes the editor");
    }

    #[test]
    fn budget_rejects_a_second_dot_and_letters() {
        let model = board_model();
        let mut ui = DeckUi::default();
        ui.cards.raise(Card::Budget);
        for c in ['1', '.', '.', 'x', '5'] {
            handle_card_key(key(KeyCode::Char(c)), &model, &mut ui);
        }
        assert_eq!(ui.cards.budget_input, "1.5");
    }

    #[test]
    fn budget_enter_on_an_empty_input_clears_the_cap() {
        let model = board_model();
        let mut ui = DeckUi::default();
        ui.cards.raise(Card::Budget);
        let action = handle_card_key(key(KeyCode::Enter), &model, &mut ui);
        assert_eq!(
            action,
            Some(DeckAction::Send(WorkspaceInput::SetBudget {
                limit_usd: None
            }))
        );
    }

    #[test]
    fn scope_e_proposes_an_edit_only_post_approval() {
        // Pending gate: `e` does nothing (the gate card owns the decision).
        let mut model = board_model();
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::ScopeReview {
                proposal: stella_protocol::ScopeProposal {
                    summary: "s".into(),
                    steps: vec!["a".into()],
                    estimated_files: 1,
                    estimated_cost_usd: None,
                    ..Default::default()
                },
            },
        });
        let mut ui = DeckUi::default();
        ui.cards.raise(Card::Plan);
        let action = handle_card_key(key(KeyCode::Char('e')), &model, &mut ui);
        assert_eq!(action, Some(DeckAction::Handled));

        // Approved (first non-ScopeReview stage): `e` proposes the change.
        model.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Stage {
                name: stella_protocol::StageKind::Execute.into(),
                scope: stella_protocol::StageScope::Run,
            },
        });
        let action = handle_card_key(key(KeyCode::Char('e')), &model, &mut ui);
        assert_eq!(
            action,
            Some(DeckAction::Send(WorkspaceInput::ScopeChangeRequest {
                agent: "lead".into(),
            }))
        );
    }
}
