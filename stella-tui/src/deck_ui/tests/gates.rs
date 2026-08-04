//! The focused-agent gates: the scope-review card and the `ask_user` question,
//! and specifically who owns the reviewer's *submission* while one is pending.
//!
//! The rule both gates follow: a pending, unanswered card claims the submit
//! chord. Without that, typing is a way to make a gate unanswerable — the deck
//! reads a mid-turn submission as a new request and spawns a sidecar
//! sub-session for it, leaving the gate parked with nobody answering it.

use super::*;

/// A pending scope card, raised on the lead.
fn scope_card() -> Inbound {
    Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::ScopeReview {
            proposal: stella_protocol::ScopeProposal {
                summary: "8 steps to accomplish: add issue keybindings".into(),
                steps: vec!["s1".into(), "s2".into()],
                estimated_files: 8,
                estimated_cost_usd: None,
            },
        },
    }
}

fn type_str(s: &str, model: &WorkspaceModel, ui: &mut DeckUi) {
    for c in s.chars() {
        handle_deck_key(ch(c), model, ui);
    }
}

/// The reported bug, as a test. A reviewer typed a line at the scope card and
/// hit ⏎; the deck read it as a new mid-turn request and enqueued it, which the
/// driver drains into a sidecar sub-session ("req:1 started in parallel…") while
/// the gate stays parked. The submission belongs to the gate.
#[test]
fn a_typed_note_at_a_scope_card_answers_the_card_instead_of_spawning_a_sidecar() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&scope_card(), &mut model, &mut ui);

    type_str("only the ctrl+O dialog", &model, &mut ui);
    let action = handle_deck_key(key(KeyCode::Enter), &model, &mut ui);

    assert_eq!(
        action,
        DeckAction::Send(WorkspaceInput::ToAgent {
            agent: "lead".into(),
            input: UserInput::ScopeDecision(ScopeDecision::Revise {
                note: "only the ctrl+O dialog".into()
            }),
        }),
        "the note answers the review; it must never become an Enqueue"
    );
    assert!(
        !matches!(action, DeckAction::Send(WorkspaceInput::Enqueue { .. })),
        "an Enqueue here is what the driver turns into a sidecar sub-session"
    );
}

/// The literal second submission from the report: the reviewer typed "ok",
/// meaning approve. It spawned a second sidecar instead.
#[test]
fn typing_ok_at_a_scope_card_approves_it() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&scope_card(), &mut model, &mut ui);

    type_str("ok", &model, &mut ui);
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::ToAgent {
            agent: "lead".into(),
            input: UserInput::ScopeDecision(ScopeDecision::Approve),
        })
    );
}

/// A note can span lines: the newline chord composes, only the submit chord
/// answers. Otherwise a multi-line revision would be impossible to write.
#[test]
fn the_newline_chord_composes_a_multi_line_note_without_answering() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&scope_card(), &mut model, &mut ui);

    type_str("only the dialog", &model, &mut ui);
    assert_eq!(
        handle_deck_key(cmd_enter(), &model, &mut ui),
        DeckAction::Handled,
        "the newline chord edits — it does not answer the card"
    );
    type_str("and nothing else", &model, &mut ui);
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::ToAgent {
            agent: "lead".into(),
            input: UserInput::ScopeDecision(ScopeDecision::Revise {
                note: "only the dialog\nand nothing else".into()
            }),
        })
    );
}

/// `!` is a shell command even while a gate is pending — the same carve-out
/// `ask_user` makes. A reviewer checking `!git status` before deciding must not
/// have it read as their revision note.
#[test]
fn a_shell_line_still_runs_while_a_scope_card_is_pending() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&scope_card(), &mut model, &mut ui);

    type_str("!git status", &model, &mut ui);
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Shell("git status".into())
    );
    assert!(
        !ui.scope_answered.contains("lead"),
        "running a shell command must not answer the review"
    );
}

/// An empty submit is not an answer — the card stays up rather than sending a
/// blank note the planner would have nothing to do with.
#[test]
fn an_empty_submit_at_a_scope_card_keeps_the_card_up() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&scope_card(), &mut model, &mut ui);

    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Ignored
    );
    assert!(!ui.scope_answered.contains("lead"));
}

/// Whitespace-only is the same case, after the composer has been drained.
#[test]
fn a_whitespace_only_note_keeps_the_card_up() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&scope_card(), &mut model, &mut ui);

    type_str("   ", &model, &mut ui);
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Ignored
    );
    assert!(!ui.scope_answered.contains("lead"));
}

