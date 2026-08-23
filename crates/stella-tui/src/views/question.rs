// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The Command Deck's `ask_question` overlay (#4220): the wizard a parked
//! turn waits on.
//!
//! # Why the deck needed its own
//!
//! `ask_question` (#4212) parks a turn on whoever is driving it, through the
//! `QuestionResponder` port. The plain-TTY responder answers by printing a
//! card and reading a line of stdin — which is exactly what the deck cannot
//! do: it owns the terminal in raw mode, and a blocking stdin read behind its
//! render loop would fight it for every keystroke. So the deck, the *default*
//! interactive shell on a TTY, was the one interactive surface where every
//! question resolved to the headless "no driver is attached" decline. Honest,
//! and not the feature.
//!
//! This module is the deck's half. It is a **pure fold** like every other
//! overlay here: [`QuestionOverlay::key`] takes a keystroke and returns what
//! it did, [`render`] paints the state, and neither touches a channel, a
//! clock, or the filesystem. The host ([`crate::envelope::Inbound::QuestionAsked`]
//! in, [`crate::envelope::WorkspaceInput::QuestionAnswered`] out) is the only
//! thing that knows a tool call is waiting on the other side.
//!
//! # The flow matches the plain TTY's, key for key
//!
//! The two surfaces must ask the same question, or the same agent asking the
//! same thing reads as two different questions depending on which shell the
//! person launched. `stella_cli::question`'s three steps map across exactly:
//!
//! | plain TTY | here |
//! | --- | --- |
//! | numbered options, `❯` on the recommendation | ↑/↓ over the option rows |
//! | one card per question, in order | Tab / ⇧Tab across the tab strip |
//! | `… # note` appended to the answer line | `n` opens the note editor |
//! | the appended free-text row | the last row, `⏎` opens the editor |
//! | review block: submit / chat / cancel | the review pane, same three |
//!
//! The affordances differ (a grid can highlight where a line can only
//! number); the *decisions* available do not, and neither does the
//! [`QuestionOutcome`] that comes out.
//!
//! # Two things the card must say
//!
//! - **Who is asking.** [`QuestionRequest::asker`] names the sub-agent when a
//!   delegated child raised the question. A driver answering a fanned-out
//!   delegation who cannot see which child is asking is answering a coin
//!   flip, so the card carries it as its first body row — the same fact the
//!   TTY card renders as "(from the `<id>` sub-agent)". A **body** row, not
//!   the title: the title shares its width with the right-aligned key hints,
//!   and this overlay's first golden caught the hints winning and the chip
//!   rendering as `fro`.
//! - **That nothing is running.** The turn is parked. The overlay owns the
//!   keyboard ahead of the composer while it is up, which is what makes that
//!   legible without a word of chrome saying so.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use stella_protocol::{Answer, FREE_TEXT_LABEL, Question, QuestionOutcome, QuestionRequest};

use crate::theme;
use crate::views::cards;

/// The overlay is wider than the `/plan`-family cards: a question's options
/// carry descriptions, and eliding the half that says what a choice *means*
/// turns an informed decision back into a guess.
const QUESTION_CARD_W: u16 = 76;

/// What owns the keyboard inside the overlay.
///
/// One enum rather than a set of booleans, because the modes are genuinely
/// exclusive and the text-entry ones all borrow the same buffer — three
/// `bool`s would make "note editor open while the review pane is up" a
/// representable state that means nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QuestionMode {
    /// Moving over one question's options. Where a freshly raised card
    /// starts, and where every editor returns to.
    #[default]
    Answering,
    /// Typing a note for the focused question's answer.
    Note,
    /// Typing a free-text answer for the focused question.
    FreeText,
    /// The whole answer set, with submit / chat / cancel.
    Review,
    /// Typing the words that ride along with a "chat about this instead".
    Chat,
}

/// One question's working answer, as the wizard builds it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Pick {
    /// Indices into the question's own `options`, in the order chosen. A
    /// single-select question holds at most one.
    chosen: Vec<usize>,
    /// The answerer's own words, when they took the free-text row. Mutually
    /// exclusive with `chosen` by construction — taking one clears the other,
    /// because "option 2, and also my own thing" is not a decision the tool
    /// can report.
    free_text: Option<String>,
    /// The note attached to *this* answer.
    note: Option<String>,
}

/// The rows of the review pane, in the order they are drawn.
const REVIEW_ROWS: [&str; 3] = [
    "Submit answers",
    "Chat about this instead",
    "Cancel — no answer",
];

/// What a keystroke did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionAction {
    /// Not the overlay's key (it is not open).
    Ignored,
    /// Consumed; nothing left the overlay.
    Handled,
    /// The driver settled it — send this back to the parked call and close.
    Resolve(QuestionOutcome),
}

