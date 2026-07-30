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

    // Opens with 'o', not a decision key — see the sharp-edge note in
    // `handle_focused_gates` about a/t/x claiming the first keystroke.
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

/// The rule the decision keys already followed must survive: once text exists,
/// a/t/x are prompt characters. Only the first keystroke into an empty composer
/// commits (see the sharp-edge note in `handle_focused_gates`).
#[test]
fn decision_letters_type_into_a_non_empty_composer() {
    let mut model = model_with(&["lead"]);
    let mut ui = ready_ui();
    ingest_inbound(&scope_card(), &mut model, &mut ui);

    type_str("keep", &model, &mut ui);
    for c in ['a', 't', 'x'] {
        assert_eq!(
            handle_deck_key(ch(c), &model, &mut ui),
            DeckAction::Handled,
            "{c} must type once a note is being written"
        );
    }
    assert_eq!(ui.composer.buffer(), "keepatx");
    assert!(!ui.scope_answered.contains("lead"));
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