/// No letter answers the card, from any composer state — the rule that lets a
/// note begin with any word at all. `a`/`t`/`x` are ordinary prompt characters,
/// and a card that claimed them from a live text field turned a note opening
/// "also do X" into a silent approve of an eight-step plan.
#[test]
fn no_bare_letter_answers_the_scope_card() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&scope_card(), &mut model, &mut ui);

    // From an EMPTY composer — the keystroke that used to commit.
    for c in ['a', 't', 'x'] {
        assert_eq!(
            handle_deck_key(ch(c), &model, &mut ui),
            DeckAction::Handled,
            "{c} types; it must not answer the card on its own"
        );
    }
    assert_eq!(ui.composer.buffer(), "atx");
    assert!(!ui.scope_answered.contains("lead"));
}

/// The case that named the rule: "also do the tests" is a revision, and its
/// first letter used to approve the plan it was arguing with.
#[test]
fn a_note_opening_with_a_decision_letter_is_a_revision_not_an_approval() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&scope_card(), &mut model, &mut ui);

    type_str("also do the tests", &model, &mut ui);
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::ToAgent {
            agent: "lead".into(),
            input: UserInput::ScopeDecision(ScopeDecision::Revise {
                note: "also do the tests".into()
            }),
        })
    );
}

/// `Esc` is the one key that still acts alone — it cannot collide with prose,
/// and it stops work rather than starting it. It means the same thing with a
/// half-typed note in the composer: without that, "type a note, change your
/// mind, press Esc" fell past the card into the turn-stop chain, which for a
/// pipeline turn is a *hard cancel* — the same gesture ending the turn cleanly
/// or violently depending on whether the user had started typing.
#[test]
fn esc_aborts_the_card_whether_or_not_a_note_is_half_typed() {
    let abort = DeckAction::Send(WorkspaceInput::ToAgent {
        agent: "lead".into(),
        input: UserInput::ScopeDecision(ScopeDecision::Abort),
    });

    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&scope_card(), &mut model, &mut ui);
    assert_eq!(handle_deck_key(key(KeyCode::Esc), &model, &mut ui), abort);

    let mut ui = ready_ui();
    ingest_inbound(&scope_card(), &mut model, &mut ui);
    type_str("only the dial", &model, &mut ui);
    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        abort,
        "a half-typed note must not turn Esc into a turn cancel"
    );
}

/// With no card pending, a submission is still a prompt — the sidecar path is
/// correct behavior and must not be collateral damage of this fix.
#[test]
fn without_a_pending_card_a_submission_is_still_an_ordinary_prompt() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();

    type_str("go look at the graph", &model, &mut ui);
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::Enqueue {
            text: "go look at the graph".into()
        })
    );
}

// ---------------------------------------------------------------------------
// The STATE overlay (ctrl+s)
// ---------------------------------------------------------------------------

/// The complaint this overlay answers: an approved scope had no way back.
/// `⌃S` is the way back, and it toggles from anywhere.
#[test]
fn ctrl_s_opens_and_closes_the_state_overlay() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    assert!(!ui.state_open);
    assert_eq!(
        handle_deck_key(ctrl('s'), &model, &mut ui),
        DeckAction::Handled
    );
    assert!(ui.state_open, "ctrl+s opens it");
    assert_eq!(
        handle_deck_key(ctrl('s'), &model, &mut ui),
        DeckAction::Handled
    );
    assert!(!ui.state_open, "a second ctrl+s closes it");
}

/// Every way in has a way out — Esc closes it like every other overlay.
#[test]
fn esc_closes_the_state_overlay() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    handle_deck_key(ctrl('s'), &model, &mut ui);
    handle_deck_key(KeyEvent::from(KeyCode::Esc), &model, &mut ui);
    assert!(!ui.state_open);
}

/// Modal while open: a keystroke must not reach the composer behind it, or
/// opening the overlay to check a plan would quietly edit the prompt.
#[test]
fn the_state_overlay_owns_the_keyboard_while_it_is_up() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    handle_deck_key(ctrl('s'), &model, &mut ui);
    type_str("hello", &model, &mut ui);
    assert!(
        ui.composer.is_empty(),
        "typing leaked into the composer behind the overlay: {:?}",
        ui.composer.buffer()
    );
}

/// The collision this binding has to avoid: `ctrl+s` is the *save* chord in the
/// SKILLS and SETTINGS editors, and those are modal. The overlay is dispatched
/// after them on purpose — a save must never be stolen by a recall surface.
#[test]
fn ctrl_s_still_saves_inside_a_modal_editor() {
    let model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ui.tab = crate::deck_ui::DeckTab::Settings;
    ui.engine.focused = true;
    handle_deck_key(ctrl('s'), &model, &mut ui);
    assert!(
        !ui.state_open,
        "the STATE overlay stole the engine panel's save chord"
    );
}

// ── per-hunk approval (#1265) ───────────────────────────────────────────────

