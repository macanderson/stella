//! Transcript navigation on the Session tab: the ⌃F search bar, the ⌃N/⌃P
//! failure seek, and ⌃Z turn folding.
//!
//! The pure answers ("where are the failures", "which entries match") are
//! pinned in [`crate::transcript_nav`]. What is pinned here is the wiring the
//! reader actually touches: which key claims which state, and what survives a
//! keystroke that was meant for something else.

use super::*;

/// Dispatch a prompt, opening a new turn in `agent`'s transcript.
fn prompt(m: &mut WorkspaceModel, agent: &str, text: &str) {
    m.apply_inbound(&Inbound::PromptStarted {
        agent: agent.into(),
        text: text.into(),
    });
}

/// Push one tool call and its result. `ok` decides whether the result is a
/// failure — the thing ⌃N seeks — and `body` is the output search reads.
fn tool_call(m: &mut WorkspaceModel, agent: &str, id: &str, body: &str, ok: bool) {
    use stella_protocol::{ToolCall, ToolOutput};
    m.apply_inbound(&Inbound::Event {
        agent: agent.into(),
        event: AgentEvent::ToolStart {
            call: ToolCall {
                call_id: id.into(),
                name: "run_command".into(),
                input: serde_json::json!({ "path": "src/main.rs" }),
            },
        },
    });
    m.apply_inbound(&Inbound::Event {
        agent: agent.into(),
        event: AgentEvent::ToolResult {
            call_id: id.into(),
            output: if ok {
                ToolOutput::Ok {
                    content: body.into(),
                }
            } else {
                ToolOutput::Error {
                    message: body.into(),
                }
            },
            duration_ms: 20,
            speculated: false,
        },
    });
}

/// Settle the turn in flight — only a settled turn may fold.
fn complete(m: &mut WorkspaceModel, agent: &str) {
    m.apply_inbound(&Inbound::Event {
        agent: agent.into(),
        event: AgentEvent::Complete {
            model: "m".into(),
            cost_usd: 0.0,
        },
    });
}

/// Two settled turns. Entries 0–3 are the first (its tool output the only
/// place `zebra` appears anywhere in the session), 4–7 the second.
fn model_with_two_turns() -> WorkspaceModel {
    let mut m = model_with(&["lead"]);
    prompt(&mut m, "lead", "first task");
    tool_call(&mut m, "lead", "c1", "panic: zebra lock timeout", true);
    complete(&mut m, "lead");
    prompt(&mut m, "lead", "second task");
    tool_call(&mut m, "lead", "c2", "all clear", true);
    complete(&mut m, "lead");
    m
}

/// The turns of the focused agent's transcript, for asserting fold state.
fn turns_of(model: &WorkspaceModel) -> Vec<crate::transcript_nav::Turn> {
    crate::transcript_nav::turns(&model.agents[0].model.transcript)
}

/// A transcript whose only matches for `alpha` sit at entries 2, 9 and 15,
/// with `alpha p` narrowing to {9, 15} and `alpha q` to {2, 9} — the two
/// shapes that tell re-anchoring apart from resetting to the first hit.
fn model_with_spread_matches() -> WorkspaceModel {
    let mut m = model_with(&["lead"]);
    for i in 0..16 {
        let reason = match i {
            2 => "alpha q",
            9 => "alpha p alpha q",
            15 => "alpha p",
            _ => "filler",
        };
        m.apply_inbound(&Inbound::Event {
            agent: "lead".into(),
            event: AgentEvent::Retry {
                attempt: i as u32,
                reason: reason.into(),
            },
        });
    }
    m
}

/// Type `text` into whatever currently owns the keyboard.
fn type_str(text: &str, model: &WorkspaceModel, ui: &mut DeckUi) {
    for c in text.chars() {
        handle_deck_key(ch(c), model, ui);
    }
}

/// A letter typed into the search bar is a search letter, full stop. The bar
/// is claimed high in `handle_deck_key` for exactly this reason: a reader
/// hunting for `auth.rs` mid-session must not discover afterwards that they
/// dictated `auth.rs` into a half-written prompt.
#[test]
fn the_search_bar_is_modal_over_the_composer() {
    let model = model_with_two_turns();
    let mut ui = ready_ui();
    handle_deck_key(ctrl('f'), &model, &mut ui);
    assert!(ui.search.open, "⌃F raises the bar on the Session tab");

    type_str("zebra", &model, &mut ui);
    assert_eq!(ui.search.query, "zebra", "the letters land in the query");
    assert!(
        ui.composer.buffer().is_empty(),
        "and never leak into the prompt"
    );
}

