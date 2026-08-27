//! Session-transcript selection and expansion: the ↑/↓ highlight, ⌃O expand
//! (per-row and the expand-all overlay), the bracket end-jumps, and what a
//! focus change, a deregistration or a front-eviction do to a live selection.

use super::*;

/// A settled `delete_file` exchange on `agent` — the event whose head carries
/// the `· git-backed · u undo` affordance.
fn with_delete_exchange(m: &mut WorkspaceModel, agent: &str) {
    use stella_protocol::{ToolCall, ToolOutput};
    m.apply_inbound(&Inbound::Event {
        agent: agent.into(),
        event: AgentEvent::ToolStart {
            call: ToolCall {
                call_id: "d1".into(),
                name: "delete_file".into(),
                input: serde_json::json!({ "path": "src/old.rs" }),
            },
            sub_agent_id: None,
            task_id: None,
        },
    });
    m.apply_inbound(&Inbound::Event {
        agent: agent.into(),
        event: AgentEvent::ToolResult {
            call_id: "d1".into(),
            output: ToolOutput::Ok {
                content: "deleted src/old.rs".into(),
                data: None,
            },
            duration_ms: 3,
            speculated: false,
            sub_agent_id: None,
            task_id: None,
        },
    });
}

/// One logged memory on `agent` — the event whose footer carries the
/// `· x reject` affordance.
fn with_logged_memory(m: &mut WorkspaceModel, agent: &str) {
    m.apply_inbound(&Inbound::Event {
        agent: agent.into(),
        event: AgentEvent::MemoryLogged {
            memory_id: "nod_83b3f1d29a".into(),
            text: "dedup keys must be stable across runs".into(),
            class: stella_protocol::MemoryClass::Observation,
            confidence: 62,
            kind: "domain".into(),
            decays: false,
            promotes_at: 85,
            task_id: None,
        },
    });
}

/// **The witness (#5231).** `e` on a logged memory hands its words to the
/// composer and latches the edit, and the submission leaves as
/// [`WorkspaceInput::EditMemory`] rather than as a prompt.
///
/// The second half is what makes this more than a paste. A transcript row holds no buffer
/// and the deck's one buffer is the composer, so `e` has to borrow it — and
/// without the latch a reader who rewrote a memory would have dispatched a
/// *turn* saying the new words instead of storing them. Asserting only that
/// the text reaches the composer would pass on that bug.
#[test]
fn e_on_a_highlighted_memory_edits_it_rather_than_prompting() {
    let mut model = model_with(&["lead"]);
    with_logged_memory(&mut model, "lead");
    let mut ui = ready_ui();

    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    let action = handle_deck_key(key(KeyCode::Char('e')), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Handled,
        "e opens the edit, sends nothing"
    );
    assert_eq!(
        ui.composer.buffer(),
        "dedup keys must be stable across runs",
        "the memory's current words are what a reader edits"
    );
    assert_eq!(ui.editing_memory.as_deref(), Some("nod_83b3f1d29a"));

    // Replace the words and submit.
    ui.composer.clear();
    for c in "dedup keys are stable across runs".chars() {
        ui.composer.insert_char(c);
    }
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::EditMemory {
            memory_id: "nod_83b3f1d29a".into(),
            text: "dedup keys are stable across runs".into(),
        }),
        "the submission must store the memory, not run a turn saying it"
    );
    assert!(
        ui.editing_memory.is_none(),
        "the latch is spent by the submission"
    );
}

/// `e` with a non-blank composer types the letter, and Esc abandons the edit.
///
/// The first is the constraint that made this more than a keybinding: a bare
/// `e` must not steal a keystroke from someone mid-prompt. The second is
/// SPEC 13 — every overlay closes on Esc — and it has to clear the borrowed
/// buffer too, or a reader who backed out is left holding words whose next
/// `⏎` still rewrites the memory.
#[test]
fn e_does_not_steal_a_keystroke_and_esc_abandons_the_edit() {
    let mut model = model_with(&["lead"]);
    with_logged_memory(&mut model, "lead");

    let mut ui = ready_ui();
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    for c in "wri".chars() {
        ui.composer.insert_char(c);
    }
    handle_deck_key(key(KeyCode::Char('e')), &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "wrie", "e mid-prompt types");
    assert!(ui.editing_memory.is_none(), "and latches nothing");

    let mut ui = ready_ui();
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    handle_deck_key(key(KeyCode::Char('e')), &model, &mut ui);
    assert!(ui.editing_memory.is_some());
    handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert!(ui.editing_memory.is_none(), "Esc abandons the edit");
    assert_eq!(ui.composer.buffer(), "", "and clears the words it borrowed");
}

