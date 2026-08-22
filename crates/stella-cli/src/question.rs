// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The plain-TTY answering surface for `ask_question` (#4212).
//!
//! The Command Deck has its own card-backed responder; this is the one every
//! *other* interactive surface gets — the non-deck REPL, a `stella run` on a
//! terminal — and the one a session falls back to when the deck is not
//! running. Same three-step flow as the deck, rendered in lines instead of a
//! grid:
//!
//! 1. **Step through.** One card per question, numbered options with their
//!    descriptions, and a free-text escape the runtime appends rather than
//!    the asker listing it ([`crate::interactive::FREE_TEXT_LABEL`]).
//! 2. **A note per answer.** Appended to the same line after ` # `, so an
//!    answer and the constraint that qualifies it are one keystroke apart. A
//!    note is not a comment: it is the half of the answer the option list
//!    could not hold, and the tool tells the model to treat it as narrowing
//!    or overriding the option it sits under.
//! 3. **Review, then submit.** The whole set is shown back before anything
//!    is sent, with three ways out: submit, talk it through instead
//!    ([`stella_protocol::QuestionOutcome::Deferred`]), or cancel.
//!
//! # Why the parsing is a pure function
//!
//! [`parse_answer`] and [`parse_review`] are pure over borrowed strings, and
//! every case below is unit-tested without a terminal. What a person can type
//! at a prompt is the part with real edge cases — an empty line, a stray
//! comma, a number out of range, a free-text answer that happens to start
//! with a digit — and a surface that can only be tested by driving a TTY is
//! a surface whose edge cases are tested by users.
//!
//! # Known hazard, inherited
//!
//! [`TtyLineIo`] reads stdin on the blocking pool, and the broker's TTL
//! cannot cancel that read once it starts — the same hazard
//! [`crate::approval`] documents for its own io, and the same fix (a
//! cancellable line reader) would close both. It is tracked in #4219 rather
//! than hidden here.

use std::sync::Arc;

use async_trait::async_trait;
use stella_protocol::{Answer, Question, QuestionOutcome, QuestionRequest};
use stella_tools::ToolRegistry;
use stella_tools::registry::question::{DEFAULT_QUESTION_TTL, QuestionResponder};

use crate::interactive::FREE_TEXT_LABEL;

/// The separator between an answer and the note attached to it.
///
/// Spaces on both sides deliberately: a bare `#` appears inside real
/// free-text answers (`use the #main branch`), and splitting on that would
/// silently amputate an answer into a note. ` # ` is rare enough in a short
/// answer to be safe and short enough to type.
pub const NOTE_SEPARATOR: &str = " # ";

/// How a line of input at the per-question prompt reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAnswer {
    /// The labels chosen, or the answerer's own words for a free-text answer.
    pub chosen: Vec<String>,
    /// The note attached after [`NOTE_SEPARATOR`], when there was one.
    pub note: Option<String>,
}

/// What the review prompt's answer means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewChoice {
    /// Send the answers.
    Submit,
    /// Talk it through instead — the options were the wrong shape.
    Chat,
    /// Send nothing.
    Cancel,
}