/// The overlay's whole state. `request: None` is "nothing is parked", which
/// is also the closed state — there is no second `open` flag to disagree with
/// it.
#[derive(Debug, Clone, Default)]
pub struct QuestionOverlay {
    /// The parked request, or `None` when no question is waiting.
    pub request: Option<QuestionRequest>,
    /// One working answer per question, same length and order as
    /// `request.questions` whenever a request is live.
    picks: Vec<Pick>,
    /// Which question the tab strip is on.
    focus: usize,
    /// The highlighted row within the focused question: `0..options.len()`
    /// are the asker's options, and `options.len()` is the appended free-text
    /// row.
    row: usize,
    /// Which of [`REVIEW_ROWS`] is highlighted.
    review_row: usize,
    /// The mode that owns the keyboard.
    pub mode: QuestionMode,
    /// The buffer the three text-entry modes share.
    editor: String,
}

impl QuestionOverlay {
    /// Whether a question is parked and the overlay owns the keyboard.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.request.is_some()
    }

    /// Park `request`, discarding whatever was there.
    ///
    /// Discarding rather than queueing is correct and not a shortcut: the
    /// broker holds a fairness gate and asks one question at a time, so a
    /// second request can only arrive after the first resolved. Modelling a
    /// queue here would be modelling a state the port cannot produce.
    pub fn open(&mut self, request: QuestionRequest) {
        self.picks = vec![Pick::default(); request.questions.len()];
        self.request = Some(request);
        self.focus = 0;
        self.row = 0;
        self.review_row = 0;
        self.mode = QuestionMode::Answering;
        self.editor.clear();
    }

    /// Take the card down and forget the working answers.
    ///
    /// Called both when the driver settles it and when the host withdraws it
    /// — a request whose broker timed out is a card that must not stay up
    /// offering to resolve a oneshot nobody is holding.
    pub fn close(&mut self) {
        self.request = None;
        self.picks.clear();
        self.focus = 0;
        self.row = 0;
        self.review_row = 0;
        self.mode = QuestionMode::Answering;
        self.editor.clear();
    }

    /// Apply an inbound envelope, reporting whether it was one of ours.
    ///
    /// The overlay owns **both** directions of its own envelope contract:
    /// `QuestionAsked` / `QuestionWithdrawn` in here, `QuestionAnswered` out
    /// of [`Self::key`]. Keeping the mapping beside the state it drives is
    /// what lets `deck_ui`'s fold spend two lines on it rather than a dozen
    /// — that file is a god file closed to growth
    /// (`scripts/file-size-baseline.txt`), and this is the shape the
    /// constraint asks for rather than a workaround for it.
    pub fn ingest(&mut self, inbound: &crate::envelope::Inbound) -> bool {
        match inbound {
            crate::envelope::Inbound::QuestionAsked(request) => {
                self.open(request.as_ref().clone());
                true
            }
            crate::envelope::Inbound::QuestionWithdrawn => {
                self.close();
                true
            }
            _ => false,
        }
    }

    /// The focused question, when one is parked.
    fn question(&self) -> Option<&Question> {
        self.request.as_ref()?.questions.get(self.focus)
    }

    /// How many selectable rows the focused question has: its options plus
    /// the free-text row the runtime appends.
    fn row_count(&self) -> usize {
        self.question().map_or(1, |q| q.options.len() + 1)
    }

    /// Whether `row` is the appended free-text row rather than one of the
    /// asker's options.
    fn is_free_text_row(&self, row: usize) -> bool {
        self.question().is_some_and(|q| row == q.options.len())
    }

    /// The answer set as it stands — what a submit would send.
    ///
    /// Every question yields an [`Answer`] even when nothing was chosen: the
    /// tool reports one answer per question asked, and silently dropping the
    /// unanswered ones would let a half-filled card read as a complete one.
    #[must_use]
    pub fn answers(&self) -> Vec<Answer> {
        let Some(request) = &self.request else {
            return Vec::new();
        };
        request
            .questions
            .iter()
            .enumerate()
            .map(|(i, question)| {
                let pick = self.picks.get(i).cloned().unwrap_or_default();
                let chosen = match &pick.free_text {
                    Some(words) => vec![words.clone()],
                    None => pick
                        .chosen
                        .iter()
                        .filter_map(|&o| question.options.get(o))
                        .map(|o| o.label.clone())
                        .collect(),
                };
                Answer {
                    header: question.header.clone(),
                    question: question.question.clone(),
                    chosen,
                    note: pick.note.clone(),
                }
            })
            .collect()
    }

    /// Choose the highlighted option for the focused question.
    ///
    /// Multi-select toggles; single-select replaces. **Neither advances** —
    /// `⇥` is the only thing that moves between questions, and that is a
    /// deliberate divergence from the plain TTY, which advances on the
    /// answer only because a line-oriented prompt has no other way to move.
    ///
    /// A grid does, and the difference matters for exactly one gesture: `n`
    /// annotates the *focused* question, so a `⏎` that also advanced would
    /// put "pick option 2 and say why" — the single most common thing a
    /// driver wants to do here — one question out of step, silently
    /// attaching the note to whatever came next. One rule for both select
    /// kinds also means there is no mode where `⏎` means two different
    /// things.
    fn select(&mut self) {
        let Some(question) = self.question().cloned() else {
            return;
        };
        let row = self.row;
        let Some(pick) = self.picks.get_mut(self.focus) else {
            return;
        };
        // Any option selection retires a free-text answer: the two are one
        // decision, and keeping both would report an answer the driver
        // replaced.
        pick.free_text = None;
        if question.multi_select {
            match pick.chosen.iter().position(|&c| c == row) {
                Some(at) => {
                    pick.chosen.remove(at);
                }
                None => pick.chosen.push(row),
            }
            return;
        }
        pick.chosen = vec![row];
    }

    /// Commit whatever the text editor holds, per the mode that opened it.
    ///
    /// Returns the action, because committing a chat note is the one text
    /// entry that resolves the whole card rather than returning to the grid.
    fn commit_editor(&mut self) -> QuestionAction {
        let text = self.editor.trim().to_string();
        match self.mode {
            QuestionMode::Chat => {
                self.editor.clear();
                QuestionAction::Resolve(QuestionOutcome::Deferred { note: text })
            }
            QuestionMode::Note => {
                if let Some(pick) = self.picks.get_mut(self.focus) {
                    // An emptied note is no note — the same rule
                    // `parse_answer` applies to a trailing ` # ` with nothing
                    // after it, so clearing the field removes the note rather
                    // than attaching an empty string.
                    pick.note = (!text.is_empty()).then_some(text);
                }
                self.editor.clear();
                self.mode = QuestionMode::Answering;
                QuestionAction::Handled
            }
            QuestionMode::FreeText => {
                if let Some(pick) = self.picks.get_mut(self.focus) {
                    if text.is_empty() {
                        // Nothing typed is not an answer: leave whatever was
                        // chosen before standing rather than blanking it.
                        pick.free_text = None;
                    } else {
                        pick.free_text = Some(text);
                        pick.chosen.clear();
                    }
                }
                self.editor.clear();
                self.mode = QuestionMode::Answering;
                QuestionAction::Handled
            }
            _ => QuestionAction::Handled,
        }
    }

    /// Fold one keystroke.
    ///
    /// Returns [`QuestionAction::Ignored`] when nothing is parked, so the
    /// caller can route it in front of every other handler unconditionally
    /// and let the closed overlay decline.
    ///
    /// Ctrl-C is **also** declined while open, deliberately and for the same
    /// reason [`crate::deck_ui::dispatch::handle_key`] declines it: this
    /// handler runs ahead of the deck's quit branch, so claiming it would
    /// make a parked question the one state a user cannot Ctrl-C out of.
    pub fn key(&mut self, key: crossterm::event::KeyEvent) -> QuestionAction {
        use crossterm::event::{KeyCode, KeyModifiers};

        if self.request.is_none() {
            return QuestionAction::Ignored;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return QuestionAction::Ignored;
        }

        // The text-entry modes own every printable key, so they are claimed
        // before the navigation letters below — otherwise `n` typed into a
        // note would open a second note editor.
        if matches!(
            self.mode,
            QuestionMode::Note | QuestionMode::FreeText | QuestionMode::Chat
        ) {
            return match key.code {
                KeyCode::Esc => {
                    // Abandon the text, keep the card: Esc out of an editor
                    // must not cancel the question, or a typo becomes a
                    // declined turn.
                    self.editor.clear();
                    self.mode = if self.mode == QuestionMode::Chat {
                        QuestionMode::Review
                    } else {
                        QuestionMode::Answering
                    };
                    QuestionAction::Handled
                }
                KeyCode::Enter => self.commit_editor(),
                KeyCode::Backspace => {
                    self.editor.pop();
                    QuestionAction::Handled
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.editor.push(c);
                    QuestionAction::Handled
                }
                _ => QuestionAction::Handled,
            };
        }

        if self.mode == QuestionMode::Review {
            // Movement is the deck's one vocabulary rather than this card's
            // own copy of it, so `⇞`/`⇟` and `Home`/`End` reach the review
            // rows too. Modal, so `letters` is true (#4370).
            if crate::deck_ui::list_nav::select(key, &mut self.review_row, REVIEW_ROWS.len(), true)
            {
                return QuestionAction::Handled;
            }
            return match key.code {
                KeyCode::Esc => QuestionAction::Resolve(cancelled()),
                KeyCode::Tab | KeyCode::BackTab => {
                    // Back to the questions to change something — the review
                    // is a checkpoint, not a one-way door.
                    self.mode = QuestionMode::Answering;
                    QuestionAction::Handled
                }
                KeyCode::Enter => match self.review_row {
                    0 => QuestionAction::Resolve(QuestionOutcome::Answered {
                        answers: self.answers(),
                    }),
                    1 => {
                        self.mode = QuestionMode::Chat;
                        self.editor.clear();
                        QuestionAction::Handled
                    }
                    _ => QuestionAction::Resolve(cancelled()),
                },
                _ => QuestionAction::Handled,
            };
        }

        let total = self.request.as_ref().map_or(0, |r| r.questions.len());
        // As above: one vocabulary for the answer rows too.
        let rows = self.row_count();
        if crate::deck_ui::list_nav::select(key, &mut self.row, rows, true) {
            return QuestionAction::Handled;
        }
        match key.code {
            KeyCode::Esc => QuestionAction::Resolve(cancelled()),
            KeyCode::Tab => {
                if self.focus + 1 < total {
                    self.focus += 1;
                    self.row = 0;
                } else {
                    self.mode = QuestionMode::Review;
                    self.review_row = 0;
                }
                QuestionAction::Handled
            }
            KeyCode::BackTab => {
                self.focus = self.focus.saturating_sub(1);
                self.row = 0;
                QuestionAction::Handled
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if self.is_free_text_row(self.row) {
                    self.mode = QuestionMode::FreeText;
                    self.editor = self
                        .picks
                        .get(self.focus)
                        .and_then(|p| p.free_text.clone())
                        .unwrap_or_default();
                } else {
                    self.select();
                }
                QuestionAction::Handled
            }
            KeyCode::Char('n') => {
                self.mode = QuestionMode::Note;
                // Seeded with the existing note so `n` is an edit, not a
                // silent overwrite of what is already attached.
                self.editor = self
                    .picks
                    .get(self.focus)
                    .and_then(|p| p.note.clone())
                    .unwrap_or_default();
                QuestionAction::Handled
            }
            KeyCode::Char('r') => {
                self.mode = QuestionMode::Review;
                self.review_row = 0;
                QuestionAction::Handled
            }
            _ => QuestionAction::Handled,
        }
    }
}

