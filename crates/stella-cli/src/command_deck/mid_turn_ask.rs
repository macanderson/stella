// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! How the Command Deck answers a **mid-turn ask** — the two places a tool
//! call parks on the person driving.
//!
//! There are exactly two, they are the same shape, and they are declared
//! together as one [`crate::rules::MidTurnAsk::Surface`] at session assembly:
//!
//! - an **approval** (#2676): a gate decided a call needs a human yes/no, so
//!   [`DeckApprovalResponder`] raises the #4240 card — every field of the
//!   request as a field, `read_only` and `gate` included — and waits for the
//!   driver's [`ApprovalResponse`];
//! - a **question** (#4212): an agent cannot make a decision itself, so
//!   [`DeckQuestionResponder`] raises the #4220 overlay and waits for the
//!   whole [`QuestionOutcome`] the fold produces.
//!
//! [`DeckAskUserIo`] is here too, but it is no longer either of them: it
//! backs the deck's *generic* `AskUser` card, which slash commands
//! (`/init`'s confirmations) still ask through.
//!
//! # Why the deck cannot use the plain-TTY responders
//!
//! Both of `stella-cli`'s TTY responders answer by printing to stdout and
//! blocking on a stdin line. The deck holds the terminal in raw mode and
//! owns its own input loop, so that read would fight it for every keystroke
//! — which is why the deck was stuck declaring itself headless, and every
//! `ask_question` on Stella's default interactive shell resolved to the
//! "no driver is attached" decline (#4220).
//!
//! Everything here is transport. No decision about what a driver may do
//! lives in this file: the folds (`stella_tui::views::question` and
//! `stella_tui::v2::approval`) own those, and are unit-tested without a
//! terminal.
//!
//! The one invariant this file does carry is the **direction of failure**.
//! An approval that cannot be answered — closed deck, dropped wait, stranded
//! decision — denies. No path here produces an `Approve` the driver did not
//! choose.
//!
//! # This module is deliberately separate from `command_deck.rs`
//!
//! That file is a god file closed to growth
//! (`scripts/file-size-baseline.txt`). These types are cohesive on their own
//! — one subject, one seam — so they land here rather than pushing the
//! ceiling up.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use stella_protocol::{AgentEvent, QuestionOutcome, QuestionRequest, ToolOutput};
use stella_tui::Inbound;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use stella_tools::registry::approval::{ApprovalRequest, ApprovalResponse};

use crate::interactive::{AskUserIo, FREE_TEXT_LABEL};

/// Ids for the cards [`DeckAskUserIo`] mints (`deck-ask-N`). Process-unique
/// rather than per-session: a stale answer from a cancelled turn must never
/// match a live card's id.
static NEXT_DECK_ASK: AtomicU64 = AtomicU64::new(0);

/// How long the driver gets to answer an approval **on the deck** (#4253).
///
/// Longer than [`stella_tools::registry::approval::DEFAULT_APPROVAL_TTL`],
/// which is the number for a line-oriented prompt — one sentence and a `y`.
/// The deck's card (#4240) is a different question to answer: five fields to
/// read, a gate to weigh, often a path or command line worth going and
/// looking at before you decide, and a third row that opens a text editor for
/// the refusal reason. Two minutes covers reading it and nothing else.
///
/// Ten, not thirty. The upper bound is not the driver's patience but the cost
/// of being wrong in the other direction: this TTL is how long a wedged or
/// abandoned surface can hold a tool call parked, and an approval — unlike a
/// question — interrupts someone who is already watching the turn. It stays
/// **below** [`DECK_QUESTION_TTL`] on purpose; see
/// `stella_tui::deck_ui::parked`, whose precedence rule leans on the approval
/// being the tighter of the two deadlines.
///
/// An expiry denies, so the failure direction is safe either way. What the
/// old number cost was not safety but *silence*: the card vanished mid-read
/// and the turn proceeded on a decision the driver never made.
pub(crate) const DECK_APPROVAL_TTL: Duration = Duration::from_secs(10 * 60);

/// How long the driver gets to answer a question on the deck.
///
/// The same thirty minutes the port defaults to — the deck's overlay presents
/// what the plain-TTY card presents, so nothing about this surface changes
/// the reasoning in
/// [`stella_tools::registry::question::DEFAULT_QUESTION_TTL`]. Named here
/// anyway rather than reaching for the default at the call site, so both of
/// the deck's deadlines are stated in one place and a future change to either
/// is a visible edit rather than a silently inherited constant.
pub(crate) const DECK_QUESTION_TTL: Duration =
    stella_tools::registry::question::DEFAULT_QUESTION_TTL;

