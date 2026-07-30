//! Where a prompt submitted while a turn is running should go — asked, not
//! assumed.
//!
//! The deck used to answer this silently: any mid-turn prompt without a `>`
//! marker became a sidecar sub-session. That is a defensible default for a
//! *concurrent* request and a bad one for the far more common case, which is
//! the next thing the user wants to say to the agent they are already talking
//! to. The two are indistinguishable from the text, so the deck stops
//! guessing and asks — three routes, one keystroke each.
//!
//! Only genuinely ambiguous submissions raise the card. An explicit `>`, a
//! slash command, a `!` shell line, and the first prompt after a double-Esc
//! hold all carry their own intent already, and a card over intent the user
//! has stated is just a keystroke tax.

use super::*;

/// A submission parked at the routing card, with the text it will send once
/// the user picks a route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDispatch {
    pub text: String,
    /// The lane a sidecar would get (`req:2`), so the card can name it rather
    /// than describing it. Purely cosmetic — the driver assigns the real one.
    pub next_lane: String,
}

/// The routes the card offers. Each maps onto a `WorkspaceInput` the driver
/// already understands, so nothing new crosses the channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchRoute {
    /// Inject at the running turn's next step boundary (the `>` marker).
    Steer,
    /// Run as this agent's next turn, continuing the conversation.
    NextTurn,
    /// A parallel sidecar sub-session — the old unconditional behavior.
    Sidecar,
}

impl DispatchRoute {
    /// The input this route sends for `text`.
    ///
    /// `Steer` re-adds the `>` marker rather than inventing a message type:
    /// the driver's mid-turn arm already reads it, including the case where
    /// the turn finished while the card was up (it becomes the next turn
    /// instead of being dropped). `NextTurn` uses `EnqueueFront`, which the
    /// driver queues *without* draining to a lane — so the idle arm picks it
    /// up as the lead's next turn.
    pub fn input(self, text: String) -> WorkspaceInput {
        match self {
            Self::Steer => WorkspaceInput::Enqueue {
                text: format!("> {text}"),
            },
            Self::NextTurn => WorkspaceInput::EnqueueFront { text },
            Self::Sidecar => WorkspaceInput::Enqueue { text },
        }
    }
}

/// Whether `text` states its own routing and must not be second-guessed.
fn carries_its_own_intent(text: &str) -> bool {
    let head = text.trim_start();
    // `!` never reaches here (shell dispatch precedes it), but listing it
    // keeps the rule readable as "the three markers".
    head.starts_with('>') || head.starts_with('/') || head.starts_with('!')
}

/// Route one submission. `Some` is the action to take now; `None` means the
/// card was raised and the caller should treat the key as handled.
///
/// The running check is the focused agent's own status, so a prompt typed at
/// an idle agent — including one whose turn just finished — never sees the
/// card. That is the same boundary the driver enforces on its side
/// (`SteeringTap::is_settling`), which is what keeps the two layers agreeing
/// about what "still running" means.
pub fn route(ui: &mut DeckUi, model: &WorkspaceModel, text: String) -> Option<WorkspaceInput> {
    let running = model
        .agents
        .get(ui.focused)
        .is_some_and(|a| a.status == crate::AgentStatus::Running);
    if !running || carries_its_own_intent(&text) || ui.ask_before_spawn.not_asking() {
        return Some(WorkspaceInput::Enqueue { text });
    }
    let live = model
        .agents
        .iter()
        .filter(|a| a.meta.id.starts_with("req:"))
        .count();
    ui.pending_dispatch = Some(PendingDispatch {
        text,
        next_lane: format!("req:{}", live + 1),
    });
    None
}

