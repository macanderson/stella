// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! How an agent's question reaches whoever is driving it (#4212).
//!
//! Deliberately the same shape as [`super::approval`], one layer over: a port
//! a surface implements, a broker that bounds the park with a TTL, an
//! assembly-time `attach_*`, and a headless outcome that names the missing
//! surface *and* what to do instead. The two flows differ in what they ask
//! — an approval asks permission for a call already decided on, a question
//! asks for a decision the model cannot make — but the park is the same park,
//! and inventing a second one would give the CLI two interactive-wait shapes
//! to keep in step.
//!
//! # Who answers is the host's fact, not the model's
//!
//! [`QuestionResponder`] is attached once per session by whoever is driving
//! that session. A top-level turn's host attaches the terminal surface, so a
//! human answers. A delegated sub-agent runs against this same registry
//! (`stella_core::ports::ReadOnlyTools` wraps it rather than replacing it),
//! so its questions reach that same driver — which is what makes the
//! agent-to-agent case work with no branch anywhere in the tool: a child asks
//! exactly the way its parent does, and the runtime resolves the audience.
//!
//! The one thing the flow adds for that case is *attribution*:
//! [`stella_protocol::QuestionRequest::asker`] is stamped from the bus's
//! agent stack, so the card can say which agent wants to know.
//!
//! # Why this flow serializes
//!
//! `ask_question` advertises `read_only: true` — truthfully, it changes
//! nothing — and that is load-bearing twice over: it is what lets a
//! `ReadOnlyTools`-wrapped child call the tool at all, and it is what makes
//! the engine's dispatcher run sibling calls **concurrently**. The second
//! consequence is a hazard the approval flow never had: two questions
//! arriving in one step would both reach for the terminal and interleave
//! their prompts, and the driver would answer a card assembled from two
//! agents' questions at once.
//!
//! So the broker holds a fairness gate ([`QuestionBroker::gate`]) and takes
//! it before asking. A concurrent second question waits for the first to
//! resolve and is then asked on its own. The TTL starts **after** the gate is
//! acquired, so time spent queueing never counts against the driver's time to
//! think.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use stella_protocol::{QuestionOutcome, QuestionRequest};

use super::ToolRegistry;

/// How long one question waits for its answer once it is actually being
/// asked, before the flow resolves to [`QuestionOutcome::Declined`].
///
/// Much longer than [`super::approval::DEFAULT_APPROVAL_TTL`], and for a
/// reason worth stating: an approval interrupts a human who is already
/// watching the turn and asks a yes/no about a call they can see, while a
/// question asks them to make a decision — read the options, weigh them,
/// often go and look at something first. Thirty minutes is long enough that a
/// driver who steps away mid-decision still comes back to a live card, and
/// bounded so a surface that dies while holding a question cannot wedge the
/// turn forever.
///
/// Hosts pass their own; this is the default they reach for absent a
/// better-informed number.
pub const DEFAULT_QUESTION_TTL: Duration = Duration::from_secs(30 * 60);

/// The [`QuestionOutcome::Declined`] reason of a TTL expiry — the exact
/// string, so callers and tests match it instead of parsing prose.
pub const QUESTION_TIMED_OUT: &str =
    "no answer came back in time — nobody appears to be at the controls; \
     decide it yourself and say which way you went and why";

/// How a question reaches whoever is driving — the port a surface implements
/// and injects at assembly time over its own interactive io.
///
/// The broker calls this at most once per question and bounds the await with
/// its TTL, so an implementation may block on real input indefinitely without
/// risking a hang. It is also called under the broker's fairness gate, so an
/// implementation never has to handle two overlapping questions.
#[async_trait]
pub trait QuestionResponder: Send + Sync {
    /// Present `request` and return how the driver resolved it.
    async fn respond(&self, request: &QuestionRequest) -> QuestionOutcome;
}