/// Typing another character must feel like *narrowing*, not like starting
/// over: the reader is already looking at a match, and the one they were
/// looking at is the one they want to keep looking at if it survives.
#[test]
fn narrowing_the_query_re_anchors_onto_the_surviving_match() {
    let model = model_with_spread_matches();
    let mut ui = ready_ui();
    handle_deck_key(ctrl('f'), &model, &mut ui);
    type_str("alpha ", &model, &mut ui);
    assert_eq!(ui.search.hits, vec![2, 9, 15], "three matches, spread out");

    // Walk to the middle match — the reader's place in the transcript.
    handle_deck_key(key(KeyCode::Down), &model, &mut ui);
    assert_eq!(ui.search.current(), Some(9));

    // Narrowing that drops the FIRST hit: the surviving anchor is now the
    // head of the list, so the reader does not move.
    type_str("p", &model, &mut ui);
    assert_eq!(ui.search.hits, vec![9, 15]);
    assert_eq!(ui.search.current(), Some(9), "still on the same entry");

    // Widening again re-finds it rather than snapping to the top.
    handle_deck_key(key(KeyCode::Backspace), &model, &mut ui);
    assert_eq!(ui.search.hits, vec![2, 9, 15]);
    assert_eq!(ui.search.current(), Some(9));

    // The discriminating case: narrowing that drops the LAST hit leaves the
    // anchor in the middle of the list. Resetting the cursor to 0 would jump
    // the reader backwards to entry 2; re-anchoring holds them at 9.
    type_str("q", &model, &mut ui);
    assert_eq!(ui.search.hits, vec![2, 9]);
    assert_eq!(ui.search.cursor, 1, "the cursor followed the entry, not 0");
    assert_eq!(ui.search.current(), Some(9));
    assert_eq!(ui.session_selected, Some(9), "and the transcript with it");
}

/// Esc dismisses the bar, not the journey. The reader searched their way to
/// this row on purpose — snapping the transcript back to where they started
/// would throw away the only thing the search was for.
#[test]
fn esc_closes_the_search_bar_but_keeps_the_row_it_found() {
    let model = model_with_two_turns();
    let mut ui = ready_ui();
    handle_deck_key(ctrl('f'), &model, &mut ui);
    type_str("zebra", &model, &mut ui);
    assert_eq!(ui.session_selected, Some(2), "the search moved the reader");

    let action = handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled);
    assert!(!ui.search.open, "the bar is down");
    assert_eq!(
        ui.session_selected,
        Some(2),
        "and the transcript stayed where it was searched to"
    );
}

/// ⌃N means "next match" to a reader looking at a search bar, and "next
/// failure" only to one who is not. The bar has to claim the chord ahead of
/// the transcript behind it — otherwise its own hint line ("⏎ next · ⌃P
/// prev") advertises a key that silently walks off to somewhere else, and the
/// reader loses their place in the match list to a row they never asked for.
#[test]
fn ctrl_n_steps_matches_while_the_bar_is_open_instead_of_seeking_failures() {
    let mut model = model_with(&["lead"]);
    prompt(&mut model, "lead", "run the suite");
    tool_call(&mut model, "lead", "c1", "zebra one", true);
    tool_call(&mut model, "lead", "c2", "exit status 1", false);
    tool_call(&mut model, "lead", "c3", "zebra two", true);
    complete(&mut model, "lead");
    let mut ui = ready_ui();

    handle_deck_key(ctrl('f'), &model, &mut ui);
    type_str("zebra", &model, &mut ui);
    assert_eq!(
        ui.search.hits,
        vec![2, 6],
        "two matches, with the failure at 4 sitting between them"
    );
    assert_eq!(ui.search.cursor, 0);

    handle_deck_key(ctrl('n'), &model, &mut ui);
    assert!(ui.search.open, "the bar keeps the keyboard");
    assert_eq!(ui.search.cursor, 1, "⌃N stepped along the match list");
    assert_eq!(
        ui.session_selected,
        Some(6),
        "to the next match — never to the failure at 4"
    );

    // ⌃P walks back over the same list, not to the previous failure.
    handle_deck_key(ctrl('p'), &model, &mut ui);
    assert_eq!(ui.search.cursor, 0);
    assert_eq!(ui.session_selected, Some(2));
}