/// Read one line of the answerer's input against `question`.
///
/// The rules, in the order they are tried:
///
/// - **An empty line takes the first option.** The asking model is told to
///   put its recommendation first, and the prompt says which option Enter
///   takes, so this is a stated default rather than a silent one.
/// - **Numbers select.** `2`, or `1,3` for a multi-select question.
///   Out-of-range or repeated indices are dropped rather than refused — a
///   person who typed `1,1,9` meant option 1, and re-prompting them over it
///   spends more of their patience than it saves.
/// - **A number beyond the options is the free-text escape** when it names
///   the appended free-text row; the caller then asks for the words.
/// - **Anything else is the answer, verbatim.** Including something that
///   merely starts with a digit (`3 or 4 replicas`), which is why the
///   numeric path requires *every* comma-separated token to be a number.
///
/// A single-select question given several numbers keeps the first: the
/// question said one answer, and inventing a multi-answer for it would
/// misreport what was asked.
#[must_use]
pub fn parse_answer(line: &str, question: &Question) -> ParsedAnswer {
    let (choice, note) = match line.split_once(NOTE_SEPARATOR) {
        Some((choice, note)) => (choice.trim(), non_empty(note.trim())),
        None => (line.trim(), None),
    };

    if choice.is_empty() {
        let first = question
            .options
            .first()
            .map(|o| o.label.clone())
            .unwrap_or_default();
        return ParsedAnswer {
            chosen: if first.is_empty() {
                Vec::new()
            } else {
                vec![first]
            },
            note,
        };
    }

    let tokens: Vec<&str> = choice.split(',').map(str::trim).collect();
    let all_numeric = tokens
        .iter()
        .all(|t| !t.is_empty() && t.parse::<usize>().is_ok());
    if all_numeric {
        // The row the runtime appends after the asker's options. Naming it
        // is a real selection — it means "none of these, let me type" — and
        // is why an out-of-range *index* and the free-text *row* cannot
        // share a branch: both fail `options.get`, and collapsing them
        // recorded the literal digit as the person's answer.
        let free_text_row = question.options.len() + 1;
        let mut chosen: Vec<String> = Vec::new();
        let mut wants_free_text = false;
        for token in &tokens {
            let Ok(index) = token.parse::<usize>() else {
                continue;
            };
            if index == free_text_row {
                wants_free_text = true;
                continue;
            }
            let Some(option) = index
                .checked_sub(1)
                .and_then(|zero_based| question.options.get(zero_based))
            else {
                continue;
            };
            if !chosen.iter().any(|c| c == &option.label) {
                chosen.push(option.label.clone());
            }
            if !question.multi_select {
                break;
            }
        }
        if !chosen.is_empty() {
            return ParsedAnswer { chosen, note };
        }
        if wants_free_text {
            // Nothing chosen, deliberately: the caller reads an empty
            // `chosen` as "ask them for words".
            return ParsedAnswer {
                chosen: Vec::new(),
                note,
            };
        }
        // Every number was out of range and none was the free-text row —
        // treat the line as free text rather than as an empty answer, so `9`
        // against a 3-option question is recorded as what the person actually
        // typed instead of vanishing.
    }

    ParsedAnswer {
        chosen: vec![choice.to_string()],
        note,
    }
}

/// Read the review prompt's answer. Empty submits — the review has already
/// shown what would be sent, so Enter meaning "yes, that" is the same
/// affordance the per-question prompt gives.
#[must_use]
pub fn parse_review(line: &str) -> ReviewChoice {
    let answer = line.trim();
    if answer.is_empty() || answer == "1" || answer.eq_ignore_ascii_case("submit") {
        return ReviewChoice::Submit;
    }
    if answer == "2" || answer.eq_ignore_ascii_case("chat") {
        return ReviewChoice::Chat;
    }
    ReviewChoice::Cancel
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_string())
}

/// How this responder reaches the person: render some text, read one line.
///
/// Narrower than [`crate::interactive::AskUserIo`] on purpose — that seam
/// renders its own numbered card, and this flow needs to draw descriptions,
/// tab positions and a review block that a fixed card shape cannot express.
/// Injectable so every case in this module is tested without a terminal.
#[async_trait]
pub trait QuestionIo: Send + Sync {
    /// Render `card` (already formatted, multi-line) and read one line of
    /// input. An `Err` means the input channel is gone — the flow then
    /// cancels rather than looping.
    async fn ask(&self, card: &str) -> Result<String, String>;
}

/// Production io: prints to stdout, reads one line from stdin.
pub struct TtyLineIo;