/// Both of the deck's mid-turn ask responders, as the one posture
/// `enforce_workspace_rules` consumes — plus the [`DeckAskUserIo`] the
/// caller also needs for its slash commands.
///
/// One constructor rather than three exported types, because they are one
/// declaration: a surface that can park a call on a yes/no is exactly a
/// surface that can park one on a question, and handing them out separately
/// would let a caller arm half of it.
///
/// The io comes back because slash commands ask their own questions through
/// the generic `AskUser` card (`/init`'s confirmations), and it must be the
/// **same** io — it holds the receiver, and a second one built over a clone
/// of the channel would race it for every answer. Note it is no longer what
/// answers *approvals*: #4240 gave those their own card, because the generic
/// one flattened a five-field `ApprovalRequest` into a line of prose.
pub(crate) fn surface(
    agent: String,
    inbound: UnboundedSender<Inbound>,
    asks: UnboundedReceiver<String>,
    approvals: UnboundedReceiver<ApprovalResponse>,
    questions: UnboundedReceiver<QuestionOutcome>,
) -> (crate::rules::MidTurnAsk, DeckAskUserIo) {
    let ask_io = DeckAskUserIo {
        agent,
        inbound: inbound.clone(),
        answers: Arc::new(tokio::sync::Mutex::new(asks)),
    };
    let posture = crate::rules::MidTurnAsk::Surface {
        approval: Arc::new(DeckApprovalResponder {
            inbound: inbound.clone(),
            answers: Arc::new(tokio::sync::Mutex::new(approvals)),
        }),
        approval_ttl: DECK_APPROVAL_TTL,
        question: Arc::new(DeckQuestionResponder {
            inbound,
            answers: Arc::new(tokio::sync::Mutex::new(questions)),
        }),
        question_ttl: DECK_QUESTION_TTL,
    };
    (posture, ask_io)
}

/// [`AskUserIo`] over the deck's channels — how the approvals plane's
/// questions (scope review's confirm) reach the human at the deck. `prompt`
/// emits an `AskUser` card, awaits the user's `AskUserAnswer`, echoes the
/// answer back as the card's own `ToolResult` (the event-pure clear), and
/// returns the answer with an exact option match becoming its 1-based index
/// (the numeric quick-pick), anything else passing verbatim as free text.
#[derive(Clone)]
pub(crate) struct DeckAskUserIo {
    pub(crate) agent: String,
    pub(crate) inbound: UnboundedSender<Inbound>,
    pub(crate) answers: Arc<tokio::sync::Mutex<UnboundedReceiver<String>>>,
}

#[async_trait]
impl AskUserIo for DeckAskUserIo {
    async fn prompt(&self, question: &str, options: &[String]) -> Result<String, String> {
        // A caller may append the free-text affordance; the deck's card
        // renders its own (Enter submits the composer), so presenting the
        // label as a pickable option would double it — and picking it would
        // return the label itself as an "answer". Strip it; every other
        // option passes through untouched.
        let mut presented: Vec<String> = options.to_vec();
        if presented
            .last()
            .is_some_and(|o| o.starts_with(FREE_TEXT_LABEL))
        {
            presented.pop();
        }

        let id = format!("deck-ask-{}", NEXT_DECK_ASK.fetch_add(1, Ordering::Relaxed));
        let mut answers = self.answers.lock().await;
        // Drop answers stranded by a cancelled turn — they belong to a card
        // that no longer exists.
        while answers.try_recv().is_ok() {}

        let _ = self.inbound.send(Inbound::Event {
            agent: self.agent.clone(),
            event: AgentEvent::AskUser {
                id: id.clone(),
                question: question.to_string(),
                options: presented.clone(),
            },
        });

        let answer = answers
            .recv()
            .await
            .ok_or_else(|| "the deck closed before the question was answered".to_string())?;

        // The echoed ToolResult is what clears the pending card in the fold
        // (matched by this exact id) — without it the gate would keep eating
        // keys for the rest of the turn.
        let _ = self.inbound.send(Inbound::Event {
            agent: self.agent.clone(),
            event: AgentEvent::ToolResult {
                call_id: id,
                output: ToolOutput::Ok {
                    content: answer.clone(),
                    data: None,
                },
                duration_ms: 0,
                speculated: false,
                sub_agent_id: None,
            },
        });

        match presented.iter().position(|option| *option == answer) {
            Some(i) => Ok((i + 1).to_string()),
            None => Ok(answer),
        }
    }
}

