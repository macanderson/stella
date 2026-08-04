//! The floating card overlays — the plan, model routing, and the budget
//! editor — their shared view state and their modal key handlers. Split out
//! of `deck_ui.rs` (already the crate's largest file) so the file-size guard
//! holds; the rendering lives beside the other views in
//! `crate::views::{plan_card, models_card, budget_card}` over the shared
//! chrome in `crate::views::cards`.
//!
//! `/plan` is one card where there used to be three. `/tasks` showed a board
//! nothing ever populated, `/scope` showed the same plan's envelope without
//! its steps, and `/witness` showed the verification records that are now the
//! rail's permanent bottom panel. Three cards, one subject, and no single one
//! of them could answer "what is step 3".
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
}

/// The card overlays' ephemeral view state.
#[derive(Debug, Clone, Default)]
pub struct CardState {
    /// The raised card, if any.
    pub open: Option<Card>,
    /// Plan-card step selection, clamped to the plan at render time.
    pub plan_sel: usize,
    /// Whether the plan card's selected step is expanded (⏎ shows its full
    /// elaboration under it).
    pub plan_expanded: bool,
    /// The budget editor's input buffer — digits and at most one `.`.
    pub budget_input: String,
}

impl CardState {
    /// Whether any card is up (the statline's collapse trigger).
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Raise `card`, lowering whichever was up. Re-raising resets the card's
    /// own transient state (selection, expansion, the budget draft) so
    /// `/plan` always opens at the first step and `/budget` opens with a
    /// clean input.
    pub fn raise(&mut self, card: Card) {
        self.open = Some(card);
        self.plan_sel = 0;
        self.plan_expanded = false;
        self.budget_input.clear();
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
    // Esc closes any card. ctrl+s closes the plan specifically, because that
    // chord *raised* it — a toggle that only opens is a trap, and the card
    // handler runs before the deck-level chord, so swallowing it here would
    // make the second press do nothing.
    let ctrl_s = key
        .modifiers
        .contains(crossterm::event::KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('s'));
    if matches!(key.code, KeyCode::Esc) || (ctrl_s && card == Card::Plan) {
        ui.cards.close();
        return Some(DeckAction::Handled);
    }
    Some(match card {
        Card::Plan => handle_plan_key(key, model, ui),
        Card::Budget => handle_budget_key(key, ui),
        // Read-only surface: every key is swallowed so a stray letter never
        // reaches the composer behind a card the user is looking at.
        Card::Models => DeckAction::Handled,
    })
}

/// Plan card: ↑/↓ select a step, ⏎ expand/collapse it, `x` asks the driver to
/// skip the selected still-open step, and `e` proposes a change to the plan's
/// envelope once it is approved.
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
            ui.cards.plan_expanded = !ui.cards.plan_expanded;
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
                    },
                    TaskItem {
                        id: "2".into(),
                        subject: "doing two".into(),
                        description: Some("the long form".into()),
                        status: TaskStatus::InProgress,
                        owner: Some("lead".into()),
                    },
                    TaskItem {
                        id: "3".into(),
                        subject: "queued three".into(),
                        description: None,
                        status: TaskStatus::Pending,
                        owner: None,
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
                name: stella_protocol::StageKind::Execute,
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
