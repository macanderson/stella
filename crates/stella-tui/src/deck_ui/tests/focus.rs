//! The focus tree (`deck_ui::focus`): `←`/`→` walk the tab strip from every
//! tab, `⌫` backs up one level and never stops anything, `⏎` opens the
//! highlighted message, and the `ctrl-e`/`ctrl-k` chords that took over the
//! SESSIONS/CONTEXT overlays from the arrows.

use super::*;
use crate::model::TranscriptEntry;

/// A lead with one dispatched lane, both running.
fn lead_and_lane() -> WorkspaceModel {
    let mut m = model_with(&["lead"]);
    m.apply_inbound(&Inbound::Register(
        AgentMeta::new("sub:2", "task 2", 0)
            .with_role("subagent")
            .with_parent("lead"),
    ));
    for agent in ["lead", "sub:2"] {
        m.apply_inbound(&Inbound::Status {
            agent: agent.into(),
            status: AgentStatus::Running,
        });
    }
    m
}

/// How many `→` presses a tab spends on its own panes before the key rises
/// to the strip: one per pane, so a tab with none hands the first press up.
fn panes_to_cross(tab: DeckTab) -> usize {
    match tab {
        DeckTab::Settings => crate::v2::settings::SettingsPane::ALL.len(),
        DeckTab::Skills => 2, // installed → search → (rise)
        _ => 1,
    }
}

/// **The witness for the arrows.** From an empty composer `→` and `←` move
/// between sibling tabs on every tab — the SESSION tab included, where they
/// used to open two overlays — and the strip is a ring. A tab with panes
/// spends the key on them first and hands it up at the edge (bubbling).
#[test]
fn left_and_right_walk_the_tab_strip_from_every_tab() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    assert_eq!(ui.tab, DeckTab::Session);
    for expected in DeckTab::ALL.iter().skip(1) {
        let from = ui.tab;
        let mut presses = 0;
        while ui.tab == from {
            handle_deck_key(key(KeyCode::Right), &model, &mut ui);
            presses += 1;
            assert!(presses <= 4, "→ never left {from:?}");
        }
        assert_eq!(ui.tab, *expected, "→ steps to the next tab");
        assert_eq!(
            presses,
            panes_to_cross(from),
            "{from:?}: panes first, then the strip"
        );
    }
    for _ in 0..panes_to_cross(ui.tab) {
        handle_deck_key(key(KeyCode::Right), &model, &mut ui);
    }
    assert_eq!(ui.tab, DeckTab::Session, "…and wraps at the end");
    // On SESSION, `←` is the AGENTS-page chord (`tests::agents_page`), so
    // the backward wrap starts one tab in: `⇧Tab` still reaches the far end
    // in one press, and `←` walks backward from anywhere but SESSION.
    handle_deck_key(key(KeyCode::BackTab), &model, &mut ui);
    assert_eq!(
        ui.tab,
        *DeckTab::ALL.last().expect("tabs exist"),
        "⇧Tab wraps the other way"
    );
    let mut presses = 0;
    while ui.tab == *DeckTab::ALL.last().expect("tabs exist") {
        handle_deck_key(key(KeyCode::Left), &model, &mut ui);
        presses += 1;
        assert!(presses <= 4, "← never left the last tab");
    }
    assert_eq!(
        ui.tab,
        DeckTab::ALL[DeckTab::ALL.len() - 2],
        "← walks backward from anywhere but SESSION (panes first)"
    );
    assert!(!ui.sessions_open && !ui.context_open, "no overlay opened");
}

/// The arrows edit a draft: with text in the composer `←` is cursor motion
/// and the tab stays put.
#[test]
fn with_a_draft_the_arrows_are_cursor_motion() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.composer.load("hello".to_string());
    handle_deck_key(key(KeyCode::Left), &model, &mut ui);
    handle_deck_key(key(KeyCode::Right), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Session);
    assert_eq!(ui.composer.buffer(), "hello", "the draft is intact");
}

/// `ctrl-e` and `ctrl-k` are where the SESSIONS and CONTEXT overlays went.
#[test]
fn ctrl_e_and_ctrl_k_open_the_sessions_and_context_overlays() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.tab = DeckTab::Files;
    handle_deck_key(ctrl('e'), &model, &mut ui);
    assert!(ui.sessions_open, "ctrl-e opens SESSIONS from any tab");
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert!(!ui.sessions_open);
    handle_deck_key(ctrl('k'), &model, &mut ui);
    assert!(ui.context_open, "ctrl-k opens CONTEXT from any tab");
    handle_deck_key(ch('q'), &model, &mut ui);
    assert!(!ui.context_open, "q closes it like every overlay");
}

