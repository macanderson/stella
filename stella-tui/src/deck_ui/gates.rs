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
    // follow-on event, so the latch is what stops a second press re-sending the
    // decision during that round-trip; the card reads "decision sent" until
    // then and the keys type into the composer as usual.
    //
    // Two ways in, exactly like `ask_user` below: a/t/x/Esc from an empty
    // composer, or the submit chord over typed text. The typed path is what
    // makes the card answerable at all once the user has started writing — the
    // decision keys are ordinary prompt characters, so they cannot claim a
    // non-empty composer, and without a submit path the reviewer's line fell
    // through to the driver, which reads a mid-turn prompt as a *new request*
    // and spawns a sidecar sub-session for it. The gate stayed parked, the
    // words went to a stranger agent, and the only key that still reached the
    // card was Esc — which aborts. That is the whole bug: a review nobody could
    // answer by typing, whose recovery gesture killed the turn.
    //
    // Known sharp edge, unchanged by this fix and deliberately not widened
    // here: the single-key decisions claim the *first* keystroke into an empty
    // composer, so a note that opens with a/t/x sends that decision instead of
    // starting a note ("also do X" approves). It is survivable mostly by
    // accident — the words most likely to open such a note are "approve",
    // "abort" and "trim", which map to the same decision the key sent — but a
    // note beginning "add…" or "also…" is a silent approve. Fixing it means
    // deciding whether the card's advertised keys should require ⏎, which is a
    // change to how every reviewer approves, not a bug fix.
    if entry.model.pending_scope_review.is_some() && !ui.scope_answered.contains(agent) {
        let decision = match key.code {
            KeyCode::Char('a') if composer_empty => Some(ScopeDecision::Approve),
            KeyCode::Char('t') if composer_empty => Some(ScopeDecision::Trim),
            KeyCode::Char('x') | KeyCode::Esc if composer_empty => Some(ScopeDecision::Abort),
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
