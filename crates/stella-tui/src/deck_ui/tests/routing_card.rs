//! The two things that seize the keyboard ahead of the composer while a turn
//! is in flight: the `ask_user` quick-pick and the mid-turn routing card — and
//! in particular how each one lets go again.

use super::*;

#[test]
fn ask_user_answer_latches_until_a_fresh_question_rearms() {
    let ask = Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::AskUser {
            id: "call_ask_1".into(),
            question: "which db?".into(),
            options: vec!["postgres".into(), "sqlite".into()],
        },
    };
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&ask, &mut model, &mut ui);

    assert_eq!(
        handle_deck_key(ch('1'), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::ToAgent {
            agent: "lead".into(),
            input: UserInput::AskUserAnswer {
                id: "call_ask_1".into(),
                answer: "postgres".into(),
            },
        })
    );
    // The question stays pending until its ToolResult lands; a second digit
    // must type into the composer, not answer the question again.
    assert!(model.agents[0].model.pending_ask_user.is_some());
    handle_deck_key(ch('2'), &model, &mut ui);
    assert_eq!(
        ui.composer.buffer(),
        "2",
        "second press types, never re-sends"
    );

    // A FRESH question re-arms the quick-pick.
    handle_deck_key(key(KeyCode::Backspace), &model, &mut ui);
    ingest_inbound(&ask, &mut model, &mut ui);
    assert_eq!(
        handle_deck_key(ch('2'), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::ToAgent {
            agent: "lead".into(),
            input: UserInput::AskUserAnswer {
                id: "call_ask_1".into(),
                answer: "sqlite".into(),
            },
        }),
        "a new question re-arms the quick-pick"
    );
}

/// The routing card owns every key ahead of the composer once raised — but
/// it used to have no way out if the turn it was asking about finished (or
/// errored, or itself started asking a question) before the user answered
/// `s`/`n`/`p`/Esc: every further keystroke, including Enter and Backspace,
/// died silently at the card's catch-all. This is the "can't clear or submit
/// anything after a turn completes" bug — the fix releases the card the same
/// way Esc does the moment its agent stops being `Running`.
#[test]
fn a_finished_turn_releases_a_routing_card_stuck_on_its_prompt() {
    let mut model = model_with(&["lead"]);
    model.apply_inbound(&Inbound::Status {
        agent: "lead".into(),
        status: crate::AgentStatus::Running,
    });
    let mut ui = ready_ui();

    for c in "and now the tests".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Handled,
        "an ambiguous mid-turn prompt raises the card instead of sending"
    );
    assert!(ui.pending_dispatch.is_some(), "the card is up");

    ingest_inbound(
        &Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::RunComplete {
                model: "test-model".into(),
                cost_usd: 0.0,
            },
        },
        &mut model,
        &mut ui,
    );

    assert!(
        ui.pending_dispatch.is_none(),
        "a finished turn releases the card rather than leaving it stuck"
    );
    assert_eq!(
        ui.composer.buffer(),
        "and now the tests",
        "the held prompt returns to the composer, same as Esc"
    );

    // The keyboard is unstuck: the same text submits normally now, since
    // `dispatch::route`'s own `running` check is false for an idle agent.
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Enqueue {
            text: "and now the tests".into()
        })
    );
}

/// The release is scoped to the agent the card was raised against — another
/// agent finishing its own turn must not discard a card still legitimately
/// waiting on the user.
#[test]
fn a_finished_turn_on_another_agent_does_not_release_someone_elses_card() {
    let mut model = model_with(&["lead"]);
    model.apply_inbound(&Inbound::Status {
        agent: "lead".into(),
        status: crate::AgentStatus::Running,
    });
    let mut ui = ready_ui();

    for c in "keep me".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert!(ui.pending_dispatch.is_some());

    ingest_inbound(
        &Inbound::Event {
            agent: "req:2".into(),
            event: AgentEvent::TurnComplete {
                model: "test-model".into(),
                cost_usd: 0.0,
            },
        },
        &mut model,
        &mut ui,
    );

    assert!(
        ui.pending_dispatch.is_some(),
        "a different agent's completion must not release this card"
    );
}
