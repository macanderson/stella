// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The Command Deck's approval card (#4240): the yes/no a parked dispatch
//! waits on.
//!
//! # Why this is not the question overlay
//!
//! #4220 gave the deck both mid-turn asks and wired the approval half
//! through the deck's generic `AskUser` card, which closed the dead-end with
//! one declaration and no second mechanism. It also flattened a structured
//! [`ApprovalRequest`] into a single prose line, and two of its five fields
//! never reached the person deciding at all: `read_only` — the bit that
//! separates *this reads something* from *this writes something* — and
//! `gate`, which names the rule or hook that raised the demand and is what
//! makes a deny defensible.
//!
//! So this is a card of its own, in a file of its own. An approval is a
//! yes/no over a call that is **already decided**; a question
//! ([`crate::views::question`]) is a decision the model could not make. One
//! fold with both jobs would serve neither, and their keys genuinely differ:
//! a question wants a note editor and a review pane, an approval wants the
//! shortest path to a defensible refusal.
//!
//! # Default-deny is structural here, not a preference
//!
//! The cursor lands on **Deny**, and every way out that is not an explicit
//! choice of *Allow* denies: Esc, a withdrawn card, a closed deck, the
//! broker's TTL. A stray `⏎` on a card the driver has not read must never
//! run a destructive call, and the plain-TTY responder already takes this
//! line — an empty line at its prompt is a deny
//! (`stella_cli::approval::AskUserApprovalResponder`). #2128's rule, applied
//! where the permissive branch is the expensive one.
//!
//! # Deny must be able to carry words
//!
//! [`ApprovalResponse::Deny`] has a `reason`, and the broker forwards it to
//! the model verbatim. "No, use the staging bucket instead" is a redirection
//! the turn can act on; a bare refusal is a wall it has to guess its way
//! around. The third row exists for that, and is the reason this card has a
//! text mode at all.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use stella_tools::registry::approval::{ApprovalRequest, ApprovalResponse};

use crate::theme;
use crate::views::cards;

/// Matches the question overlay's width so the two mid-turn asks read as one
/// system — a driver should not have to re-find the layout depending on
/// which kind of card came up.
const APPROVAL_CARD_W: u16 = 76;

/// Width of the dimmed label column, as `views::plan_card` uses.
const LABEL_W: usize = 8;

/// What owns the keyboard inside the card.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ApprovalMode {
    /// Moving over the three ways out.
    #[default]
    Choosing,
    /// Typing the words that ride along with a deny.
    Reason,
}

/// The three ways out, in the order they are drawn.
///
/// Allow reads first because that is the order the question is asked in
/// ("may I?"), but [`DENY_ROW`] is where the cursor starts — see the module
/// docs on why those are not the same decision.
const CHOICES: [&str; 3] = ["Allow this call, once", "Deny", "Deny, and say why"];

/// The row the cursor starts on, and the row every non-choice exit resolves
/// to. Named rather than spelled `1` at each use, because it being the
/// *deny* row is the safety property, not an index.
const DENY_ROW: usize = 1;

/// The row that opens the reason editor.
const DENY_WITH_REASON_ROW: usize = 2;

/// What a keystroke did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalAction {
    /// Not the card's key (it is not open, or it is Ctrl-C).
    Ignored,
    /// Consumed; nothing left the card.
    Handled,
    /// The driver decided — send this back to the parked dispatch and close.
    Resolve(ApprovalResponse),
}

/// The card's whole state. `request: None` is "nothing is parked", which is
/// also the closed state.
#[derive(Debug, Clone, Default)]
pub struct ApprovalOverlay {
    /// The parked request, or `None` when no dispatch is waiting.
    pub request: Option<ApprovalRequest>,
    /// Which of [`CHOICES`] is highlighted.
    row: usize,
    /// The mode that owns the keyboard.
    pub mode: ApprovalMode,
    /// The deny reason as it is typed.
    editor: String,
}

/// A bare refusal — no words, and the model is told nothing but "no".
fn deny() -> ApprovalResponse {
    ApprovalResponse::Deny {
        reason: String::new(),
    }
}

