//! Session-transcript selection and expansion: the ↑/↓ highlight, ⌃O expand
//! (per-row and the expand-all overlay), the bracket end-jumps, and what a
//! focus change, a deregistration or a front-eviction do to a live selection.

use super::*;

#[test]
fn up_selects_the_last_message_and_ctrl_o_toggles_it() {
    let mut model = model_with(&["lead"]);
    with_tool_exchange(&mut model, "lead");
    let mut ui = ready_ui();

    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    assert_eq!(
        ui.session_selected,
        Some(1),
        "up highlights the newest message"
    );

    handle_deck_key(ctrl('o'), &model, &mut ui);
    assert!(
        ui.expanded.get("lead").is_some_and(|set| set.contains(&1)),
        "ctrl+o expands the highlighted message"
    );
    let rev = ui.expanded_rev;
    handle_deck_key(ctrl('o'), &model, &mut ui);
    assert!(
        ui.expanded.get("lead").is_none_or(|set| !set.contains(&1)),
        "a second ctrl+o collapses it again"
    );
    assert!(
        ui.expanded_rev > rev,
        "each toggle invalidates the fold cache"
    );
}

#[test]
fn ctrl_o_with_no_selection_toggles_the_expand_all_overlay() {
    let mut model = model_with(&["lead"]);
    with_tool_exchange(&mut model, "lead");
    let mut ui = ready_ui();

    // First press (no selection): every expandable message opens at once —
    // no per-entry set is touched.
    handle_deck_key(ctrl('o'), &model, &mut ui);
    assert!(ui.transcript_expand_all, "expand-all overlay on");
    assert!(
        ui.expanded.get("lead").is_none_or(|set| set.is_empty()),
        "the overlay does not write the per-entry sets"
    );
    // Second press: everything closes again — ctrl+o is its own way out.
    handle_deck_key(ctrl('o'), &model, &mut ui);
    assert!(!ui.transcript_expand_all, "ctrl+o again collapses");
}

#[test]
fn esc_collapses_the_expand_all_overlay_before_stopping_the_turn() {
    let mut model = model_with(&["lead"]);
    with_tool_exchange(&mut model, "lead"); // events flip the agent to Running
    let mut ui = ready_ui();
    handle_deck_key(ctrl('o'), &model, &mut ui);
    assert!(ui.transcript_expand_all);

    // Esc's first job here is the overlay — NOT cancelling the running
    // turn (precedence rule 8 beats rules 9–10).
    let action = handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert!(!ui.transcript_expand_all, "esc is a graceful way out");
    assert!(
        matches!(action, DeckAction::Handled),
        "the overlay-collapsing esc must not reach the turn-stop rules"
    );
    // With the overlay gone, the next Esc resumes normal duty (stop the
    // in-flight turn).
    let action = handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert!(
        matches!(action, DeckAction::Send(WorkspaceInput::Control { .. })),
        "esc after the overlay closes stops the turn as before"
    );
}

#[test]
fn ctrl_o_on_a_highlight_peels_one_row_out_of_the_expand_all_overlay() {
    let mut model = model_with(&["lead"]);
    with_tool_exchange(&mut model, "lead"); // entries 0 (call) + 1 (result)
    let mut ui = ready_ui();
    handle_deck_key(ctrl('o'), &model, &mut ui); // overlay on
    handle_deck_key(key(KeyCode::Up), &model, &mut ui); // highlight entry 1

    handle_deck_key(ctrl('o'), &model, &mut ui);
    assert!(
        !ui.transcript_expand_all,
        "the overlay materializes into per-entry expansions"
    );
    let set = ui.expanded.get("lead").expect("materialized set");
    assert!(
        set.contains(&0) && !set.contains(&1),
        "the highlighted row collapsed; the rest stay open: {set:?}"
    );
}