/// An empty submission leaves the memory alone rather than blanking it.
///
/// `⏎` on nothing is how a reader backs out of a buffer, and a memory with no
/// words steers nothing while still being recalled — so the empty case is the
/// one that must not reach the store. `run_memory_edit` refuses it too; this
/// keeps the deck from asking.
#[test]
fn an_empty_edit_submission_leaves_the_memory_as_it_was() {
    let mut model = model_with(&["lead"]);
    with_logged_memory(&mut model, "lead");
    let mut ui = ready_ui();

    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    handle_deck_key(key(KeyCode::Char('e')), &model, &mut ui);
    ui.composer.clear();
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled, "nothing is sent");
    assert!(ui.editing_memory.is_none(), "and the latch is released");
}

/// **The witness (#5032).** With a logged memory highlighted, `x` sends
/// [`WorkspaceInput::RejectMemory`] naming the memory *and its text* — the
/// row's own `· x reject` affordance. With anything else highlighted the same
/// key stays typing and lands in the composer.
///
/// The text is asserted, not just the id: the driver's tombstone is
/// content-addressed as well as id-addressed, so a rejection that travelled
/// with the id alone would be undone by the next turn that re-learned the
/// same lesson under a fresh one — which is the loop's ordinary behaviour, not
/// an edge case.
#[test]
fn x_on_a_highlighted_memory_sends_the_rejection_and_otherwise_types() {
    let mut model = model_with(&["lead"]);
    with_logged_memory(&mut model, "lead");
    let mut ui = ready_ui();

    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    let action = handle_deck_key(key(KeyCode::Char('x')), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::RejectMemory {
            memory_id: "nod_83b3f1d29a".into(),
            text: "dedup keys must be stable across runs".into(),
        }),
        "x on a memory row must reject that memory by id and by content"
    );

    // A non-memory highlight leaves `x` to the composer: it types.
    let mut model = model_with(&["lead"]);
    with_tool_exchange(&mut model, "lead");
    let mut ui = ready_ui();
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    handle_deck_key(key(KeyCode::Char('x')), &model, &mut ui);
    assert_eq!(
        ui.composer.buffer(),
        "x",
        "x with a non-memory highlight falls through to typing"
    );

    // And a draft in the composer outranks the affordance, exactly as `u`'s
    // guard does: someone mid-sentence must not lose the letter.
    let mut model = model_with(&["lead"]);
    with_logged_memory(&mut model, "lead");
    let mut ui = ready_ui();
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    handle_deck_key(key(KeyCode::Char('f')), &model, &mut ui);
    handle_deck_key(key(KeyCode::Char('x')), &model, &mut ui);
    assert_eq!(
        ui.composer.buffer(),
        "fx",
        "x with a draft in the composer types rather than rejecting"
    );
}

/// The `u` binding (SPEC 11): with a delete event highlighted — its result
/// row or its head — `u` sends [`WorkspaceInput::UndoDelete`] naming the
/// deleted path; with anything else highlighted the same key stays typing and
/// lands in the composer. The row has rendered `· u undo` since the head
/// landed; this is the half that makes the label true (#5036).
#[test]
fn u_on_a_highlighted_delete_sends_the_undo_and_otherwise_types() {
    let mut model = model_with(&["lead"]);
    with_delete_exchange(&mut model, "lead");
    let mut ui = ready_ui();

    // ↑ lands on the newest entry — the delete's *result* — and `u` still
    // resolves it back to the call it settles.
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    let action = handle_deck_key(key(KeyCode::Char('u')), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::UndoDelete {
            paths: vec!["src/old.rs".into()]
        }),
        "u on the delete's result row sends the undo"
    );

    // On the head itself, the same.
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    let action = handle_deck_key(key(KeyCode::Char('u')), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::UndoDelete {
            paths: vec!["src/old.rs".into()]
        }),
        "u on the delete's head sends the undo"
    );

    // A non-delete highlight leaves `u` to the composer: it types.
    let mut model = model_with(&["lead"]);
    with_tool_exchange(&mut model, "lead"); // read_file, not delete_file
    let mut ui = ready_ui();
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    handle_deck_key(key(KeyCode::Char('u')), &model, &mut ui);
    assert_eq!(
        ui.composer.buffer(),
        "u",
        "u with a non-delete highlight falls through to typing"
    );
}

