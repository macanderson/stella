// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The overflow summarizer and its working-set restoration (#2685) — a child
//! module of `driver` (the `settlement.rs` pattern), holding the one
//! compaction path that destroys protected content and the machinery that
//! puts the destroyed working set back.
//!
//! [`Engine::summarize_overflow_span`] replaces the oldest viable span with a
//! model-written summary when the pure compaction passes cannot reach the
//! budget. That splice loses the working set inside the span — the body of
//! any skill the model was mid-way through executing. Immediately after the
//! splice lands, the engine restores it under one marker-prefixed tail
//! message:
//!
//! - **What** to restore is pure decision logic over the span, in
//!   [`crate::restore`] — captured before the splice, budgeted against the
//!   same compaction budget that triggered the round.
//! - **Active skill bodies** need no I/O at all: the body is the invocation
//!   message the span already held, and the live-slug set comes from the tool
//!   stack through [`crate::ports::ToolExecutor::active_skill_slugs`].
//!   (File contents are deliberately not restored — see [`crate::restore`]'s
//!   module docs: a file is re-readable on demand, a folded-away invocation
//!   body is not.)
//!
//! Restoration runs only on the committed success path. The budget-abort arm
//! applies a paid summary purely to *shrink* the context a resumed session
//! reloads — growing it back with restored content would spend headroom the
//! tripped budget no longer has, for a turn that is already over.

use stella_protocol::{
    AgentEvent, CompletionMessage, CompletionRequest, CompletionResult, MessageRole,
    ReasoningEffort,
};

use super::{Engine, SUMMARY_MARKER_PREFIX, lifecycle};
use crate::budget::BudgetGuard;
use crate::bus;
use crate::estimator::estimate_conversation_tokens;
use crate::event_sender::EventSender;
use crate::restore::WorkingSet;
use crate::retry::RetryPolicy;
use crate::starvation::{starved_of_output, starved_retry_cap, with_reasoning_headroom};
use crate::step::SummarizerHealth;
use crate::{AccountedCall, AccountedCallError, run_accounted_call};

/// The summarizer's **visible-output** contract: the dense bullet summary
/// `SUMMARIZE_SYSTEM` asks for. What reaches the wire is this plus
/// [`with_reasoning_headroom`]'s thinking room, because `max_output_tokens`
/// is one number and a reasoning model bills its thinking against it — the
/// bare 1,200 this site used to send could be spent entirely before the
/// first visible token (#2503), and an empty summary here trips the give-up
/// latch that disables compaction for the rest of the turn.
const SUMMARY_OUTPUT_CONTRACT: u32 = 1_200;