/// A two-hunk approval card, raised on the lead.
fn hunk_card() -> Inbound {
    Inbound::Event {
        agent: "lead".into(),
        event: AgentEvent::HunkReview {
            proposal: stella_protocol::HunkProposal {
                id: "hunk-review-1".into(),
                tool: "apply_edits".into(),
                hunks: vec![
                    stella_protocol::ProposedHunk {
                        path: "a.rs".into(),
                        diff: "@@ -1,1 +1,1 @@\n-a\n+A\n".into(),
                        lines_added: 1,
                        lines_removed: 1,
                    },
                    stella_protocol::ProposedHunk {
                        path: "a.rs".into(),
                        diff: "@@ -9,1 +9,1 @@\n-b\n+B\n".into(),
                        lines_added: 1,
                        lines_removed: 1,
                    },
                ],
            },
        },
    }
}

fn decision(accepted: Vec<usize>) -> DeckAction {
    DeckAction::Send(WorkspaceInput::ToAgent {
        agent: "lead".into(),
        input: UserInput::HunkDecision {
            id: "hunk-review-1".into(),
            accepted,
        },
    })
}

/// A fresh card arrives pre-marked accepted, so a bare ⏎ applies what the model
/// proposed — the same thing that would have happened with no gate at all.
#[test]
fn a_bare_enter_at_a_hunk_card_applies_every_hunk() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&hunk_card(), &mut model, &mut ui);

    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        decision(vec![0, 1])
    );
}

/// The issue's shape at the key layer: move to the second hunk, unmark it, and
/// the decision that goes out keeps exactly the first.
#[test]
fn space_unmarks_the_hunk_under_the_cursor() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&hunk_card(), &mut model, &mut ui);

    assert_eq!(
        handle_deck_key(key(KeyCode::Down), &model, &mut ui),
        DeckAction::Ignored
    );
    assert_eq!(
        handle_deck_key(ch(' '), &model, &mut ui),
        DeckAction::Ignored
    );
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        decision(vec![0])
    );
}

/// Esc declines everything — a real answer (apply nothing), not a fall-through
/// to the turn-stop chain, which for a pipeline turn is a hard cancel.
#[test]
fn esc_at_a_hunk_card_declines_every_hunk() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&hunk_card(), &mut model, &mut ui);

    assert_eq!(
        handle_deck_key(key(KeyCode::Esc), &model, &mut ui),
        decision(vec![])
    );
}

/// A typed selection is the other way to answer, for a reviewer who would
/// rather name the hunks than walk them.
#[test]
fn a_typed_selection_answers_the_hunk_card() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&hunk_card(), &mut model, &mut ui);

    type_str("2", &model, &mut ui);
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        decision(vec![1])
    );
}

/// Prose is NOT an answer here. There is no revise channel to absorb it, and a
/// generous reading at this gate writes files — so the card stays up and the
/// submission must never escape to the driver as a new request.
#[test]
fn prose_at_a_hunk_card_is_ignored_rather_than_guessed_at() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&hunk_card(), &mut model, &mut ui);

    type_str("keep the first one please", &model, &mut ui);
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Ignored
    );
}

/// The latch: the pending card clears only on the host's follow-on event, so a
/// second ⏎ during that round-trip must not re-send the decision.
#[test]
fn a_second_enter_does_not_re_answer_a_hunk_card() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&hunk_card(), &mut model, &mut ui);

    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        decision(vec![0, 1])
    );
    assert!(!matches!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        DeckAction::Send(WorkspaceInput::ToAgent {
            input: UserInput::HunkDecision { .. },
            ..
        })
    ));
}

/// While the composer holds a line, the mark keys belong to the text — the
/// space bar types a space. This is the rule the scope card learned the hard
/// way, applied before it could bite twice.
#[test]
fn space_types_a_space_when_the_composer_is_not_empty() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&hunk_card(), &mut model, &mut ui);

    type_str("1", &model, &mut ui);
    handle_deck_key(ch(' '), &model, &mut ui);
    type_str("2", &model, &mut ui);
    assert_eq!(ui.composer.buffer(), "1 2");
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        decision(vec![0, 1])
    );
}

/// Scrollback must survive a pending card: `↑`/`↓` drive the hunk cursor, but
/// `PgUp`/`PgDn` stay pure transcript scroll, so a reviewer can still read back
/// for context before deciding what to keep.
#[test]
fn page_keys_still_scroll_the_transcript_while_a_hunk_card_is_up() {
    let mut model = model_with(&["lead"]);
    with_tool_exchange(&mut model, "lead");
    let mut ui = ready_ui();
    ingest_inbound(&hunk_card(), &mut model, &mut ui);

    assert!(
        !matches!(
            handle_deck_key(key(KeyCode::PageUp), &model, &mut ui),
            DeckAction::Ignored
        ),
        "PgUp must not be swallowed by the card"
    );
    // …and the card is still answerable afterwards.
    assert_eq!(
        handle_deck_key(key(KeyCode::Enter), &model, &mut ui),
        decision(vec![0, 1])
    );
}