/// [`ApprovalResponder`][a] over the deck's channels (#4240): raise the
/// approval card, park until the driver decides, take the card down.
///
/// Replaces wrapping [`DeckAskUserIo`] in the shared
/// [`crate::approval::AskUserApprovalResponder`], which worked but had to
/// flatten a five-field [`ApprovalRequest`] into one line of prose — losing
/// `read_only` and `gate` entirely on the way to the person deciding.
/// Handing the whole request to the card keeps every field a field.
///
/// **Denies on every path that is not an explicit approval**: a closed deck,
/// a dropped wait, a card the driver dismissed. There is no arm here that
/// produces [`ApprovalResponse::Approve`] without the driver having chosen
/// it, and that is the property to preserve if this is ever edited.
///
/// [a]: stella_tools::registry::approval::ApprovalResponder
pub(crate) struct DeckApprovalResponder {
    pub(crate) inbound: UnboundedSender<Inbound>,
    pub(crate) answers: Arc<tokio::sync::Mutex<UnboundedReceiver<ApprovalResponse>>>,
}

#[async_trait]
impl stella_tools::registry::approval::ApprovalResponder for DeckApprovalResponder {
    async fn respond(&self, request: &ApprovalRequest) -> ApprovalResponse {
        let mut answers = self.answers.lock().await;
        // Drop decisions stranded by a cancelled turn — they answer a card
        // that no longer exists, and reading one here would let a stale
        // "allow" approve a call the driver never saw.
        while answers.try_recv().is_ok() {}

        let _withdraw = WithdrawOnDrop {
            inbound: self.inbound.clone(),
            withdraw: Inbound::ApprovalWithdrawn,
        };
        let _ = self
            .inbound
            .send(Inbound::ApprovalAsked(Box::new(request.clone())));

        match answers.recv().await {
            Some(response) => response,
            // The deck went away mid-approval. Deny, naming the cause —
            // never approve on silence.
            None => ApprovalResponse::Deny {
                reason: "the deck closed before the approval was answered".to_string(),
            },
        }
    }
}

/// [`QuestionResponder`][r] over the deck's channels (#4220): raise the question
/// overlay, park until the driver settles it, take the card down.
///
/// The deck's counterpart to [`crate::question::TtyQuestionResponder`]. That
/// one renders a card to stdout and blocks on a stdin line; this one hands
/// the whole [`QuestionRequest`] to the render loop as
/// [`Inbound::QuestionAsked`] and waits for the [`QuestionOutcome`] the
/// overlay folds out — so the wait never touches the terminal the deck is
/// holding, and the deck keeps rendering (and stays Ctrl-C-able) throughout.
///
/// The overlay is a pure fold, so **every** decision about what the driver
/// may do — the note editor, the free-text row, the review pane's three ways
/// out — lives in `stella_tui::views::question` and is unit-tested without a
/// terminal. Nothing here interprets an answer; it only carries one.
///
/// [r]: stella_tools::registry::question::QuestionResponder
pub(crate) struct DeckQuestionResponder {
    pub(crate) inbound: UnboundedSender<Inbound>,
    pub(crate) answers: Arc<tokio::sync::Mutex<UnboundedReceiver<QuestionOutcome>>>,
}

/// Takes a card down however its wait ends — `withdraw` is the envelope for
/// whichever card is up.
///
/// A `Drop` guard rather than a line at each exit, because the exit that
/// matters is the one with no line to put it on: at its TTL the broker's
/// `timeout` drops the `respond` future where it stands, and no code in this
/// file runs again. Without this the deck would keep a live-looking card up
/// over a call that had already given up on it, and a driver's considered
/// answer would go into a oneshot nobody was holding.
///
/// Both TTLs need it, and [`DECK_APPROVAL_TTL`] is the shorter, so the
/// approval card is the one a driver is more likely to watch expire.
struct WithdrawOnDrop {
    inbound: UnboundedSender<Inbound>,
    withdraw: Inbound,
}

