//! Key-handling and ingest witness tests for the Command Deck UI.
//!
//! Split out of `deck_ui.rs` (#458): the module had grown to 6,884 lines, of
//! which ~2,960 were these tests. Shared fixtures live here; each tab or
//! overlay with its own fixtures owns a submodule below.

#![allow(clippy::field_reassign_with_default)]

use super::*;
use crate::envelope::AgentMeta;
use stella_protocol::AgentEvent;

mod graph;
mod help;
mod issues;
mod queue;
mod skills;
mod transcript_nav;

/// A model whose lead already has `prompts` queued, for the queue-editor and
/// dispatch tests. Shared: both this module and `queue` build on it.
fn model_with_queue(prompts: &[&str]) -> WorkspaceModel {
    let mut m = model_with(&["lead"]);
    for (i, p) in prompts.iter().enumerate() {
        m.queue.enqueue((*p).to_string(), i as u64);
    }
    m
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn ch(c: char) -> KeyEvent {
    key(KeyCode::Char(c))
}
fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}
/// The newline chord — `⌘⏎` as the kitty keyboard protocol reports it
/// (a modified Enter inserts a line break; a bare Enter submits).
fn cmd_enter() -> KeyEvent {
    KeyEvent::new(KeyCode::Enter, KeyModifiers::SUPER)
}
fn alt(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
}

fn model_with(ids: &[&str]) -> WorkspaceModel {
    let mut m = WorkspaceModel::new();
    for id in ids {
        m.apply_inbound(&Inbound::Register(AgentMeta::new(*id, *id, 0)));
    }
    m
}

/// Push one tool call + multi-line result onto `agent`'s transcript.
fn with_tool_exchange(m: &mut WorkspaceModel, agent: &str) {
    use stella_protocol::{AgentEvent, ToolCall, ToolOutput};
    m.apply_inbound(&Inbound::Event {
        agent: agent.into(),
        event: AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "c1".into(),
                name: "read_file".into(),
                input: serde_json::json!({ "path": "src/main.rs" }),
            },
        },
    });
    m.apply_inbound(&Inbound::Event {
        agent: agent.into(),
        event: AgentEvent::ToolResult {
            call_id: "c1".into(),
            output: ToolOutput::Ok {
                content: "line one\nline two\nline three".into(),
            },
            duration_ms: 7,
            speculated: false,
        },
    });
}

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
            delta: "hello".into(),
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

fn ready_ui() -> DeckUi {
    let mut ui = DeckUi::default();
    ui.splash.skip(); // past the splash for interaction tests
    ui
}

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

    // …and Release hands straight back to the deck — a fast init must never
    // be made to wait out a reveal (exact timing is `splash::tests`).
    ingest_inbound(&Inbound::Splash(SplashCue::Release), &mut model, &mut ui);
    assert!(ui.splash.is_done(), "release cuts straight to the deck");
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

    // Grouped order: InProgress (mine) · Paused (resumable) · Complete.
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
fn paused_sessions_group_between_needs_input_and_cancelled() {
    use crate::envelope::SessionPhase;
    let mut ui = DeckUi::default();
    ui.sessions = vec![
        session_row("c", SessionPhase::Cancelled, false, true),
        session_row("p", SessionPhase::Paused, false, true),
        session_row("n", SessionPhase::NeedsInput, false, false),
    ];
    let order: Vec<&str> = grouped_session_rows(&ui)
        .iter()
        .map(|s| s.id.as_str())
        .collect();
    assert_eq!(order, ["n", "p", "c"]);
}

#[test]
fn only_tab_switches_tabs_and_digits_always_type() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    assert_eq!(ui.tab, DeckTab::Session);
    handle_deck_key(key(KeyCode::Tab), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Agents);
    handle_deck_key(key(KeyCode::BackTab), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Session);
    // A digit with an empty composer starts the prompt — it never jumps
    // to a tab, so prompts can begin with 1–5.
    handle_deck_key(ch('3'), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Session, "digit typed, tab unchanged");
    handle_deck_key(ch('h'), &model, &mut ui);
    handle_deck_key(ch('2'), &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "3h2");
}

#[test]
fn bare_enter_always_enqueues_a_prompt_without_blocking() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    for c in "do the thing".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::Enqueue {
            text: "do the thing".into()
        })
    );
    assert!(
        ui.composer.buffer().is_empty(),
        "composer clears after submit"
    );
}

