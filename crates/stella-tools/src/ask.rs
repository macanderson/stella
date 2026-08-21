// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `ask_question` tool — how an agent puts a decision back to whoever is
//! driving it (#4212).
//!
//! An agent that reaches a genuine fork has three moves available to it, and
//! before this tool existed only two of them were: guess and hope, or state an
//! assumption and build on it. The third — ask the one party who actually
//! knows — needs a surface, and this is it.
//!
//! # What it is for, and what it is not for
//!
//! The schema's description carries the rule the model reads; the rule behind
//! the rule is that **asking is expensive for the answerer and cheap for the
//! asker**, which is exactly the asymmetry that produces a tool nobody wants
//! to be asked by. A question earns its call when the answer is genuinely the
//! driver's to give — a product decision, a preference between two designs
//! that both work, a constraint only they know — and never when the codebase,
//! the tests or one `search` call would settle it.
//!
//! # A batch, and a note per answer
//!
//! Both shapes are load-bearing rather than decoration. A batch because the
//! questions an agent needs settled arrive together, and asking them one call
//! at a time costs a model round trip apiece. A note per answer because an
//! option list alone forces the driver to pick the least-wrong option and
//! then re-explain themselves in the next message — "option 1, but only for
//! the files under `src/`" is the answer, and there is nowhere to put it.
//!
//! # Why this tool is `read_only`
//!
//! It changes nothing — no file, no board, no state that outlives the call —
//! so the flag is simply true. But it is load-bearing twice beyond honesty:
//!
//! - `stella_core::ports::ReadOnlyTools` filters on exactly this flag, so a
//!   delegated sub-agent can still reach the tool. That is the whole
//!   agent-to-agent story, and it works by construction rather than by a
//!   special case anywhere in this file.
//! - The engine dispatches sibling read-only calls **concurrently**, so two
//!   questions in one step run at once. That hazard is handled one layer
//!   down, by the broker's fairness gate
//!   ([`crate::registry::question`]) — not here.
//!
//! It is deliberately **not** `speculation_safe`: a speculated call would ask
//! a human the same question twice, and the second ask is not free even when
//! the first is discarded.

use async_trait::async_trait;
use serde_json::{Value, json};
use stella_protocol::tool::{ErrorClass, ToolOutput, ToolSchema};
use stella_protocol::{Question, QuestionOption, QuestionOutcome, QuestionRequest};

use crate::registry::Tool;
use crate::registry::question::{QUESTION_TIMED_OUT, QuestionBroker, QuestionSlot};

/// The dispatch name, in one place.
pub const NAME: &str = "ask_question";

/// How many questions one call may carry.
///
/// The floor is one — a call with no questions is a bug, not an empty
/// success. The ceiling is four because the answering surface presents them
/// as a set the driver walks and then reviews whole: past four the review
/// step stops being a review and becomes a form, and the driver starts
/// approving it unread, which costs exactly the care the review exists to
/// buy.
pub const MAX_QUESTIONS: usize = 4;

/// How many options one question may offer.
///
/// The floor is two because a one-option question is not a question. The
/// ceiling is four for the same reason as [`MAX_QUESTIONS`], plus one of its
/// own: the answering surface always appends its own free-text escape, so a
/// question that needs a fifth option already has one — the driver types it.
pub const MIN_OPTIONS: usize = 2;
/// See [`MIN_OPTIONS`].
pub const MAX_OPTIONS: usize = 4;

/// Puts a decision back to whoever is driving this agent.
pub struct AskQuestion {
    question: QuestionSlot,
}

impl AskQuestion {
    /// A tool that asks through whatever broker `slot` holds **at call
    /// time**.
    ///
    /// Reading the slot per call rather than capturing a broker once is the
    /// load-bearing half: the host attaches its responder after this tool is
    /// already registered, and a captured broker would stay headless for the
    /// life of the session. See [`QuestionSlot`].
    #[must_use]
    pub fn new(slot: QuestionSlot) -> Self {
        Self { question: slot }
    }

