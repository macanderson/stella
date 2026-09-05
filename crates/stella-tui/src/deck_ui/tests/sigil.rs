//! The composer marks: `$` runs a shell command, `!` interrupts the turn, and
//! `!!`/`!!!` interrupt it and keep the words.

use super::*;

/// Type `text` into the composer and submit it.
fn submit(text: &str, model: &WorkspaceModel, ui: &mut DeckUi) -> DeckAction {
    for c in text.chars() {
        handle_deck_key(ch(c), model, ui);
    }
    handle_deck_key(key(KeyCode::Enter), model, ui)
}

fn lead(status: crate::AgentStatus) -> WorkspaceModel {
    let mut m = model_with(&["lead"]);
    m.apply_inbound(&Inbound::Status {
        agent: "lead".into(),
        status,
    });
    m
}

/// **The witness for the bang mark.** A turn is running. The user says
/// `! use short sentences`. The deck sends the interrupt. That is the message
/// the red composer mode sends too, so the stop, the front-insert and the
/// resume run on one driver path
/// (`command_deck::steer::interrupt_lead`, whose own witness pins the order).
///
/// It fails on the old deck by construction. That deck read any leading `!`
/// as a shell mark. It answered `DeckAction::Shell("use short sentences")`
/// and ran the sentence as a command.
#[test]
fn a_bang_at_a_running_lead_interrupts_the_turn_with_the_text() {
    let model = lead(crate::AgentStatus::Running);
    let mut ui = ready_ui();
    assert_eq!(
        submit("! use short sentences", &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Interrupt {
            agent: "lead".into(),
            texts: vec!["use short sentences".into()],
            keep: None,
        })
    );
}

/// With no turn to stop, the words are just the next prompt. The deck says
/// why. Muscle memory must never eat a message.
#[test]
fn a_bang_at_an_idle_lead_dispatches_the_text_and_says_so() {
    let model = lead(crate::AgentStatus::WaitingInput);
    let mut ui = ready_ui();
    assert_eq!(
        submit("! use short sentences", &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Enqueue {
            text: "use short sentences".into()
        })
    );
    assert!(
        ui.notice
            .entries()
            .iter()
            .any(|n| n.contains("nothing is running")),
        "the deck explains the mark that did not interrupt: {:?}",
        ui.notice.entries()
    );
}

/// **The witness for the shell mark.** `$` runs the rest now. It goes ahead
/// of the queue and of the running turn.
#[test]
fn the_dollar_mark_runs_a_shell_command_immediately_never_enqueued() {
    let model = lead(crate::AgentStatus::Running);
    let mut ui = ready_ui();
    assert_eq!(
        submit("$ cargo build", &model, &mut ui),
        DeckAction::Shell("cargo build".into())
    );
    assert!(
        ui.notice.entries().is_empty(),
        "the current spelling is not deprecated"
    );
}

/// **The witness for `!!`.** Two bangs interrupt exactly as one does, and the
/// message carries the strength the driver saves the words at.
///
/// It fails on the old deck by construction. That deck owned only the first
/// bang and read the second as part of a command, so `!! use short sentences`
/// answered `DeckAction::Shell("! use short sentences")` and ran the sentence.
#[test]
fn two_bangs_interrupt_and_ask_for_the_words_to_be_kept_as_guidance() {
    let model = lead(crate::AgentStatus::Running);
    let mut ui = ready_ui();
    assert_eq!(
        submit("!! use short sentences", &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Interrupt {
            agent: "lead".into(),
            texts: vec!["use short sentences".into()],
            keep: Some(crate::envelope::KeepStrength::Guidance),
        })
    );
    assert!(
        ui.notice.entries().iter().any(|n| n.contains("guidance")),
        "a keystroke that keeps something says what it keeps: {:?}",
        ui.notice.entries()
    );
}

/// **The witness for `!!!`.** Three bangs ask for the rule strength.
#[test]
fn three_bangs_interrupt_and_ask_for_the_words_to_be_kept_as_a_rule() {
    let model = lead(crate::AgentStatus::Running);
    let mut ui = ready_ui();
    assert_eq!(
        submit("!!! do not force-push", &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Interrupt {
            agent: "lead".into(),
            texts: vec!["do not force-push".into()],
            keep: Some(crate::envelope::KeepStrength::Rule),
        })
    );
    assert!(
        ui.notice.entries().iter().any(|n| n.contains("rule")),
        "a keystroke that keeps a rule says so: {:?}",
        ui.notice.entries()
    );
}

/// The save must not depend on a turn being in flight, so a keep sigil at an
/// idle lead still sends the message that carries it. The driver reads an
/// interrupt with nothing to stop as "run this now".
#[test]
fn a_keep_sigil_at_an_idle_lead_still_carries_the_save() {
    let model = lead(crate::AgentStatus::WaitingInput);
    let mut ui = ready_ui();
    assert_eq!(
        submit("!! use short sentences", &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Interrupt {
            agent: "lead".into(),
            texts: vec!["use short sentences".into()],
            keep: Some(crate::envelope::KeepStrength::Guidance),
        })
    );
    assert!(
        ui.notice
            .entries()
            .iter()
            .any(|n| n.contains("nothing was running")),
        "the deck says the words also went out as a prompt: {:?}",
        ui.notice.entries()
    );
}

/// The old spelling still runs for one release. It says where the mark went.
/// A bang with the command against it is a shell line, turn or no turn. Only
/// a bang and a space make the interrupt.
#[test]
fn the_bang_shell_spelling_still_runs_and_names_its_replacement() {
    let model = lead(crate::AgentStatus::Running);
    let mut ui = ready_ui();
    assert_eq!(
        submit("!ls", &model, &mut ui),
        DeckAction::Shell("ls".into())
    );
    assert!(
        ui.notice.entries().iter().any(|n| n.contains("$ cmd")),
        "the deprecation names the new mark: {:?}",
        ui.notice.entries()
    );
}