#[test]
fn bracket_jumps_reach_both_ends_of_the_transcript() {
    let mut model = model_with(&["lead"]);
    with_tool_exchange(&mut model, "lead");
    let mut ui = ready_ui();
    // A scrollable transcript (metrics as the render pass would set them).
    ui.metrics.session_total = 100;
    ui.metrics.session_height = 10;

    // ⌘/⌃ [ pins the window to the very beginning of the session…
    handle_deck_key(ctrl('['), &model, &mut ui);
    assert!(!ui.session_scroll.follow);
    assert_eq!(ui.session_scroll.window(100, 10), 0..10, "jumped to start");

    // …and ⌘/⌃ ] returns to the end, re-arming tail-follow. Both also
    // drop any highlight so the pinned selection can't yank the view back.
    let cmd_close = KeyEvent::new(KeyCode::Char(']'), KeyModifiers::SUPER);
    handle_deck_key(cmd_close, &model, &mut ui);
    assert!(ui.session_scroll.follow, "jumped to end = follow the tail");
    assert_eq!(ui.session_selected, None);
}

#[test]
fn down_past_the_last_message_clears_selection_and_rearms_follow() {
    let mut model = model_with(&["lead"]);
    with_tool_exchange(&mut model, "lead");
    let mut ui = ready_ui();
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(ui.session_selected, None, "down past the tail deselects");
    assert!(ui.session_scroll.follow, "…and re-arms tail-follow");
}

#[test]
fn ctrl_o_on_a_non_expandable_selection_is_a_no_op() {
    let mut model = model_with(&["lead"]);
    // A plain text message — `entry_lines` ignores the expanded flag for
    // it, so ctrl+o has nothing to toggle.
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Text {
            text: "hello".into(),
        },
    });
    let mut ui = ready_ui();
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    assert_eq!(ui.session_selected, Some(0));

    let rev = ui.expanded_rev;
    handle_deck_key(ctrl('o'), &model, &mut ui);
    assert!(
        ui.expanded.get("lead").is_none_or(|set| set.is_empty()),
        "nothing marked expanded"
    );
    assert_eq!(
        ui.expanded_rev, rev,
        "a no-op press must not invalidate the settled fold cache"
    );
}

#[test]
fn switching_focus_drops_the_session_selection() {
    let mut model = model_with(&["lead", "sub"]);
    with_tool_exchange(&mut model, "lead");
    let mut ui = ready_ui();
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    assert!(ui.session_selected.is_some());

    // Focus another agent from the Agents tab: the selection indexes the
    // *previous* agent's transcript and must not carry across.
    ui.tab = DeckTab::Agents;
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(ui.focused, 1);
    assert_eq!(
        ui.session_selected, None,
        "selection cleared on focus change"
    );
    assert!(ui.session_scroll.follow, "…and tail-follow re-arms");
}

#[test]
fn deregister_keeps_focus_in_bounds_and_on_the_same_agent() {
    let mut model = model_with(&["lead", "req:1", "req:2"]);
    let mut ui = ready_ui();
    ui.focused = 2; // req:2

    // An EARLIER row vanishing shifts indexes down: the focused AGENT
    // stays focused at its new index.
    ingest_inbound(
        &Inbound::Deregister {
            agent: "req:1".into(),
        },
        &mut model,
        &mut ui,
    );
    assert_eq!(ui.focused, 1);
    assert_eq!(model.agents[ui.focused].meta.id, "req:2");

    // The focused LAST row vanishing clamps focus back into range.
    ingest_inbound(
        &Inbound::Deregister {
            agent: "req:2".into(),
        },
        &mut model,
        &mut ui,
    );
    assert_eq!(ui.focused, 0, "focus stays in bounds");
    assert_eq!(model.agents[ui.focused].meta.id, "lead");
}