impl<'a> Engine<'a> {
    /// Replace the oldest viable span with a model-written summary. Failures
    /// leave the conversation untouched. Returns the summarizer call's spend.
    /// `step` is carried only to key this call's receipt against the step it
    /// rides, the same way [`Self::apply_overflow_summary`] carries the budget
    /// pair it reports. `instructions` is a PreCompact hook's `modify`
    /// steering for the summarizer, appended to its request (#2684).
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn summarize_overflow_span(
        &self,
        messages: &mut Vec<CompletionMessage>,
        budget: &mut BudgetGuard,
        compaction_budget: u64,
        factor: f64,
        health: &mut SummarizerHealth,
        step: usize,
        instructions: Option<&str>,
        events: &EventSender,
    ) -> f64 {
        // Span start: after the system prompt and the FIRST user message —
        // the task statement anchors every later step and must survive
        // verbatim. A Tool message can't open the kept tail either side of
        // the span (its assistant partner would be summarized away and the
        // provider rejects orphaned tool results), so both bounds walk off
        // Tool messages.
        let first_user = messages
            .iter()
            .position(|m| m.role == MessageRole::User)
            .unwrap_or(0);
        let mut start = first_user + 1;
        while start < messages.len() && messages[start].role == MessageRole::Tool {
            start += 1;
        }
        let mut end = messages
            .len()
            .saturating_sub(self.config.summarize_keep_recent);
        // `end == messages.len()` (keep_recent 0) has no kept tail to walk
        // off — indexing it would be out of bounds, not a Tool message.
        while end > start && end < messages.len() && messages[end].role == MessageRole::Tool {
            end -= 1;
        }
        // A tiny span isn't worth a model call — and this guard is also the
        // convergence backstop: once a summary message occupies the span,
        // the next over-budget step finds nothing left to replace and skips
        // instead of summarizing its own summary every step.
        if end <= start || end - start < 4 {
            return 0.0;
        }
        // Give-up latch: once the summarizer has failed this many steps in a
        // row this turn, stop re-firing it — each attempt is a completion and
        // its latency. The next model call overflows and surfaces one clear
        // failure instead of one per remaining step.
        if health.is_latched() {
            return 0.0;
        }
        // After the guards, not before: a latched or tiny-span step returns
        // above without needing this number, and the walk is Θ(transcript)
        // on precisely the steps whose transcript is at its largest.
        let before_tokens = crate::estimator::estimate_conversation_tokens(messages);
        let mut rendered = crate::summarize::render_span_for_summary(&messages[start..end]);
        // PreCompact steering rides the user message, after the span: the
        // system prompt stays the byte-stable SUMMARIZE_SYSTEM, and the
        // hook's words are visibly operator input, not span content.
        if let Some(extra) = instructions {
            rendered.push_str("\n\n[PreCompact hook instructions]\n");
            rendered.push_str(extra);
        }
        let summary_messages = vec![
            CompletionMessage::system(crate::summarize::SUMMARIZE_SYSTEM),
            CompletionMessage::user(&rendered),
        ];
        // The written contract plus room to think (#2503): `max_output_tokens`
        // is one number on the wire and a reasoning model bills its thinking
        // against it, so `effort: Low` alone bounds the reasoning budget
        // without zeroing it.
        let cap = with_reasoning_headroom(SUMMARY_OUTPUT_CONTRACT);
        // Kept only because a starved retry could change the outcome — the
        // same one-`Vec`-copy trade `metered_raw_call` makes for a management
        // prompt it might have to re-send (#2128).
        let retry = starved_retry_cap(Some(cap)).map(|raised| (raised, summary_messages.clone()));
        let mut outcome = self
            .dispatch_summarizer(summary_messages, cap, step, budget, events)
            .await;
        // What a superseded starved attempt already spent. Every arm below
        // adds it, because the retry is a real paid call: reporting only the
        // surviving attempt's cost would understate the turn by exactly what
        // the starvation cost.
        let mut superseded_cost = 0.0;
        // A provider that stopped at the token limit with nothing visible to
        // show for it has stated that the budget ran out before the first
        // answer token — the #2128 signature. Retried once with real room,
        // and deliberately BEFORE the give-up latch sees anything: the latch
        // exists for a summarizer that is broken, and a starved one is merely
        // short of room, which one retry fixes. A retry that also comes back
        // empty falls through to the empty-summary arm below, which is where
        // that failure is recorded (#2503).
        if let (Ok(result), Some((raised, summary_messages))) = (&outcome, retry)
            && starved_of_output(result)
        {
            superseded_cost = result.cost_usd;
            outcome = self
                .dispatch_summarizer(summary_messages, raised, step, budget, events)
                .await;
        }
        let result = match outcome {
            Ok(result) => result,
            // The summary was generated and paid for before the budget
            // tripped. Splicing it in only shrinks the context the resumed
            // session reloads, so apply it rather than discard the spend.
            // Deliberately no restoration on this arm — see the module docs.
            Err(AccountedCallError::Budget { result, .. }) => {
                let text = result.text.trim();
                if !text.is_empty() {
                    self.apply_overflow_summary(
                        messages,
                        start,
                        end,
                        before_tokens,
                        text,
                        compaction_budget,
                        factor,
                        events,
                    );
                }
                return superseded_cost + result.cost_usd;
            }
            // A silent 0.0 hid a failing summarizer that re-fired every step
            // until the provider hard-failed. Surface it and count it toward
            // the give-up latch.
            Err(AccountedCallError::Provider(e)) => {
                health.record_failure();
                let _ = events.send(AgentEvent::Error {
                    message: format!("overflow summarizer failed ({e}); context left intact"),
                    retryable: true,
                });
                return superseded_cost;
            }
            Err(AccountedCallError::Timeout) => {
                health.record_failure();
                let _ = events.send(AgentEvent::Error {
                    message: "overflow summarizer timed out; context left intact".to_string(),
                    retryable: true,
                });
                return superseded_cost;
            }
            // Refused before dispatch (#2238): nothing spent, nothing misbehaved.
            Err(AccountedCallError::Deadline { .. }) => {
                let _ = events.send(AgentEvent::Error {
                    message: "overflow summarizer skipped: the task deadline passed".into(),
                    retryable: false,
                });
                return superseded_cost;
            }
        };
        let cost_usd = superseded_cost + result.cost_usd;
        let text = result.text.trim();
        if text.is_empty() {
            // An empty summary shrinks nothing, so the next step re-fires on
            // the same span — the same non-progress the error paths latch
            // against, and just as invisible if left unannounced. Reached by
            // an empty `Stop` answer, and by a starved call whose retry came
            // back empty too — more room was bought once and did not help, so
            // this failure counts (#2503).
            health.record_failure();
            let _ = events.send(AgentEvent::Error {
                message: "overflow summarizer returned an empty summary; context left intact"
                    .to_string(),
                retryable: true,
            });
            return cost_usd;
        }
        health.reset();
        // Captured BEFORE the splice destroys the span (#2685): which live
        // skill bodies are about to be folded away, minus anything the kept
        // tail still holds.
        let working_set = crate::restore::collect_working_set(
            &messages[start..end],
            &messages[end..],
            &self.tools.active_skill_slugs(),
        );
        self.apply_overflow_summary(
            messages,
            start,
            end,
            before_tokens,
            text,
            compaction_budget,
            factor,
            events,
        );
        self.restore_working_set(messages, working_set, compaction_budget);
        cost_usd
    }

