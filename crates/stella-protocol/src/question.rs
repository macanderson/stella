// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The structured question an agent poses to whoever is driving it, and the
//! answer that comes back.
//!
//! These are the types the `ask_question` tool, its broker
//! (`stella_tools::registry::question`), and every surface that renders the
//! question card share. They cross a crate boundary in both directions — the
//! CLI and the Command Deck implement the responder port over them — so they
//! round-trip through `serde_json` byte-for-byte (invariant #4).
//!
//! # Who answers is not encoded here
//!
//! A [`QuestionRequest`] never names its audience. Whether the answer comes
//! from a human at a terminal or from the agent that dispatched this one is a
//! fact about **who is driving**, resolved by the host that attached the
//! responder — not a parameter the asking model chooses. That is what lets
//! one tool serve both: a delegated sub-agent asks exactly the way a
//! top-level turn does, and the answer arrives in the same shape.
//!
//! [`QuestionRequest::asker`] is the *other* direction of that fact — it
//! records which agent is asking, so a driver rendering the card can say who
//! wants to know. It is set by the runtime from the bus's attribution stack,
//! never by the model.
//!
//! # Free text is the runtime's affordance, not the asker's option
//!
//! An asker lists the structured options it can act on. The answering surface
//! always appends a free-text escape of its own, because a question whose
//! options do not fit is a question that must still be answerable. So a
//! [`Answer::chosen`] entry is not guaranteed to be one of the labels the
//! asker offered, and a consumer must not assume it is.

use serde::{Deserialize, Serialize};

/// The label every answering surface appends as its free-text escape.
///
/// It lives here rather than in one surface because **more than one surface
/// renders it** — the plain-TTY card numbers it after the asker's options,
/// and the Command Deck's overlay draws it as the last selectable row — and
/// the two must name the same affordance. A second copy is a string that
/// drifts: the deck would offer "Something else" while the TTY offered this,
/// and the same question would read as two different questions depending on
/// which shell the person happened to be in.
///
/// Not a wire field. It is a rendering constant that crosses a crate
/// boundary, which is the same reason the types around it live here.
pub const FREE_TEXT_LABEL: &str = "Type your own answer";

/// One choice offered for a [`Question`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    /// The short text the answerer picks — what comes back in
    /// [`Answer::chosen`].
    pub label: String,
    /// What choosing it means or implies. Rendered under the label; the half
    /// that makes a choice informed rather than a guess.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
}

/// One question, with the options the asker can act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    /// A very short label for the question — what a tab strip or chip shows
    /// when several questions are in flight ("Auth method", "Library").
    pub header: String,
    /// The question itself, in full.
    pub question: String,
    /// The choices the asker offers. The answering surface appends its own
    /// free-text escape; see the module docs.
    pub options: Vec<QuestionOption>,
    /// Whether several options may be chosen at once. `false` — one answer —
    /// is the common case and the default when the field is absent.
    #[serde(default)]
    pub multi_select: bool,
}

/// A batch of questions posed in one call, and who is asking.
///
/// A batch rather than one question because the questions an agent needs
/// settled at a given moment usually arrive together, and asking them one
/// call at a time costs a model round trip apiece while the driver sits
/// through a prompt per answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionRequest {
    /// The agent posing the questions, when one is attributed — a delegated
    /// sub-agent's id, so a driver can tell whose question it is reading.
    /// `None` for a top-level turn, which is the driver's own agent.
    ///
    /// Set by the runtime from the bus attribution stack, never by the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asker: Option<String>,
    /// The questions, in the order they should be presented.
    pub questions: Vec<Question>,
}

/// One question's answer, carrying the question it answers so the result
/// reads without cross-referencing the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    /// The answered question's [`Question::header`].
    pub header: String,
    /// The answered question's [`Question::question`].
    pub question: String,
    /// What was chosen. One entry for a single-select question, several for a
    /// multi-select one, and — for a free-text answer — the answerer's own
    /// words, which need not match any offered label.
    pub chosen: Vec<String>,
    /// The free-form note the answerer attached to *this* answer.
    ///
    /// The half a bare option list cannot carry: "option 1, but only for the
    /// files under `src/`". Absent when none was attached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// How one `ask_question` call resolved.
