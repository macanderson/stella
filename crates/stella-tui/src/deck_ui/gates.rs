//! The focused agent's blocking gates: the scope-review card, the per-hunk
//! approval card (#1265), and the `ask_user` question.
//!
//! All three follow one rule — a pending, unanswered gate owns the user's next
//! submission. That rule is what makes a card answerable at all: the deck's
//! driver reads any other mid-turn submission as a *new request* and spawns a
//! sidecar sub-session for it, so a gate that does not claim the submit chord
//! watches the reviewer's words go to a different agent while it stays parked.
//! Split out of `deck_ui.rs` beside `nav`/`create` (#458).

use super::*;

/// A reviewer's in-progress marks on one hunk-review card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkMarks {
    /// Which hunk the navigation keys act on.
    pub cursor: usize,
    /// Per-hunk accept flag, indexed exactly like `HunkProposal::hunks`.
    pub accepted: Vec<bool>,
}

impl HunkMarks {
    /// Marks for a fresh card: every hunk accepted, cursor on the first.
    #[must_use]
    pub fn all_accepted(hunks: usize) -> Self {
        Self {
            cursor: 0,
            accepted: vec![true; hunks],
        }
    }

    /// Move the cursor by `delta`, saturating at both ends rather than
    /// wrapping — a reviewer holding ↓ must not silently land back at hunk 1
    /// and start toggling the wrong one.
    pub fn move_cursor(&mut self, delta: isize) {
        let last = self.accepted.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
    }

    /// Flip the hunk under the cursor.
    pub fn toggle(&mut self) {
        if let Some(flag) = self.accepted.get_mut(self.cursor) {
            *flag = !*flag;
        }
    }

    /// The indices this card would send — what the footer counts.
    #[must_use]
    pub fn selection(&self) -> Vec<usize> {
        self.accepted
            .iter()
            .enumerate()
            .filter(|(_, keep)| **keep)
            .map(|(i, _)| i)
            .collect()
    }
}

/// The focused agent's gates, in the order a turn can raise them: scope review,
/// per-hunk approval, then `ask_user`. Returns `Some` to short-circuit; `None`
/// to fall through to normal editing.
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
                    // A *modified* `⏎` is NOT claimed above (`classify_enter`
                    // answers Newline for it), so a note can span lines
                    // before a bare `⏎` sends it.
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

    // Per-hunk approval (#1265). Same latch discipline as the two gates around
    // it: the pending card clears only on the host's follow-on `ToolResult`, so
    // without `hunk_answered` a second ⏎ re-sends the decision.
    //
    // The keys are chosen to obey the rule the scope card learned the hard way:
    // **no unmodified letter acts on its own**, because the composer is an
    // always-live band and a letter belongs to whatever the reviewer is typing.
    // So navigation and marking ride non-letter keys — `↑`/`↓` move, `Space`
    // toggles, `⏎` applies, `Esc` declines everything — and every one of them is
    // claimed only while the composer is EMPTY, except `⏎` and `Esc`, which are
    // the two that must work with a note in hand: `⏎` reads a typed selection
    // (`1 3`, `2-4`, `all`, `none`) and `Esc` means "get out of this card".
    //
    // `↑`/`↓` are the only navigation keys that reach here at all: `Tab` is
    // consumed by tab-cycling and `←`/`→` by the sessions/context overlays,
    // both upstream of this handler. Borrowing them costs the transcript's
    // message highlight while a card is up, which is affordable precisely
    // because `PgUp`/`PgDn` are pure scroll and are NOT claimed here — a
    // reviewer can still read back for context before deciding.
    if let Some(proposal) = &entry.model.pending_hunk_review
        && !ui.hunk_answered.contains(agent)
    {
        let total = proposal.hunks.len();
        let accepted = match key.code {
            // Esc is the one immediate action, with or without a composer
            // note — declining everything is the direction that STOPS a write.
            KeyCode::Esc => Some(Vec::new()),
            KeyCode::Up | KeyCode::Down if composer_empty => {
                let delta = if key.code == KeyCode::Up { -1 } else { 1 };
                if let Some(marks) = ui.hunk_marks.get_mut(agent) {
                    marks.move_cursor(delta);
                }
                return Some(DeckAction::Ignored);
            }
            KeyCode::Char(' ') if composer_empty => {
                if let Some(marks) = ui.hunk_marks.get_mut(agent) {
                    marks.toggle();
                }
                return Some(DeckAction::Ignored);
            }
            // A `!` line is a shell command even while a gate is pending — the
            // same carve-out the two gates around this one make.
            KeyCode::Enter
                if classify_enter(&key) == EnterAction::Submit
                    && !ui.composer.buffer().trim_start().starts_with('!') =>
            {
                match ui.composer.take_submission() {
                    // A typed line is read as a selection. An unparseable one
                    // is NOT an answer: there is no revise channel here to
                    // absorb prose, and guessing at this gate edits files.
                    Some(submission) => {
                        match crate::input::hunk_selection_from_typed(&submission.text, total) {
                            Some(accepted) => Some(accepted),
                            None => return Some(DeckAction::Ignored),
                        }
                    }
                    // An empty ⏎ commits the marks, which is the ordinary
                    // path — the card's footer has been naming the surviving
                    // count the whole time.
                    None => Some(
                        ui.hunk_marks
                            .get(agent)
                            .map(HunkMarks::selection)
                            .unwrap_or_else(|| (0..total).collect()),
                    ),
                }
            }
            _ => None,
        };
        if let Some(accepted) = accepted {
            ui.hunk_answered.insert(agent.clone());
            return Some(DeckAction::Send(WorkspaceInput::ToAgent {
                agent: agent.clone(),
                input: UserInput::HunkDecision {
                    id: proposal.id.clone(),
                    accepted,
                },
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
            // A bare `⏎` dispatches the typed free text as the answer.
            // A *modified* `⏎` is NOT claimed — it falls through to composer
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