    /// One metered summarizer dispatch, so the starvation retry re-sends
    /// through exactly the accounting seam the first attempt used — the shape
    /// `complete_standalone`'s `dispatch` takes for the same reason (#2444).
    /// Everything but the messages and the cap is fixed: the summarizer
    /// dispatches exactly one role, which is why this is a helper and not a
    /// third per-role bounds table (#2503).
    async fn dispatch_summarizer(
        &self,
        messages: Vec<CompletionMessage>,
        max_output_tokens: u32,
        step: usize,
        budget: &mut BudgetGuard,
        events: &EventSender,
    ) -> Result<CompletionResult, AccountedCallError> {
        let estimated_input_tokens = estimate_conversation_tokens(&messages);
        run_accounted_call(
            AccountedCall {
                provider: self.active_provider(),
                role: stella_protocol::ModelCallRole::Summarization,
                // The same placeholder #2831 removed from the two engine emit
                // sites, and reachable the same way: this hint is what a
                // failed summarizer call's `UsageIncomplete` names, and the
                // provider dispatching it is right here.
                model_hint: self
                    .active_provider()
                    .model()
                    .unwrap_or(stella_protocol::UNKNOWN_MODEL)
                    .to_string(),
                request: CompletionRequest {
                    messages,
                    max_output_tokens: Some(max_output_tokens),
                    temperature: Some(0.0),
                    effort: Some(ReasoningEffort::Low),
                    tools: vec![],
                    reasoning: None,
                    params: None,
                },
                // The last line of defense before a terminal context
                // overflow — a transient blip here must be retried, not
                // fast-failed into an oversized, doomed next call.
                retry_policy: RetryPolicy::standard(),
                // The same generation deadline every worker call runs under.
                // `None` left a wedged summarizer provider parking the whole
                // turn indefinitely — the one un-bounded await on the step
                // path — and a trip lands in the caller's Timeout arm, which
                // counts toward the give-up latch like any other failure.
                timeout: self.config.model_timeout,
                estimated_input_tokens,
                // The summarizer rewrites the conversation it is called on, so
                // its own input is the only record of what it was given to
                // compress — and the span it replaces is gone from the history
                // afterwards. Reserved seat 1 at this step; compaction runs at
                // most once per step, so it collides with no other call. A
                // starved retry (#2503) re-emits at the same seat with the
                // identical messages, so its receipt overwrites the first
                // attempt's row with the same context rather than losing it.
                receipt: Some(crate::ReceiptContext {
                    turn_instance: self.config.turn_instance,
                    step,
                    call_seq: crate::receipts::RECEIPT_SEQ_SUMMARIZER,
                    lifecycle_enabled: self.config.lifecycle_enabled,
                }),
            },
            budget,
            events,
            self.sleeper,
        )
        .await
    }

