//! The launch mark and the startup notice dialog: how a key press cuts the
//! splash, how the replay/release cues bound its life, and why session chrome
//! goes to the dialog rather than to an agent's transcript.

use super::*;

#[test]
fn any_key_dismisses_the_splash_first() {
    let model = model_with(&["lead"]);
    let mut ui = DeckUi::default(); // splash NOT skipped
    assert!(!ui.splash.is_done());
    assert_eq!(
        handle_deck_key(ch('a'), &model, &mut ui),
        DeckAction::Handled
    );
    assert!(ui.splash.is_done(), "first key skips the splash");
    assert!(ui.composer.buffer().is_empty(), "and does not type");
}

#[test]
fn splash_cues_replay_and_release_the_launch_mark() {
    let mut model = WorkspaceModel::new();
    let mut ui = DeckUi::default();
    ui.splash.skip(); // the deck is up; `/init` arrives later
    assert!(ui.splash.is_done());

    // Replay: a fresh held mark owns the frame again for as long as init runs…
    ingest_inbound(&Inbound::Splash(SplashCue::Replay), &mut model, &mut ui);
    assert!(!ui.splash.is_done(), "replay re-holds the mark over init");

    // …and Release bounds the mark's life: it plays the assemble out (the
    // brand's one beat of screen time — exact timing is `splash::tests`)
    // rather than cutting mid-build, while a key press still cuts at once.
    ingest_inbound(&Inbound::Splash(SplashCue::Release), &mut model, &mut ui);
    assert!(
        !ui.splash.is_done(),
        "a release straight after replay finishes the assemble first"
    );
    ui.splash.skip();
    assert!(ui.splash.is_done(), "any key still cuts immediately");
}

/// The invariant this whole path exists for: the transcript is the home for
/// agent and user messages ONLY. Session chrome used to ride an
/// `AgentEvent::Text`, which auto-registered a lead lane and gave the notice a
/// transcript row indistinguishable from the model speaking.
#[test]
fn a_system_notice_goes_to_the_dialog_and_never_to_the_transcript() {
    let mut model = WorkspaceModel::new();
    let mut ui = DeckUi::default();
    ui.splash.skip();

    let text = "◂ a previous session is resumable — ← opens SESSIONS";
    ingest_inbound(&Inbound::Notice(text.to_string()), &mut model, &mut ui);

    assert_eq!(ui.notice.entries(), [text], "the dialog holds the notice");
    assert!(ui.notice.is_visible(), "and shows it");
    assert!(
        model.agents.is_empty(),
        "a notice must not conjure an agent lane — an Event would have, and \
         that lane's transcript is exactly where chrome does not belong"
    );
}

/// Dismissal is total but not greedy: the notice goes, the keystroke lands.
/// Swallowing it would eat the first character typed, in precisely the second
/// or two when the dialog is up and the user is most likely to start typing.
#[test]
fn any_key_dismisses_the_startup_notice_without_eating_the_keystroke() {
    let mut model = WorkspaceModel::new();
    let mut ui = DeckUi::default();
    ui.splash.skip();
    ingest_inbound(
        &Inbound::Notice("indexing…".to_string()),
        &mut model,
        &mut ui,
    );
    assert!(ui.notice.is_visible());

    handle_deck_key(key(KeyCode::Char('a')), &model, &mut ui);
    assert!(!ui.notice.is_visible(), "any key dismisses the dialog");
    assert_eq!(
        ui.composer.buffer(),
        "a",
        "and the keystroke still reaches the composer"
    );
}

#[test]
fn no_anim_sessions_ignore_splash_replays() {
    let mut model = WorkspaceModel::new();
    let mut ui = DeckUi::default();
    ui.no_anim = true;
    ui.splash.skip();
    ingest_inbound(&Inbound::Splash(SplashCue::Replay), &mut model, &mut ui);
    assert!(
        ui.splash.is_done(),
        "a no-anim session never re-holds the launch mark"
    );
}