impl ApprovalOverlay {
    /// Whether a dispatch is parked and the card owns the keyboard.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.request.is_some()
    }

    /// Park `request`.
    ///
    /// The cursor lands on the **deny** row, never on Allow — see the module
    /// docs. Re-opening always re-lands there, so a card raised while the
    /// driver's hand is on `⏎` cannot inherit a permissive cursor from the
    /// last one they answered.
    pub fn open(&mut self, request: ApprovalRequest) {
        self.request = Some(request);
        self.row = DENY_ROW;
        self.mode = ApprovalMode::Choosing;
        self.editor.clear();
    }

    /// Take the card down and forget the half-typed reason.
    pub fn close(&mut self) {
        self.request = None;
        self.row = DENY_ROW;
        self.mode = ApprovalMode::Choosing;
        self.editor.clear();
    }

    /// Apply an inbound envelope, reporting whether it was one of ours.
    ///
    /// The card owns both directions of its own envelope contract, the shape
    /// [`crate::views::question::QuestionOverlay::ingest`] establishes — and
    /// for the same reason: `deck_ui.rs` is a god file closed to growth
    /// (`scripts/file-size-baseline.txt`), so the fold there costs two lines.
    pub fn ingest(&mut self, inbound: &crate::envelope::Inbound) -> bool {
        match inbound {
            crate::envelope::Inbound::ApprovalAsked(request) => {
                self.open(request.as_ref().clone());
                true
            }
            crate::envelope::Inbound::ApprovalWithdrawn => {
                self.close();
                true
            }
            _ => false,
        }
    }

    /// Fold one keystroke.
    ///
    /// Returns [`ApprovalAction::Ignored`] when nothing is parked, so the
    /// caller can route it unconditionally, and for Ctrl-C, so the deck's
    /// quit branch still fires — a parked approval must not be the one state
    /// a user cannot Ctrl-C out of.
    pub fn key(&mut self, key: crossterm::event::KeyEvent) -> ApprovalAction {
        use crossterm::event::{KeyCode, KeyModifiers};

        if self.request.is_none() {
            return ApprovalAction::Ignored;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return ApprovalAction::Ignored;
        }

        if self.mode == ApprovalMode::Reason {
            return match key.code {
                // Abandon the words, keep the card — a typo must not decide
                // the call by itself, in either direction.
                KeyCode::Esc => {
                    self.editor.clear();
                    self.mode = ApprovalMode::Choosing;
                    ApprovalAction::Handled
                }
                KeyCode::Enter => {
                    let reason = self.editor.trim().to_string();
                    self.editor.clear();
                    // Still a deny with nothing typed: the row the driver
                    // chose was a deny, and the words were only ever the
                    // optional half.
                    ApprovalAction::Resolve(ApprovalResponse::Deny { reason })
                }
                KeyCode::Backspace => {
                    self.editor.pop();
                    ApprovalAction::Handled
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.editor.push(c);
                    ApprovalAction::Handled
                }
                _ => ApprovalAction::Handled,
            };
        }

        // Movement is the deck's one vocabulary rather than this card's own
        // copy of it, so `⇞`/`⇟` and `Home`/`End` reach the choices too.
        // Modal, so `letters` is true (#4370).
        if crate::deck_ui::list_nav::select(key, &mut self.row, CHOICES.len(), true) {
            return ApprovalAction::Handled;
        }
        match key.code {
            // Every non-choice exit denies. Never Approve on silence.
            KeyCode::Esc => ApprovalAction::Resolve(deny()),
            KeyCode::Enter => match self.row {
                0 => ApprovalAction::Resolve(ApprovalResponse::Approve),
                DENY_WITH_REASON_ROW => {
                    self.mode = ApprovalMode::Reason;
                    self.editor.clear();
                    ApprovalAction::Handled
                }
                _ => ApprovalAction::Resolve(deny()),
            },
            // Deliberately no bare `y`/`n` hotkeys. On a gate whose wrong
            // answer runs somebody's `rm -rf`, one keystroke must not be the
            // whole decision — the plain-TTY responder accepts them because a
            // line prompt has nothing else, and a modal card does.
            _ => ApprovalAction::Handled,
        }
    }
}

// ───────────────────────────────── render ─────────────────────────────────