/// Where the session's broker lives, so the tool can read the *current* one
/// per call.
///
/// A slot rather than a broker handed to the tool at construction, and the
/// distinction is not cosmetic: [`ToolRegistry::attach_question_responder`]
/// runs **after** the registry is built (the host has to assemble its
/// interactive surface first), and it *replaces* the broker. A tool holding a
/// clone taken at construction would hold the headless one forever, and every
/// question in an interactive session would answer itself with the
/// decide-for-yourself decline — green tests, and the feature silently
/// absent. The same late-attachment shape as
/// [`crate::subagent::DispatcherSlot`], for the same reason.
pub type QuestionSlot = Arc<std::sync::RwLock<QuestionBroker>>;

/// The session's question flow: an optional responder, the TTL that bounds
/// every park, and the gate that keeps concurrent questions from interleaving.
///
/// Headless by default — [`QuestionBroker::ask`] then declines with the
/// decide-it-yourself message instead of asking.
#[derive(Clone)]
pub struct QuestionBroker {
    responder: Option<Arc<dyn QuestionResponder>>,
    ttl: Duration,
    /// Held across one ask so concurrent `ask_question` calls queue rather
    /// than race for the driver's attention — see the module docs on why
    /// `read_only: true` makes that a live case rather than a theoretical
    /// one. Shared across clones by construction: a per-clone gate would
    /// serialize nothing, since [`ToolRegistry::question_broker`] hands every
    /// caller a clone.
    gate: Arc<tokio::sync::Mutex<()>>,
}