///
/// Three outcomes rather than two, because "answered" and "refused" do not
/// cover the case the driver most often wants: the question is the right
/// question and the option list is the wrong shape, so the answer is a
/// conversation rather than a pick. [`Self::Deferred`] is that case, and it
/// is behaviourally distinct — the model should keep talking, not proceed on
/// an assumption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum QuestionOutcome {
    /// Every question was answered and the set was submitted.
    Answered {
        /// One entry per question, in the order they were asked.
        answers: Vec<Answer>,
    },
    /// The driver chose to talk it through instead of picking from the
    /// options. `note` carries whatever they said while doing so, which may
    /// be empty — the choice itself is the signal.
    Deferred {
        /// The driver's words, when they gave any.
        #[serde(default)]
        note: String,
    },
    /// No answer is coming: the driver cancelled, the wait timed out, or no
    /// surface was attached to ask at all. `reason` says which, in words the
    /// asking model can act on.
    Declined {
        /// Why no answer came back.
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> QuestionRequest {
        QuestionRequest {
            asker: Some("research-child".into()),
            questions: vec![
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
                    ],
                    multi_select: false,
                },
                Question {
                    header: "Surfaces".into(),
                    question: "Which surfaces should it ship on?".into(),
                    options: vec![QuestionOption {
                        label: "CLI".into(),
                        description: String::new(),
                    }],
                    multi_select: true,
                },
            ],
        }
    }

    /// Invariant #4: every type crossing a crate boundary round-trips
    /// byte-for-byte.
    #[test]
    fn a_request_round_trips_byte_for_byte() {
        let request = sample_request();
        let json = serde_json::to_string(&request).unwrap();
        let back: QuestionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(request, back);
        assert_eq!(json, serde_json::to_string(&back).unwrap());
    }

    /// Each outcome is tagged, so a reader branches on `outcome` rather than
    /// on which keys happen to be present.
    #[test]
    fn every_outcome_round_trips_and_is_tagged() {
        let outcomes = [
            QuestionOutcome::Answered {
                answers: vec![Answer {
                    header: "Auth method".into(),
                    question: "Which auth should the new endpoint use?".into(),
                    chosen: vec!["Session cookie".into()],
                    note: Some("but only for the admin routes".into()),
                }],
            },
            QuestionOutcome::Deferred {
                note: "the second option needs more thought".into(),
            },
            QuestionOutcome::Declined {
                reason: "cancelled by the user".into(),
            },
        ];
        let tags = ["answered", "deferred", "declined"];

        for (outcome, tag) in outcomes.iter().zip(tags) {
            let json = serde_json::to_string(outcome).unwrap();
            let value: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(value["outcome"], tag, "outcome tag for {outcome:?}");
            let back: QuestionOutcome = serde_json::from_str(&json).unwrap();
            assert_eq!(*outcome, back);
            assert_eq!(json, serde_json::to_string(&back).unwrap());
        }
    }

    /// The optional fields are additive: a stream written before they existed
    /// still parses. `multi_select` defaults to the single-select common
    /// case, and an absent `note`/`asker`/`description` is a genuine absence
    /// rather than an error.
    #[test]
    fn the_optional_fields_are_additive() {
        let minimal = serde_json::json!({
            "questions": [{
                "header": "Scope",
                "question": "How far should this go?",
                "options": [{ "label": "Just the parser" }]
            }]
        });
        let request: QuestionRequest = serde_json::from_value(minimal).unwrap();
        assert_eq!(request.asker, None);
        assert!(!request.questions[0].multi_select);
        assert_eq!(request.questions[0].options[0].description, "");

        let answered: QuestionOutcome = serde_json::from_value(serde_json::json!({
            "outcome": "answered",
            "answers": [{
                "header": "Scope",
                "question": "How far should this go?",
                "chosen": ["Just the parser"]
            }]
        }))
        .unwrap();
        match answered {
            QuestionOutcome::Answered { answers } => assert_eq!(answers[0].note, None),
            other => panic!("expected an answered outcome, got {other:?}"),
        }
    }

    /// A free-text answer is not one of the offered labels — the type must
    /// carry it unchanged rather than snapping it to the nearest option.
    #[test]
    fn a_free_text_answer_need_not_match_an_offered_label() {
        let answer = Answer {
            header: "Auth method".into(),
            question: "Which auth should the new endpoint use?".into(),
            chosen: vec!["neither — reuse the gateway's mTLS".into()],
            note: None,
        };
        let back: Answer = serde_json::from_str(&serde_json::to_string(&answer).unwrap()).unwrap();
        assert_eq!(back.chosen, vec!["neither — reuse the gateway's mTLS"]);
    }
}