#[test]
fn a_modified_enter_inserts_a_line_break_preserved_through_submit() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    for c in "line one".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert_eq!(
        handle_deck_key(cmd_enter(), &model, &mut ui),
        DeckAction::Handled,
        "⌘⏎ is a line break, not a submit"
    );
    for c in "line two".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::Enqueue {
            text: "line one\nline two".into()
        }),
        "the typed line break survives into the submitted prompt"
    );
}

#[test]
fn plain_enter_on_a_blank_composer_inserts_nothing() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.tab = DeckTab::Graph; // a tab with no Enter binding of its own
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Ignored
    );
    assert!(ui.composer.buffer().is_empty(), "no stray leading newline");
}

#[test]
fn alt_brackets_jump_the_cursor_to_start_and_end() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    for c in "abc".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert_eq!(ui.composer.cursor(), 3);
    assert_eq!(
        handle_deck_key(alt('['), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(ui.composer.cursor(), 0, "⌥[ → before the first character");
    assert_eq!(
        handle_deck_key(alt(']'), &model, &mut ui),
        DeckAction::Handled
    );
    assert_eq!(ui.composer.cursor(), 3, "⌥] → one past the last character");
}

#[test]
fn bare_enter_queues_and_a_modified_enter_inserts_a_break() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    for c in "hi".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
    assert_eq!(
        handle_deck_key(alt_enter, &model, &mut ui),
        DeckAction::Handled,
        "⌥⏎ inserts a line break"
    );
    assert_eq!(ui.composer.buffer(), "hi\n");
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::Enqueue {
            text: "hi\n".into()
        }),
        "bare ⏎ queues (never blocks)"
    );
}

#[test]
fn arrow_keys_edit_a_multiline_prompt_instead_of_scrolling() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    for c in "ab".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    handle_deck_key(cmd_enter(), &model, &mut ui); // ⌘⏎ inserts a line break
    for c in "cd".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    // ↑ moves the cursor into the first line (not the session scroll,
    // and NOT the queue editor — the composer is not empty).
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    assert_eq!(ui.composer.cursor(), 2, "column kept on the line above");
    handle_deck_key(key(KeyCode::Left), &model, &mut ui);
    handle_deck_key(ch('X'), &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "aXb\ncd", "typed at the cursor");
}

#[test]
fn ctrl_c_quits_from_any_tab() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.tab = DeckTab::Graph;
    assert_eq!(
        handle_deck_key(ctrl('c'), &model, &mut ui),
        DeckAction::Quit
    );
}

/// A one-agent model whose lead is mid-turn (`Running`).
fn running_model() -> WorkspaceModel {
    let mut m = model_with(&["lead"]);
    m.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::Text {
            delta: "working".into(),
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

#[test]
fn a_typed_draft_survives_both_esc_forms() {
    let model = running_model();
    let mut ui = ready_ui();
    for c in "keep me".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    // The cursor lives in the global composer, so the stop fires even
    // mid-draft — and must leave the draft untouched.
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        stop_lead()
    );
    assert_eq!(ui.composer.buffer(), "keep me");
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::StopAndHold {
            agent: "lead".into()
        })
    );
    assert_eq!(
        ui.composer.buffer(),
        "keep me",
        "neither cancel form clears what the user typed"
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
        stop_lead(),
        "the next Esc is a fresh single stop, not an escalation"
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
        event: AgentEvent::Text { delta: "hi".into() },
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
fn esc_still_aborts_a_pending_scope_review() {
    let mut model = model_with(&["lead"]);
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::ScopeReview {
            proposal: stella_protocol::ScopeProposal {
                summary: "big".into(),
                steps: vec![],
                estimated_files: 3,
                estimated_cost_usd: None,
            },
        },
    });
    let mut ui = ready_ui();
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::ToAgent {
            agent: "lead".into(),
            input: UserInput::ScopeDecision(ScopeDecision::Abort),
        }),
        "rule 5: the gate claims Esc — never a turn stop"
    );
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