impl Default for QuestionBroker {
    fn default() -> Self {
        Self {
            responder: None,
            ttl: DEFAULT_QUESTION_TTL,
            gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}

impl QuestionBroker {
    /// A broker that parks on `responder`, for at most `ttl` per question
    /// once that question reaches the front of the queue.
    #[must_use]
    pub fn interactive(responder: Arc<dyn QuestionResponder>, ttl: Duration) -> Self {
        Self {
            responder: Some(responder),
            ttl,
            gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// Whether a driver is attached at all.
    ///
    /// The tool reads this to shape its refusal, not to decide whether to
    /// ask: [`Self::ask`] answers correctly either way. A separate accessor
    /// exists so a caller can say "no driver" without first constructing a
    /// question nobody will read.
    #[must_use]
    pub fn has_responder(&self) -> bool {
        self.responder.is_some()
    }

    /// Ask `request` and return how it resolved.
    ///
    /// Takes the fairness gate first, then starts the TTL — queueing behind
    /// another question must not spend the driver's time to think.
    pub async fn ask(&self, request: &QuestionRequest) -> QuestionOutcome {
        let Some(responder) = &self.responder else {
            return QuestionOutcome::Declined {
                reason: headless_refusal(),
            };
        };
        let _turn = self.gate.lock().await;
        match tokio::time::timeout(self.ttl, responder.respond(request)).await {
            Ok(outcome) => outcome,
            Err(_elapsed) => QuestionOutcome::Declined {
                reason: QUESTION_TIMED_OUT.to_string(),
            },
        }
    }
}

/// The headless decline: names the missing surface AND what to do instead.
///
/// The second half is the whole point. A refusal that only says "nobody is
/// here" leaves the model to invent its own recovery, and the recovery it
/// invents is usually to ask again. Naming the alternative — decide, and
/// surface the assumption where a human will see it — turns a dead end into
/// an instruction, which is the same argument
/// [`super::approval`]'s grant-path message makes.
fn headless_refusal() -> String {
    "no driver is attached to answer questions in this run (it is unattended, \
     or the surface that would ask has no way to reach anyone) — do not ask \
     again; make the call yourself, and state the assumption and its \
     alternatives plainly in your final report so a human can correct it"
        .to_string()
}

impl ToolRegistry {
    /// Attach the session's question responder — the assembly-time seam.
    ///
    /// Until it is called the broker is headless and every question resolves
    /// to the decide-it-yourself decline; calling again replaces the previous
    /// responder.
    pub fn attach_question_responder(&self, responder: Arc<dyn QuestionResponder>, ttl: Duration) {
        *self.question.write().unwrap_or_else(|p| p.into_inner()) =
            QuestionBroker::interactive(responder, ttl);
    }

    /// The session's question broker (cheap clone — shared responder, and a
    /// **shared gate**, which is what makes serialization hold across the
    /// clones every dispatch takes).
    #[must_use]
    pub fn question_broker(&self) -> QuestionBroker {
        self.question
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use stella_protocol::{Answer, Question, QuestionOption};

    use super::*;

    fn one_question() -> QuestionRequest {
        QuestionRequest {
            asker: None,
            questions: vec![Question {
                header: "Scope".into(),
                question: "How far should this go?".into(),
                options: vec![QuestionOption {
                    label: "Just the parser".into(),
                    description: String::new(),
                }],
                multi_select: false,
            }],
        }
    }

    /// A responder that records the order calls entered and left, so a test
    /// can prove two asks did not overlap.
    struct OrderingResponder {
        log: Mutex<Vec<String>>,
        live: AtomicUsize,
        overlapped: AtomicUsize,
    }

    #[async_trait]
    impl QuestionResponder for OrderingResponder {
        async fn respond(&self, request: &QuestionRequest) -> QuestionOutcome {
            let who = request.asker.clone().unwrap_or_else(|| "top".into());
            if self.live.fetch_add(1, Ordering::SeqCst) > 0 {
                self.overlapped.fetch_add(1, Ordering::SeqCst);
            }
            self.log.lock().unwrap().push(format!("enter {who}"));
            // Yield across an await point so an unserialized flow would
            // genuinely interleave here rather than running to completion.
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.log.lock().unwrap().push(format!("leave {who}"));
            self.live.fetch_sub(1, Ordering::SeqCst);
            QuestionOutcome::Answered {
                answers: vec![Answer {
                    header: "Scope".into(),
                    question: "How far should this go?".into(),
                    chosen: vec!["Just the parser".into()],
                    note: None,
                }],
            }
        }
    }

    /// **The witness for the concurrency hazard `read_only: true` creates.**
    ///
    /// The engine dispatches sibling read-only calls concurrently, so two
    /// `ask_question` calls in one step run at once. Without the broker's
    /// gate they would both reach the responder and interleave their prompts
    /// on one terminal. Serialized, the log is two complete enter/leave
    /// pairs — never `enter, enter`.
    #[tokio::test]
    async fn concurrent_questions_are_asked_one_at_a_time() {
        let responder = Arc::new(OrderingResponder {
            log: Mutex::new(Vec::new()),
            live: AtomicUsize::new(0),
            overlapped: AtomicUsize::new(0),
        });
        let broker = QuestionBroker::interactive(responder.clone(), Duration::from_secs(30));

        let (first, second) = {
            let a = broker.clone();
            let b = broker.clone();
            let mut ask_a = one_question();
            ask_a.asker = Some("a".into());
            let mut ask_b = one_question();
            ask_b.asker = Some("b".into());
            tokio::join!(
                async move { a.ask(&ask_a).await },
                async move { b.ask(&ask_b).await },
            )
        };

        assert!(matches!(first, QuestionOutcome::Answered { .. }));
        assert!(matches!(second, QuestionOutcome::Answered { .. }));
        assert_eq!(
            responder.overlapped.load(Ordering::SeqCst),
            0,
            "two questions were live at once: {:?}",
            responder.log.lock().unwrap()
        );

        let log = responder.log.lock().unwrap().clone();
        assert_eq!(log.len(), 4, "two enter/leave pairs: {log:?}");
        for pair in log.chunks(2) {
            let who = pair[0].strip_prefix("enter ").expect("enter first");
            assert_eq!(pair[1], format!("leave {who}"), "interleaved: {log:?}");
        }
    }

    /// A clone must share the gate. `question_broker()` hands every dispatch
    /// a fresh clone, so a per-clone mutex would serialize nothing at all —
    /// the test above would still pass if it held one broker, which is why
    /// this one asks the question the production path actually asks.
    #[tokio::test]
    async fn clones_share_one_gate() {
        let broker = QuestionBroker::interactive(
            Arc::new(OrderingResponder {
                log: Mutex::new(Vec::new()),
                live: AtomicUsize::new(0),
                overlapped: AtomicUsize::new(0),
            }),
            Duration::from_secs(30),
        );
        let clone = broker.clone();
        let held = broker.gate.clone().lock_owned().await;
        // With a shared gate the clone cannot get in while `held` stands.
        let blocked = tokio::time::timeout(Duration::from_millis(50), clone.ask(&one_question()))
            .await
            .is_err();
        drop(held);
        assert!(blocked, "a clone acquired its own gate — nothing serializes");
    }

    /// Headless is not a hang and not a bare wall: the decline names what to
    /// do instead, and says not to ask again.
    #[tokio::test]
    async fn a_headless_broker_declines_with_an_instruction() {
        let broker = QuestionBroker::default();
        assert!(!broker.has_responder());
        match broker.ask(&one_question()).await {
            QuestionOutcome::Declined { reason } => {
                assert!(reason.contains("no driver is attached"), "{reason}");
                assert!(reason.contains("do not ask again"), "{reason}");
                assert!(reason.contains("state the assumption"), "{reason}");
            }
            other => panic!("nobody is here, so nothing may answer: {other:?}"),
        }
    }

    /// A responder that never answers resolves to a decline at the TTL, not
    /// to an indefinite park.
    #[tokio::test]
    async fn a_silent_driver_times_out_rather_than_hanging() {
        struct Never;
        #[async_trait]
        impl QuestionResponder for Never {
            async fn respond(&self, _request: &QuestionRequest) -> QuestionOutcome {
                std::future::pending().await
            }
        }
        let broker = QuestionBroker::interactive(Arc::new(Never), Duration::from_millis(30));
        match broker.ask(&one_question()).await {
            QuestionOutcome::Declined { reason } => assert_eq!(reason, QUESTION_TIMED_OUT),
            other => panic!("a silent driver must time out: {other:?}"),
        }
    }

    /// Queueing must not spend the driver's time to think: the TTL starts
    /// once the question is actually being asked. A second question held
    /// behind a slow first one for longer than the TTL still gets its full
    /// window.
    #[tokio::test]
    async fn the_ttl_starts_after_the_queue_not_before() {
        struct Slow;
        #[async_trait]
        impl QuestionResponder for Slow {
            async fn respond(&self, request: &QuestionRequest) -> QuestionOutcome {
                let hold = if request.asker.as_deref() == Some("first") {
                    Duration::from_millis(120)
                } else {
                    Duration::from_millis(20)
                };
                tokio::time::sleep(hold).await;
                QuestionOutcome::Deferred {
                    note: request.asker.clone().unwrap_or_default(),
                }
            }
        }
        // A TTL shorter than the first question's hold: if the second
        // question's clock started when it was submitted, it would expire
        // while queued.
        let broker = QuestionBroker::interactive(Arc::new(Slow), Duration::from_millis(80));
        let a = broker.clone();
        let b = broker.clone();
        let mut first = one_question();
        first.asker = Some("first".into());
        let mut second = one_question();
        second.asker = Some("second".into());

        let (first_out, second_out) = tokio::join!(
            async move { a.ask(&first).await },
            async move {
                // Make the ordering deterministic: the first ask takes the
                // gate before this one is submitted.
                tokio::time::sleep(Duration::from_millis(10)).await;
                b.ask(&second).await
            },
        );

        assert!(
            matches!(&first_out, QuestionOutcome::Declined { reason } if reason == QUESTION_TIMED_OUT),
            "the first question outran its own TTL: {first_out:?}"
        );
        assert!(
            matches!(&second_out, QuestionOutcome::Deferred { note } if note == "second"),
            "the second question's TTL was spent while it queued: {second_out:?}"
        );
    }
}
