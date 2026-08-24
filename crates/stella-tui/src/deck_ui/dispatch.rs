//! Where a prompt submitted while a turn is running should go.
//!
//! The deck used to answer this silently: any mid-turn prompt without a `>`
//! marker became a sidecar sub-session. That is a defensible default for a
//! *concurrent* request and a bad one for the far more common case, which is
//! the next thing the user wants to say to the agent they are already talking
//! to. So the default is now [`MidTurnPrompt::Queue`]: the prompt waits in the
//! backlog as the lead's next turn, and Esc delivers the whole backlog into
//! the running turn (`deck_ui::steer`) — type, type, type, Esc, and the agent
//! keeps working with everything you said. Nothing completes, nothing is
//! cancelled, no stranger lane starts.
//!
//! The other two answers stay reachable through `ui.mid_turn_prompt`:
//! [`MidTurnPrompt::Ask`] raises a routing card (three routes, one keystroke
//! each), and [`MidTurnPrompt::AlwaysSpawn`] is the old silent fork.
//!
//! Whatever the policy, a submission that states its own routing is never
//! second-guessed: an explicit `>`, a slash command, a `!` shell line, and
//! the first prompt after a double-Esc hold all carry their own intent.

use super::*;

/// A submission parked at the routing card, with the text it will send once
/// the user picks a route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDispatch {
    pub text: String,
    /// The lane a sidecar would get (`req:2`), so the card can name it rather
    /// than describing it. Purely cosmetic — the driver assigns the real one.
    pub next_lane: String,
    /// The agent whose `Running` status raised this card, so the deck can
    /// tell when that turn ends out from under it (see
    /// `deck_ui::ingest_inbound`'s auto-release of a stale card).
    pub agent_id: String,
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

/// The queue-free command route: a submission whose head is a slash command
/// the vocabulary declares [`crate::composer::SlashCommand::sideband`] leaves
/// as [`WorkspaceInput::Command`] — executed at once beside the prompt queue,
/// mid-turn included, never listed as a pending prompt. `None` for everything
/// else: prose, custom (⚡) commands (they expand into prompts driver-side),
/// and the turn-coupled builtins (`/clear`, `/init`, `/reload`, …), which all
/// keep their queue behavior.
pub(super) fn sideband(ui: &DeckUi, text: &str) -> Option<WorkspaceInput> {
    let head = text.split_whitespace().next()?;
    if !head.starts_with('/') {
        return None;
    }
    ui.slash_commands
        .iter()
        .find(|c| c.name == head && c.sideband)
        .map(|_| WorkspaceInput::Command {
            text: text.trim().to_string(),
        })
}

/// Whether `text` states its own routing and must not be second-guessed.
fn carries_its_own_intent(text: &str) -> bool {
    let head = text.trim_start();
    // `!` never reaches here (shell dispatch precedes it), but listing it
    // keeps the rule readable as "the three markers".
    head.starts_with('>') || head.starts_with('/') || head.starts_with('!')
}

/// A lead-bound prompt typed at a finished lane, with the lane named in
/// front: `[about sub:2 — Simplify the crate READMEs · failed] why?`. The
/// status is in the bracket because it is the fact the question is usually
/// about, and the lead's own view of the lane is two transcript rows old.
pub fn about_lane(lane: &crate::deck::AgentEntry, text: &str) -> String {
    format!(
        "[about {} — {} · {}] {}",
        lane.meta.id,
        crate::v2::subagents::purpose(&lane.meta),
        lane.status.label(),
        text.trim()
    )
}