/// The card's keys, checked before composer editing so the answer cannot be
/// typed into the prompt behind it.
///
/// Single letters commit here, unlike the scope card. The difference is that
/// the composer is *empty* while this card is up — its content is what the
/// card is holding — so there is no text field competing for `s`, and the
/// three routes are a modal choice in the way a scope note never was.
/// Esc returns the text to the composer rather than discarding it: the user
/// asked a question, and losing it to a dismissal would be the same theft in
/// a different costume.
pub fn handle_key(key: KeyEvent, ui: &mut DeckUi) -> Option<DeckAction> {
    let pending = ui.pending_dispatch.as_ref()?;
    let route = match key.code {
        KeyCode::Char('s') => DispatchRoute::Steer,
        KeyCode::Char('n') => DispatchRoute::NextTurn,
        KeyCode::Char('p') => DispatchRoute::Sidecar,
        KeyCode::Esc => {
            let text = pending.text.clone();
            ui.pending_dispatch = None;
            ui.composer.load(text);
            return Some(DeckAction::Handled);
        }
        // Anything else is swallowed: a card whose keys leaked into the
        // transcript behind it would be worse than no card.
        _ => return Some(DeckAction::Handled),
    };
    let text = pending.text.clone();
    ui.pending_dispatch = None;
    Some(DeckAction::Send(route.input(text)))
}

/// Whether the deck asks before forking a turn into a sidecar.
///
/// A preference rather than a constant because the answer is genuinely
/// personal: someone dispatching many independent requests at one agent wants
/// the old silent spawn, and someone in a long single-threaded collaboration
/// wants never to be forked at all. Both are reachable without editing code.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AskBeforeSpawn {
    /// Raise the card (the default — the deck does not guess).
    #[default]
    Ask,
    /// Never ask; a mid-turn prompt always forks, as it did before.
    AlwaysSpawn,
}

