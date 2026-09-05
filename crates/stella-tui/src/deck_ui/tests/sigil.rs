//! The composer marks: `$` runs a shell command, `!` interrupts the turn.

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