#[test]
fn deregister_of_the_focused_row_drops_the_stale_selection() {
    let mut model = model_with(&["lead", "req:1", "req:2"]);
    with_tool_exchange(&mut model, "req:1");
    with_tool_exchange(&mut model, "req:2");
    let mut ui = ready_ui();
    ui.focused = 1; // req:1
    ui.session_selected = Some(1); // a row of req:1's transcript

    ingest_inbound(
        &Inbound::Deregister {
            agent: "req:1".into(),
        },
        &mut model,
        &mut ui,
    );
    // The successor (req:2) slides into the focused index…
    assert_eq!(ui.focused, 1);
    assert_eq!(model.agents[1].meta.id, "req:2");
    // …but the selection indexed the REMOVED agent's transcript, so it
    // must not re-attach to the successor's (which is long enough that
    // range-clamping alone would have kept it).
    assert_eq!(ui.session_selected, None);
    assert!(ui.session_scroll.follow, "tail-follow re-arms");
}

#[test]
fn eviction_clamps_the_selection_and_drops_stale_expansions() {
    use crate::model::MAX_TRANSCRIPT_ENTRIES;
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    let retry = |i: usize| Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Retry {
            attempt: i as u32,
            reason: "r".into(),
        },
    };
    // Grow to just under the cap, then highlight + expand near the tail.
    for i in 0..(MAX_TRANSCRIPT_ENTRIES - 1) {
        ingest_inbound(&retry(i), &mut model, &mut ui);
    }
    ui.session_selected = Some(MAX_TRANSCRIPT_ENTRIES - 2);
    toggle_expanded(&mut ui, "lead", MAX_TRANSCRIPT_ENTRIES - 2);
    let rev = ui.expanded_rev;

    // One more event crosses the cap: a chunk of the front evicts.
    ingest_inbound(&retry(MAX_TRANSCRIPT_ENTRIES), &mut model, &mut ui);
    let len = model.agents[0].model.transcript.len();
    assert!(len < MAX_TRANSCRIPT_ENTRIES, "a chunk was evicted");
    assert!(
        ui.session_selected.is_some_and(|sel| sel < len),
        "selection clamped into the retained window"
    );
    assert!(
        !ui.expanded.contains_key("lead"),
        "index-keyed expansions are stale once the front moved"
    );
    assert!(ui.expanded_rev > rev, "fold cache invalidated");
}

/// Front-eviction moves every retained index down, and the scrollback counter
/// is an index. Off by even one and accessible mode either re-announces an
/// entry a reader already heard or — worse — suppresses one from the live pane
/// that was never written anywhere (#1258).
///
/// The assertion is exact rather than approximate: the entry the counter now
/// points at must be the very next one after the last that was flushed.
#[test]
fn eviction_keeps_the_scrollback_counter_pointing_at_the_same_entry() {
    use crate::model::{MAX_TRANSCRIPT_ENTRIES, TranscriptEntry};
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    let retry = |i: usize| Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Retry {
            attempt: i as u32,
            reason: "r".into(),
        },
    };
    for i in 0..(MAX_TRANSCRIPT_ENTRIES - 1) {
        ingest_inbound(&retry(i), &mut model, &mut ui);
    }
    // Everything up to (not including) attempt 1000 is in scrollback — past
    // the eviction chunk, so the survivors include flushed entries and the
    // counter has real work to do (a prefix entirely inside the evicted chunk
    // would correctly floor at zero and prove nothing about the arithmetic).
    ui.scrollback.set_live(true);
    ui.scrollback.record(&crate::accessible::FlushBlock {
        agent: "lead".into(),
        from: 0,
        to: 1_000,
        header: None,
    });

    ingest_inbound(&retry(MAX_TRANSCRIPT_ENTRIES), &mut model, &mut ui);
    let transcript = &model.agents[0].model.transcript;
    assert!(
        transcript.len() < MAX_TRANSCRIPT_ENTRIES,
        "a chunk was evicted"
    );
    let flushed = ui.scrollback.flushed_for("lead");
    assert!(
        matches!(&transcript[flushed], TranscriptEntry::Retry { attempt, .. } if *attempt == 1_000),
        "the counter must still point one past the last flushed entry, got {:?}",
        transcript[flushed]
    );
}
