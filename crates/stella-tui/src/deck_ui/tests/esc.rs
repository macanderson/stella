//! The Esc precedence ladder: who claims the keystroke before it can reach the
//! turn-stop rules, what a single press does with a draft or a queue waiting
//! (steer, not cancel), and the double-press escalation to stop-and-hold.

use super::*;

/// A one-agent model whose lead is mid-turn (`Running`).
fn running_model() -> WorkspaceModel {
    let mut m = model_with(&["lead"]);
    m.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Text {
            text: "working".into(),
        },
    });
    m
}

/// The single-Esc outcome: a clean stop for the lead's in-flight turn.
fn stop_lead() -> DeckAction {
    DeckAction::Send(WorkspaceInput::Control {
        agent: "lead".into(),
        control: AgentControl::Stop,
    })
}

/// Esc at a sub-agent lane the user opened from the overlay steers THAT lane
/// with the draft alone: the queue is the lead's backlog and stays where it
/// is. With no draft it is the lane's stop.
#[test]
fn esc_at_an_opened_lane_steers_the_draft_into_that_lane_and_leaves_the_queue() {
    let mut model = running_model();
    model.apply_inbound(&Inbound::Register(
        AgentMeta::new("sub:2", "task 2", 0).with_role("subagent"),
    ));
    model.apply_inbound(&Inbound::Status {
        agent: "sub:2".into(),
        status: AgentStatus::Running,
    });
    model.queue.enqueue("queued for the lead".to_string(), 0);
    let mut ui = ready_ui();
    ui.focus_agent(1);
    ui.composer.load("use the parser".to_string());
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Steer {
            agent: "sub:2".into(),
            texts: vec!["use the parser".into()],
        })
    );
    assert_eq!(model.queue.pending(), 1, "the lead's backlog is untouched");
    let mut ui = ready_ui();
    ui.focus_agent(1);
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Control {
            agent: "sub:2".into(),
            control: AgentControl::Stop,
        }),
        "the queue alone is not something to say to a lane"
    );
}

#[test]
fn esc_stops_a_running_turn_and_arms_the_double_press() {
    let model = running_model();
    let mut ui = ready_ui();
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        stop_lead()
    );
    assert!(
        ui.esc_armed_at.is_some(),
        "the stop arms the double-Esc window"
    );
}

#[test]
fn esc_with_no_turn_running_stays_inert() {
    let model = model_with(&["lead"]); // Queued — nothing in flight
    let mut ui = ready_ui();
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Ignored,
        "an idle Esc must not send a stray stop"
    );
    assert!(ui.esc_armed_at.is_none());
}

/// **The steering witness.** Esc with something typed *delivers* it into the
/// running turn and lets the turn continue.
///
/// Esc used to cancel unconditionally, which on the staged pipeline is not an
/// interrupt but a restart: the driver truncates the partial turn and
/// re-dispatches the backlog as a new one, re-entering at triage and context
/// recall. So a user who typed one correcting sentence and pressed Esc paid
/// for the whole turn again to deliver it — and the sentence itself stayed
/// stranded in the composer while the turn it was written for died.
#[test]
fn esc_with_a_typed_draft_steers_the_running_turn_instead_of_cancelling_it() {
    let model = running_model();
    let mut ui = ready_ui();
    for c in "use the existing helper".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Steer {
            agent: "lead".into(),
            texts: vec!["use the existing helper".into()],
        }),
        "the draft is delivered to the turn in flight, not thrown away with it"
    );
    assert_eq!(
        ui.composer.buffer(),
        "",
        "the draft was DELIVERED, so it must not also survive as a next-turn \
         prompt — the same words dispatched twice is its own defect"
    );
    // The double-Esc escalation is untouched: a full stop is still one more
    // press away, which is what keeps "I want it to stop" reachable.
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::StopAndHold {
            agent: "lead".into()
        })
    );
}

/// The queue half: every waiting prompt goes in, in order, in one message —
/// which is the behaviour the request described ("whatever is in my prompt
/// queue, even if there is more than one, gets sent in and the agent keeps
/// working").
///
/// One message rather than N, deliberately: draining the queue as separate
/// inputs would let a second Esc land between two of them and cancel the turn
/// holding the rest.
#[test]
fn esc_delivers_the_whole_prompt_queue_in_order() {
    let mut model = running_model();
    model.queue.enqueue("first correction".into(), 0);
    model.queue.enqueue("> second correction".into(), 1);
    let mut ui = ready_ui();
    for c in "and the draft".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Steer {
            agent: "lead".into(),
            texts: vec![
                "first correction".into(),
                // The `>` marker is stripped: it already means "steer", and
                // this whole path is steering.
                "second correction".into(),
                // The draft is newest, so it is read last.
                "and the draft".into(),
            ],
        })
    );
}

