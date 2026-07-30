//! The focused agent's blocking gates: the scope-review card and the
//! `ask_user` question.
//!
//! Both follow one rule — a pending, unanswered gate owns the user's next
//! submission. That rule is what makes a card answerable at all: the deck's
//! driver reads any other mid-turn submission as a *new request* and spawns a
//! sidecar sub-session for it, so a gate that does not claim the submit chord
//! watches the reviewer's words go to a different agent while it stays parked.
//! Split out of `deck_ui.rs` beside `nav`/`create` (#458).

use super::*;

/// Scope-review / ask-user gates for the focused agent. Returns `Some` to
/// short-circuit; `None` to fall through to normal editing.
pub(super) fn handle_focused_gates(
    key: KeyEvent,
    model: &WorkspaceModel,
    ui: &mut DeckUi,
    agent: &AgentId,
) -> Option<DeckAction> {
    let entry = model.agents.get(ui.focused)?;
    let composer_empty = ui.composer.buffer().is_empty();

    // Scope review, while UNANSWERED — the pending gate clears on the engine's
    // follow-on event, so the latch is what stops a second submit re-sending the
    // decision during that round-trip; the card reads "decision sent" until
    // then and keys type into the composer as usual.
    //
    // **Only non-text keys act on their own.** `Esc` aborts immediately; every
    // other answer — `a`, `t`, `x`, `ok`, or a sentence describing a different
    // scope — is typed into the composer and sent with the submit chord, read by
    // [`ScopeDecision::from_typed`].
    //
    // A single unmodified letter used to commit from an empty composer, and that
    // is wrong for *this* surface. A single-key commit is fine where the prompt
    // is modal (git's `y/n`, a permission dialog) because nothing else wants the
    // letter. Here the composer is an unconditional band, always live, and
    // typing a note is now the advertised way to ask for a different scope — so
    // the card was competing with the text field for `a`, and losing either way:
    // a note opening "also do X" silently approved an eight-step plan, while a
    // reviewer who typed past that first letter had no way to send what they
    // wrote (it fell through to the driver, which reads a mid-turn prompt as a
    // *new request* and spawns a sidecar sub-session for it — the gate stayed
    // parked, the words went to a stranger agent, and the only key still
    // reaching the card was Esc, which aborts).
    //
    // `Esc` keeps acting immediately because it cannot collide with prose, and
    // because it is the direction that *stops* work rather than starting it.
    // The one keystroke this costs buys deliberate consent at the one gate whose
    // whole purpose is deliberate consent.
    //
    // Esc is claimed with a *non-empty* composer too, which the letter keys
    // never are. Otherwise "type a note, change your mind, press Esc" fell past
    // the gate into the turn-stop chain — and for a pipeline turn that is a hard
    // cancel, so the same gesture ended the turn cleanly or violently depending
    // on whether the user had started typing. While a card is up, Esc means one
    // thing: get out of this card.
    if entry.model.pending_scope_review.is_some() && !ui.scope_answered.contains(agent) {
        let decision = match key.code {
            KeyCode::Esc => Some(ScopeDecision::Abort),
            // A `!` line is a shell command even while a gate is pending — it
            // must run immediately, not be read as the decision (same carve-out
            // as `ask_user`).
            KeyCode::Enter
                if classify_enter(&key) == EnterAction::Submit
                    && !ui.composer.buffer().trim_start().starts_with('!') =>
            {
                match ui.composer.take_submission() {
                    // A plain `⏎` is NOT claimed above (`classify_enter` only
                    // answers Submit for the submit chord), so a note can span
                    // lines before it is sent.
                    Some(submission) => Some(ScopeDecision::from_typed(&submission.text)),
                    // An empty submit while a card is pending: force an
                    // explicit answer rather than sending a blank note.
                    None => return Some(DeckAction::Ignored),
                }
            }
            _ => None,
        };
        // A blank note is not an answer — keep the card up (the composer is
        // already drained, so this is the "typed only whitespace" case).
        if let Some(ScopeDecision::Revise { note }) = &decision
            && note.is_empty()
        {
            return Some(DeckAction::Ignored);
        }
        if let Some(decision) = decision {
            ui.scope_answered.insert(agent.clone());
            return Some(DeckAction::Send(WorkspaceInput::ToAgent {
                agent: agent.clone(),
                input: UserInput::ScopeDecision(decision),
            }));
        }
    }

    // Ask-user: digit quick-pick when nothing typed; Enter submits free text.
    // Same latch: the answer returns as the tool's own ToolResult, and until
    // it lands a second digit/Enter must not re-answer the question.
    if let Some(prompt) = &entry.model.pending_ask_user
        && !ui.ask_answered.contains(agent)
    {
        match key.code {
            KeyCode::Char(d @ '1'..='9') if composer_empty => {
                let idx = (d as usize) - ('1' as usize);
                if let Some(option) = prompt.options.get(idx) {
                    let answer = option.clone();
                    let id = prompt.id.clone();
                    ui.ask_answered.insert(agent.clone());
                    return Some(DeckAction::Send(WorkspaceInput::ToAgent {
                        agent: agent.clone(),
                        input: UserInput::AskUserAnswer { id, answer },
                    }));
                }
            }
            // The submit chord dispatches the typed free text as the answer.
            // A plain `⏎` is NOT claimed — it falls through to composer
            // editing, so the answer can span lines. A `!` line is a shell
            // command even while a question is pending — it must run
            // immediately, not be swallowed as the answer.
            KeyCode::Enter
                if classify_enter(&key) == EnterAction::Submit
                    && !ui.composer.buffer().trim_start().starts_with('!') =>
            {
                if let Some(submission) = ui.composer.take_submission() {
                    let id = prompt.id.clone();
                    ui.ask_answered.insert(agent.clone());
                    return Some(DeckAction::Send(WorkspaceInput::ToAgent {
                        agent: agent.clone(),
                        input: UserInput::AskUserAnswer {
                            id,
                            answer: submission.text,
                        },
                    }));
                }
                return Some(DeckAction::Ignored); // force an explicit answer
            }
            _ => {}
        }
    }
    None
}