/// Paint the card over `area`. A no-op when nothing is parked.
///
/// `accessible` spans the card across the frame instead of floating it
/// (`cards::card_area` — not a link: it is `pub(crate)`, and this is a
/// `pub fn`). A float clips at its right border, and the fields most likely
/// to be long are `subject` and `reason` — the command line about to run and
/// the rule that stopped it, which is to say the whole basis for the answer.
pub fn render(overlay: &ApprovalOverlay, accessible: bool, area: Rect, buf: &mut Buffer) {
    let Some(request) = &overlay.request else {
        return;
    };
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let inner_w = usize::from(APPROVAL_CARD_W) - 2;
    let mut rows: Vec<Line<'static>> = Vec::new();

    // The headline: what would run, and whether running it changes anything.
    // `read_only` is rendered as a WORD rather than a colour — the golden
    // suite strips style, and this is the field a reviewer most needs pinned.
    rows.push(Line::from(vec![
        Span::styled(
            request.tool.clone(),
            Style::new()
                .fg(theme::TEXT_PRIMARY)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if request.read_only {
                "  read-only"
            } else {
                "  mutating"
            },
            dim,
        ),
    ]));
    if let Some(subject) = &request.subject {
        rows.push(Line::from(Span::styled(
            format!("  {}", cards::truncate_cols(subject, inner_w - 2)),
            Style::new().fg(theme::TEXT_PRIMARY),
        )));
    }
    rows.push(Line::from(""));
    rows.push(labelled("gate", &request.gate, inner_w, accessible));
    rows.push(labelled("reason", &request.reason, inner_w, accessible));
    rows.push(Line::from(""));

    if overlay.mode == ApprovalMode::Reason {
        rows.push(Line::from(Span::styled(
            "  denying — say why (the model is told, verbatim):",
            dim,
        )));
        rows.push(editor_row(&overlay.editor, inner_w));
    } else {
        for (i, label) in CHOICES.iter().enumerate() {
            rows.push(Line::from(vec![
                cards::marker(i == overlay.row),
                Span::styled(
                    (*label).to_string(),
                    if i == overlay.row {
                        theme::accent()
                    } else {
                        dim
                    },
                ),
            ]));
        }
    }

    let height = u16::try_from(rows.len()).unwrap_or(u16::MAX);
    let card = cards::card_area(area, height, APPROVAL_CARD_W, accessible);
    let inner = cards::card_frame(card, "approval", Vec::new(), hints(overlay.mode), buf);
    cards::render_body(rows, None, inner, buf);
}

/// The right-aligned key hints for the mode that owns the keyboard.
fn hints(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::Choosing => "↑↓ move · ⏎ choose · esc denies",
        ApprovalMode::Reason => "⏎ deny with these words · esc back",
    }
}

/// One labelled field: dim fixed-width label, then the value.
///
/// In accessible mode the column alignment goes away and the row reads as a
/// labelled record — column alignment carries meaning only to an eye, which
/// is `crate::views::plan_card`'s rule and the deck's convention.
fn labelled(label: &str, value: &str, inner_w: usize, accessible: bool) -> Line<'static> {
    let dim = Style::new().fg(theme::TEXT_TERTIARY);
    let room = inner_w.saturating_sub(LABEL_W + 2).max(8);
    let value = cards::truncate_cols(value, room);
    if accessible {
        return Line::from(vec![
            Span::styled(format!("· {label} "), dim),
            Span::styled(value, Style::new().fg(theme::TEXT_PRIMARY)),
        ]);
    }
    Line::from(vec![
        Span::styled(format!("{label:<LABEL_W$}"), dim),
        Span::styled(value, Style::new().fg(theme::TEXT_PRIMARY)),
    ])
}