/// The decline a cancel produces.
///
/// Worded as an instruction rather than a bare "cancelled", matching
/// `stella_cli::question`'s own cancel: a refusal that only says no leaves
/// the model to invent a recovery, and the one it invents is to ask again.
fn cancelled() -> QuestionOutcome {
    QuestionOutcome::Declined {
        reason: "the driver cancelled without answering — do not re-ask; proceed with your best \
                 judgement and state the assumption you made"
            .to_string(),
    }
}

// ───────────────────────────────── render ─────────────────────────────────

/// Paint the overlay over `area`. A no-op when nothing is parked.
///
/// `accessible` spans the card across the frame instead of floating it, the
/// same contract every other card here honours (`cards::card_area` — not a
/// link: it is `pub(crate)`, and this is a `pub fn`). It matters more on this
/// one than on any of them: a float clips its rows at
/// its right border, and a screen reader that never reaches the end of an
/// option is a driver answering a question they only half heard — on the one
/// overlay where the answer stops a turn.
pub fn render(overlay: &QuestionOverlay, accessible: bool, area: Rect, buf: &mut Buffer) {
    let Some(request) = &overlay.request else {
        return;
    };
    let mut body = match overlay.mode {
        QuestionMode::Review | QuestionMode::Chat => review_body(overlay),
        _ => question_body(overlay, request),
    };
    // Attribution goes in the BODY, never the title row. It shares that row
    // with the right-aligned key hints, and at this card's width the hints
    // win: the first golden of this overlay rendered the chip as `fro`,
    // silently amputating the one fact that tells a driver which fanned-out
    // child is asking. A body row cannot be squeezed by a neighbour.
    if let Some(row) = asker_row(request.asker.as_deref()) {
        body.insert(0, row);
        body.insert(1, Line::from(""));
    }
    let rows = u16::try_from(body.len()).unwrap_or(u16::MAX);
    let card = cards::card_area(area, rows, QUESTION_CARD_W, accessible);
    let inner = cards::card_frame(card, "question", Vec::new(), hints(overlay.mode), buf);
    cards::render_body(body, None, inner, buf);
}