#[test]
fn the_first_submission_after_a_hold_enqueues_at_the_front() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.dispatch_held = true;
    for c in "urgent fix".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::EnqueueFront {
            text: "urgent fix".into()
        }),
        "the held submission jumps the queue"
    );
    assert!(!ui.dispatch_held, "the submission releases the hold");
    for c in "later".chars() {
        handle_deck_key(ch(c), &model, &mut ui);
    }
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Enqueue {
            text: "later".into()
        }),
        "after the hold clears, submissions append as usual"
    );
}

#[test]
fn agents_tab_controls_fire_only_when_composer_empty() {
    let model = model_with(&["lead", "sub"]);
    let mut ui = ready_ui();
    ui.tab = DeckTab::Agents;
    ui.focused = 1;
    // 's' with empty composer → Stop control for the focused agent.
    assert_eq!(
        handle_deck_key(ch('s'), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Control {
            agent: "sub".into(),
            control: AgentControl::Stop,
        })
    );
    // With text typed, 's' types instead.
    handle_deck_key(ch('h'), &model, &mut ui);
    handle_deck_key(ch('s'), &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "hs");
}

#[test]
fn agents_updown_moves_focus_and_enter_opens_session() {
    let model = model_with(&["a", "b", "c"]);
    let mut ui = ready_ui();
    ui.tab = DeckTab::Agents;
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(ui.focused, 2);
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    assert_eq!(ui.focused, 1);
    handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Session);
}

#[test]
fn focused_scope_gate_routes_decision_to_that_agent() {
    let mut model = model_with(&["lead"]);
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::ScopeReview {
            proposal: stella_protocol::ScopeProposal {
                summary: "big".into(),
                steps: vec![],
                estimated_files: 3,
                estimated_cost_usd: None,
            },
        },
    });
    let mut ui = ready_ui();
    let action = handle_deck_key(ch('a'), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::ToAgent {
            agent: "lead".into(),
            input: UserInput::ScopeDecision(ScopeDecision::Approve),
        })
    );
}

#[test]
fn scope_decision_latches_until_a_fresh_review_rearms() {
    let scope_review = Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::ScopeReview {
            proposal: stella_protocol::ScopeProposal {
                summary: "big".into(),
                steps: vec![],
                estimated_files: 3,
                estimated_cost_usd: None,
            },
        },
    };
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&scope_review, &mut model, &mut ui);

    assert_eq!(
        handle_deck_key(ch('a'), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::ToAgent {
            agent: "lead".into(),
            input: UserInput::ScopeDecision(ScopeDecision::Approve),
        })
    );
    // The gate stays pending until the engine's follow-on event, but the
    // latch keeps a second press from re-sending — it types instead.
    assert!(model.agents[0].model.pending_scope_review.is_some());
    handle_deck_key(ch('a'), &model, &mut ui);
    assert_eq!(
        ui.composer.buffer(),
        "a",
        "second press types, never re-sends"
    );

    // A FRESH review re-arms the decision keys.
    handle_deck_key(key(KeyCode::Backspace), &model, &mut ui);
    ingest_inbound(&scope_review, &mut model, &mut ui);
    assert_eq!(
        handle_deck_key(ch('x'), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::ToAgent {
            agent: "lead".into(),
            input: UserInput::ScopeDecision(ScopeDecision::Abort),
        }),
        "a new card re-arms the decision keys"
    );
}

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

#[test]
fn tab_and_backtab_walk_the_tab_bar() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    assert_eq!(ui.tab, DeckTab::Session);

    handle_deck_key(key(KeyCode::Tab), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Agents);
    handle_deck_key(key(KeyCode::BackTab), &model, &mut ui);
    assert_eq!(ui.tab, DeckTab::Session);

    // Re-selecting the active tab is a no-op, not an error.
    ui.set_tab(DeckTab::Session);
    assert_eq!(ui.tab, DeckTab::Session);
}

#[test]
fn traces_filter_cycles_through_agents_and_back() {
    let model = model_with(&["a", "b"]);
    let mut ui = ready_ui();
    ui.tab = DeckTab::Traces;
    assert_eq!(ui.trace_filter, None);
    handle_deck_key(ch('f'), &model, &mut ui);
    assert_eq!(ui.trace_filter.as_deref(), Some("a"));
    handle_deck_key(ch('f'), &model, &mut ui);
    assert_eq!(ui.trace_filter.as_deref(), Some("b"));
    handle_deck_key(ch('f'), &model, &mut ui);
    assert_eq!(ui.trace_filter, None);
}
