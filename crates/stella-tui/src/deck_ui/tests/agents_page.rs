//! The AGENTS page (`←` twice on SESSION): the chord, the page's own
//! composer starting a lane, its scoped command menu, and the queue-free
//! command route the page shares with the deck composer.

use super::*;
use crate::composer::SlashCommand;
use crate::envelope::SessionInfo;

fn vocabulary() -> Vec<SlashCommand> {
    vec![
        SlashCommand::new("/help", "show commands").sideband(),
        SlashCommand::new("/clear", "reset the conversation"),
        SlashCommand::new("/model", "set the default model").sideband(),
        SlashCommand::new("/models", "model routing").sideband(),
        SlashCommand::new("/theme", "switch colour theme").sideband(),
        SlashCommand::new("/export", "export session telemetry").sideband(),
        SlashCommand::new("/init", "index the workspace"),
    ]
}

fn page_ui() -> DeckUi {
    let mut ui = ready_ui();
    ui.slash_commands = vocabulary();
    ui.agents_page.open = true;
    ui
}

/// **The witness for the chord.** On SESSION with an empty prompt the first
/// `←` arms (and no longer wraps the tab strip backward), the second inside
/// the window opens the page full-frame and asks for a fresh session
/// snapshot. Any key between the two presses disarms.
#[test]
fn left_left_opens_the_agents_page_from_the_session_tab() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    assert_eq!(
        handle_deck_key(key(KeyCode::Left), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(ui.tab, DeckTab::Session, "the first ← stays put");
    assert!(!ui.agents_page.open, "…and only arms");
    assert_eq!(
        handle_deck_key(key(KeyCode::Left), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::SessionsRefresh),
        "the second ← opens the page and refreshes the registry"
    );
    assert!(ui.agents_page.open);

    // A key between the presses breaks the pair.
    let mut ui = ready_ui();
    handle_deck_key(key(KeyCode::Left), &model, &mut ui);
    handle_deck_key(key(KeyCode::Right), &model, &mut ui);
    handle_deck_key(key(KeyCode::Left), &model, &mut ui);
    assert!(!ui.agents_page.open, "an interposed key disarms the chord");
}

/// The page's composer starts a NEW lane on the described task — a
/// [`WorkspaceInput::SpawnLane`], never an enqueue for the lead.
#[test]
fn describing_a_task_on_the_page_spawns_a_lane() {
    let model = model_with(&["lead"]);
    let mut ui = page_ui();
    for c in "fix the parser".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::SpawnLane {
            text: "fix the parser".into()
        })
    );
    assert!(ui.agents_page.open, "the page stays up to show the lane");
    assert!(ui.agents_page.notice.is_some(), "and says what it did");
}

/// **The scoped menu.** `/model` works from the page (queue-free); `/export`
/// — queue-free on the deck — is refused here with a notice, exactly as
/// asked: not every command belongs on the fleet view.
#[test]
fn the_page_menu_is_scoped_and_refuses_export() {
    let model = model_with(&["lead"]);
    let mut ui = page_ui();
    for c in "/model zai/glm-5.2".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Command {
            text: "/model zai/glm-5.2".into()
        })
    );

    let mut ui = page_ui();
    for c in "/export".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    // Typed out in full the popup may be up; Enter must still refuse.
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled, "nothing leaves the deck");
    assert!(
        ui.agents_page
            .notice
            .as_deref()
            .is_some_and(|n| n.contains("/export")),
        "the refusal names the command: {:?}",
        ui.agents_page.notice
    );
}

/// `n` from an empty page composer starts a brand-new FULL session — the
/// SESSIONS overlay's verb, reachable from the page — while `n` typed into a
/// draft stays a letter.
#[test]
fn n_on_the_page_starts_a_new_full_session() {
    let model = model_with(&["lead"]);
    let mut ui = page_ui();
    assert_eq!(
        handle_deck_key(ch('n'), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::SessionNew)
    );
    assert!(!ui.agents_page.open, "the hand-over closes the page");

    let mut ui = page_ui();
    handle_deck_key(ch('a'), &model, &mut ui);
    handle_deck_key(ch('n'), &model, &mut ui);
    assert_eq!(
        ui.agents_page.composer.buffer(),
        "an",
        "a draft keeps its letters"
    );
}

/// `⏎` on a resumable session row hands over to the driver; the page closes.
#[test]
fn enter_on_a_resumable_session_resumes_it() {
    let model = model_with(&["lead"]);
    let mut ui = page_ui();
    ui.sessions = vec![SessionInfo {
        id: "s-1".into(),
        title: "stella: fix the parser".into(),
        summary: String::new(),
        description: None,
        workspace: "/w".into(),
        phase: crate::envelope::SessionPhase::Complete,
        started_ms: 0,
        updated_ms: 0,
        mine: false,
        resumable: true,
        turns: 3,
        spend_micros: 0,
        model: None,
    }];
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::SessionResume { id: "s-1".into() })
    );
    assert!(!ui.agents_page.open);
}

/// **The queue-free route on the deck composer.** A submitted `/export`
/// leaves as [`WorkspaceInput::Command`] — mid-turn included, and ahead of a
/// held dispatch — so it never sits in the prompt queue. `/clear` and
/// `/init` keep their old routes.
#[test]
fn a_sideband_command_bypasses_the_prompt_queue() {
    let mut model = model_with(&["lead"]);
    model.apply_inbound(&Inbound::Status {
        agent: "lead".into(),
        status: AgentStatus::Running,
    });
    let mut ui = ready_ui();
    ui.slash_commands = vocabulary();
    ui.composer.load("/export".to_string());
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Command {
            text: "/export".into()
        }),
        "queue-free even while the lead runs"
    );

    // A held dispatch must not capture it either.
    let mut ui = ready_ui();
    ui.slash_commands = vocabulary();
    ui.dispatch_held = true;
    ui.composer.load("/export".to_string());
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Command {
            text: "/export".into()
        })
    );
    assert!(ui.dispatch_held, "the hold stays armed for the next prompt");

    // The turn-coupled exceptions keep their routes.
    let mut ui = ready_ui();
    ui.slash_commands = vocabulary();
    ui.composer.load("/init".to_string());
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Enqueue {
            text: "/init".into()
        }),
        "/init still rides the queue"
    );
}

/// **The `/model` argument menu on the deck composer.** Candidates narrow as
/// the argument is typed, Tab completes into the buffer, and ⏎ submits the
/// completed command down the queue-free route.
#[test]
fn the_model_argument_menu_completes_and_submits() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.slash_commands = vocabulary();
    ui.model_candidates = vec!["zai/glm-5.2".into(), "zai/glm-5.1".into()];
    ui.composer.load("/model glm-5.1".to_string());
    assert_eq!(
        handle_deck_key(key(KeyCode::Tab), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(
        ui.composer.buffer(),
        "/model zai/glm-5.1",
        "Tab completes the spec instead of cycling tabs"
    );
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Command {
            text: "/model zai/glm-5.1".into()
        })
    );
    assert!(ui.composer.is_blank(), "submit clears the composer");
}