/// **The witness for `ctrl-a` (#4368).** The SUB-AGENTS overlay's own tests
/// call `subagents::open` directly, so the chord the keymap advertises had
/// nothing pressing it. It opens from any tab, and — unlike the `↓` route
/// beside it — from a composer with a draft in it, because a ctrl-chord is
/// never the next character of a prompt.
#[test]
fn ctrl_a_opens_the_sub_agents_overlay_from_any_tab() {
    let model = lead_and_lane();
    let mut ui = ready_ui();
    ui.tab = DeckTab::Files;
    ui.composer.load("half a prompt".to_string());

    handle_deck_key(ctrl('a'), &model, &mut ui);
    assert!(ui.subagents.open, "ctrl-a opens SUB-AGENTS from any tab");
    assert_eq!(
        ui.composer.buffer(),
        "half a prompt",
        "and left the draft alone"
    );
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert!(!ui.subagents.open);
}

/// **The witness for `⌫`.** At an opened lane it returns to the dispatcher
/// and sends nothing — the lane keeps running; at the lead, with nothing to
/// peel, it is ignored rather than reaching the turn interrupt.
#[test]
fn backspace_backs_out_of_a_lane_and_never_stops_it() {
    let model = lead_and_lane();
    let mut ui = ready_ui();
    ui.focus_agent(1);
    assert_eq!(
        handle_deck_key(key(KeyCode::Backspace), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(ui.focused, 0, "back to the lead");
    let at_root = handle_deck_key(key(KeyCode::Backspace), &model, &mut ui);
    assert!(
        !matches!(at_root, DeckAction::Send(_)),
        "the lead has no dispatcher and nothing is open: nothing is sent ({at_root:?})"
    );
    assert_eq!(ui.focused, 0);
    assert!(
        ui.esc_armed_at.is_none(),
        "⌫ never arms the double-Esc stop"
    );
}

/// `⌫` peels the same layers Esc does, innermost first: the FILES diff,
/// then a highlighted message, then the expand-all overlay.
#[test]
fn backspace_peels_one_layer_at_a_time() {
    let mut model = model_with(&["lead"]);
    with_tool_exchange(&mut model, "lead");
    let mut ui = ready_ui();

    ui.tab = DeckTab::Files;
    ui.files_diff_open = true;
    handle_deck_key(key(KeyCode::Backspace), &model, &mut ui);
    assert!(!ui.files_diff_open, "the diff closes first");

    ui.tab = DeckTab::Session;
    ui.session_selected = Some(0);
    ui.transcript_expand_all = true;
    handle_deck_key(key(KeyCode::Backspace), &model, &mut ui);
    assert_eq!(ui.session_selected, None, "the highlight goes first");
    assert!(ui.transcript_expand_all, "…the overlay is still up");
    handle_deck_key(key(KeyCode::Backspace), &model, &mut ui);
    assert!(!ui.transcript_expand_all, "…and goes on the next press");
}

/// With a draft, `⌫` is a delete — the tree never sees it.
#[test]
fn with_a_draft_backspace_deletes() {
    let model = lead_and_lane();
    let mut ui = ready_ui();
    ui.focus_agent(1);
    ui.composer.load("ab".to_string());
    handle_deck_key(key(KeyCode::Backspace), &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "a");
    assert_eq!(ui.focused, 1, "focus did not move");
}

/// **The witness for `⏎`.** On a highlighted expandable message it opens
/// (and closes) that message; on the blank tail it submits nothing.
#[test]
fn enter_opens_the_highlighted_message() {
    let mut model = model_with(&["lead"]);
    with_tool_exchange(&mut model, "lead");
    let mut ui = ready_ui();
    // The tool result is the expandable row.
    let idx = model.agents[0]
        .model
        .transcript
        .iter()
        .position(|e| matches!(e, TranscriptEntry::ToolResult { .. }))
        .expect("a tool result is on the transcript");
    ui.session_selected = Some(idx);
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Handled
    );
    assert!(
        ui.expanded.get("lead").is_some_and(|s| s.contains(&idx)),
        "⏎ expanded the highlighted row"
    );
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert!(
        !ui.expanded.get("lead").is_some_and(|s| s.contains(&idx)),
        "a second ⏎ folds it again"
    );
    ui.session_selected = None;
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Ignored,
        "nothing highlighted: a blank ⏎ does nothing"
    );
}

/// The agent tree the deck walks: a lane's parent is the dispatcher it was
/// registered with, a lane registered without one falls back to the lead,
/// and the root has none.
#[test]
fn the_agent_tree_reads_parents_off_the_meta() {
    let mut model = lead_and_lane();
    model.apply_inbound(&Inbound::Register(
        AgentMeta::new("req:1", "older driver", 0).with_role("subagent"),
    ));
    assert_eq!(model.parent_of(0), None, "the lead is the root");
    assert_eq!(model.parent_of(1), Some(0));
    assert_eq!(
        model.parent_of(2),
        Some(0),
        "no parent on the row: the lead"
    );
    assert_eq!(model.children_of(0), vec![1, 2]);
    assert_eq!(
        model.ancestry(1),
        vec!["lead", "sub:2"],
        "the breadcrumb's path"
    );
}
