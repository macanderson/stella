//! Delivering a committed step's result — [`Engine::dispatch_completion`],
//! moved out of `driver.rs` (closed to growth) when the #2684 `Stop` gate
//! joined the completion path. A child module of `driver` so the engine
//! internals stay reachable.

use stella_protocol::{AgentEvent, CompletionMessage, FinishReason, MessageRole, StageKind};

use super::truncation::{self, ContinuationBudget, ContinuationPlan, TIME_EXHAUSTED_PARTIAL};
use super::user_hooks::STOP_HOOK_MARKER_PREFIX;
use super::{
    CommittedStep, Engine, SPECULATION_DISCARD_HARVEST_MISMATCH, TurnOutcome, confident_zero,
    live_services,
};
use crate::event_sender::EventSender;

impl<'a> Engine<'a> {
    /// Deliver a committed step's result: emit its text, then either
    /// finish the turn (no tool calls — `Some(Completed)`) or record the
    /// assistant message, execute its tool calls, record their results,
    /// and return `None` so the loop takes another step. Consumes the
    /// step: the result's text moves into the `Completed` outcome.
    ///
    /// A tool-less step that ended at the output-token limit is the one
    /// no-tool shape that does NOT finish the turn (up to
    /// `driver::truncation::MAX_LENGTH_CONTINUATIONS` times): the model was
    /// cut off, not done, so the step is recorded and the turn continues with
    /// [`LENGTH_CONTINUATION_NUDGE`](truncation::LENGTH_CONTINUATION_NUDGE).
    /// `length_continuations` is the turn's running count
    /// ([`crate::step::TurnState`]'s, threaded rather than owned so the
    /// bound survives across steps).
    ///
    /// A completion that survives every other gate consults the `Stop`
    /// hooks last ([`Engine::stop_hook_feedback`]): a blocking decision
    /// injects the hook's reason as a marked tail user message and returns
    /// `None`, holding the turn open for another round. `stop_consults` is
    /// the bounded consultation counter (`driver::user_hooks` module docs).
    #[allow(clippy::too_many_arguments)] // threaded turn-state fields, same shape as its siblings
    pub(super) async fn dispatch_completion(
        &self,
        committed: CommittedStep,
        total_cost_usd: f64,
        messages: &mut Vec<CompletionMessage>,
        length_continuations: &mut u32,
        stop_consults: &mut u32,
        continuation_budget: Option<ContinuationBudget>,
        events: &EventSender,
    ) -> Option<TurnOutcome> {
        let CommittedStep {
            result,
            read_only_tools,
            speculation,
            ..
        } = committed;

        // Trimmed guard, matching the empty-turn check below: a
        // whitespace-only response must not stream a blank `Text` event and
        // then abort as "no text" — events and history stay consistent.
        if !result.text.trim().is_empty() {
            let _ = events.send(AgentEvent::Text {
                text: result.text.clone(),
            });
        }

        if result.tool_calls.is_empty() {
            // A committed step that runs no tools can still have speculated
            // read-only calls off a divergent stream (announced, then dropped
            // from the commit): none are harvested here, so account for the
            // discarded I/O rather than dropping the pool silently (#370).
            self.discard_speculation_pool(
                speculation,
                SPECULATION_DISCARD_HARVEST_MISMATCH,
                events,
            );
            // A step that ended at the output-token limit with no tool call is
            // not a finished turn — it ran out of room. `plan_continuation`
            // owns what happens next and what history keeps (see
            // `driver::truncation`); `None` means the turn's allowance is
            // spent and the terminal handling below applies.
            let mut out_of_time = false;
            if result.finish_reason == Some(FinishReason::Length) {
                match truncation::plan_continuation(
                    &result.text,
                    result.usage.output_tokens,
                    *length_continuations,
                    continuation_budget,
                ) {
                    ContinuationPlan::Continue(plan) => {
                        *length_continuations += 1;
                        let (note, appended) = plan.into_parts();
                        let _ = events.send(AgentEvent::Text { text: note });
                        messages.extend(appended);
                        return None;
                    }
                    // Declined because the turn is nearly out of wall clock,
                    // not because it ran out of tries. The abort below is the
                    // wrong ending for that: it exits nonzero, which a harness
                    // records as the agent dying — the exact outcome the
                    // deadline exists to avoid. Stopping early is only worth
                    // anything if stopping produces a result, so this path
                    // completes the turn with whatever it has.
                    ContinuationPlan::OutOfTime => out_of_time = true,
                    ContinuationPlan::AllowanceSpent => {}
                }
            }
            // The tool-less-completion legitimacy gate — empty-response
            // abort, #1477 confident zero, #2663 prove-it nudge, in order.
            match confident_zero::check(
                messages,
                &read_only_tools,
                &result,
                out_of_time,
                total_cost_usd,
                self.config.completion_gate,
                events,
            ) {
                confident_zero::CompletionRuling::Abort(outcome) => return Some(outcome),
                confident_zero::CompletionRuling::Nudged => return None,
                confident_zero::CompletionRuling::Clean => {}
            }
            // The end-of-turn service assertion (#2764), after the gates
            // above because a turn that is aborting has nothing to be asked
            // about. The executor answers what is still up — the engine holds
            // no process table and must not (invariant 1) — and the nudge
            // only names it: nothing here stops anything (#2666).
            if live_services::check(messages, &self.tools.live_services(), &result.text, events) {
                return None;
            }
            // A non-empty answer truncated with the continuation allowance
            // spent: keep the partial answer (already emitted above) but tell
            // the user it was cut off, so a mid-thought stop is never
            // mistaken for a full one. The verification ladder's no-op rung
            // is what stops a turn that still did nothing from reporting
            // success past this point.
            if result.finish_reason == Some(FinishReason::Length) {
                let _ = events.send(AgentEvent::Text {
                    text: if out_of_time {
                        // Names the deadline, not the token limit, because the
                        // token limit is what happened and the deadline is why
                        // nothing followed it. A reader who sees only "output
                        // limit" concludes the cap is mispriced and raises it,
                        // which is the one change that cannot help here.
                        format!(
                            "\n\n⚠ Stopped at the output-token limit ({} tokens) with too little \
                             time left in this turn to finish another continuation — ending here \
                             with what the turn has already done.",
                            result.usage.output_tokens
                        )
                    } else {
                        format!(
                            "\n\n⚠ Response was truncated at the output-token limit ({} tokens); \
                             ask to continue if it was cut off mid-thought.",
                            result.usage.output_tokens
                        )
                    },
                });
            }
            // Empty only on the out-of-time path — every other empty-text route
            // aborted above — where the transcript and the turn's final text
            // still owe an honest account of why nothing came back.
            let text = if result.text.trim().is_empty() {
                TIME_EXHAUSTED_PARTIAL.to_string()
            } else {
                result.text
            };
            // History keeps the elided form of a truncated partial; the
            // outcome below keeps it whole (`retained_partial`'s contract).
            let retained = if result.finish_reason == Some(FinishReason::Length) {
                truncation::retained_partial(&text)
            } else {
                text.clone()
            };
            messages.push(CompletionMessage {
                role: MessageRole::Assistant,
                content: retained,
                tool_calls: Vec::new(),
                tool_results: Vec::new(),
                attachments: Vec::new(),
            });
            // The Stop gate (#2684), after the assistant message is in
            // history so the hook's feedback answers a recorded turn: a
            // blocking hook holds the turn open with its reason as the
            // model's next observation instead of finishing.
            if let Some(reason) = self.stop_hook_feedback(&text, stop_consults, events).await {
                messages.push(CompletionMessage::user(format!(
                    "{STOP_HOOK_MARKER_PREFIX} — a workspace Stop hook held this turn open; \
                     address it, then finish]\n\n{reason}"
                )));
                return None;
            }
            let _ = events.send(AgentEvent::Stage {
                name: StageKind::Complete,
            });
            let _ = events.send(AgentEvent::TurnComplete {
                model: result.model.clone(),
                cost_usd: total_cost_usd,
            });
            return Some(TurnOutcome::Completed {
                text,
                cost_usd: total_cost_usd,
            });
        }

        messages.push(CompletionMessage {
            role: MessageRole::Assistant,
            // A length-truncated step that still carried tool calls proceeds
            // normally — but its narration was cut off mid-stream, and
            // retained whole it is the same compounding debris as a tool-less
            // truncation (`retained_partial`). A natural stop keeps its text.
            content: if result.finish_reason == Some(FinishReason::Length) {
                truncation::retained_partial(&result.text)
            } else {
                result.text.clone()
            },
            tool_calls: result.tool_calls.clone(),
            tool_results: Vec::new(),
            attachments: Vec::new(),
        });

        // Dispatch grouping is wider than the read-only set: the executor may
        // declare tools that run concurrently despite not being read-only
        // (`ToolExecutor::parallel_safe_names` — the sub-agent spawn tool,
        // whose children only read and whose dispatcher carves budget per
        // child). The union is built HERE, at the one dispatch site, so the
        // evidence-side consumers of `read_only_tools` above (confident_zero)
        // and the speculation fence keep the narrower read-only semantics.
        let mut dispatch_safe_tools = read_only_tools.clone();
        dispatch_safe_tools.extend(self.tools.parallel_safe_names());
        let tool_results = self
            .execute_tool_calls(
                &result.tool_calls,
                &dispatch_safe_tools,
                &read_only_tools,
                speculation,
                events,
            )
            .await;

        messages.push(CompletionMessage {
            role: MessageRole::Tool,
            content: String::new(),
            tool_calls: Vec::new(),
            tool_results,
            attachments: Vec::new(),
        });

        None
    }
}