    /// Splice `text` (trimmed, non-empty) over `messages[start..end]` as the
    /// overflow summary and announce the resulting shrink. Shared by the
    /// normal path and the budget-abort path: a paid summary is applied even
    /// when the turn is about to abort, since it only shrinks the context the
    /// resumed session reloads.
    ///
    /// The `Compaction` event's `after_tokens` measures the splice itself;
    /// the restoration message appended after it reports its own size on the
    /// `agent.working_set.restored` lifecycle event instead of retroactively
    /// inflating the splice's number.
    #[allow(clippy::too_many_arguments)]
    fn apply_overflow_summary(
        &self,
        messages: &mut Vec<CompletionMessage>,
        start: usize,
        end: usize,
        before_tokens: u64,
        text: &str,
        compaction_budget: u64,
        factor: f64,
        events: &EventSender,
    ) {
        let replaced = end - start;
        // Name which blocks left context (spec §6.2): the identity-bearing
        // tool-result blocks in the span the summary folds away, keyed the same
        // way the pure passes key theirs (`receipts::tool_result_block_id`), so
        // a receipt consumer can reconcile the summary splice against the
        // manifest. Captured before the splice mutates `messages`. Text
        // messages in the span carry no block identity, so this is a subset of
        // the `summarized` count, not a one-for-one list.
        let summarized_blocks: Vec<String> = messages[start..end]
            .iter()
            .flat_map(|m| m.tool_results.iter())
            .map(|r| crate::receipts::tool_result_block_id(&r.output))
            .collect();
        let summary = CompletionMessage::user(format!(
            "{SUMMARY_MARKER_PREFIX} to fit context — full detail was compacted away; \
             re-read files or re-run tools for specifics]\n\n{text}"
        ));
        messages.splice(start..end, std::iter::once(summary));
        let _ = events.send(AgentEvent::Compaction {
            before_tokens,
            after_tokens: crate::estimator::estimate_conversation_tokens(messages),
            evicted: 0,
            deduped: 0,
            superseded: 0,
            aged: 0,
            summarized: replaced,
            // The summary path runs none of the pure passes, so their block
            // vectors stay empty; the folded blocks are named in
            // `summarized_blocks`. It targets the same effective budget.
            evicted_blocks: Vec::new(),
            deduped_blocks: Vec::new(),
            superseded_blocks: Vec::new(),
            aged_blocks: Vec::new(),
            summarized_blocks,
            // A splice replaces whole messages; nothing is rewritten in place.
            rewrites: Vec::new(),
            effective_budget_tokens: compaction_budget,
            calibration_factor: factor,
        });
    }

    /// Re-attach the summarized-away working set as one marker-prefixed tail
    /// message, charged against the headroom the splice just opened under
    /// `compaction_budget` — so restoration structurally cannot defeat the
    /// compaction that triggered it.
    ///
    /// Skill bodies were captured from the span and need no I/O — the whole
    /// step is synchronous. An empty working set appends nothing.
    pub(super) fn restore_working_set(
        &self,
        messages: &mut Vec<CompletionMessage>,
        working_set: WorkingSet,
        compaction_budget: u64,
    ) {
        if working_set.is_empty() {
            return;
        }
        let headroom = compaction_budget.saturating_sub(estimate_conversation_tokens(messages));
        let Some(restoration) = crate::restore::render_restoration(&working_set.skills, headroom)
        else {
            return;
        };
        self.emit_lifecycle(bus::names::AGENT_WORKING_SET_RESTORED, || {
            lifecycle::working_set_restored_payload(&restoration)
        });
        // The restoration rides the volatile tail — the same zone as a user
        // steer or a parked-wait wake (invariant 7): the stable prefix and
        // every cacheable block before it stay byte-identical.
        messages.push(CompletionMessage::user(restoration.message));
    }
}

#[cfg(test)]
mod tests {
    //! Witnesses for #2685. The load-bearing assertions spell the marker as
    //! a string literal rather than naming `RESTORE_MARKER_PREFIX`, so the
    //! fail half of the witness can run against the base tree (which has
    //! neither the constant nor the behavior); `the_literal_is_the_constant`
    //! pins the spelling.

    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::Value;
    use stella_protocol::{
        CompletionMessage, CompletionRequestRef, CompletionResult, CompletionUsage, MessageRole,
        Provider, ProviderError, ToolOutput, ToolSchema,
    };
    use tokio::sync::mpsc;