#[async_trait]
impl QuestionIo for TtyLineIo {
    async fn ask(&self, card: &str) -> Result<String, String> {
        use std::io::Write as _;
        print!("{card}");
        std::io::stdout().flush().map_err(|e| e.to_string())?;
        tokio::task::spawn_blocking(|| {
            use std::io::BufRead as _;
            let mut line = String::new();
            let read = std::io::stdin().lock().read_line(&mut line);
            match read {
                // Zero bytes is EOF — the person closed the input. Reporting
                // it as an error is what makes the flow cancel instead of
                // spinning on an endless stream of empty lines, each of which
                // would be read as "take the default".
                Ok(0) => Err("input stream closed".to_string()),
                Ok(_) => Ok(line),
                Err(e) => Err(e.to_string()),
            }
        })
        .await
        .map_err(|e| e.to_string())?
    }
}

/// The plain-TTY [`QuestionResponder`].
pub struct TtyQuestionResponder {
    io: Box<dyn QuestionIo>,
}

impl TtyQuestionResponder {
    /// A responder that talks through `io`.
    #[must_use]
    pub fn new(io: Box<dyn QuestionIo>) -> Self {
        Self { io }
    }
}

/// The card for one question: position, header, the question, its options
/// with descriptions, and the appended free-text row.
fn question_card(index: usize, total: usize, question: &Question, asker: Option<&str>) -> String {
    let mut card = String::from("\n");
    let who = match asker {
        Some(agent) => format!(" (from the `{agent}` sub-agent)"),
        None => String::new(),
    };
    card.push_str(&format!(
        "  ? [{}/{total}] {}{who}\n",
        index + 1,
        question.header
    ));
    card.push_str(&format!("    {}\n", question.question));
    for (i, option) in question.options.iter().enumerate() {
        let marker = if i == 0 { "❯" } else { " " };
        if option.description.is_empty() {
            card.push_str(&format!("    {marker} {}) {}\n", i + 1, option.label));
        } else {
            card.push_str(&format!(
                "    {marker} {}) {} — {}\n",
                i + 1,
                option.label,
                option.description
            ));
        }
    }
    card.push_str(&format!(
        "      {}) {FREE_TEXT_LABEL}\n",
        question.options.len() + 1
    ));
    let how = if question.multi_select {
        "numbers (comma-separated), or your own words"
    } else {
        "number, or your own words"
    };
    card.push_str(&format!(
        "    answer ({how}; Enter takes 1; append `{}note` to attach a note): ",
        NOTE_SEPARATOR.trim_start()
    ));
    card
}

/// The review block: everything that would be sent, then the three ways out.
fn review_card(answers: &[Answer]) -> String {
    let mut card = String::from("\n  Review your answers\n");
    for answer in answers {
        card.push_str(&format!("\n    • {}\n", answer.question));
        for choice in &answer.chosen {
            card.push_str(&format!("      → {choice}\n"));
        }
        if answer.chosen.is_empty() {
            card.push_str("      → (no answer)\n");
        }
        if let Some(note) = &answer.note {
            card.push_str(&format!("        note: {note}\n"));
        }
    }
    card.push_str(
        "\n    1) Submit answers\n    2) Chat about this instead\n    3) Cancel\n\
         \n    ready to submit? (Enter submits): ",
    );
    card
}

#[async_trait]
impl QuestionResponder for TtyQuestionResponder {
    async fn respond(&self, request: &QuestionRequest) -> QuestionOutcome {
        let total = request.questions.len();
        let mut answers: Vec<Answer> = Vec::with_capacity(total);

        for (index, question) in request.questions.iter().enumerate() {
            let card = question_card(index, total, question, request.asker.as_deref());
            let Ok(line) = self.io.ask(&card).await else {
                return cancelled();
            };
            let mut parsed = parse_answer(&line, question);

            // The free-text row carries no label, so selecting it by number
            // leaves nothing chosen — ask for the words rather than
            // recording an empty answer.
            if parsed.chosen.is_empty() {
                let Ok(words) = self.io.ask("    your answer: ").await else {
                    return cancelled();
                };
                let words = words.trim().to_string();
                if !words.is_empty() {
                    parsed.chosen = vec![words];
                }
            }

            answers.push(Answer {
                header: question.header.clone(),
                question: question.question.clone(),
                chosen: parsed.chosen,
                note: parsed.note,
            });
        }

        let Ok(line) = self.io.ask(&review_card(&answers)).await else {
            return cancelled();
        };
        match parse_review(&line) {
            ReviewChoice::Submit => QuestionOutcome::Answered { answers },
            ReviewChoice::Chat => {
                let note = self
                    .io
                    .ask("    what would you like to say? ")
                    .await
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                QuestionOutcome::Deferred { note }
            }
            ReviewChoice::Cancel => cancelled(),
        }
    }
}