/// Who is asking, when a sub-agent is.
///
/// `None` for a top-level turn — the driver's own agent needs no
/// attribution, and a "from the lead" chip on every card would train the eye
/// to skip the one place the answer matters.
fn asker_row(asker: Option<&str>) -> Option<Line<'static>> {
    let agent = asker?;
    Some(Line::from(Span::styled(
        format!("from the `{agent}` sub-agent"),
        Style::new().fg(theme::TEXT_TERTIARY),
    )))
}

/// The right-aligned key hints for the mode that owns the keyboard.
fn hints(mode: QuestionMode) -> &'static str {
    match mode {
        QuestionMode::Answering => "↑↓ move · ⏎ pick · ⇥ next · n note · r review · esc cancel",
        QuestionMode::Note => "⏎ save note · esc discard",
        QuestionMode::FreeText => "⏎ save answer · esc discard",
        QuestionMode::Review => "↑↓ move · ⏎ choose · ⇥ back · esc cancel",
        QuestionMode::Chat => "⏎ send · esc back",
    }
}

/// The answering pane: the tab strip (when there is more than one question),
/// the question, its options, the free-text row, and any attached note.
fn question_body(overlay: &QuestionOverlay, request: &QuestionRequest) -> Vec<Line<'static>> {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let mut rows: Vec<Line<'static>> = Vec::new();

    // The tab strip earns its row only when there is something to move
    // between; a one-question card drawing a strip of one is chrome that
    // says nothing.
    if request.questions.len() > 1 {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (i, question) in request.questions.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled("  ", dim));
            }
            let answered = overlay.picks.get(i).is_some_and(is_answered);
            // A glyph, not a colour: the golden suite strips style, so a
            // style-only "this one is done" mark would be invisible to it.
            let mark = if answered { "●" } else { "○" };
            let label = format!("{mark} {}", question.header);
            spans.push(if i == overlay.focus {
                Span::styled(label, theme::accent().add_modifier(Modifier::BOLD))
            } else {
                Span::styled(label, dim)
            });
        }
        rows.push(Line::from(spans));
        rows.push(Line::from(""));
    }

    let Some(question) = request.questions.get(overlay.focus) else {
        return rows;
    };
    let position = if request.questions.len() > 1 {
        format!("[{}/{}] ", overlay.focus + 1, request.questions.len())
    } else {
        String::new()
    };
    rows.push(Line::from(vec![
        Span::styled(position, dim),
        Span::styled(
            question.question.clone(),
            Style::new().fg(theme::TEXT_PRIMARY),
        ),
    ]));
    if question.multi_select {
        rows.push(Line::from(Span::styled(
            "several answers may be chosen",
            dim,
        )));
    }
    rows.push(Line::from(""));

    let pick = overlay
        .picks
        .get(overlay.focus)
        .cloned()
        .unwrap_or_default();
    let inner_w = usize::from(QUESTION_CARD_W) - 2;
    for (i, option) in question.options.iter().enumerate() {
        let chosen = pick.chosen.contains(&i);
        let mut spans = vec![
            cards::marker(i == overlay.row && overlay.mode == QuestionMode::Answering),
            Span::styled(
                if chosen { "[x] " } else { "[ ] " },
                if chosen { theme::accent() } else { dim },
            ),
            Span::styled(option.label.clone(), Style::new().fg(theme::TEXT_PRIMARY)),
        ];
        if !option.description.is_empty() {
            let used: usize = 4 + 4 + option.label.chars().count();
            spans.push(Span::styled(
                format!(
                    " — {}",
                    cards::truncate_cols(
                        &option.description,
                        inner_w.saturating_sub(used + 3).max(8)
                    )
                ),
                dim,
            ));
        }
        rows.push(Line::from(spans));
    }

    // The runtime's own escape, always last and never listed by the asker.
    let free_row = question.options.len();
    let free_selected = pick.free_text.is_some();
    let mut spans = vec![
        cards::marker(overlay.row == free_row && overlay.mode == QuestionMode::Answering),
        Span::styled(
            if free_selected { "[x] " } else { "[ ] " },
            if free_selected { theme::accent() } else { dim },
        ),
        Span::styled(FREE_TEXT_LABEL.to_string(), dim),
    ];
    if let Some(words) = &pick.free_text {
        spans.push(Span::styled(
            format!(" — {}", cards::truncate_cols(words, inner_w / 2)),
            Style::new().fg(theme::TEXT_PRIMARY),
        ));
    }
    rows.push(Line::from(spans));

    match overlay.mode {
        QuestionMode::FreeText => {
            rows.push(Line::from(""));
            rows.push(editor_row("your answer", &overlay.editor, inner_w));
        }
        QuestionMode::Note => {
            rows.push(Line::from(""));
            rows.push(editor_row("note", &overlay.editor, inner_w));
        }
        _ => {
            if let Some(note) = &pick.note {
                rows.push(Line::from(Span::styled(
                    format!("    note: {}", cards::truncate_cols(note, inner_w - 10)),
                    dim,
                )));
            }
        }
    }
    rows
}