    use crate::budget::BudgetGuard;
    use crate::driver::{Engine, EngineConfig};
    use crate::event_sender::EventSender;
    use crate::ports::ToolExecutor;
    use crate::retry::Sleeper;
    use crate::step::SummarizerHealth;
    use stella_protocol::BudgetMode;

    struct NoSleep;
    #[async_trait]
    impl Sleeper for NoSleep {
        async fn sleep(&self, _duration_ms: u64) {}
    }

    /// An executor that offers nothing. The starvation witnesses below drive
    /// the summarizer, never a tool, so the port only has to exist.
    struct NoTools;
    #[async_trait]
    impl ToolExecutor for NoTools {
        fn schemas(&self) -> Vec<ToolSchema> {
            Vec::new()
        }
        async fn execute(&self, _name: &str, _input: &Value) -> ToolOutput {
            ToolOutput::Ok {
                content: String::new(),
            }
        }
    }

    /// Always answers "SUMMARY" — the summarizer path under test is the
    /// restoration that follows the splice, not the summary itself.
    struct SummaryProvider;
    #[async_trait]
    impl Provider for SummaryProvider {
        fn id(&self) -> &str {
            "scripted"
        }
        async fn complete_ref(
            &self,
            _req: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            Ok(CompletionResult {
                text: "SUMMARY".into(),
                tool_calls: vec![],
                usage: CompletionUsage::reported_zero(),
                model: "scripted".into(),
                cost_usd: 0.0001,
                finish_reason: None,
                upstream_provider: None,
            })
        }
    }