    /// The broker as it stands right now — a cheap clone sharing the
    /// session's responder and its fairness gate.
    fn broker(&self) -> QuestionBroker {
        self.question
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

#[async_trait]
impl Tool for AskQuestion {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: NAME.to_string(),
            description: format!(
                "Ask whoever is driving you to settle a decision you cannot settle yourself, \
                 and wait for the answer. Use it when the answer is genuinely theirs to give — \
                 a product or design choice where several options all work, a preference, \
                 a constraint only they know, an ambiguity where two readings lead to \
                 materially different work. Do NOT use it for anything the codebase, the tests, \
                 or one search call would answer: read first, ask only what reading cannot \
                 settle. Pose {MAX_QUESTIONS} questions at most, in one call rather than one \
                 per turn, each with {MIN_OPTIONS}-{MAX_OPTIONS} concrete options that name \
                 what would actually happen. Put your recommendation first and say so in its \
                 label. Never offer a free-text or 'other' option: one is always added for you. \
                 The answer comes back with any notes the driver attached to it — read those, \
                 they usually carry the constraint the options could not. In an unattended run \
                 there may be nobody to ask: you will be told so plainly, and then you decide \
                 yourself and say which way you went."
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "description": format!(
                            "The questions to ask, 1-{MAX_QUESTIONS} of them, in the order \
                             they should be presented."
                        ),
                        "items": {
                            "type": "object",
                            "properties": {
                                "header": {
                                    "type": "string",
                                    "description":
                                        "A very short label for this question — two or three \
                                         words, shown as a tab when several are in flight \
                                         (\"Auth method\", \"Rollout\")."
                                },
                                "question": {
                                    "type": "string",
                                    "description":
                                        "The question itself, in full. State what hangs on the \
                                         answer so the driver can weigh it."
                                },
                                "options": {
                                    "type": "array",
                                    "description": format!(
                                        "{MIN_OPTIONS}-{MAX_OPTIONS} options. Order them best \
                                         first; mark your recommendation in its own label."
                                    ),
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": {
                                                "type": "string",
                                                "description":
                                                    "The choice itself, in a few words."
                                            },
                                            "description": {
                                                "type": "string",
                                                "description":
                                                    "What choosing it means or costs — the half \
                                                     that makes the choice informed."
                                            }
                                        },
                                        "required": ["label"]
                                    }
                                },
                                "multi_select": {
                                    "type": "boolean",
                                    "description":
                                        "True when the options are not mutually exclusive and \
                                         several may be chosen. Defaults to false."
                                }
                            },
                            "required": ["header", "question", "options"]
                        }
                    }
                },
                "required": ["questions"]
            }),
            read_only: true,
            // Never: a speculated call asks a human twice.
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, ctx: &crate::ctx::ToolCtx) -> ToolOutput {
        let questions = match parse_questions(input) {
            Ok(questions) => questions,
            Err(message) => {
                return ToolOutput::Error {
                    message,
                    class: Some(ErrorClass::InvalidInput),
                };
            }
        };

        let request = QuestionRequest {
            // Who is asking is the runtime's fact, read from the bus's
            // attribution stack — never something the model may set, or a
            // child could claim to be its parent.
            asker: ctx.current_agent(),
            questions,
        };

        match self.broker().ask(&request).await {
            QuestionOutcome::Answered { answers } => {
                let outcome = QuestionOutcome::Answered { answers };
                ToolOutput::Ok {
                    content: render_outcome(&outcome),
                    data: serde_json::to_value(&outcome).ok(),
                }
            }
            QuestionOutcome::Deferred { note } => {
                let outcome = QuestionOutcome::Deferred { note };
                ToolOutput::Ok {
                    content: render_outcome(&outcome),
                    data: serde_json::to_value(&outcome).ok(),
                }
            }
            // A decline is not a tool defect — the tool did exactly its job
            // and the answer is "no answer". `RefusedByPolicy` is the class
            // whose doc names this case ("a human 'no', an approval that
            // could not be asked"); a TTL expiry is the one arm that is
            // honestly a timeout, and saying so keeps a wedged surface
            // distinguishable from a driver who said no.
            QuestionOutcome::Declined { reason } => ToolOutput::Error {
                class: Some(if reason == QUESTION_TIMED_OUT {
                    ErrorClass::Timeout
                } else {
                    ErrorClass::RefusedByPolicy
                }),
                message: reason,
            },
        }
    }
}