/// Route one submission. `Some` is the action to take now; `None` means the
/// card was raised and the caller should treat the key as handled.
///
/// The running check is the focused agent's own status, so a prompt typed at
/// an idle agent — including one whose turn just finished — never queues or
/// sees the card: it is simply the next turn. That is the same boundary the
/// driver enforces on its side (`SteeringTap::is_settling`), which is what
/// keeps the two layers agreeing about what "still running" means.
///
/// A prompt typed at a **live sub-agent lane** — the user opened it from
/// the SUB-AGENTS overlay — is a steer at that lane, sent as one, whatever
/// the policy: queueing it for the lead, the card's other routes, and a
/// sidecar are all things a lane cannot do. A paused lane takes the steer
/// too: its tap drains at the first boundary after it resumes.
///
/// At a **finished** lane there is no turn to steer, so the words go to the
/// lead as its next prompt — with the lane named in front of them
/// ([`about_lane`]), because the lead cannot see what the deck is looking at
/// and "why did this fail?" is a different question about every lane. The
/// prefix is visible in the transcript, never a hidden rewrite.
pub fn route(ui: &mut DeckUi, model: &WorkspaceModel, text: String) -> Option<WorkspaceInput> {
    let focused = model.agents.get(ui.focused);
    if let Some(lane) = focused.filter(|a| a.is_subagent()) {
        if lane.status.is_active() || lane.status == crate::AgentStatus::Paused {
            let text = text.trim_start().trim_start_matches('>').trim().to_string();
            return Some(WorkspaceInput::Steer {
                agent: lane.meta.id.clone(),
                texts: vec![text],
            });
        }
        // The lead's state decides the route for the lead-bound prompt, so a
        // reader parked on a finished lane while the lead works gets the same
        // queue / card / sidecar policy they would get at the lead.
        let text = about_lane(lane, &text);
        let lead_running = model
            .parent_of(ui.focused)
            .and_then(|p| model.agents.get(p))
            .is_some_and(|a| a.status == crate::AgentStatus::Running);
        if !lead_running || carries_its_own_intent(&text) {
            return Some(WorkspaceInput::Enqueue { text });
        }
        return Some(WorkspaceInput::EnqueueNext { text });
    }
    let running = focused.is_some_and(|a| a.status == crate::AgentStatus::Running);
    if !running || carries_its_own_intent(&text) {
        return Some(WorkspaceInput::Enqueue { text });
    }
    match ui.mid_turn_prompt {
        MidTurnPrompt::Queue => return Some(WorkspaceInput::EnqueueNext { text }),
        MidTurnPrompt::AlwaysSpawn => return Some(WorkspaceInput::Enqueue { text }),
        MidTurnPrompt::Ask => {}
    }
    let live = model
        .agents
        .iter()
        .filter(|a| a.meta.id.starts_with("req:"))
        .count();
    ui.pending_dispatch = Some(PendingDispatch {
        text,
        next_lane: format!("req:{}", live + 1),
        // `running` is only true when `focused` is `Some`.
        agent_id: focused.expect("running agent is present").meta.id.clone(),
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
    // Quit must win over the card's modality: the caller's Ctrl-C branch runs
    // AFTER this function, so claiming Ctrl-C in the catch-all made the deck
    // unquittable while a card was up.
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
        return None;
    }
    let route = match key.code {
        // Bare letters only: `ctrl+n`/`ctrl+p` are transcript-nav chords
        // everywhere else on this tab, and a modified key must not commit the
        // held words down a route the user never chose.
        KeyCode::Char('s') if key.modifiers.is_empty() => DispatchRoute::Steer,
        KeyCode::Char('n') if key.modifiers.is_empty() => DispatchRoute::NextTurn,
        KeyCode::Char('p') if key.modifiers.is_empty() => DispatchRoute::Sidecar,
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

/// Release a card raised against `agent` if `event` just ended that agent's
/// turn — a `Complete`, a hard (non-retryable) `Error`, or a fresh
/// `AskUser`/`ScopeReview` gate, the same set the model fold's own
/// `status_from_event` maps away from [`crate::AgentStatus::Running`]. A
/// no-op unless the card is up and belongs to this agent.
///
/// Without this the card is sticky forever once raised: it owns every key
/// ahead of the composer ([`handle_key`], checked first in
/// `deck_ui::handle_deck_key`), and only `s`/`n`/`p`/Esc clear it. If the
/// turn it was asking about finishes before the user answers, every further
/// keystroke — Enter, Backspace, anything typed — dies silently at the
/// catch-all, and the deck reads as though it stopped accepting input.
/// Released exactly like Esc: the text goes back to the composer, nothing is
/// sent, and the very next keystroke reaches it instead of the card.
pub fn release_if_settled(ui: &mut DeckUi, agent: &str, event: &stella_protocol::AgentEvent) {
    use stella_protocol::AgentEvent;
    let vacates_running = matches!(
        event,
        AgentEvent::TurnComplete { .. }
            // The run ending settles the agent too — and it is the event that
            // settles it for a wrapped run, whose turns keep ending (#3379).
            | AgentEvent::RunComplete { .. }
            | AgentEvent::Error {
                retryable: false,
                ..
            }
            | AgentEvent::AskUser { .. }
            | AgentEvent::ScopeReview { .. }
    );
    if !vacates_running {
        return;
    }
    let Some(pending) = ui.pending_dispatch.as_ref() else {
        return;
    };
    if pending.agent_id != agent {
        return;
    }
    let text = pending.text.clone();
    ui.pending_dispatch = None;
    ui.composer.load(text);
}

/// What a plain prompt submitted at a running agent does — the
/// `ui.mid_turn_prompt` setting.
///
/// A preference rather than a constant because the answer is genuinely
/// personal: someone in a long single-threaded collaboration wants the
/// backlog-then-Esc rhythm and never to be forked; someone dispatching many
/// independent requests at one agent wants the old silent spawn; someone who
/// does both wants to be asked. All three are reachable from settings
/// (`"queue"` / `"ask"` / `"spawn"`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MidTurnPrompt {
    /// Wait in the backlog as the lead's next turn; Esc steers the backlog
    /// into the running turn. The default.
    #[default]
    Queue,
    /// Raise the routing card (`s` steer / `n` next / `p` sidecar).
    Ask,
    /// Never ask; a mid-turn prompt always forks to a sidecar lane.
    AlwaysSpawn,
}

impl MidTurnPrompt {
    /// Parse the settings slug. `None` for an unrecognised value, so the
    /// reader keeps the default rather than failing the file.
    pub fn parse(slug: &str) -> Option<Self> {
        match slug.trim() {
            "queue" => Some(Self::Queue),
            "ask" => Some(Self::Ask),
            "spawn" => Some(Self::AlwaysSpawn),
            _ => None,
        }
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

    /// A deck under the routing-card policy.
    fn asking_ui() -> DeckUi {
        DeckUi {
            mid_turn_prompt: MidTurnPrompt::Ask,
            ..Default::default()
        }
    }

    /// **The witness for the default.** A plain prompt typed at a running
    /// agent waits in the backlog as the lead's next turn — no card, no
    /// sidecar lane — which is exactly what an Esc then steers into the turn.
    #[test]
    fn by_default_a_prompt_at_a_running_agent_queues_for_the_lead() {
        let model = model_with_lead(crate::AgentStatus::Running);
        let mut ui = DeckUi::default();
        assert_eq!(ui.mid_turn_prompt, MidTurnPrompt::Queue);
        assert_eq!(
            route(&mut ui, &model, "add the tests".into()),
            Some(WorkspaceInput::EnqueueNext {
                text: "add the tests".into()
            })
        );
        assert!(ui.pending_dispatch.is_none(), "no card under the default");
    }

    /// The settings slugs, and that an unknown one keeps the default.
    #[test]
    fn the_policy_parses_its_three_slugs_and_nothing_else() {
        assert_eq!(MidTurnPrompt::parse("queue"), Some(MidTurnPrompt::Queue));
        assert_eq!(MidTurnPrompt::parse(" ask "), Some(MidTurnPrompt::Ask));
        assert_eq!(
            MidTurnPrompt::parse("spawn"),
            Some(MidTurnPrompt::AlwaysSpawn)
        );
        assert_eq!(MidTurnPrompt::parse("sidecar"), None);
    }

    /// Under `ask`, a prompt typed at a running agent parks on the card
    /// instead of silently becoming someone else's problem.
    #[test]
    fn under_ask_a_prompt_at_a_running_agent_raises_the_card_and_sends_nothing_yet() {
        let model = model_with_lead(crate::AgentStatus::Running);
        let mut ui = asking_ui();
        assert_eq!(route(&mut ui, &model, "add the tests".into()), None);
        assert_eq!(
            ui.pending_dispatch.as_ref().map(|p| p.text.as_str()),
            Some("add the tests"),
            "the card holds the user's words verbatim"
        );
    }

    /// **The witness for steering an opened lane.** A prompt typed at a
    /// running sub-agent lane is a steer at that lane — no card, no `>`
    /// needed, marker stripped if typed — because the card's other routes
    /// are the lead's and a lane has none of them.
    #[test]
    fn a_prompt_at_a_running_lane_is_a_steer_at_that_lane() {
        let mut model = model_with_lead(crate::AgentStatus::WaitingInput);
        model.apply_inbound(&Inbound::Register(
            AgentMeta::new("sub:2", "task 2", 0).with_role("subagent"),
        ));
        model.apply_inbound(&Inbound::Status {
            agent: "sub:2".into(),
            status: crate::AgentStatus::Running,
        });
        let mut ui = DeckUi::default();
        ui.focus_agent(1);
        assert_eq!(
            route(&mut ui, &model, "> narrow it to the parser".into()),
            Some(WorkspaceInput::Steer {
                agent: "sub:2".into(),
                texts: vec!["narrow it to the parser".into()],
            })
        );
        assert!(ui.pending_dispatch.is_none(), "no card at a lane");
    }

    /// **The witness for a prompt at a finished lane.** There is no turn to
    /// steer, so the words go to the lead with the lane named in front —
    /// queued behind the lead's running turn, never to a sidecar and never
    /// behind a card. A paused lane still takes a steer.
    #[test]
    fn a_prompt_at_a_finished_lane_asks_the_lead_about_it() {
        let mut model = model_with_lead(crate::AgentStatus::Running);
        model.apply_inbound(&Inbound::Register(
            AgentMeta::new("sub:2", "task 2", 0)
                .with_role("subagent")
                .with_purpose("Simplify the crate READMEs")
                .with_parent("lead"),
        ));
        model.apply_inbound(&Inbound::Status {
            agent: "sub:2".into(),
            status: crate::AgentStatus::Failed,
        });
        let mut ui = DeckUi {
            mid_turn_prompt: MidTurnPrompt::Ask,
            ..Default::default()
        };
        ui.focus_agent(1);
        assert_eq!(
            route(&mut ui, &model, "why did it fail?".into()),
            Some(WorkspaceInput::EnqueueNext {
                text: "[about sub:2 — Simplify the crate READMEs · failed] why did it fail?".into(),
            }),
            "named, and queued as the lead's next turn"
        );
        assert!(
            ui.pending_dispatch.is_none(),
            "no card: a lane cannot answer one"
        );

        model.apply_inbound(&Inbound::Status {
            agent: "lead".into(),
            status: crate::AgentStatus::Done,
        });
        assert!(
            matches!(
                route(&mut ui, &model, "and now?".into()),
                Some(WorkspaceInput::Enqueue { .. })
            ),
            "an idle lead takes it as its next prompt"
        );

        let mut paused = model_with_lead(crate::AgentStatus::Running);
        paused.apply_inbound(&Inbound::Register(
            AgentMeta::new("sub:3", "task 3", 0).with_role("subagent"),
        ));
        paused.apply_inbound(&Inbound::Status {
            agent: "sub:3".into(),
            status: crate::AgentStatus::Paused,
        });
        ui.focus_agent(1);
        assert!(
            matches!(
                route(&mut ui, &paused, "try the other parser".into()),
                Some(WorkspaceInput::Steer { agent, .. }) if agent == "sub:3"
            ),
            "a paused lane drains the steer when it resumes"
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
            let mut ui = asking_ui();
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
        let mut ui = asking_ui();
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
        let mut ui = asking_ui();
        route(&mut ui, &model, "held".into());
        assert_eq!(
            handle_key(key(KeyCode::Char('z')), &mut ui),
            Some(DeckAction::Handled)
        );
        assert!(ui.pending_dispatch.is_some(), "the card stays up");
        assert_eq!(ui.composer.buffer(), "", "nothing leaked into the composer");
    }

    /// Under `spawn`, the deck behaves exactly as it originally did.
    #[test]
    fn always_spawn_restores_the_old_silent_fork() {
        let model = model_with_lead(crate::AgentStatus::Running);
        let mut ui = DeckUi {
            mid_turn_prompt: MidTurnPrompt::AlwaysSpawn,
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
