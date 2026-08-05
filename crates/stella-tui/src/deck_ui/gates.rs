//! The focused agent's blocking gates: the plan-review dialog, the per-hunk
//! approval card (#1265), and the `ask_user` question.
//!
//! All three follow one rule — a pending, unanswered gate owns the user's next
//! submission. That rule is what makes a gate answerable at all: the deck's
//! driver reads any other mid-turn submission as a *new request* and spawns a
//! sidecar sub-session for it, so a gate that does not claim the submit chord
//! watches the reviewer's words go to a different agent while it stays parked.
//! The plan-review gate goes further: it is a modal dialog
//! ([`crate::views::scope_dialog`]) that owns the whole keyboard, answered
//! with a single keypress. Split out of `deck_ui.rs` beside `nav`/`create`
//! (#458).

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
    // follow-on event, so the latch is what stops a second keypress re-sending
    // the decision during that round-trip (the dialog drops the moment the
    // latch is set — see `views::scope_dialog::pending`).
    //
    // The gate is a **modal dialog** now, answered with a single keypress:
    // `a` approve · `t` trim · `r` refine (type a note, `⏎` sends it) ·
    // `x`/Esc abort. Single letters were tried once before and removed,
    // because the gate then rendered as a *band* while the composer stayed
    // live underneath — the two surfaces competed for every letter, and a
    // note opening "also do X" silently approved an eight-step plan. The
    // dialog resolves that collision structurally instead of by ceremony:
    // while it is up it owns the keyboard (every unclaimed key is swallowed
    // here, never typed into the composer), so a letter can only ever mean
    // what the dialog says it means — the same modality that makes `git`'s
    // `y/n` and the routing card's `s/n/p` safe. Free-text intent survives as
    // the `r` note, which is its own input inside the dialog rather than the
    // composer.
    //
    // `PgUp`/`PgDn` deliberately fall through: they are pure transcript
    // scroll, and a reviewer reads back for context before deciding — the
    // same carve-out the hunk card makes. (`!` shell lines, which the typed
    // flow used to carve out, do not: the composer is unreachable while a
    // modal owns the keyboard. Abort first, or answer.)
    if entry.model.pending_scope_review.is_some() && !ui.scope_answered.contains(agent) {
        // Note-entry (`r` was pressed): the dialog is a text input until `⏎`
        // sends the note or Esc returns to the options.
        if let Some(note) = ui.scope_note.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    ui.scope_note = None;
                    return Some(DeckAction::Handled);
                }
                KeyCode::Enter => {
                    let note = note.trim().to_string();
                    // A blank note is not an answer — stay in the input.
                    if note.is_empty() {
                        return Some(DeckAction::Ignored);
                    }
                    ui.scope_note = None;
                    ui.scope_answered.insert(agent.clone());
                    return Some(DeckAction::Send(WorkspaceInput::ToAgent {
                        agent: agent.clone(),
                        input: UserInput::ScopeDecision(ScopeDecision::Revise { note }),
                    }));
                }
                KeyCode::Backspace => {
                    note.pop();
                    return Some(DeckAction::Handled);
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    note.push(c);
                    return Some(DeckAction::Handled);
                }
                _ => return Some(DeckAction::Handled),
            }
        }
        let decision = match key.code {
            KeyCode::Char('a') if key.modifiers.is_empty() => Some(ScopeDecision::Approve),
            KeyCode::Char('t') if key.modifiers.is_empty() => Some(ScopeDecision::Trim),
            KeyCode::Char('x') if key.modifiers.is_empty() => Some(ScopeDecision::Abort),
            KeyCode::Esc => Some(ScopeDecision::Abort),
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                ui.scope_note = Some(String::new());
                return Some(DeckAction::Handled);
            }
            // Pure scroll stays available — reading back is part of deciding.
            KeyCode::PageUp | KeyCode::PageDown => None,
            // Everything else is swallowed: a dialog whose keys leaked into
            // the composer behind it would be worse than no dialog.
            _ => return Some(DeckAction::Handled),
        };
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