/// With no bar up, ⌃N is the "take me to what broke" key — a different
/// question from "take me to the next match", and answered against the
/// failures rather than against a query.
#[test]
fn ctrl_n_with_the_bar_closed_seeks_the_next_failure() {
    let mut model = model_with(&["lead"]);
    prompt(&mut model, "lead", "run the suite");
    tool_call(&mut model, "lead", "c1", "ok", true);
    tool_call(&mut model, "lead", "c2", "exit status 1", false);
    complete(&mut model, "lead");
    let mut ui = ready_ui();
    assert!(!ui.search.open);

    handle_deck_key(ctrl('n'), &model, &mut ui);
    let sel = ui.session_selected.expect("⌃N landed somewhere");
    assert!(
        matches!(
            model.agents[0].model.transcript[sel],
            crate::model::TranscriptEntry::ToolResult { ok: false, .. }
        ),
        "on the failed result, not on the successful one: {:?}",
        model.agents[0].model.transcript[sel]
    );
    assert_eq!(
        ui.session_pending_scroll,
        Some(sel),
        "and asked the renderer to bring it into view"
    );
}

/// Nothing went wrong, so nothing moves. Reporting that by leaving the
/// selection alone is honest; jumping somewhere arbitrary would read as
/// "here is the failure" about a row that is fine.
#[test]
fn ctrl_n_on_a_clean_transcript_leaves_the_selection_alone() {
    let mut model = model_with(&["lead"]);
    prompt(&mut model, "lead", "run the suite");
    tool_call(&mut model, "lead", "c1", "ok", true);
    complete(&mut model, "lead");
    let mut ui = ready_ui();
    ui.session_selected = Some(1);

    handle_deck_key(ctrl('n'), &model, &mut ui);
    assert_eq!(ui.session_selected, Some(1), "the highlight did not move");
    assert_eq!(
        ui.session_pending_scroll, None,
        "and no scroll was queued for a row nobody asked to see"
    );
}

/// The mirror of ⌃O's two modes: with nothing highlighted the key is about
/// the whole transcript, and pressing it twice must put the reader back
/// exactly where they were rather than leaving them in a third state.
#[test]
fn ctrl_z_with_no_selection_toggles_the_fold_all_overlay() {
    let model = model_with_two_turns();
    let mut ui = ready_ui();
    assert_eq!(ui.session_selected, None);

    let rev = ui.fold_rev;
    handle_deck_key(ctrl('z'), &model, &mut ui);
    assert!(ui.fold_all_turns, "every settled turn folds at once");
    assert!(ui.fold_rev > rev, "the fold cache is invalidated");

    handle_deck_key(ctrl('z'), &model, &mut ui);
    assert!(!ui.fold_all_turns, "a second press restores the open view");
    let turns = turns_of(&model);
    assert!(
        turns.iter().all(|t| !is_folded(&ui, "lead", t)),
        "and leaves no turn folded behind the overlay"
    );
}

/// Every way into a collapsed view needs a way back out, and Esc is the one
/// the hint line promises. It peels off the *overlay* only: a turn the reader
/// folded by hand is a decision they made, not part of the blanket they are
/// dismissing, so it must still be folded when the overlay is gone.
#[test]
fn esc_lifts_the_fold_all_overlay_without_forgetting_hand_folded_turns() {
    let model = model_with_two_turns();
    let mut ui = ready_ui();

    // Fold the second turn by hand, then drop the blanket over everything.
    ui.session_selected = Some(5);
    handle_deck_key(ctrl('z'), &model, &mut ui);
    ui.session_selected = None;
    handle_deck_key(ctrl('z'), &model, &mut ui);
    assert!(ui.fold_all_turns);

    let rev = ui.fold_rev;
    let action = handle_deck_key(key(KeyCode::Esc), &model, &mut ui);
    assert_eq!(action, DeckAction::Handled, "Esc is claimed by the overlay");
    assert!(!ui.fold_all_turns, "the blanket lifts");
    assert!(ui.fold_rev > rev, "and the fold cache is invalidated");

    let turns = turns_of(&model);
    assert!(
        !is_folded(&ui, "lead", &turns[0]),
        "the turn only the overlay was folding is open again"
    );
    assert!(
        is_folded(&ui, "lead", &turns[1]),
        "but the one the reader folded by hand stays folded"
    );
}

