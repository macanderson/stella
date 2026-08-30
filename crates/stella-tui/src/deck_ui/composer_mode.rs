//! The composer's intent — what Enter does with the words.
//!
//! One prompt line, three intents, told apart by the chevron's color: gold
//! dispatches a prompt (today's routing, `ui.mid_turn_prompt` included), teal
//! steers the running turn — the text lands at its next step boundary as a
//! real user message — and red interrupts: a soft stop that keeps every
//! completed step, with the words front-queued to run next. Shift-Tab cycles
//! the three while the composer holds a draft; with an empty composer it
//! keeps its deck job of cycling tabs, resolving the collision on what the
//! user is visibly doing.
//!
//! The mode is derived, never latched: with no override the chevron follows
//! the session — gold at rest, teal while the focused agent runs under the
//! `steer` policy — so there is no event hook to forget and no stale mode to
//! submit through. A cycled override lasts until the next submission.

use super::*;

/// What Enter does with the composer's text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ComposerMode {
    /// A new prompt, routed by [`dispatch::route`].
    #[default]
    Dispatch,
    /// Injected into the running turn at its next step boundary, as a real
    /// user message the model answers on its very next call.
    Steer,
    /// Soft-stop the running turn — completed steps kept — and run the text
    /// as the next thing. At an idle agent this degrades to "run this now".
    Interrupt,
}

impl ComposerMode {
    /// The next mode in the Shift-Tab cycle.
    pub fn next(self) -> Self {
        match self {
            Self::Dispatch => Self::Steer,
            Self::Steer => Self::Interrupt,
            Self::Interrupt => Self::Dispatch,
        }
    }
}

/// The mode in effect: the user's cycled override when one is set, else
/// derived from the focused agent — [`ComposerMode::Steer`] while it runs
/// under [`MidTurnPrompt::Steer`], [`ComposerMode::Dispatch`] everywhere
/// else.
pub fn effective(ui: &DeckUi, model: &WorkspaceModel) -> ComposerMode {
    if let Some(mode) = ui.composer_mode {
        return mode;
    }
    let running = model
        .agents
        .get(ui.focused)
        .is_some_and(|a| a.status == crate::AgentStatus::Running);
    if running && ui.mid_turn_prompt == MidTurnPrompt::Steer {
        ComposerMode::Steer
    } else {
        ComposerMode::Dispatch
    }
}

/// Claim Shift-Tab for mode-cycling while the composer holds a draft. `None`
/// hands the key back to the deck's tab-cycling, which owns it when there is
/// nothing to submit and therefore no intent to pick.
pub(super) fn backtab(
    key: KeyEvent,
    ui: &mut DeckUi,
    model: &WorkspaceModel,
    composer_empty: bool,
) -> Option<DeckAction> {
    if key.code != KeyCode::BackTab || composer_empty {
        return None;
    }
    ui.composer_mode = Some(effective(ui, model).next());
    Some(DeckAction::Handled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{AgentMeta, Inbound};

    fn model_with_lead(status: crate::AgentStatus) -> WorkspaceModel {
        let mut m = WorkspaceModel::new();
        m.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        m.apply_inbound(&Inbound::Status {
            agent: "lead".into(),
            status,
        });
        m
    }

    #[test]
    fn the_cycle_visits_all_three_modes_and_returns() {
        let mut mode = ComposerMode::Dispatch;
        let mut seen = vec![mode];
        for _ in 0..3 {
            mode = mode.next();
            seen.push(mode);
        }
        assert_eq!(
            seen,
            vec![
                ComposerMode::Dispatch,
                ComposerMode::Steer,
                ComposerMode::Interrupt,
                ComposerMode::Dispatch,
            ]
        );
    }

    /// The chevron follows the session: under the `steer` policy the mode is
    /// teal exactly while the focused agent runs, with no state to reset.
    #[test]
    fn under_the_steer_policy_the_mode_follows_the_running_turn() {
        let mut ui = DeckUi {
            mid_turn_prompt: MidTurnPrompt::Steer,
            ..Default::default()
        };
        assert_eq!(
            effective(&ui, &model_with_lead(crate::AgentStatus::Running)),
            ComposerMode::Steer
        );
        assert_eq!(
            effective(&ui, &model_with_lead(crate::AgentStatus::WaitingInput)),
            ComposerMode::Dispatch,
            "an idle agent has no turn to steer"
        );
        ui.mid_turn_prompt = MidTurnPrompt::Queue;
        assert_eq!(
            effective(&ui, &model_with_lead(crate::AgentStatus::Running)),
            ComposerMode::Dispatch,
            "the default policy keeps today's gold dispatch"
        );
    }

    /// Shift-Tab with a draft cycles the mode; with an empty composer it is
    /// declined, so tab-cycling keeps its key.
    #[test]
    fn backtab_cycles_only_while_the_composer_holds_a_draft() {
        let model = model_with_lead(crate::AgentStatus::Running);
        let mut ui = DeckUi::default();
        let key = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(backtab(key, &mut ui, &model, true), None);
        assert_eq!(ui.composer_mode, None);
        assert_eq!(
            backtab(key, &mut ui, &model, false),
            Some(DeckAction::Handled)
        );
        assert_eq!(ui.composer_mode, Some(ComposerMode::Steer));
        backtab(key, &mut ui, &model, false);
        assert_eq!(ui.composer_mode, Some(ComposerMode::Interrupt));
    }
}
