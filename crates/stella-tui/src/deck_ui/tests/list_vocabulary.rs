//! **The witness (#4370).** The four surfaces that used to spell their own
//! arrows answer the deck's one list vocabulary.
//!
//! Each test presses `j` and `End` — the two keys `list_nav` adds that a
//! hand-rolled `↑`/`↓` pair does not have — so it fails on a surface that has
//! gone back to matching `KeyCode` itself. The question and approval cards
//! keep their own tests beside their handlers, where their fixtures live.

use super::*;

use crate::envelope::{EngineAgentState, EngineConfigState, ToolPolicyState, ToolRow};

/// A deck on SETTINGS with the ENGINE panel focused over a loaded snapshot.
fn engine_ui() -> (WorkspaceModel, DeckUi) {
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Settings);
    ui.engine.focused = true;
    ui.engine.state = Some(EngineConfigState {
        agents: vec![EngineAgentState::default(); 4],
        ..Default::default()
    });
    (WorkspaceModel::new(), ui)
}

/// A deck on SETTINGS with the TOOLS panel focused over a loaded snapshot.
fn tools_ui() -> (WorkspaceModel, DeckUi) {
    let mut ui = ready_ui();
    ui.set_tab(DeckTab::Settings);
    ui.tools.focused = true;
    ui.tools.state = Some(ToolPolicyState {
        tools: vec![
            ToolRow {
                name: "get_state".into(),
                group: "scratch".into(),
                locked: false,
                off: None,
            },
            ToolRow {
                name: "save_state".into(),
                group: "scratch".into(),
                locked: false,
                off: None,
            },
            ToolRow {
                name: "delegate".into(),
                group: "task".into(),
                locked: false,
                off: None,
            },
        ],
        ..Default::default()
    });
    (WorkspaceModel::new(), ui)
}

#[test]
fn the_engine_panel_takes_j_and_end() {
    let (model, mut ui) = engine_ui();
    let last = ui.engine.row_count() - 1;
    assert!(last > 0, "the fixture needs more than one row to move to");

    assert_eq!(
        handle_deck_key(ch('j'), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(ui.engine.row, 1, "`j` did not move the ENGINE selection");
    assert_eq!(
        handle_deck_key(ch('k'), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(ui.engine.row, 0);
    handle_deck_key(key(KeyCode::End), &model, &mut ui);
    assert_eq!(ui.engine.row, last, "`End` did not reach the last row");
    handle_deck_key(key(KeyCode::Home), &model, &mut ui);
    assert_eq!(ui.engine.row, 0);
}

#[test]
fn the_tools_panel_takes_j_and_end() {
    let (model, mut ui) = tools_ui();
    let last = ui.tools.rows().len() - 1;
    assert!(last > 0, "the fixture needs more than one row to move to");

    assert_eq!(
        handle_deck_key(ch('j'), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(ui.tools.row, 1, "`j` did not move the TOOLS selection");
    handle_deck_key(key(KeyCode::End), &model, &mut ui);
    assert_eq!(ui.tools.row, last, "`End` did not reach the last row");
    handle_deck_key(key(KeyCode::Home), &model, &mut ui);
    assert_eq!(ui.tools.row, 0);
}

/// The scope picker is two options drawn side by side, so `←`/`→` and the
/// `p`/`u` mnemonics stay its own — but the vertical keys are the deck's.
#[test]
fn the_skills_scope_picker_takes_j_and_end_beside_its_own_arrows() {
    let model = WorkspaceModel::new();
    let mut ui = ready_ui();
    ui.tab = DeckTab::Skills;
    ui.skills.prompt = Some(SkillPrompt::Scope {
        action: ScopeAction::Install {
            id: "acme/auth".into(),
        },
        user: false,
    });

    let picked = |ui: &DeckUi| match &ui.skills.prompt {
        Some(SkillPrompt::Scope { user, .. }) => *user,
        other => panic!("the picker closed: {other:?}"),
    };

    handle_deck_key(ch('j'), &model, &mut ui);
    assert!(picked(&ui), "`j` did not move the scope picker");
    handle_deck_key(ch('k'), &model, &mut ui);
    assert!(!picked(&ui));
    handle_deck_key(key(KeyCode::End), &model, &mut ui);
    assert!(picked(&ui), "`End` did not reach the last option");
    handle_deck_key(key(KeyCode::Home), &model, &mut ui);
    assert!(!picked(&ui));

    // Its own axis still works: the two options are drawn left and right.
    handle_deck_key(key(KeyCode::Right), &model, &mut ui);
    assert!(picked(&ui));
    handle_deck_key(ch('p'), &model, &mut ui);
    assert!(!picked(&ui));
}
