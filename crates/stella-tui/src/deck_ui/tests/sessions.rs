//! The SESSIONS and INBOX overlays: what ⏎ and ␣ do to a selected row, how a
//! resumable session differs from a read-only replay, and the phase grouping
//! the overlay lists rows in.

use super::*;

fn session_info(id: &str) -> crate::envelope::SessionInfo {
    crate::envelope::SessionInfo {
        id: id.into(),
        title: format!("title for {id}"),
        summary: String::new(),
        workspace: "/tmp/w".into(),
        phase: crate::envelope::SessionPhase::Complete,
        started_ms: 0,
        updated_ms: 0,
        mine: false,
        resumable: false,
        description: None,
        turns: 0,
        spend_micros: 0,
        model: None,
        autofix: None,
    }
}

fn notification(id: &str, read: bool, session: Option<&str>) -> crate::envelope::NotificationInfo {
    crate::envelope::NotificationInfo {
        id: id.into(),
        title: "a title".into(),
        body: "a body".into(),
        source: String::new(),
        created_ms: 0,
        read,
        session_id: session.map(str::to_string),
    }
}

#[test]
fn sessions_overlay_enter_opens_the_selected_session_and_closes() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.sessions_open = true;
    ui.sessions = vec![session_info("ses-1"), session_info("ses-2")];
    ui.sessions_sel = 1;

    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::SessionOpen { id: "ses-2".into() }),
        "⏎ opens (replays) the selected session"
    );
    assert!(!ui.sessions_open, "the overlay closes on open");
}

#[test]
fn sessions_overlay_enter_with_no_rows_is_a_no_op() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.sessions_open = true; // registry snapshot empty

    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled, "nothing to open");
    assert!(ui.sessions_open, "the overlay stays up (Esc closes it)");
}

#[test]
fn inbox_enter_on_a_linked_notification_marks_read_and_opens_the_session() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.inbox_open = true;
    ui.notifications = vec![notification("n1", false, Some("ses-9"))];

    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::NotificationRead { id: "n1".into() }),
        "the read goes out as the key's action"
    );
    assert_eq!(
        ui.pending_inputs,
        vec![WorkspaceInput::SessionOpen { id: "ses-9".into() }],
        "…and the open rides pending_inputs right behind it"
    );
    assert!(!ui.inbox_open, "the overlay closes when a session opens");
}

#[test]
fn inbox_enter_on_an_already_read_linked_notification_just_opens() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.inbox_open = true;
    ui.notifications = vec![notification("n1", true, Some("ses-9"))];

    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::SessionOpen { id: "ses-9".into() }),
        "already read — no second NotificationRead, just the open"
    );
    assert!(ui.pending_inputs.is_empty());
    assert!(!ui.inbox_open);
}

#[test]
fn inbox_enter_without_a_session_link_keeps_the_mark_read_behavior() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.inbox_open = true;
    ui.notifications = vec![notification("n1", false, None)];

    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::NotificationRead { id: "n1".into() }),
        "unlinked ⏎ is exactly the old mark-read"
    );
    assert!(ui.pending_inputs.is_empty(), "no session to open");
    assert!(ui.inbox_open, "the overlay stays open, as before");

    // Once read, ⏎ on an unlinked notification is a no-op.
    ui.notifications = vec![notification("n1", true, None)];
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled);
    assert!(ui.inbox_open);
}

#[test]
fn inbox_space_only_marks_read_and_never_opens() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.inbox_open = true;
    ui.notifications = vec![notification("n1", false, Some("ses-9"))];

    let action = handle_deck_key(key(KeyCode::Char(' ')), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::NotificationRead { id: "n1".into() }),
        "␣ keeps its plain mark-read meaning"
    );
    assert!(ui.pending_inputs.is_empty(), "␣ never opens the session");
    assert!(ui.inbox_open, "␣ never closes the overlay");
}

fn session_row(
    id: &str,
    phase: crate::envelope::SessionPhase,
    mine: bool,
    resumable: bool,
) -> crate::envelope::SessionInfo {
    crate::envelope::SessionInfo {
        id: id.into(),
        title: format!("ws: {id}"),
        summary: String::new(),
        workspace: "/w".into(),
        phase,
        started_ms: 0,
        updated_ms: 0,
        mine,
        resumable,
        description: None,
        turns: 0,
        spend_micros: 0,
        model: None,
        autofix: None,
    }
}

#[test]
fn sessions_overlay_enter_resumes_resumable_rows_and_opens_the_rest() {
    use crate::envelope::SessionPhase;
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.sessions_open = true;
    ui.sessions = vec![
        session_row("ses-mine", SessionPhase::InProgress, true, false),
        session_row("ses-paused", SessionPhase::Paused, false, true),
        session_row("ses-foreign", SessionPhase::Complete, false, false),
    ];

    // Order: live first (mine, then the paused one), then Complete.
    // ⏎ on the resumable row navigates into it LIVE: the overlay closes
    // and the driver is told to resume exactly that session.
    ui.sessions_sel = 1;
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::SessionResume {
            id: "ses-paused".into()
        })
    );
    assert!(!ui.sessions_open, "the overlay closes on navigation");

    // ⏎ on any non-resumable row — this deck's own included — opens a
    // read-only replay instead (the `replay:<id>` lane).
    for (sel, id) in [(0, "ses-mine"), (2, "ses-foreign")] {
        ui.sessions_open = true;
        ui.sessions_sel = sel;
        assert_eq!(
            handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
            DeckAction::Send(WorkspaceInput::SessionOpen { id: id.into() })
        );
        assert!(!ui.sessions_open, "the overlay closes on open too");
    }
}

#[test]
fn sessions_overlay_n_asks_for_a_new_session_and_closes() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.sessions_open = true;
    let action = handle_deck_key(key(KeyCode::Char('n')), &model, &mut ui);
    assert_eq!(action, DeckAction::Send(WorkspaceInput::SessionNew));
    assert!(!ui.sessions_open, "the overlay closes on hand-over");
}