/// Read the model's JSON into the wire type, or say exactly what is wrong.
///
/// Deep validation lives here rather than in the schema because
/// `crate::registry::validate` implements a deliberately small JSON-Schema
/// subset that does not descend into an array's object items — so the bounds
/// this tool's *prose* promises would otherwise be advertised and unenforced,
/// which is worse than not promising them.
///
/// Every refusal names the offending index, because "an option is missing a
/// label" is unactionable when four questions are in flight.
fn parse_questions(input: &Value) -> Result<Vec<Question>, String> {
    let raw = crate::input::present(input, "questions")
        .ok_or_else(|| "missing required field `questions`".to_string())?;
    let raw = raw.as_array().ok_or_else(|| {
        format!(
            "field `questions` must be an array, got {}",
            crate::input::type_name(raw)
        )
    })?;

    if raw.is_empty() {
        return Err("`questions` is empty — ask at least one question, or do not call this tool"
            .to_string());
    }
    if raw.len() > MAX_QUESTIONS {
        return Err(format!(
            "`questions` has {} entries but at most {MAX_QUESTIONS} may be asked in one call — \
             ask the {MAX_QUESTIONS} that most change what you would do, and ask the rest after \
             those are answered",
            raw.len()
        ));
    }

    raw.iter()
        .enumerate()
        .map(|(index, value)| parse_question(index, value))
        .collect()
}

fn parse_question(index: usize, value: &Value) -> Result<Question, String> {
    let at = || format!("questions[{index}]");
    if !value.is_object() {
        return Err(format!(
            "{} must be an object, got {}",
            at(),
            crate::input::type_name(value)
        ));
    }

    let header = non_empty_str(value, "header", &at())?;
    let question = non_empty_str(value, "question", &at())?;

    let raw_options = crate::input::present(value, "options")
        .ok_or_else(|| format!("missing required field `{}.options`", at()))?;
    let raw_options = raw_options.as_array().ok_or_else(|| {
        format!(
            "field `{}.options` must be an array, got {}",
            at(),
            crate::input::type_name(raw_options)
        )
    })?;
    if raw_options.len() < MIN_OPTIONS || raw_options.len() > MAX_OPTIONS {
        return Err(format!(
            "`{}.options` has {} entries — offer {MIN_OPTIONS} to {MAX_OPTIONS}. A question with \
             fewer is not a question; one that needs more is really two questions, or wants the \
             free-text answer the driver always has.",
            at(),
            raw_options.len()
        ));
    }

    let mut options = Vec::with_capacity(raw_options.len());
    for (option_index, raw) in raw_options.iter().enumerate() {
        let where_ = format!("{}.options[{option_index}]", at());
        if !raw.is_object() {
            return Err(format!(
                "{where_} must be an object with a `label`, got {}",
                crate::input::type_name(raw)
            ));
        }
        let label = non_empty_str(raw, "label", &where_)?;
        // A duplicate label makes the answer ambiguous: `Answer::chosen`
        // carries labels, so two identical ones cannot be told apart by
        // anything downstream.
        if options
            .iter()
            .any(|existing: &QuestionOption| existing.label == label)
        {
            return Err(format!(
                "{where_} repeats the label `{label}` — two options that read the same cannot be \
                 told apart in the answer"
            ));
        }
        let description = crate::input::optional_str(raw, "description")
            .map_err(|e| format!("{where_}: {e}"))?
            .unwrap_or_default()
            .to_string();
        options.push(QuestionOption {
            label: label.to_string(),
            description,
        });
    }

    let multi_select = crate::input::optional_bool(value, "multi_select")
        .map_err(|e| format!("{}: {e}", at()))?
        .unwrap_or(false);

    Ok(Question {
        header: header.to_string(),
        question: question.to_string(),
        options,
        multi_select,
    })
}

