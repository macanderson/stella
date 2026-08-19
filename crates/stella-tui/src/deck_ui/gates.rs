//! The focused agent's blocking gates: the per-hunk approval card (#1265) and
//! the `ask_user` question.
//!
//! Both follow one rule — a pending, unanswered gate owns the user's next
//! submission. That rule is what makes a gate answerable at all: the deck's
//! driver reads any other mid-turn submission as a *new request* and spawns a
//! sidecar sub-session for it, so a gate that does not claim the submit chord
//! watches the reviewer's words go to a different agent while it stays parked.
//! Split out of `deck_ui.rs` beside `nav`/`create` (#458).
//!
//! A third gate lived here: the plan-review dialog, a modal that owned the
//! whole keyboard and was answered with a single keypress. It was removed in
//! #3861 — nothing has raised its card since the staged pipeline was deleted
//! (#3865), so its keys answered a question no door asked.

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