/// A verify turn's gate board on `agent`, one gate red.
fn with_failed_gate_board(m: &mut WorkspaceModel, agent: &str) {
    use stella_protocol::{GateBoard, GateRow, GateState};
    m.apply_inbound(&Inbound::Event {
        agent: agent.into(),
        event: AgentEvent::GateBoard {
            board: GateBoard {
                patch: Some("patch-7".into()),
                gates: vec![
                    GateRow {
                        name: "fmt".into(),
                        state: GateState::Green,
                        deterministic: true,
                    },
                    GateRow {
                        name: "tests".into(),
                        state: GateState::Failed {
                            case: "a_short_cycle_is_detected".into(),
                            log: "left: 3\nright: 2\nbacktrace elided".into(),
                        },
                        deterministic: true,
                    },
                ],
            },
        },
    });
}

/// SPEC 8.1's `l full log`: with a failed gate board highlighted, `l` opens
/// its failure block to the whole log and closes it again; with anything else
/// highlighted the same key stays typing. The row has rendered `l full log`
/// since the board landed; this is the half that makes the label true.
#[test]
fn l_on_a_failed_gate_board_opens_the_log_and_otherwise_types() {
    let mut model = model_with(&["lead"]);
    with_failed_gate_board(&mut model, "lead");
    let mut ui = ready_ui();

    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    let sel = ui.session_selected.expect("up highlights the board");
    let action = handle_deck_key(key(KeyCode::Char('l')), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled, "l did not claim the keystroke");
    assert!(
        ui.expanded
            .get("lead")
            .is_some_and(|set| set.contains(&sel)),
        "l did not open the failure block"
    );
    // And closes it: every way in has a way out, which is what the block's own
    // `l fold log` row promises once it is open.
    handle_deck_key(key(KeyCode::Char('l')), &model, &mut ui);
    assert!(
        ui.expanded
            .get("lead")
            .is_none_or(|set| !set.contains(&sel)),
        "l did not close the failure block again"
    );
    assert!(
        ui.composer.is_blank(),
        "l reached the composer while claiming the keystroke"
    );

    // A non-board highlight leaves `l` to the composer: it types.
    let mut model = model_with(&["lead"]);
    with_tool_exchange(&mut model, "lead");
    let mut ui = ready_ui();
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    handle_deck_key(key(KeyCode::Char('l')), &model, &mut ui);
    assert_eq!(
        ui.composer.buffer(),
        "l",
        "l with a non-board highlight falls through to typing"
    );
}

/// SPEC 8.1's `r rerun gate`: with a failed gate board highlighted, `r` sends
/// [`WorkspaceInput::RerunGate`] naming the failed gate — not the green one
/// above it — and with anything else highlighted the same key types.
#[test]
fn r_on_a_failed_gate_board_re_requests_the_gate_and_otherwise_types() {
    let mut model = model_with(&["lead"]);
    with_failed_gate_board(&mut model, "lead");
    let mut ui = ready_ui();

    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    let action = handle_deck_key(key(KeyCode::Char('r')), &model, &mut ui);
    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::RerunGate {
            gate: "tests".into()
        }),
        "r must name the gate that failed, never the first gate on the board"
    );

    // A board with nothing red has no gate to re-request, so `r` types.
    let mut model = model_with(&["lead"]);
    model.apply_inbound(&Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::GateBoard {
            board: stella_protocol::GateBoard {
                patch: None,
                gates: vec![stella_protocol::GateRow {
                    name: "fmt".into(),
                    state: stella_protocol::GateState::Green,
                    deterministic: true,
                }],
            },
        },
    });
    let mut ui = ready_ui();
    handle_deck_key(key(KeyCode::Up), &model, &mut ui);
    handle_deck_key(key(KeyCode::Char('r')), &model, &mut ui);
    assert_eq!(
        ui.composer.buffer(),
        "r",
        "r on an all-green board falls through to typing"
    );
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

    // Focus another agent: the selection indexes the *previous* agent's
    // transcript and must not carry across.
    ui.focus_agent(1);
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