/// **The rhythm this exists for**, end to end on the deck's side: type a
/// prompt at a running agent and press ⏎ — it queues for the lead (no routing
/// card, no sidecar lane); type another, ⏎ — it queues behind the first; then
/// Esc delivers both into the running turn, in order, as one steer. The turn
/// never completes or cancels on the way: nothing here is a `Stop`.
///
/// Before this the deck raised a card on the first ⏎ and the second test
/// keystrokes would have been swallowed by it, so this fails on the old code
/// at the first assertion.
#[test]
fn a_mid_turn_enter_queues_for_the_lead_and_the_next_esc_steers_the_queue() {
    let mut model = running_model();
    let mut ui = ready_ui();
    for (i, prompt) in ["first correction", "second correction"].iter().enumerate() {
        for c in prompt.chars() {
            handle_deck_key(ch(c), &model, &mut ui);
        }
        let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
        assert_eq!(
            action,
            DeckAction::Send(WorkspaceInput::EnqueueNext {
                text: (*prompt).into()
            }),
            "⏎ at a running agent queues for the lead, with no card raised"
        );
        assert!(ui.pending_dispatch.is_none());
        // The shell mirrors every queue input into the model the instant it
        // is sent (`deck_shell`'s one mutation site); do the same here.
        model.queue.enqueue((*prompt).into(), i as u64);
    }
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Steer {
            agent: "lead".into(),
            texts: vec!["first correction".into(), "second correction".into()],
        }),
        "Esc steers the whole backlog into the running turn, in order"
    );
}

/// And the case where Esc still means stop: nothing typed, nothing queued.
/// With no guidance to deliver, an interrupt is the only thing the keystroke
/// can mean — so the escape hatch never moved.
#[test]
fn esc_with_nothing_to_say_is_still_a_plain_interrupt() {
    let model = running_model();
    let mut ui = ready_ui();
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        stop_lead()
    );
}

#[test]
fn double_esc_inside_the_window_escalates_to_stop_and_hold() {
    let model = running_model();
    let mut ui = ready_ui();
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        stop_lead()
    );
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::StopAndHold {
            agent: "lead".into()
        })
    );
    assert!(
        ui.dispatch_held,
        "the deck now front-inserts its next submission"
    );
    assert!(
        ui.esc_armed_at.is_none(),
        "the pair resets after escalating"
    );
}

#[test]
fn the_second_esc_fires_even_if_the_cancel_already_folded() {
    // Between the two presses the first cancel's error event may fold
    // (status `Failed`) before the auto-dispatched next prompt produces
    // any event — the escalation must not be lost to that gap.
    let mut model = running_model();
    let mut ui = ready_ui();
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        stop_lead()
    );
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Error {
            message: "turn stopped by user".into(),
            retryable: false,
        },
    });
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::StopAndHold {
            agent: "lead".into()
        })
    );
}

#[test]
fn an_intervening_key_breaks_the_double_esc_pair() {
    let model = running_model();
    let mut ui = ready_ui();
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    handle_deck_key(ch('x'), &model, &mut ui); // types into the composer
    assert!(ui.esc_armed_at.is_none(), "any other key disarms");
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Steer {
            agent: "lead".into(),
            texts: vec!["x".into()],
        }),
        "the next Esc is a fresh single press, not an escalation — and with a \
         draft in the composer a single press steers"
    );
}

#[test]
fn a_stale_arm_outside_the_window_does_not_escalate() {
    let model = running_model();
    let mut ui = ready_ui();
    // Backdate the arm past the window. (If the monotonic clock is too
    // young to backdate, `checked_sub` leaves it unarmed — which expects
    // the same single-stop outcome.)
    ui.esc_armed_at = std::time::Instant::now()
        .checked_sub(ESC_DOUBLE_WINDOW + std::time::Duration::from_secs(1));
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        stop_lead(),
        "past the window, Esc is a single stop again"
    );
}

#[test]
fn esc_dismisses_the_slash_popup_instead_of_stopping_the_turn() {
    let model = running_model();
    let mut ui = ready_ui();
    ui.slash_commands = vec![SlashCommand::new("/help", "help")];
    handle_deck_key(ch('/'), &model, &mut ui);
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Handled,
        "rule 4: the popup claims Esc — no stop is sent"
    );
    assert!(ui.esc_armed_at.is_none(), "a claimed Esc never arms");
}

#[test]
fn an_esc_claimed_by_the_queue_editor_breaks_the_pair_too() {
    let mut model = model_with_queue(&["one"]);
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Text { text: "hi".into() },
    });
    let mut ui = ready_ui();
    ui.queue_open = true;
    ui.esc_armed_at = Some(std::time::Instant::now());
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Handled,
        "rule 3: the queue editor claims Esc — no stop is sent"
    );
    assert!(!ui.queue_open, "…it closed the editor");
    assert!(ui.esc_armed_at.is_none(), "…and broke the double-Esc pair");
}

#[test]
fn esc_closes_an_open_diff_before_it_stops_the_turn() {
    let model = running_model();
    let mut ui = ready_ui();
    ui.tab = DeckTab::Files;
    ui.files_diff_open = true;
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Handled,
        "rule 6: the diff overlay claims Esc"
    );
    assert!(!ui.files_diff_open);
}

#[test]
fn esc_clears_a_session_highlight_before_it_stops_the_turn() {
    let mut model = running_model();
    with_tool_exchange(&mut model, "lead");
    let mut ui = ready_ui();
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    assert!(ui.session_selected.is_some());
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Handled,
        "rule 7: the highlight claims Esc first"
    );
    assert_eq!(ui.session_selected, None);
    // With nothing left to claim it, the NEXT Esc stops the turn (and
    // since the claimed Esc broke the pair, it is a single stop).
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        stop_lead()
    );
}