/// Whether a working answer holds anything at all — what the tab strip's
/// filled/hollow mark reports.
fn is_answered(pick: &Pick) -> bool {
    !pick.chosen.is_empty() || pick.free_text.is_some()
}

/// One open text field: a dim label, the buffer, and a block caret.
///
/// The caret is a glyph rather than the hardware cursor because the deck
/// parks its real cursor in the composer, and because a style-blind golden
/// can pin a `▏` where it cannot pin a cursor position.
fn editor_row(label: &str, text: &str, inner_w: usize) -> Line<'static> {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let room = inner_w.saturating_sub(label.chars().count() + 8).max(8);
    // Show the tail, not the head: a person typing past the field's width
    // needs to see what they are typing now.
    let shown: String = if text.chars().count() > room {
        text.chars().skip(text.chars().count() - room).collect()
    } else {
        text.to_string()
    };
    Line::from(vec![
        Span::styled(format!("  {label}: "), dim),
        Span::styled(shown, Style::new().fg(theme::TEXT_PRIMARY)),
        Span::styled("▏", theme::accent()),
    ])
}

/// The review pane: everything that would be sent, then the three ways out.
fn review_body(overlay: &QuestionOverlay) -> Vec<Line<'static>> {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let inner_w = usize::from(QUESTION_CARD_W) - 2;
    let mut rows = vec![Line::from(Span::styled(
        "Review your answers",
        Style::new()
            .fg(theme::TEXT_PRIMARY)
            .add_modifier(Modifier::BOLD),
    ))];

    for answer in overlay.answers() {
        rows.push(Line::from(""));
        rows.push(Line::from(Span::styled(
            format!(
                "  • {}",
                cards::truncate_cols(&answer.question, inner_w - 4)
            ),
            Style::new().fg(theme::TEXT_PRIMARY),
        )));
        if answer.chosen.is_empty() {
            rows.push(Line::from(Span::styled("      → (no answer)", dim)));
        }
        for chosen in &answer.chosen {
            rows.push(Line::from(Span::styled(
                format!("      → {}", cards::truncate_cols(chosen, inner_w - 8)),
                theme::accent(),
            )));
        }
        if let Some(note) = &answer.note {
            rows.push(Line::from(Span::styled(
                format!("        note: {}", cards::truncate_cols(note, inner_w - 14)),
                dim,
            )));
        }
    }

    rows.push(Line::from(""));
    if overlay.mode == QuestionMode::Chat {
        rows.push(editor_row(
            "what would you like to say",
            &overlay.editor,
            inner_w,
        ));
        return rows;
    }
    for (i, label) in REVIEW_ROWS.iter().enumerate() {
        rows.push(Line::from(vec![
            cards::marker(i == overlay.review_row),
            Span::styled(
                (*label).to_string(),
                if i == overlay.review_row {
                    theme::accent()
                } else {
                    dim
                },
            ),
        ]));
    }
    rows
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use stella_protocol::QuestionOption;

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typing(overlay: &mut QuestionOverlay, text: &str) {
        for c in text.chars() {
            assert_eq!(overlay.key(key(KeyCode::Char(c))), QuestionAction::Handled);
        }
    }

    fn question(header: &str, multi: bool) -> Question {
        Question {
            header: header.into(),
            question: format!("{header}?"),
            options: vec![
                QuestionOption {
                    label: "Session cookie".into(),
                    description: "Matches the rest of the app".into(),
                },
                QuestionOption {
                    label: "Bearer token".into(),
                    description: String::new(),
                },
            ],
            multi_select: multi,
        }
    }

    fn open(questions: Vec<Question>) -> QuestionOverlay {
        let mut overlay = QuestionOverlay::default();
        overlay.open(QuestionRequest {
            asker: None,
            questions,
        });
        overlay
    }

    /// **The witness for #4370.** The card answers the deck's one list
    /// vocabulary rather than its own copy of half of it: `j`/`k` moved before,
    /// `Home`/`End` did not — on the answer rows and on the review rows alike.
    #[test]
    fn the_card_takes_j_and_end_on_both_its_lists() {
        let mut overlay = open(vec![question("Auth method", false)]);
        let last = overlay.row_count() - 1;
        assert!(last > 0, "the fixture needs more than one row to move to");

        assert_eq!(
            overlay.key(key(KeyCode::Char('j'))),
            QuestionAction::Handled
        );
        assert_eq!(overlay.row, 1, "`j` did not move the answer rows");
        overlay.key(key(KeyCode::End));
        assert_eq!(overlay.row, last, "`End` did not reach the last row");
        overlay.key(key(KeyCode::Home));
        assert_eq!(overlay.row, 0);

        overlay.key(key(KeyCode::Char('r')));
        assert_eq!(overlay.mode, QuestionMode::Review);
        overlay.key(key(KeyCode::Char('j')));
        assert_eq!(overlay.review_row, 1, "`j` did not move the review rows");
        overlay.key(key(KeyCode::End));
        assert_eq!(
            overlay.review_row,
            REVIEW_ROWS.len() - 1,
            "`End` did not reach the last review row"
        );
        overlay.key(key(KeyCode::Home));
        assert_eq!(overlay.review_row, 0);
    }

    /// **The witness for #4220.** Select an option, attach a note, submit —
    /// and the `QuestionOutcome` that leaves the fold carries both. This is
    /// the whole feature: before it, every `ask_question` on the deck
    /// resolved to the headless decline no matter what the driver did.
    #[test]
    fn select_then_note_then_submit_produces_the_answer_and_its_note() {
        let mut overlay = open(vec![question("Auth method", false)]);

        // Down to the second option, pick it — and stay, so the note that
        // follows lands on the answer just given.
        assert_eq!(overlay.key(key(KeyCode::Down)), QuestionAction::Handled);
        assert_eq!(overlay.key(key(KeyCode::Enter)), QuestionAction::Handled);
        assert_eq!(
            overlay.mode,
            QuestionMode::Answering,
            "picking must not advance: `n` annotates the FOCUSED question, so an \
             advancing ⏎ would attach the note to the next one"
        );
        assert_eq!(overlay.focus, 0);

        // Attach the note the option list could not hold.
        assert_eq!(
            overlay.key(key(KeyCode::Char('n'))),
            QuestionAction::Handled
        );
        assert_eq!(overlay.mode, QuestionMode::Note);
        typing(&mut overlay, "only for the admin routes");
        assert_eq!(overlay.key(key(KeyCode::Enter)), QuestionAction::Handled);
        assert_eq!(overlay.mode, QuestionMode::Answering);

        // Review, submit.
        assert_eq!(
            overlay.key(key(KeyCode::Char('r'))),
            QuestionAction::Handled
        );
        let QuestionAction::Resolve(QuestionOutcome::Answered { answers }) =
            overlay.key(key(KeyCode::Enter))
        else {
            panic!("submitting the review must answer");
        };
        assert_eq!(answers.len(), 1);
        assert_eq!(answers[0].header, "Auth method");
        assert_eq!(answers[0].chosen, vec!["Bearer token"]);
        assert_eq!(
            answers[0].note.as_deref(),
            Some("only for the admin routes")
        );
    }

    /// A note typed into the editor must not be readable as a navigation key.
    /// `n` and `r` are the note and review hotkeys on the grid; inside the
    /// editor they are letters, or every note containing an `n` would reopen
    /// the editor it was typed in.
    #[test]
    fn the_note_editor_owns_its_letters() {
        let mut overlay = open(vec![question("Scope", false)]);
        overlay.key(key(KeyCode::Char('n')));
        typing(&mut overlay, "narrow");
        assert_eq!(overlay.mode, QuestionMode::Note, "still in the editor");
        overlay.key(key(KeyCode::Enter));
        assert_eq!(overlay.picks[0].note.as_deref(), Some("narrow"));
    }

    /// Esc out of an editor abandons the text and keeps the card. Cancelling
    /// the whole question on a typo would turn a slip into a declined turn.
    #[test]
    fn esc_in_an_editor_discards_the_text_not_the_question() {
        let mut overlay = open(vec![question("Scope", false)]);
        overlay.key(key(KeyCode::Char('n')));
        typing(&mut overlay, "oops");
        assert_eq!(overlay.key(key(KeyCode::Esc)), QuestionAction::Handled);
        assert_eq!(overlay.mode, QuestionMode::Answering);
        assert!(overlay.request.is_some(), "the question is still parked");
        assert_eq!(overlay.picks[0].note, None);
    }

    /// Esc on the grid declines — and the decline is an instruction, the same
    /// wording the plain-TTY cancel produces, so the model reacts the same
    /// way whichever surface the driver was using.
    #[test]
    fn esc_on_the_grid_declines_with_an_instruction() {
        let mut overlay = open(vec![question("Scope", false)]);
        let QuestionAction::Resolve(QuestionOutcome::Declined { reason }) =
            overlay.key(key(KeyCode::Esc))
        else {
            panic!("esc must decline");
        };
        assert!(reason.contains("do not re-ask"), "{reason}");
        assert!(reason.contains("state the assumption"), "{reason}");
    }

    /// Multi-select toggles and stays; single-select replaces and moves on.
    #[test]
    fn multi_select_accumulates_where_single_select_replaces() {
        let mut multi = open(vec![question("Targets", true)]);
        multi.key(key(KeyCode::Enter));
        multi.key(key(KeyCode::Down));
        multi.key(key(KeyCode::Enter));
        assert_eq!(
            multi.mode,
            QuestionMode::Answering,
            "multi-select stays put"
        );
        assert_eq!(
            multi.answers()[0].chosen,
            vec!["Session cookie", "Bearer token"]
        );
        // Toggling the same row again removes it.
        multi.key(key(KeyCode::Enter));
        assert_eq!(multi.answers()[0].chosen, vec!["Session cookie"]);

        let mut single = open(vec![question("Auth", false)]);
        single.key(key(KeyCode::Enter));
        single.key(key(KeyCode::Down));
        assert_eq!(
            single.answers()[0].chosen,
            vec!["Session cookie"],
            "a single-select question reports one answer"
        );
    }

    /// Tab walks the questions and lands on the review pane past the last —
    /// and every question is reported, including one nobody answered.
    #[test]
    fn tab_walks_every_question_and_unanswered_ones_still_report() {
        let mut overlay = open(vec![question("Auth", false), question("Rollout", false)]);
        overlay.key(key(KeyCode::Enter)); // answers Auth
        assert_eq!(overlay.key(key(KeyCode::Tab)), QuestionAction::Handled);
        assert_eq!(overlay.focus, 1, "⇥ is what moves between questions");
        assert_eq!(overlay.key(key(KeyCode::Tab)), QuestionAction::Handled);
        assert_eq!(overlay.mode, QuestionMode::Review, "past the last question");

        let QuestionAction::Resolve(QuestionOutcome::Answered { answers }) =
            overlay.key(key(KeyCode::Enter))
        else {
            panic!("submit");
        };
        assert_eq!(answers.len(), 2, "one answer per question asked");
        assert_eq!(answers[0].chosen, vec!["Session cookie"]);
        assert!(
            answers[1].chosen.is_empty(),
            "an unanswered question reports as unanswered, not as absent"
        );
    }

    /// The free-text row prompts for words, and those words are the answer —
    /// the deck's form of the escape the runtime appends everywhere.
    #[test]
    fn the_free_text_row_takes_the_answerers_own_words() {
        let mut overlay = open(vec![question("Auth", false)]);
        // Two options, so row 2 is the appended free-text row.
        overlay.key(key(KeyCode::Down));
        overlay.key(key(KeyCode::Down));
        assert_eq!(overlay.key(key(KeyCode::Enter)), QuestionAction::Handled);
        assert_eq!(overlay.mode, QuestionMode::FreeText);
        typing(&mut overlay, "reuse the gateway's mTLS");
        overlay.key(key(KeyCode::Enter));
        assert_eq!(
            overlay.answers()[0].chosen,
            vec!["reuse the gateway's mTLS"]
        );
    }

    /// Free text and an option are one decision: taking either retires the
    /// other, so an answer never reports both.
    #[test]
    fn choosing_an_option_retires_a_free_text_answer() {
        let mut overlay = open(vec![question("Auth", true)]);
        overlay.key(key(KeyCode::Down));
        overlay.key(key(KeyCode::Down));
        overlay.key(key(KeyCode::Enter));
        typing(&mut overlay, "something else");
        overlay.key(key(KeyCode::Enter));
        assert_eq!(overlay.answers()[0].chosen, vec!["something else"]);

        // Now pick a real option.
        overlay.key(key(KeyCode::Up));
        overlay.key(key(KeyCode::Up));
        overlay.key(key(KeyCode::Enter));
        assert_eq!(
            overlay.answers()[0].chosen,
            vec!["Session cookie"],
            "the free-text answer was replaced, not appended to"
        );
    }

    /// "Chat about this instead" defers with the driver's words — a distinct
    /// outcome from cancelling, which is what lets the model tell "the
    /// options were the wrong shape" from "no answer is coming".
    #[test]
    fn chatting_about_it_defers_with_their_words() {
        let mut overlay = open(vec![question("Auth", false)]);
        overlay.key(key(KeyCode::Enter)); // pick
        overlay.key(key(KeyCode::Char('r'))); // → review
        overlay.key(key(KeyCode::Down)); // → chat
        assert_eq!(overlay.key(key(KeyCode::Enter)), QuestionAction::Handled);
        assert_eq!(overlay.mode, QuestionMode::Chat);
        typing(&mut overlay, "the options miss the shared-gateway case");
        let QuestionAction::Resolve(QuestionOutcome::Deferred { note }) =
            overlay.key(key(KeyCode::Enter))
        else {
            panic!("chat must defer");
        };
        assert_eq!(note, "the options miss the shared-gateway case");
    }

    /// A closed overlay declines every key, so the caller may route keys to
    /// it unconditionally and ahead of everything else.
    #[test]
    fn a_closed_overlay_claims_nothing() {
        let mut overlay = QuestionOverlay::default();
        assert!(!overlay.is_open());
        for code in [KeyCode::Esc, KeyCode::Enter, KeyCode::Char('n')] {
            assert_eq!(overlay.key(key(code)), QuestionAction::Ignored);
        }
    }

    /// Withdrawing takes the card down and forgets the working answers — a
    /// question whose broker timed out must not leave a card offering to
    /// resolve a oneshot nobody is holding.
    #[test]
    fn closing_forgets_the_working_answers() {
        let mut overlay = open(vec![question("Auth", false)]);
        overlay.key(key(KeyCode::Enter));
        overlay.close();
        assert!(!overlay.is_open());
        assert!(overlay.answers().is_empty());

        // Re-opening starts clean rather than inheriting the last card's picks.
        overlay.open(QuestionRequest {
            asker: None,
            questions: vec![question("Auth", false)],
        });
        assert!(overlay.answers()[0].chosen.is_empty());
    }

    /// Every question's row cursor is clamped to its own option count: a
    /// question with fewer options than the one before it must not inherit an
    /// out-of-range row (which `answers` would then read as no answer).
    #[test]
    fn moving_between_questions_lands_on_a_valid_row() {
        let mut wide = question("Wide", false);
        wide.options.push(QuestionOption {
            label: "mTLS".into(),
            description: String::new(),
        });
        let mut narrow = question("Narrow", false);
        narrow.options.truncate(1);
        let mut overlay = open(vec![wide, narrow]);

        overlay.key(key(KeyCode::Down));
        overlay.key(key(KeyCode::Down));
        overlay.key(key(KeyCode::Tab));
        assert_eq!(overlay.focus, 1);
        assert_eq!(overlay.row, 0, "a fresh question starts at its first row");
        overlay.key(key(KeyCode::Enter));
        assert_eq!(overlay.answers()[1].chosen, vec!["Session cookie"]);
    }

    /// The card names the sub-agent when one is asking. A driver answering a
    /// fanned-out delegation who cannot see which child is asking is
    /// answering a coin flip.
    #[test]
    fn a_sub_agents_question_says_whose_it_is() {
        let row = asker_row(Some("research-child")).expect("a sub-agent is attributed");
        let text: String = row.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("research-child"), "{text}");
        assert!(text.contains("sub-agent"), "{text}");
        assert!(
            asker_row(None).is_none(),
            "a top-level turn needs no attribution row"
        );
    }
}