/// Folding the turn that is still arriving would hide the very thing the
/// reader is watching, so ⌃Z declines — and declines *silently*, without
/// bumping the revision that invalidates the settled fold cache.
#[test]
fn ctrl_z_refuses_to_fold_the_turn_still_in_flight() {
    let mut model = model_with(&["lead"]);
    prompt(&mut model, "lead", "still working");
    tool_call(&mut model, "lead", "c1", "half done", true);
    // No Complete: the turn has not come to rest.
    let mut ui = ready_ui();
    ui.session_selected = Some(2);

    let turns = turns_of(&model);
    assert!(!turns[0].finished, "fixture check: the turn is in flight");
    let rev = ui.fold_rev;
    handle_deck_key(ctrl('z'), &model, &mut ui);

    assert!(
        !is_folded(&ui, "lead", &turns[0]),
        "the in-flight turn stayed open"
    );
    assert!(
        ui.folded_turns.get("lead").is_none_or(|s| s.is_empty()),
        "and nothing was recorded about it"
    );
    assert_eq!(
        ui.fold_rev, rev,
        "a no-op press must not invalidate the settled fold cache"
    );
}

/// Search outranks folding. A reader who asked for a thing by name has said
/// they want to see it, so scrolling to a row that is folded away — and
/// showing them a digest line instead — would be a lie about what was found.
#[test]
fn a_match_inside_a_folded_turn_unfolds_it() {
    let model = model_with_two_turns();
    let mut ui = ready_ui();

    // Fold the first turn from a row inside it.
    ui.session_selected = Some(2);
    handle_deck_key(ctrl('z'), &model, &mut ui);
    let turns = turns_of(&model);
    assert!(
        is_folded(&ui, "lead", &turns[0]),
        "fixture check: the turn is folded"
    );

    // `zebra` appears only in that turn's collapsed tool output.
    handle_deck_key(ctrl('f'), &model, &mut ui);
    type_str("zebra", &model, &mut ui);

    assert_eq!(ui.search.hits, vec![2], "one match, inside the folded turn");
    assert!(
        !is_folded(&ui, "lead", &turns[0]),
        "finding it opened the turn holding it"
    );
    assert_eq!(ui.session_selected, Some(2), "and selected the match");
    assert_eq!(ui.session_pending_scroll, Some(2));
}

/// Fold sets are keyed on turn-start indices, which front-eviction shifts by
/// the same amount it shifts everything else. A surviving set would silently
/// re-attach to whichever turn slid into the slot — folding away a turn the
/// reader never touched — so it drops with the expansion set it travels with.
#[test]
fn eviction_drops_fold_state_alongside_expansions() {
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
    for i in 0..(MAX_TRANSCRIPT_ENTRIES - 1) {
        ingest_inbound(&retry(i), &mut model, &mut ui);
    }
    ui.folded_turns
        .entry("lead".to_string())
        .or_default()
        .insert(0);
    toggle_expanded(&mut ui, "lead", MAX_TRANSCRIPT_ENTRIES - 2);
    let (fold_rev, expanded_rev) = (ui.fold_rev, ui.expanded_rev);

    // One more event crosses the cap: a chunk of the front evicts.
    ingest_inbound(&retry(MAX_TRANSCRIPT_ENTRIES), &mut model, &mut ui);
    assert!(
        model.agents[0].model.transcript.len() < MAX_TRANSCRIPT_ENTRIES,
        "a chunk was evicted"
    );
    assert!(
        !ui.folded_turns.contains_key("lead"),
        "index-keyed folds are stale once the front moved"
    );
    assert!(
        !ui.expanded.contains_key("lead"),
        "…and go with the expansions, which shifted identically"
    );
    assert!(ui.fold_rev > fold_rev, "the fold cache is invalidated");
    assert!(ui.expanded_rev > expanded_rev);
}