/// The open reason field: the buffer and a block caret.
fn editor_row(text: &str, inner_w: usize) -> Line<'static> {
    let room = inner_w.saturating_sub(6).max(8);
    // Show the tail: a person typing past the field's width needs to see
    // what they are typing now.
    let shown: String = if text.chars().count() > room {
        text.chars().skip(text.chars().count() - room).collect()
    } else {
        text.to_string()
    };
    Line::from(vec![
        Span::raw("  "),
        Span::styled(shown, Style::new().fg(theme::TEXT_PRIMARY)),
        Span::styled("▏", theme::accent()),
    ])
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn request() -> ApprovalRequest {
        ApprovalRequest {
            tool: "bash".into(),
            read_only: false,
            reason: "matched rule no-destructive-shell".into(),
            gate: "command.started".into(),
            subject: Some("rm -rf build/".into()),
        }
    }

    fn open() -> ApprovalOverlay {
        let mut overlay = ApprovalOverlay::default();
        overlay.open(request());
        overlay
    }

    /// **The witness for #4370.** The card answers the deck's one list
    /// vocabulary rather than its own copy of half of it: `j`/`k` moved before,
    /// `Home`/`End` did not.
    #[test]
    fn the_card_takes_j_and_end() {
        let mut overlay = open();
        overlay.row = 0;
        assert_eq!(
            overlay.key(key(KeyCode::Char('j'))),
            ApprovalAction::Handled
        );
        assert_eq!(overlay.row, 1, "`j` did not move the choice");
        overlay.key(key(KeyCode::End));
        assert_eq!(
            overlay.row,
            CHOICES.len() - 1,
            "`End` did not reach the last choice"
        );
        overlay.key(key(KeyCode::Home));
        assert_eq!(overlay.row, 0);
    }

    /// **The witness for #4240.** Choosing Allow approves the parked call,
    /// and it takes a deliberate move off the default row to get there.
    #[test]
    fn allowing_takes_a_deliberate_move_off_the_deny_row() {
        let mut overlay = open();
        assert_eq!(
            overlay.row, DENY_ROW,
            "a freshly raised card starts on deny"
        );
        assert_eq!(overlay.key(key(KeyCode::Up)), ApprovalAction::Handled);
        assert_eq!(
            overlay.key(key(KeyCode::Enter)),
            ApprovalAction::Resolve(ApprovalResponse::Approve)
        );
    }

    /// **The safety property.** A `⏎` on a card nobody has moved on denies,
    /// and so does Esc. On a gate whose wrong answer runs somebody's
    /// `rm -rf`, the permissive branch must never be the one a stray
    /// keystroke reaches (#2128).
    #[test]
    fn every_exit_that_is_not_an_explicit_allow_denies() {
        for code in [KeyCode::Enter, KeyCode::Esc] {
            let mut overlay = open();
            assert_eq!(
                overlay.key(key(code)),
                ApprovalAction::Resolve(deny()),
                "{code:?} must deny"
            );
        }
        // …and no bare letter approves either.
        for code in [KeyCode::Char('y'), KeyCode::Char('a'), KeyCode::Char(' ')] {
            let mut overlay = open();
            assert_eq!(
                overlay.key(key(code)),
                ApprovalAction::Handled,
                "{code:?} must not decide the call"
            );
            assert!(overlay.is_open(), "{code:?} left the card up");
        }
    }

    /// Deny carries the driver's words to the model verbatim — the
    /// redirection ("use the staging bucket") a bare refusal cannot express.
    #[test]
    fn denying_with_a_reason_sends_the_words() {
        let mut overlay = open();
        overlay.key(key(KeyCode::Down));
        assert_eq!(overlay.row, DENY_WITH_REASON_ROW);
        assert_eq!(overlay.key(key(KeyCode::Enter)), ApprovalAction::Handled);
        assert_eq!(overlay.mode, ApprovalMode::Reason);
        for c in "use the staging bucket instead".chars() {
            overlay.key(key(KeyCode::Char(c)));
        }
        let ApprovalAction::Resolve(ApprovalResponse::Deny { reason }) =
            overlay.key(key(KeyCode::Enter))
        else {
            panic!("committing the reason must deny");
        };
        assert_eq!(reason, "use the staging bucket instead");
    }

    /// Esc out of the reason editor keeps the card up rather than deciding.
    /// A typo must not settle the call in either direction.
    #[test]
    fn esc_in_the_reason_editor_returns_to_the_choices() {
        let mut overlay = open();
        overlay.key(key(KeyCode::Down));
        overlay.key(key(KeyCode::Enter));
        for c in "oops".chars() {
            overlay.key(key(KeyCode::Char(c)));
        }
        assert_eq!(overlay.key(key(KeyCode::Esc)), ApprovalAction::Handled);
        assert_eq!(overlay.mode, ApprovalMode::Choosing);
        assert!(overlay.is_open());
        assert!(overlay.editor.is_empty());
    }

    /// Committing an empty reason is still a deny — the words were always
    /// the optional half of a decision already made.
    #[test]
    fn an_empty_reason_is_still_a_deny() {
        let mut overlay = open();
        overlay.key(key(KeyCode::Down));
        overlay.key(key(KeyCode::Enter));
        assert_eq!(
            overlay.key(key(KeyCode::Enter)),
            ApprovalAction::Resolve(deny())
        );
    }

    /// Re-opening re-lands on deny. A card raised while the driver's hand is
    /// on `⏎` must not inherit a permissive cursor from the last one.
    #[test]
    fn reopening_never_inherits_a_permissive_cursor() {
        let mut overlay = open();
        overlay.key(key(KeyCode::Up));
        assert_eq!(overlay.row, 0, "moved to allow");
        overlay.close();
        overlay.open(request());
        assert_eq!(overlay.row, DENY_ROW);
    }

    /// A closed card claims nothing, and Ctrl-C is declined even while open
    /// so the deck's quit branch still fires.
    #[test]
    fn a_closed_card_claims_nothing_and_ctrl_c_is_always_declined() {
        let mut overlay = ApprovalOverlay::default();
        for code in [KeyCode::Esc, KeyCode::Enter, KeyCode::Char('y')] {
            assert_eq!(overlay.key(key(code)), ApprovalAction::Ignored);
        }
        let mut overlay = open();
        assert_eq!(
            overlay.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            ApprovalAction::Ignored,
            "a parked approval must not be the one state you cannot quit from"
        );
    }

    /// The envelope contract, both directions.
    #[test]
    fn ingest_raises_and_withdraws_the_card() {
        let mut overlay = ApprovalOverlay::default();
        assert!(overlay.ingest(&crate::envelope::Inbound::ApprovalAsked(
            Box::new(request())
        )));
        assert!(overlay.is_open());
        assert!(overlay.ingest(&crate::envelope::Inbound::ApprovalWithdrawn));
        assert!(!overlay.is_open());
        assert!(
            !overlay.ingest(&crate::envelope::Inbound::ShowHelp),
            "an envelope that is not ours must fall through"
        );
    }
}