/// A required string field that is present, a string, and not just spaces.
///
/// Blank-but-present is its own failure and gets its own wording: a model
/// that sent `"header": ""` did not omit the field, and telling it the field
/// is missing describes a different mistake than the one it made — the exact
/// defect [`crate::input`] exists to stop.
fn non_empty_str<'a>(value: &'a Value, field: &str, at: &str) -> Result<&'a str, String> {
    let raw = crate::input::present(value, field)
        .ok_or_else(|| format!("missing required field `{at}.{field}`"))?;
    let text = raw.as_str().ok_or_else(|| {
        format!(
            "field `{at}.{field}` must be a string, got {}",
            crate::input::type_name(raw)
        )
    })?;
    if text.trim().is_empty() {
        return Err(format!("field `{at}.{field}` is empty — it must say something"));
    }
    Ok(text)
}

/// What the model reads back.
///
/// Prose rather than the JSON it also gets as `data`, because the model reads
/// `content`: an answer set rendered as a transcript of what was asked and
/// what came back is directly usable, while a JSON blob invites the model to
/// quote it instead of acting on it. Notes are rendered on their own line and
/// labelled, because a note appended to its option reads as part of the
/// option.
fn render_outcome(outcome: &QuestionOutcome) -> String {
    match outcome {
        QuestionOutcome::Answered { answers } => {
            let mut out = String::from("The driver answered:\n");
            for answer in answers {
                out.push_str(&format!("\n{} — {}\n", answer.header, answer.question));
                for choice in &answer.chosen {
                    out.push_str(&format!("  → {choice}\n"));
                }
                if answer.chosen.is_empty() {
                    out.push_str("  → (no choice recorded)\n");
                }
                if let Some(note) = &answer.note {
                    out.push_str(&format!("  note: {note}\n"));
                }
            }
            out.push_str(
                "\nAct on these. A note narrows or overrides the option it sits under — it is \
                 the driver's actual answer, not a comment on it.",
            );
            out
        }
        QuestionOutcome::Deferred { note } => {
            let mut out = String::from(
                "The driver wants to talk this through rather than pick from the options — the \
                 question landed, the choices did not fit.",
            );
            if !note.trim().is_empty() {
                out.push_str(&format!("\n\nThey said: {note}"));
            }
            out.push_str(
                "\n\nDo not re-ask the same question. Stop and reply to them in your next \
                 message: say what you understand the open decision to be, and what you need \
                 from them to settle it.",
            );
            out
        }
        QuestionOutcome::Declined { reason } => reason.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use stella_protocol::Answer;

    use super::*;
    use crate::registry::question::QuestionResponder;

    fn ctx() -> crate::ctx::ToolCtx {
        crate::ctx::ToolCtx::new(std::env::temp_dir(), NAME, None, Vec::new())
    }

    fn valid_input() -> Value {
        json!({
            "questions": [{
                "header": "Auth method",
                "question": "Which auth should the new endpoint use?",
                "options": [
                    { "label": "Session cookie", "description": "Matches the rest of the app" },
                    { "label": "Bearer token" }
                ]
            }]
        })
    }

    /// A responder that answers with a fixed outcome and records what it was
    /// asked.
    struct Scripted {
        outcome: QuestionOutcome,
        seen: std::sync::Mutex<Option<QuestionRequest>>,
    }

    #[async_trait]
    impl QuestionResponder for Scripted {
        async fn respond(&self, request: &QuestionRequest) -> QuestionOutcome {
            *self.seen.lock().unwrap() = Some(request.clone());
            self.outcome.clone()
        }
    }

    fn tool_answering(outcome: QuestionOutcome) -> (AskQuestion, Arc<Scripted>) {
        let responder = Arc::new(Scripted {
            outcome,
            seen: std::sync::Mutex::new(None),
        });
        let slot: QuestionSlot = Arc::new(std::sync::RwLock::new(
            QuestionBroker::interactive(responder.clone(), Duration::from_secs(5)),
        ));
        (AskQuestion::new(slot), responder)
    }

    /// **The witness.** A batch reaches the driver, and the answer — notes
    /// included — comes back in both halves of the output: the prose the
    /// model reads and the structured `data` a contract can check.
    #[tokio::test]
    async fn an_answer_and_its_note_reach_the_model() {
        let (tool, responder) = tool_answering(QuestionOutcome::Answered {
            answers: vec![Answer {
                header: "Auth method".into(),
                question: "Which auth should the new endpoint use?".into(),
                chosen: vec!["Session cookie".into()],
                note: Some("but only for the admin routes".into()),
            }],
        });

        let out = tool.execute(&valid_input(), &ctx()).await;
        let ToolOutput::Ok { content, data } = out else {
            panic!("a scripted answer must succeed: {out:?}");
        };

        assert!(content.contains("Session cookie"), "{content}");
        assert!(
            content.contains("note: but only for the admin routes"),
            "the note must survive to the model — it is the actual answer: {content}"
        );
        let data = data.expect("an answered call carries structured data");
        assert_eq!(data["outcome"], "answered");
        assert_eq!(data["answers"][0]["note"], "but only for the admin routes");

        // The question reached the responder intact, description included.
        let seen = responder.seen.lock().unwrap().clone().expect("asked");
        assert_eq!(seen.questions.len(), 1);
        assert_eq!(seen.questions[0].options[0].description, "Matches the rest of the app");
        assert!(!seen.questions[0].multi_select, "absent means single-select");
    }

    /// "Chat about this" is not a refusal, and the model must not read it as
    /// one: it succeeds, and the prose tells the model to reply rather than
    /// re-ask.
    #[tokio::test]
    async fn deferring_succeeds_and_is_distinct_from_declining() {
        let (tool, _) = tool_answering(QuestionOutcome::Deferred {
            note: "the second option needs more thought".into(),
        });
        let out = tool.execute(&valid_input(), &ctx()).await;
        let ToolOutput::Ok { content, data } = out else {
            panic!("a deferral is not a failure: {out:?}");
        };
        assert_eq!(data.expect("data")["outcome"], "deferred");
        assert!(content.contains("talk this through"), "{content}");
        assert!(content.contains("Do not re-ask"), "{content}");
        assert!(
            content.contains("the second option needs more thought"),
            "their words must survive: {content}"
        );
    }

    /// A decline is an error the model can act on, classed as a policy
    /// refusal rather than a tool defect — and a TTL expiry is classed as
    /// the timeout it actually is.
    #[tokio::test]
    async fn a_decline_is_classed_a_refusal_and_a_timeout_a_timeout() {
        let (tool, _) = tool_answering(QuestionOutcome::Declined {
            reason: "cancelled by the user".into(),
        });
        match tool.execute(&valid_input(), &ctx()).await {
            ToolOutput::Error { message, class } => {
                assert_eq!(class, Some(ErrorClass::RefusedByPolicy));
                assert_eq!(message, "cancelled by the user");
            }
            other => panic!("a decline must be an error: {other:?}"),
        }

        let (tool, _) = tool_answering(QuestionOutcome::Declined {
            reason: QUESTION_TIMED_OUT.to_string(),
        });
        match tool.execute(&valid_input(), &ctx()).await {
            ToolOutput::Error { class, .. } => assert_eq!(class, Some(ErrorClass::Timeout)),
            other => panic!("a TTL expiry must be a timeout: {other:?}"),
        }
    }

    /// With no responder attached the tool still answers — with the
    /// instruction to decide, not a wall and not a hang.
    #[tokio::test]
    async fn a_headless_run_is_told_to_decide_for_itself() {
        let tool = AskQuestion::new(QuestionSlot::default());
        match tool.execute(&valid_input(), &ctx()).await {
            ToolOutput::Error { message, class } => {
                assert_eq!(class, Some(ErrorClass::RefusedByPolicy));
                assert!(message.contains("make the call yourself"), "{message}");
            }
            other => panic!("headless must decline: {other:?}"),
        }
    }

    /// Every bound the description promises is enforced, and each refusal
    /// names where the problem is. A promised-but-unenforced bound is worse
    /// than an unpromised one: the model believes it complied.
    #[tokio::test]
    async fn every_advertised_bound_is_enforced_and_located() {
        let tool = AskQuestion::new(QuestionSlot::default());
        let cases: Vec<(Value, &str)> = vec![
            (json!({}), "missing required field `questions`"),
            (json!({ "questions": [] }), "is empty"),
            (json!({ "questions": "one" }), "must be an array"),
            (
                json!({ "questions": (0..5).map(|i| json!({
                    "header": format!("H{i}"),
                    "question": "?",
                    "options": [{"label": "a"}, {"label": "b"}]
                })).collect::<Vec<_>>() }),
                "at most 4",
            ),
            (
                json!({ "questions": [{ "question": "?", "options": [] }] }),
                "`questions[0].header`",
            ),
            (
                json!({ "questions": [{ "header": "  ", "question": "?", "options": [] }] }),
                "is empty — it must say something",
            ),
            (
                json!({ "questions": [{ "header": "H", "question": "?", "options": [{"label": "a"}] }] }),
                "`questions[0].options` has 1 entries",
            ),
            (
                json!({ "questions": [{ "header": "H", "question": "?", "options": [
                    {"label": "a"}, {"label": "a"}
                ] }] }),
                "repeats the label",
            ),
            (
                json!({ "questions": [{ "header": "H", "question": "?", "options": [
                    {"label": "a"}, {"description": "no label"}
                ] }] }),
                "`questions[0].options[1].label`",
            ),
            (
                json!({ "questions": [{ "header": "H", "question": 42, "options": [
                    {"label": "a"}, {"label": "b"}
                ] }] }),
                "must be a string, got number",
            ),
        ];

        for (input, expected) in cases {
            match tool.execute(&input, &ctx()).await {
                ToolOutput::Error { message, class } => {
                    assert_eq!(
                        class,
                        Some(ErrorClass::InvalidInput),
                        "a malformed call is the model's mistake: {input}"
                    );
                    assert!(
                        message.contains(expected),
                        "for {input}\n  expected a message containing {expected:?}\n  got {message:?}"
                    );
                }
                other => panic!("must refuse {input}: {other:?}"),
            }
        }
    }

    /// The schema's two flags are the load-bearing ones — `read_only` is what
    /// lets a delegated child reach the tool at all, and `speculation_safe`
    /// being false is what stops a human being asked twice.
    #[test]
    fn the_schema_declares_read_only_but_never_speculation_safe() {
        let schema = AskQuestion::new(QuestionSlot::default()).schema();
        assert_eq!(schema.name, NAME);
        assert!(
            schema.read_only,
            "a child behind ReadOnlyTools reaches this tool by exactly this flag"
        );
        assert!(
            !schema.speculation_safe,
            "a speculated call would ask a human the same question twice"
        );
    }

    /// The description must not invite the free-text option the runtime
    /// appends for itself — a model that lists its own "Other" produces a
    /// card with two of them.
    #[test]
    fn the_description_forbids_a_self_authored_free_text_option() {
        let schema = AskQuestion::new(QuestionSlot::default()).schema();
        assert!(
            schema.description.contains("Never offer a free-text"),
            "the runtime appends one; the asker must be told not to: {}",
            schema.description
        );
    }
}