    /// A tool-less executor carrying only the live-skill answer the port
    /// hands the engine — restoration needs no tools at all.
    struct SkillTools {
        active: Vec<String>,
    }
    #[async_trait]
    impl ToolExecutor for SkillTools {
        fn schemas(&self) -> Vec<ToolSchema> {
            Vec::new()
        }
        async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
            ToolOutput::error(format!("unknown tool `{name}`"))
        }
        fn active_skill_slugs(&self) -> Vec<String> {
            self.active.clone()
        }
    }

    fn filler(tag: &str) -> CompletionMessage {
        CompletionMessage {
            role: MessageRole::Assistant,
            content: format!("{tag}: {}", "analysis ".repeat(60)),
            tool_calls: vec![],
            tool_results: vec![],
            attachments: Vec::new(),
        }
    }

    fn config() -> EngineConfig {
        EngineConfig {
            summarize_keep_recent: 2,
            ..EngineConfig::default()
        }
    }

    /// The restoration tail message, if the transcript carries one.
    fn restoration_message(messages: &[CompletionMessage]) -> Option<&str> {
        messages
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .map(|m| m.content.as_str())
            .find(|c| c.starts_with("[working set restored"))
    }

    async fn summarize(
        engine: &Engine<'_>,
        messages: &mut Vec<CompletionMessage>,
        compaction_budget: u64,
    ) -> Vec<stella_protocol::AgentEvent> {
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let mut health = SummarizerHealth::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        engine
            .summarize_overflow_span(
                messages,
                &mut budget,
                compaction_budget,
                1.0,
                &mut health,
                0,
                None,
                &events,
            )
            .await;
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            out.push(event);
        }
        out
    }

    /// Witness (c): restoration is charged to the compaction budget that
    /// triggered the round — the post-restore conversation still fits it,
    /// and the skill body that did not fit is dropped whole and NAMED.
    /// Fails on base: no restoration message, so nothing names the drop.
    #[tokio::test]
    async fn restoration_fits_the_compaction_budget_and_names_the_dropped_skills() {
        let provider = SummaryProvider;
        let tools = SkillTools {
            active: vec!["small".into(), "huge".into()],
        };
        let engine = Engine::with_sleeper(&provider, &tools, config(), &NoSleep);
        let mut messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("the task"),
            filler("a0"),
            CompletionMessage::user(crate::skills::invoke::render_invocation_message(
                "small",
                "One short step.",
            )),
            CompletionMessage::user(crate::skills::invoke::render_invocation_message(
                "huge",
                &"a very long procedure step\n".repeat(2_000),
            )),
            filler("a1"),
            filler("a2"),
            filler("a3"),
        ];

        // Big enough for the splice plus the small body; far too small for
        // the huge one.
        let compaction_budget = 1_200;
        summarize(&engine, &mut messages, compaction_budget).await;

        let restored = restoration_message(&messages).expect("a restoration message exists");
        assert!(
            restored.contains("One short step."),
            "the small skill body is restored: {restored}"
        );
        assert!(
            !restored.contains(&"a very long procedure step\n".repeat(10)),
            "the over-budget body's content is not smuggled in"
        );
        assert!(
            restored.contains("Dropped for budget") && restored.contains("skill:huge"),
            "the dropped skill is named, never silent: {restored}"
        );
        assert!(
            crate::estimator::estimate_conversation_tokens(&messages) <= compaction_budget,
            "restoration never pushes the conversation back over the budget \
             that triggered the round"
        );
    }

    /// Witness (d): an ACTIVE invoked skill's body is restored verbatim; an
    /// inactive one is history and is not. Fails on base: neither appears.
    #[tokio::test]
    async fn an_active_skill_body_is_restored_and_an_inactive_one_is_not() {
        let provider = SummaryProvider;
        let tools = SkillTools {
            active: vec!["deploy".into()],
        };
        let engine = Engine::with_sleeper(&provider, &tools, config(), &NoSleep);
        let mut messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("the task"),
            filler("a0"),
            CompletionMessage::user(crate::skills::invoke::render_invocation_message(
                "deploy",
                "Step 1: build. Step 2: ship.",
            )),
            CompletionMessage::user(crate::skills::invoke::render_invocation_message(
                "audit",
                "An already-finished procedure.",
            )),
            filler("a1"),
            filler("a2"),
            filler("a3"),
        ];

        summarize(&engine, &mut messages, 100_000).await;

        let restored = restoration_message(&messages).expect("a restoration message exists");
        assert!(
            restored.contains("Step 1: build. Step 2: ship."),
            "the active skill's full body is restored verbatim: {restored}"
        );
        assert!(
            !restored.contains("already-finished procedure"),
            "an ended invocation is history, not working set: {restored}"
        );
    }

    /// Witness (e): a span with no active skill restores nothing — no empty
    /// marker message rides the transcript.
    #[tokio::test]
    async fn a_span_with_no_working_set_appends_no_marker_message() {
        let provider = SummaryProvider;
        let tools = SkillTools { active: vec![] };
        let engine = Engine::with_sleeper(&provider, &tools, config(), &NoSleep);
        let mut messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("the task"),
        ];
        messages.extend((0..6).map(|i| filler(&format!("a{i}"))));

        let events = summarize(&engine, &mut messages, 100_000).await;

        assert!(
            events
                .iter()
                .any(|e| matches!(e, stella_protocol::AgentEvent::Compaction { summarized, .. } if *summarized > 0)),
            "the summarization itself ran"
        );
        assert_eq!(
            restoration_message(&messages),
            None,
            "nothing was lost, so nothing is restored"
        );
    }

    /// The witnesses above spell the marker literally so they can run (and
    /// fail) against the base tree; this pins the spelling to the constant
    /// the production path writes.
    #[test]
    fn the_literal_is_the_constant() {
        assert_eq!(
            crate::restore::RESTORE_MARKER_PREFIX,
            "[working set restored"
        );
    }

    /// Starves — empty text, `finish_reason: length` — on its first
    /// `starve_first` calls, then answers; records the cap each dispatch
    /// asked for. The scripted shape
    /// `a_call_starved_by_its_own_reasoning_is_retried_with_room` uses on the
    /// standalone chokepoint (`stella-cli`'s `accounted_call.rs`).
    struct StarvingProvider {
        starve_first: u32,
        attempts: std::sync::atomic::AtomicU32,
        caps: Mutex<Vec<Option<u32>>>,
    }

    impl StarvingProvider {
        fn new(starve_first: u32) -> Self {
            StarvingProvider {
                starve_first,
                attempts: std::sync::atomic::AtomicU32::new(0),
                caps: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl Provider for StarvingProvider {
        fn id(&self) -> &str {
            "starving"
        }
        async fn complete_ref(
            &self,
            req: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            self.caps.lock().unwrap().push(req.max_output_tokens);
            let starved = self
                .attempts
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                < self.starve_first;
            Ok(CompletionResult {
                text: if starved {
                    String::new()
                } else {
                    "RECOVERED SUMMARY".into()
                },
                tool_calls: vec![],
                usage: CompletionUsage::reported_zero(),
                model: "starving".into(),
                cost_usd: 0.0001,
                finish_reason: starved.then_some(stella_protocol::FinishReason::Length),
                upstream_provider: None,
            })
        }
    }

    /// [`summarize`] with the caller owning the health latch, returning the
    /// reported spend — what the #2503 witnesses assert on.
    async fn summarize_owning_health(
        engine: &Engine<'_>,
        messages: &mut Vec<CompletionMessage>,
        health: &mut SummarizerHealth,
    ) -> f64 {
        let mut budget = BudgetGuard::new(BudgetMode::Off, None, None);
        let (tx, rx) = mpsc::unbounded_channel();
        let events = EventSender::new(tx);
        let cost = engine
            .summarize_overflow_span(
                messages,
                &mut budget,
                100_000,
                1.0,
                health,
                0,
                None,
                &events,
            )
            .await;
        drop(rx);
        cost
    }

    /// #2503 witness: the summarizer's cap carries reasoning headroom over
    /// its written contract, a provably starved response is retried once at
    /// `STARVED_RETRY_CAP`, the retry's summary is applied, and the give-up
    /// latch counts nothing — a starved call that recovered made progress.
    ///
    /// Fails on base three ways: the first dispatch carries a bare
    /// `Some(1_200)`, no second dispatch happens, and the empty reply
    /// records a health failure that latches here.
    #[tokio::test]
    async fn a_starved_summarizer_is_retried_with_room_and_never_latches() {
        let provider = StarvingProvider::new(1);
        let tools = SkillTools { active: vec![] };
        let engine = Engine::with_sleeper(&provider, &tools, config(), &NoSleep);
        let mut messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("the task"),
        ];
        messages.extend((0..6).map(|i| filler(&format!("a{i}"))));
        let mut health = SummarizerHealth::default();
        // One failure short of the latch: counting the starved attempt as a
        // failure would disable compaction for the rest of the turn, which is
        // exactly the conversion of a one-call problem into a doomed turn
        // this witness exists to forbid.
        health.record_failure();

        let cost = summarize_owning_health(&engine, &mut messages, &mut health).await;

        assert_eq!(
            *provider.caps.lock().unwrap(),
            vec![
                Some(crate::starvation::with_reasoning_headroom(1_200)),
                Some(crate::starvation::STARVED_RETRY_CAP),
            ],
            "the contract reaches the wire padded with thinking room, and the \
             retry buys real room — re-sending the same cap would only starve \
             again"
        );
        assert!(
            messages
                .iter()
                .any(|m| m.content.contains("RECOVERED SUMMARY")),
            "the retry's summary is applied, not discarded as \"the model had \
             nothing to say\""
        );
        assert_eq!(
            health.consecutive_failures, 0,
            "recovery resets the latch; even counting the starved attempt \
             would have latched it from here"
        );
        assert!(
            (cost - 0.0002).abs() < f64::EPSILON,
            "both attempts are paid calls and the reported spend must say so: {cost}"
        );
    }

    /// The retry is bought exactly once: a summarizer that starves again at
    /// the raised cap records ONE latch failure for the round and leaves the
    /// context intact — more room was tried and did not help, so this is the
    /// give-up latch's business after all.
    #[tokio::test]
    async fn a_retry_that_starves_again_records_one_failure_and_stops() {
        let provider = StarvingProvider::new(u32::MAX);
        let tools = SkillTools { active: vec![] };
        let engine = Engine::with_sleeper(&provider, &tools, config(), &NoSleep);
        let mut messages = vec![
            CompletionMessage::system("sys"),
            CompletionMessage::user("the task"),
        ];
        messages.extend((0..6).map(|i| filler(&format!("a{i}"))));
        let before = messages.clone();
        let mut health = SummarizerHealth::default();

        let cost = summarize_owning_health(&engine, &mut messages, &mut health).await;

        assert_eq!(
            provider.caps.lock().unwrap().len(),
            2,
            "one retry, never a loop — the second emptiness is not worth a \
             third call"
        );
        assert_eq!(
            health.consecutive_failures, 1,
            "the starved-then-retried round is one non-progress result, not two"
        );
        assert_eq!(messages, before, "context left intact");
        assert!(
            (cost - 0.0002).abs() < f64::EPSILON,
            "the superseded attempt's spend is still reported: {cost}"
        );
    }
}