impl AskBeforeSpawn {
    fn not_asking(self) -> bool {
        matches!(self, Self::AlwaysSpawn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{AgentMeta, Inbound};

    fn route_of(text: &str) -> WorkspaceInput {
        DispatchRoute::Steer.input(text.to_string())
    }

    fn model_with_lead(status: crate::AgentStatus) -> WorkspaceModel {
        let mut m = WorkspaceModel::new();
        m.apply_inbound(&Inbound::Register(AgentMeta::new("lead", "goal", 0)));
        m.apply_inbound(&Inbound::Status {
            agent: "lead".into(),
            status,
        });
        m
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// The whole point: a prompt typed at a running agent parks on the card
    /// instead of silently becoming someone else's problem.
    #[test]
    fn a_prompt_at_a_running_agent_raises_the_card_and_sends_nothing_yet() {
        let model = model_with_lead(crate::AgentStatus::Running);
        let mut ui = DeckUi::default();
        assert_eq!(route(&mut ui, &model, "add the tests".into()), None);
        assert_eq!(
            ui.pending_dispatch.as_ref().map(|p| p.text.as_str()),
            Some("add the tests"),
            "the card holds the user's words verbatim"
        );
    }

    /// …and at rest it does not. An idle agent's next prompt is just its next
    /// turn, which is the case the driver's settling latch also covers.
    #[test]
    fn a_prompt_at_an_idle_agent_goes_straight_through() {
        let model = model_with_lead(crate::AgentStatus::WaitingInput);
        let mut ui = DeckUi::default();
        assert_eq!(
            route(&mut ui, &model, "add the tests".into()),
            Some(WorkspaceInput::Enqueue {
                text: "add the tests".into()
            })
        );
        assert!(ui.pending_dispatch.is_none(), "no card at rest");
    }

    /// Each key resolves to the route it advertises, and clears the card.
    #[test]
    fn the_card_keys_resolve_to_their_advertised_routes() {
        let cases = [
            (
                KeyCode::Char('s'),
                WorkspaceInput::Enqueue {
                    text: "> go".into(),
                },
            ),
            (
                KeyCode::Char('n'),
                WorkspaceInput::EnqueueFront { text: "go".into() },
            ),
            (
                KeyCode::Char('p'),
                WorkspaceInput::Enqueue { text: "go".into() },
            ),
        ];
        for (code, expected) in cases {
            let model = model_with_lead(crate::AgentStatus::Running);
            let mut ui = DeckUi::default();
            route(&mut ui, &model, "go".into());
            assert_eq!(
                handle_key(key(code), &mut ui),
                Some(DeckAction::Send(expected)),
                "{code:?}"
            );
            assert!(ui.pending_dispatch.is_none(), "{code:?} clears the card");
        }
    }

    /// Esc is not a discard. The text goes back where the user can see and
    /// edit it — losing a prompt to a dismissal would be the same theft the
    /// silent spawn committed, just quieter.
    #[test]
    fn esc_returns_the_text_to_the_composer_instead_of_dropping_it() {
        let model = model_with_lead(crate::AgentStatus::Running);
        let mut ui = DeckUi::default();
        route(&mut ui, &model, "a careful question".into());
        assert_eq!(
            handle_key(key(KeyCode::Esc), &mut ui),
            Some(DeckAction::Handled)
        );
        assert!(ui.pending_dispatch.is_none());
        assert_eq!(ui.composer.buffer(), "a careful question");
    }

    /// While the card is up it owns every key — a stray letter must not fall
    /// through into the composer hiding behind it.
    #[test]
    fn an_unrelated_key_is_swallowed_and_leaves_the_card_up() {
        let model = model_with_lead(crate::AgentStatus::Running);
        let mut ui = DeckUi::default();
        route(&mut ui, &model, "held".into());
        assert_eq!(
            handle_key(key(KeyCode::Char('z')), &mut ui),
            Some(DeckAction::Handled)
        );
        assert!(ui.pending_dispatch.is_some(), "the card stays up");
        assert_eq!(ui.composer.buffer(), "", "nothing leaked into the composer");
    }

    /// With the preference off, the deck behaves exactly as it did before.
    #[test]
    fn always_spawn_restores_the_old_silent_fork() {
        let model = model_with_lead(crate::AgentStatus::Running);
        let mut ui = DeckUi {
            ask_before_spawn: AskBeforeSpawn::AlwaysSpawn,
            ..Default::default()
        };
        assert_eq!(
            route(&mut ui, &model, "go".into()),
            Some(WorkspaceInput::Enqueue { text: "go".into() })
        );
        assert!(ui.pending_dispatch.is_none());
    }

    /// Steering re-adds the marker the driver already parses, rather than
    /// adding a message type both sides would have to learn.
    #[test]
    fn steering_sends_the_marker_the_driver_already_reads() {
        assert_eq!(
            route_of("only the parser"),
            WorkspaceInput::Enqueue {
                text: "> only the parser".into()
            }
        );
    }

    /// `EnqueueFront` is what the driver queues without draining to a lane,
    /// so it is how "run this next, in this conversation" is expressed.
    #[test]
    fn continuing_the_thread_front_queues_without_spawning() {
        assert_eq!(
            DispatchRoute::NextTurn.input("add the tests".into()),
            WorkspaceInput::EnqueueFront {
                text: "add the tests".into()
            }
        );
    }

    #[test]
    fn a_sidecar_is_the_plain_enqueue_the_driver_drains_to_a_lane() {
        assert_eq!(
            DispatchRoute::Sidecar.input("unrelated".into()),
            WorkspaceInput::Enqueue {
                text: "unrelated".into()
            }
        );
    }

    /// A prompt that already says where it goes is not asked about again.
    #[test]
    fn stated_intent_is_never_second_guessed() {
        for text in ["> steer me", "/help", "!ls", "  > padded"] {
            assert!(
                carries_its_own_intent(text),
                "{text:?} states its own route"
            );
        }
        for text in ["and now the tests", "what about x?", "a > b comparison"] {
            assert!(
                !carries_its_own_intent(text),
                "{text:?} is ambiguous and must raise the card"
            );
        }
    }
}