impl Drop for WithdrawOnDrop {
    fn drop(&mut self) {
        let _ = self.inbound.send(self.withdraw.clone());
    }
}

#[async_trait]
impl stella_tools::registry::question::QuestionResponder for DeckQuestionResponder {
    async fn respond(&self, request: &QuestionRequest) -> QuestionOutcome {
        let mut answers = self.answers.lock().await;
        // Drop outcomes stranded by a cancelled turn — they answer a card
        // that no longer exists, and reading one as this question's answer
        // would resolve it without the driver having looked at it.
        while answers.try_recv().is_ok() {}

        let _withdraw = WithdrawOnDrop {
            inbound: self.inbound.clone(),
            withdraw: Inbound::QuestionWithdrawn,
        };
        let _ = self
            .inbound
            .send(Inbound::QuestionAsked(Box::new(request.clone())));

        match answers.recv().await {
            Some(outcome) => outcome,
            // The deck went away mid-question. Declined, never a silent
            // default: the model must hear that no answer is coming rather
            // than act on one nobody gave.
            None => QuestionOutcome::Declined {
                reason: "the deck closed before the question was answered — do not re-ask; \
                         proceed with your best judgement and state the assumption you made"
                    .to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use stella_protocol::{Answer, Question, QuestionOption};
    use stella_tools::registry::approval::ApprovalResponder as _;
    use stella_tools::registry::question::QuestionResponder as _;
    use tokio::sync::mpsc;

    use super::*;

    fn approval_request() -> ApprovalRequest {
        ApprovalRequest {
            parked: stella_tools::registry::approval::ApprovalSubject::Tool {
                name: "bash".into(),
                read_only: false,
            },
            reason: "matched rule no-destructive-shell".into(),
            gate: "command.started".into(),
            subject: Some("rm -rf build/".into()),
        }
    }

    /// **The #4253 witness.** The deck asks for more time to answer an
    /// approval than the line-oriented default allows.
    ///
    /// `DEFAULT_APPROVAL_TTL` was chosen for a one-line prompt and a `y`.
    /// #4240 replaced that with a card carrying five fields and a typed
    /// refusal, and left the deadline alone — so a driver who went to look at
    /// what `rm -rf build/` would actually delete could come back to a card
    /// that had already denied on their behalf, with no signal that it had.
    ///
    /// Asserted through `surface()` rather than on the constant, because the
    /// constant being large proves nothing: the defect was that the *wiring*
    /// reached past the surface's number for the port default.
    #[test]
    fn the_deck_asks_for_longer_than_a_line_oriented_prompt_would() {
        let (in_tx, _in_rx) = mpsc::unbounded_channel();
        let (_a, asks) = mpsc::unbounded_channel();
        let (_b, approvals) = mpsc::unbounded_channel();
        let (_c, questions) = mpsc::unbounded_channel();
        let (posture, _io) = surface("lead".into(), in_tx, asks, approvals, questions);

        let crate::rules::MidTurnAsk::Surface {
            approval_ttl,
            question_ttl,
            ..
        } = posture
        else {
            panic!("the deck declares a surface, not a headless or tty posture");
        };

        assert!(
            approval_ttl > stella_tools::registry::approval::DEFAULT_APPROVAL_TTL,
            "the deck's card takes longer to read than the prompt the default was chosen for"
        );
        // …and still the tighter of the two. `stella_tui::deck_ui::parked`
        // gives approval the keyboard ahead of a parked question partly
        // because it is the deadline that runs out first; flipping this
        // ordering would make that routing rule wrong without touching it.
        assert!(
            approval_ttl < question_ttl,
            "an approval interrupts someone already watching the turn — it must not \
             outlast a question, which asks them to go away and think"
        );
    }

    fn approval_responder() -> (
        DeckApprovalResponder,
        mpsc::UnboundedReceiver<Inbound>,
        mpsc::UnboundedSender<ApprovalResponse>,
    ) {
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        (
            DeckApprovalResponder {
                inbound: in_tx,
                answers: Arc::new(tokio::sync::Mutex::new(out_rx)),
            },
            in_rx,
            out_tx,
        )
    }

    /// **The #4240 witness.** The whole structured request reaches the deck —
    /// `read_only` and `gate` included, the two fields the generic `AskUser`
    /// card could not carry — and the driver's decision comes back.
    #[tokio::test]
    async fn the_whole_approval_request_reaches_the_card() {
        let (responder, mut inbound, decisions) = approval_responder();
        let ask = approval_request();
        let deciding = tokio::spawn(async move { responder.respond(&ask).await });

        let Some(Inbound::ApprovalAsked(carried)) = inbound.recv().await else {
            panic!("the approval must reach the deck as a card");
        };
        assert_eq!(carried.parked.tool(), Some("bash"));
        assert!(
            !carried.parked.read_only(),
            "read_only must survive — it is what separates a read from a write, \
             and the generic AskUser card dropped it entirely"
        );
        assert_eq!(
            carried.gate, "command.started",
            "the gate that raised the demand is what makes a deny defensible"
        );
        assert_eq!(carried.subject.as_deref(), Some("rm -rf build/"));

        decisions
            .send(ApprovalResponse::Deny {
                reason: "use the staging bucket instead".into(),
            })
            .expect("the parked dispatch is still listening");

        let ApprovalResponse::Deny { reason } = deciding.await.expect("settles") else {
            panic!("the driver denied");
        };
        assert_eq!(reason, "use the staging bucket instead");
    }

    /// **The safety property, at the transport layer.** Every way the wait
    /// can fail denies. There is no path to `Approve` the driver did not
    /// choose — and an approval that defaulted open would run the exact call
    /// a gate stopped.
    #[tokio::test]
    async fn every_failed_approval_wait_denies() {
        // A closed deck.
        let (responder, _inbound, decisions) = approval_responder();
        drop(decisions);
        assert!(
            matches!(
                responder.respond(&approval_request()).await,
                ApprovalResponse::Deny { .. }
            ),
            "a closed deck must deny"
        );

        // A decision stranded by a cancelled turn must not answer the next
        // card — least of all a stale `Approve`.
        let (responder, mut inbound, decisions) = approval_responder();
        decisions
            .send(ApprovalResponse::Approve)
            .expect("queued before anyone asked");
        let ask = approval_request();
        let deciding = tokio::spawn(async move { responder.respond(&ask).await });
        assert!(
            matches!(inbound.recv().await, Some(Inbound::ApprovalAsked(_))),
            "the stale approve must not have short-circuited the ask"
        );
        decisions
            .send(ApprovalResponse::Deny {
                reason: "the real answer".into(),
            })
            .expect("still listening");
        let ApprovalResponse::Deny { reason } = deciding.await.expect("settles") else {
            panic!("the live decision is the one that counts");
        };
        assert_eq!(reason, "the real answer");
    }

    /// The approval card comes down when its wait is abandoned, exactly
    /// like the question overlay's. [`DECK_APPROVAL_TTL`] is the shorter of
    /// the deck's two deadlines, so this is the card a driver is more likely
    /// to watch expire.
    #[tokio::test]
    async fn an_abandoned_approval_withdraws_the_card() {
        let (responder, mut inbound, _decisions) = approval_responder();
        let ask = approval_request();
        let timed_out = tokio::time::timeout(Duration::from_millis(30), responder.respond(&ask))
            .await
            .is_err();
        assert!(timed_out, "nobody decided, so the wait must expire");
        assert!(
            matches!(inbound.recv().await, Some(Inbound::ApprovalAsked(_))),
            "the card went up"
        );
        assert!(
            matches!(inbound.recv().await, Some(Inbound::ApprovalWithdrawn)),
            "and must come down again when the wait is abandoned"
        );
    }

    fn request() -> QuestionRequest {
        QuestionRequest {
            asker: Some("research-child".into()),
            questions: vec![Question {
                header: "Auth method".into(),
                question: "Which auth should the new endpoint use?".into(),
                options: vec![QuestionOption {
                    label: "Session cookie".into(),
                    description: String::new(),
                }],
                multi_select: false,
            }],
        }
    }

    fn responder() -> (
        DeckQuestionResponder,
        mpsc::UnboundedReceiver<Inbound>,
        mpsc::UnboundedSender<QuestionOutcome>,
    ) {
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        (
            DeckQuestionResponder {
                inbound: in_tx,
                answers: Arc::new(tokio::sync::Mutex::new(out_rx)),
            },
            in_rx,
            out_tx,
        )
    }

    /// **The transport witness.** The request reaches the deck as a card, and
    /// the outcome the overlay folds out comes back as this call's answer —
    /// note and all.
    ///
    /// The overlay's own fold is tested in `stella_tui::views::question`;
    /// what is proved here is that nothing is lost in the round trip, which
    /// is the half a pure fold cannot cover.
    #[tokio::test]
    async fn a_question_reaches_the_deck_and_its_answer_comes_back() {
        let (responder, mut inbound, answers) = responder();
        let ask = request();
        let asking = tokio::spawn(async move { responder.respond(&ask).await });

        // The card carries the whole request, attribution included — a driver
        // answering a fanned-out delegation needs to know whose question it is.
        let Some(Inbound::QuestionAsked(carried)) = inbound.recv().await else {
            panic!("the question must reach the deck as a card");
        };
        assert_eq!(carried.asker.as_deref(), Some("research-child"));
        assert_eq!(carried.questions.len(), 1);

        answers
            .send(QuestionOutcome::Answered {
                answers: vec![Answer {
                    header: "Auth method".into(),
                    question: "Which auth should the new endpoint use?".into(),
                    chosen: vec!["Session cookie".into()],
                    note: Some("only for the admin routes".into()),
                }],
            })
            .expect("the parked call is still listening");

        let QuestionOutcome::Answered { answers } = asking.await.expect("settles") else {
            panic!("the driver answered, so the call must be answered");
        };
        assert_eq!(answers[0].chosen, vec!["Session cookie"]);
        assert_eq!(
            answers[0].note.as_deref(),
            Some("only for the admin routes"),
            "the note must survive the round trip — it is the half of the answer \
             the option list could not hold"
        );
    }

    /// **The witness for the card that outlives its broker.** At the TTL the
    /// broker drops the `respond` future where it stands; the deck must be
    /// told to take the card down, or a driver types a considered answer into
    /// a oneshot nobody is holding.
    #[tokio::test]
    async fn an_abandoned_wait_withdraws_the_card() {
        let (responder, mut inbound, _answers) = responder();
        let ask = request();

        // Exactly what the broker does at `DEFAULT_QUESTION_TTL`: drop the
        // future mid-await, with no answer and no cooperation from us.
        let timed_out = tokio::time::timeout(Duration::from_millis(30), responder.respond(&ask))
            .await
            .is_err();
        assert!(timed_out, "nobody answered, so the wait must expire");

        assert!(
            matches!(inbound.recv().await, Some(Inbound::QuestionAsked(_))),
            "the card went up"
        );
        assert!(
            matches!(inbound.recv().await, Some(Inbound::QuestionWithdrawn)),
            "and must come down again when the wait is abandoned"
        );
    }

    /// A deck that goes away mid-question declines — never a silent default.
    /// The model has to hear that no answer is coming rather than act on one
    /// nobody gave.
    #[tokio::test]
    async fn a_closed_deck_declines_with_an_instruction() {
        let (responder, _inbound, answers) = responder();
        drop(answers);
        let QuestionOutcome::Declined { reason } = responder.respond(&request()).await else {
            panic!("a closed deck cannot answer");
        };
        assert!(reason.contains("do not re-ask"), "{reason}");
        assert!(reason.contains("state the assumption"), "{reason}");
    }

    /// An outcome stranded by a cancelled turn must not resolve the NEXT
    /// question: it answers a card the driver never saw, and reading it here
    /// would settle a decision nobody made.
    #[tokio::test]
    async fn a_stranded_answer_does_not_resolve_the_next_question() {
        let (responder, mut inbound, answers) = responder();
        answers
            .send(QuestionOutcome::Deferred {
                note: "from a turn that is already gone".into(),
            })
            .expect("queued before anyone asked");

        let ask = request();
        let asking = tokio::spawn(async move { responder.respond(&ask).await });
        assert!(
            matches!(inbound.recv().await, Some(Inbound::QuestionAsked(_))),
            "the stale answer must not have short-circuited the ask"
        );

        answers
            .send(QuestionOutcome::Declined {
                reason: "the real answer".into(),
            })
            .expect("still listening");
        let QuestionOutcome::Declined { reason } = asking.await.expect("settles") else {
            panic!("the live answer is the one that counts");
        };
        assert_eq!(reason, "the real answer");
    }
}