/// The decline a cancel produces — worded as an instruction, on the same
/// argument as the broker's headless refusal: a bare "cancelled" leaves the
/// model to invent a recovery, and the one it invents is to ask again.
fn cancelled() -> QuestionOutcome {
    QuestionOutcome::Declined {
        reason: "the driver cancelled without answering — do not re-ask; proceed with your best \
                 judgement and state the assumption you made"
            .to_string(),
    }
}

/// Attach the TTY-backed question responder to `registry` when a human is
/// present to answer.
///
/// `human_present` is [`crate::interactive::human_can_answer`]'s answer,
/// derived once by the session driver and passed down — never re-derived
/// here, which is the #3035 discipline. Without a human the broker stays
/// headless and the tool tells the model to decide for itself, rather than
/// reading a stdin nobody is holding.
pub(crate) fn attach_interactive_questions(registry: &ToolRegistry, human_present: bool) {
    if !human_present {
        return;
    }
    registry.attach_question_responder(
        Arc::new(TtyQuestionResponder::new(Box::new(TtyLineIo))),
        DEFAULT_QUESTION_TTL,
    );
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use stella_protocol::QuestionOption;

    use super::*;

    fn question(multi: bool) -> Question {
        Question {
            header: "Auth method".into(),
            question: "Which auth should the new endpoint use?".into(),
            options: vec![
                QuestionOption {
                    label: "Session cookie".into(),
                    description: "Matches the rest of the app".into(),
                },
                QuestionOption {
                    label: "Bearer token".into(),
                    description: String::new(),
                },
                QuestionOption {
                    label: "mTLS".into(),
                    description: String::new(),
                },
            ],
            multi_select: multi,
        }
    }

    /// Scripted io: hands back queued lines, records every card rendered.
    struct ScriptedIo {
        lines: Mutex<std::collections::VecDeque<String>>,
        cards: Mutex<Vec<String>>,
    }

    impl ScriptedIo {
        fn new(lines: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                lines: Mutex::new(lines.iter().map(|l| (*l).to_string()).collect()),
                cards: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl QuestionIo for Arc<ScriptedIo> {
        async fn ask(&self, card: &str) -> Result<String, String> {
            self.cards.lock().unwrap().push(card.to_string());
            self.lines
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| "no more scripted input".to_string())
        }
    }

    fn request(questions: Vec<Question>) -> QuestionRequest {
        QuestionRequest {
            asker: None,
            questions,
        }
    }

    #[test]
    fn a_number_selects_and_an_empty_line_takes_the_recommendation() {
        let q = question(false);
        assert_eq!(parse_answer("2", &q).chosen, vec!["Bearer token"]);
        assert_eq!(
            parse_answer("", &q).chosen,
            vec!["Session cookie"],
            "the model is told to put its recommendation first, and the prompt says Enter takes it"
        );
        assert_eq!(parse_answer("   ", &q).chosen, vec!["Session cookie"]);
    }

    /// The case that makes the "all tokens numeric" rule necessary: a real
    /// free-text answer can start with a digit, and snapping `3 or 4
    /// replicas` to option 3 would record an answer nobody gave.
    #[test]
    fn free_text_that_starts_with_a_digit_is_not_read_as_a_selection() {
        let q = question(false);
        assert_eq!(
            parse_answer("3 or 4 replicas", &q).chosen,
            vec!["3 or 4 replicas"]
        );
        assert_eq!(
            parse_answer("neither — reuse the gateway's mTLS", &q).chosen,
            vec!["neither — reuse the gateway's mTLS"]
        );
    }

    #[test]
    fn multi_select_takes_several_and_single_select_keeps_the_first() {
        assert_eq!(
            parse_answer("1,3", &question(true)).chosen,
            vec!["Session cookie", "mTLS"]
        );
        assert_eq!(
            parse_answer("1,3", &question(false)).chosen,
            vec!["Session cookie"],
            "the question asked for one answer; reporting two misreports what was asked"
        );
        // Repeats and out-of-range indices are dropped, not refused.
        assert_eq!(
            parse_answer("1,1,9", &question(true)).chosen,
            vec!["Session cookie"]
        );
    }

    /// A line of numbers that are *all* out of range is not an empty answer:
    /// it is recorded as what the person typed.
    #[test]
    fn an_entirely_out_of_range_selection_survives_as_free_text() {
        assert_eq!(parse_answer("9", &question(false)).chosen, vec!["9"]);
    }

    /// Selecting the appended free-text row leaves nothing chosen, which is
    /// the signal the responder uses to ask for words.
    #[test]
    fn the_free_text_row_resolves_to_no_label() {
        let q = question(false);
        assert!(parse_answer("4", &q).chosen.is_empty());
    }

    #[test]
    fn a_note_is_split_off_only_on_the_spaced_separator() {
        let q = question(false);
        let parsed = parse_answer("1 # only for the admin routes", &q);
        assert_eq!(parsed.chosen, vec!["Session cookie"]);
        assert_eq!(parsed.note.as_deref(), Some("only for the admin routes"));

        // A bare `#` inside an answer is part of the answer.
        let parsed = parse_answer("use the #main branch", &q);
        assert_eq!(parsed.chosen, vec!["use the #main branch"]);
        assert_eq!(parsed.note, None);

        // An empty note is no note.
        assert_eq!(parse_answer("1 # ", &q).note, None);
    }

    #[test]
    fn the_review_prompt_defaults_to_submitting() {
        assert_eq!(parse_review(""), ReviewChoice::Submit);
        assert_eq!(parse_review("1"), ReviewChoice::Submit);
        assert_eq!(parse_review("submit"), ReviewChoice::Submit);
        assert_eq!(parse_review("2"), ReviewChoice::Chat);
        assert_eq!(parse_review("chat"), ReviewChoice::Chat);
        assert_eq!(parse_review("3"), ReviewChoice::Cancel);
        assert_eq!(parse_review("no"), ReviewChoice::Cancel);
    }

    /// **The end-to-end witness.** Two questions, a note on the first, then
    /// submit — and the note reaches the answer set rather than being
    /// swallowed by the review step.
    #[tokio::test]
    async fn stepping_through_two_questions_with_a_note_then_submitting() {
        let io = ScriptedIo::new(&["1 # only for the admin routes", "2", ""]);
        let responder = TtyQuestionResponder::new(Box::new(io.clone()));
        let mut second = question(false);
        second.header = "Rollout".into();
        second.question = "How should it roll out?".into();

        let outcome = responder
            .respond(&request(vec![question(false), second]))
            .await;

        let QuestionOutcome::Answered { answers } = outcome else {
            panic!("submitting must answer: {outcome:?}");
        };
        assert_eq!(answers.len(), 2);
        assert_eq!(answers[0].chosen, vec!["Session cookie"]);
        assert_eq!(
            answers[0].note.as_deref(),
            Some("only for the admin routes")
        );
        assert_eq!(answers[1].header, "Rollout");
        assert_eq!(answers[1].chosen, vec!["Bearer token"]);
        assert_eq!(answers[1].note, None);

        // The review card showed both answers and the note before submitting.
        let cards = io.cards.lock().unwrap();
        let review = cards.last().expect("a review card was rendered");
        assert!(review.contains("Review your answers"), "{review}");
        assert!(review.contains("Session cookie"), "{review}");
        assert!(
            review.contains("note: only for the admin routes"),
            "{review}"
        );
        assert!(review.contains("How should it roll out?"), "{review}");
    }

    /// Choosing "chat about this" defers with the driver's words — a
    /// distinct outcome from cancelling, which is what lets the model tell
    /// "the options were wrong" from "no answer is coming".
    #[tokio::test]
    async fn chatting_about_it_defers_with_their_words() {
        let io = ScriptedIo::new(&["1", "2", "the options miss the shared-gateway case"]);
        let responder = TtyQuestionResponder::new(Box::new(io));
        match responder.respond(&request(vec![question(false)])).await {
            QuestionOutcome::Deferred { note } => {
                assert_eq!(note, "the options miss the shared-gateway case");
            }
            other => panic!("chat must defer: {other:?}"),
        }
    }

    #[tokio::test]
    async fn cancelling_declines_with_an_instruction_not_a_bare_no() {
        let io = ScriptedIo::new(&["1", "3"]);
        let responder = TtyQuestionResponder::new(Box::new(io));
        match responder.respond(&request(vec![question(false)])).await {
            QuestionOutcome::Declined { reason } => {
                assert!(reason.contains("do not re-ask"), "{reason}");
                assert!(reason.contains("state the assumption"), "{reason}");
            }
            other => panic!("cancel must decline: {other:?}"),
        }
    }

    /// A closed input stream cancels rather than looping on empty reads —
    /// each of which `parse_answer` would otherwise read as "take the
    /// default", answering every question in the batch on the person's
    /// behalf after they walked away.
    #[tokio::test]
    async fn a_closed_input_stream_cancels() {
        let io = ScriptedIo::new(&[]);
        let responder = TtyQuestionResponder::new(Box::new(io));
        assert!(matches!(
            responder.respond(&request(vec![question(false)])).await,
            QuestionOutcome::Declined { .. }
        ));
    }

    /// Selecting the free-text row prompts for the words, and those words
    /// are the answer.
    #[tokio::test]
    async fn the_free_text_row_prompts_for_words() {
        let io = ScriptedIo::new(&["4", "reuse the gateway's mTLS", ""]);
        let responder = TtyQuestionResponder::new(Box::new(io.clone()));
        match responder.respond(&request(vec![question(false)])).await {
            QuestionOutcome::Answered { answers } => {
                assert_eq!(answers[0].chosen, vec!["reuse the gateway's mTLS"]);
            }
            other => panic!("{other:?}"),
        }
        let cards = io.cards.lock().unwrap();
        assert!(
            cards.iter().any(|c| c.contains("your answer:")),
            "the free-text row must prompt for words: {cards:?}"
        );
    }

    /// The card names the sub-agent when one is asking — the visible half of
    /// the agent-to-agent path, so a driver knows whose question they are
    /// reading rather than assuming it is the top-level turn's.
    #[test]
    fn a_sub_agents_question_says_whose_it_is() {
        let card = question_card(0, 1, &question(false), Some("research-child"));
        assert!(
            card.contains("from the `research-child` sub-agent"),
            "{card}"
        );
        let top = question_card(0, 1, &question(false), None);
        assert!(!top.contains("sub-agent"), "{top}");
    }

    /// The runtime appends the free-text escape; the asker never lists it.
    #[test]
    fn the_card_appends_the_free_text_row_after_the_askers_options() {
        let card = question_card(0, 2, &question(false), None);
        assert!(card.contains("1) Session cookie — Matches the rest of the app"));
        assert!(card.contains(&format!("4) {FREE_TEXT_LABEL}")));
        assert!(card.contains("[1/2]"), "position is shown: {card}");
    }
}
