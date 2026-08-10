//! The one dispatch site's grouped tool execution —
//! [`Engine::execute_tool_calls`], the scheduler that turns one step's tool
//! calls into barrier-separated concurrent groups. A child module of `driver`
//! so the engine internals stay reachable, split out to keep `driver.rs`
//! under the size gate.

use std::collections::HashSet;

use futures_util::StreamExt;
use stella_protocol::{AgentEvent, ToolCall, ToolOutput, ToolResult};

use super::{Engine, MAX_CONCURRENT_TOOL_CALLS, SPECULATION_DISCARD_HARVEST_MISMATCH};
use crate::event_sender::EventSender;
use crate::speculation::SpeculationPool;

/// The synthetic answer for a call the mid-dispatch halt refused to finish.
///
/// A fixed string on purpose, twice over: the loop detector keys on
/// `ToolOutput` content, so nothing volatile (a timing, a reason that varies
/// per turn) may ride here; and [`crate::step::close_open_tool_calls`] set the
/// precedent that a synthetic closure is an `Error` output with a steady
/// message, mirrored onto the event stream with no `ToolStart`.
const HALTED_TOOL_RESULT: &str = "not executed — the goal was proven met (turn halt fired) while this call \
     was pending; its work is not needed and any process it started was killed";

impl<'a> Engine<'a> {
    /// Execute one step's tool calls, preserving sequential semantics for
    /// anything that can mutate: consecutive dispatch-safe calls (read-only
    /// per `ToolSchema::read_only`, plus the executor's declared
    /// `parallel_safe_names` — sibling sub-agent spawns) form a group
    /// executed concurrently (capped at [`MAX_CONCURRENT_TOOL_CALLS`]);
    /// every other call is its own barrier, executed alone, in call order.
    /// So `[read, read, edit, read]` runs the two reads in parallel, then
    /// the edit alone, then the final read — an observer of any *mutable*
    /// state cannot distinguish this schedule from fully-sequential
    /// execution, while the common "read five files" step (and the
    /// "research three questions" step) gets real concurrency.
    ///
    /// `ToolStart` fires when a call actually starts; `ToolResult` fires as
    /// each call completes (so results from one parallel group may
    /// interleave — consumers correlate by `call_id`, which the TUI already
    /// does). The returned `Vec<ToolResult>` is always in original call
    /// order, so message history is deterministic regardless of completion
    /// order.
    ///
    /// `speculation` holds this step's speculatively-executed read-only
    /// calls (`crate::speculation`). A call is *harvested* — its recorded
    /// output delivered without re-executing — only when the pool entry
    /// matches the committed call exactly (id, name, AND input); any
    /// mismatch falls through to normal execution and the stale entry is
    /// discarded. Harvested calls emit `ToolStart` immediately followed by
    /// `ToolResult { speculated: true }` carrying the real (overlapped)
    /// execution duration.
    ///
    /// # A hard cancel here leaves `messages` mid-pair
    ///
    /// [`Self::dispatch_completion`] appends the assistant `tool_use` message
    /// BEFORE awaiting this, and the answering `Tool` message only after. A
    /// caller-side hard cancel (dropping the turn future) in that window
    /// therefore leaves an unpaired `tool_use` in the borrowed history — the
    /// same broken shape [`Self::handle_committed_result`] explicitly repairs
    /// on the budget-abort path, and the one the next provider call rejects
    /// outright. It is deliberately not repaired here: the contract is that a
    /// hard cancel truncates the whole turn out of history caller-side (see
    /// [`crate::ports::TurnSteering`], which contrasts exactly this against the
    /// soft stop). A caller that KEEPS a hard-cancelled turn's messages must
    /// close the pairing itself.
    ///
    /// # The mid-dispatch halt (kill the worker on oracle flip, #2661)
    ///
    /// [`crate::EngineConfig::turn_halt`] is consulted here too — after each
    /// settled call and before each barrier group — not only at the step
    /// boundary in `drive`. When the predicate fires (the pipeline's flip
    /// oracle saw the tracked test go fail→pass), every call still pending is
    /// spend past the proof: the in-flight stream is dropped, which drops the
    /// tool futures, and a dropped tool future takes its child with it —
    /// `kill_on_drop(true)` plus the setsid'd process-group SIGKILL guard in
    /// `stella-tools` (`exec.rs`). Unfinished and never-started calls are then
    /// answered with [`HALTED_TOOL_RESULT`], so the returned set still covers
    /// every call and the transcript stays well-paired — which is why this
    /// does not breach the "aborts at safe boundaries" discipline: that rule
    /// exists so an interrupt never loses work or corrupts the history, and a
    /// confirmed flip means the remaining work is, by proof, not work. The
    /// halted turn then settles as `Completed` at the very next boundary check
    /// in `drive` without paying for another model call.
    pub(super) async fn execute_tool_calls(
        &self,
        calls: &[ToolCall],
        dispatch_safe_tools: &HashSet<String>,
        mut speculation: SpeculationPool,
        events: &EventSender,
    ) -> Vec<ToolResult> {
        let mut indexed: Vec<(usize, ToolResult)> = Vec::with_capacity(calls.len());
        let mut halted = false;
        let mut i = 0;
        while i < calls.len() {
            // Between barrier groups: a predicate that latched during the
            // previous group makes everything not yet started dead spend.
            if self.mid_dispatch_halt() {
                halted = true;
                break;
            }
            let group_end = if dispatch_safe_tools.contains(&calls[i].name) {
                let mut end = i + 1;
                while end < calls.len() && dispatch_safe_tools.contains(&calls[end].name) {
                    end += 1;
                }
                end
            } else {
                i + 1
            };

            // Plain copy for the closures: borrowing the loop variable
            // itself would conflict with advancing it below (E0506).
            let group_start = i;
            let speculation = &mut speculation;
            let group_futures =
                calls[group_start..group_end]
                    .iter()
                    .enumerate()
                    .map(|(offset, call)| {
                        let _ = events.send(AgentEvent::ToolStart { call: call.clone() });
                        let index = group_start + offset;
                        let harvested = match speculation.remove(&call.call_id) {
                            Some(s) if s.name == call.name && s.input == call.input => Some(s),
                            Some(stale) => {
                                // The committed call diverged from what was
                                // announced: reject the pooled result and
                                // re-execute below. The speculative execution
                                // still ran real I/O — record it (#370).
                                let _ = events.send(AgentEvent::SpeculationDiscarded {
                                    call_id: call.call_id.clone(),
                                    name: stale.name,
                                    reason: SPECULATION_DISCARD_HARVEST_MISMATCH.to_string(),
                                });
                                None
                            }
                            None => None,
                        };
                        async move {
                            match harvested {
                                Some(s) => (index, call, s.output, s.duration_ms, true),
                                None => {
                                    let started = std::time::Instant::now();
                                    let output = self.execute_with_repair(call, Some(events)).await;
                                    let duration_ms = started.elapsed().as_millis() as u64;
                                    (index, call, output, duration_ms, false)
                                }
                            }
                        }
                    });
            let mut in_flight = futures_util::stream::iter(group_futures)
                .buffer_unordered(MAX_CONCURRENT_TOOL_CALLS);
            while let Some((index, call, output, duration_ms, speculated)) = in_flight.next().await
            {
                let _ = events.send(AgentEvent::ToolResult {
                    call_id: call.call_id.clone(),
                    output: output.clone(),
                    duration_ms,
                    speculated,
                });
                indexed.push((
                    index,
                    ToolResult {
                        call_id: call.call_id.clone(),
                        output,
                    },
                ));
                // The settled call may be the one whose result latched the
                // halt (the pipeline's observer runs on the ToolResult event
                // just sent). Ask now rather than draining the group: every
                // sibling still in flight is work past the proof.
                if self.mid_dispatch_halt() {
                    halted = true;
                    break;
                }
            }
            // On the halted arm this drop IS the kill: each still-pending
            // future is dropped, and a dropped tool future takes its child
            // process group with it (`kill_on_drop` + the SIGKILL group guard
            // in stella-tools). On the normal arm the stream is already empty.
            drop(in_flight);

            if halted {
                break;
            }
            i = group_end;
        }
        if halted {
            // Answer everything the halt refused to run or finish, following
            // `close_open_tool_calls`' shape exactly: an `Error` output with a
            // steady message, mirrored onto the event stream with no
            // `ToolStart`, so the returned set covers every call and a
            // transcript reconstructed from events resolves the same way.
            // Keyed by original index, not call_id: ids are only guaranteed
            // unique within one response by SOME providers, and an id-keyed
            // set would let one answered duplicate silently absorb the other.
            let answered: HashSet<usize> = indexed.iter().map(|(index, _)| *index).collect();
            for (index, call) in calls.iter().enumerate() {
                if answered.contains(&index) {
                    continue;
                }
                let output = ToolOutput::Error {
                    message: HALTED_TOOL_RESULT.to_string(),
                };
                let _ = events.send(AgentEvent::ToolResult {
                    call_id: call.call_id.clone(),
                    output: output.clone(),
                    duration_ms: 0,
                    speculated: false,
                });
                indexed.push((
                    index,
                    ToolResult {
                        call_id: call.call_id.clone(),
                        output,
                    },
                ));
            }
        }
        // Any pool entry no committed call ever claimed ran real I/O that
        // never reached the transcript — account for it, don't drop it (#370).
        self.discard_speculation_pool(speculation, SPECULATION_DISCARD_HARVEST_MISMATCH, events);
        indexed.sort_by_key(|(index, _)| *index);
        indexed.into_iter().map(|(_, result)| result).collect()
    }

    /// Whether [`crate::EngineConfig::turn_halt`] has fired, asked from inside
    /// the dispatch loop. One consult point shared by the between-groups and
    /// after-each-settle checks so the two cannot drift.
    fn mid_dispatch_halt(&self) -> bool {
        self.config
            .turn_halt
            .as_ref()
            .and_then(|halt| halt.halt_reason())
            .is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;
    use serde_json::Value;
    use tokio::sync::mpsc;

    use super::super::TurnHalt;
    use crate::event_sender::EventSender;
    use crate::retry::Sleeper;
    use crate::step::{BudgetSnapshot, CHECKPOINT_VERSION, Checkpoint, TurnState};
    use crate::{Engine, EngineConfig, TurnOutcome};
    use stella_protocol::{
        AgentEvent, BudgetMode, CompletionMessage, CompletionRequestRef, CompletionResult,
        CompletionUsage, Provider, ProviderError, ToolCall, ToolOutput, ToolSchema,
    };

    /// First call: two dispatch-safe (read-only) tools, so they run as one
    /// concurrent group. Second call: text — reached only if no halt fires.
    struct TwoParallelToolsThenText {
        calls: Arc<AtomicU32>,
    }

    #[async_trait]
    impl Provider for TwoParallelToolsThenText {
        fn id(&self) -> &str {
            "scripted"
        }
        async fn complete_ref(
            &self,
            _req: CompletionRequestRef<'_>,
        ) -> Result<CompletionResult, ProviderError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let mut result = CompletionResult {
                text: String::new(),
                tool_calls: vec![],
                usage: CompletionUsage {
                    reported: true,
                    input_tokens: 1,
                    ..CompletionUsage::default()
                },
                model: "scripted".into(),
                cost_usd: 0.0,
                finish_reason: None,
            };
            if call == 0 {
                result.tool_calls = vec![
                    ToolCall {
                        call_id: "call_witness".into(),
                        name: "witness".into(),
                        input: serde_json::json!({}),
                    },
                    ToolCall {
                        call_id: "call_eternal".into(),
                        name: "eternal".into(),
                        input: serde_json::json!({}),
                    },
                ];
            } else {
                result.text = "finished after the halt should have fired".into();
            }
            Ok(result)
        }
    }

    /// `witness` completes instantly and raises the flag the halt reads;
    /// `eternal` (when `hang`) never resolves — the long `pytest` a flip
    /// used to have to wait out.
    struct FlagAndHang {
        flag: Arc<AtomicBool>,
        hang: bool,
    }

    #[async_trait]
    impl crate::ToolExecutor for FlagAndHang {
        fn schemas(&self) -> Vec<ToolSchema> {
            ["witness", "eternal"]
                .into_iter()
                .map(|name| ToolSchema {
                    name: name.into(),
                    description: name.into(),
                    input_schema: serde_json::json!({"type": "object"}),
                    read_only: true,
                    speculation_safe: false,
                })
                .collect()
        }
        async fn execute(&self, name: &str, _input: &Value) -> ToolOutput {
            match name {
                "witness" => {
                    self.flag.store(true, Ordering::SeqCst);
                    ToolOutput::Ok {
                        content: "1 passed".into(),
                    }
                }
                _ => {
                    if self.hang {
                        std::future::pending::<()>().await;
                    }
                    ToolOutput::Ok {
                        content: "eternal finished".into(),
                    }
                }
            }
        }
    }

    #[derive(Debug)]
    struct NoopSleeper;
    #[async_trait]
    impl Sleeper for NoopSleeper {
        async fn sleep(&self, _duration_ms: u64) {}
    }

    /// Fires once the executor's flag is up — the shape of the pipeline's
    /// `FlipHalt`, which latches synchronously inside the `ToolResult` tap.
    #[derive(Debug)]
    struct AfterFlag {
        flag: Arc<AtomicBool>,
    }
    impl TurnHalt for AfterFlag {
        fn halt_reason(&self) -> Option<String> {
            self.flag
                .load(Ordering::SeqCst)
                .then(|| "the tracked test flipped".to_string())
        }
    }

    fn step_one() -> Checkpoint {
        Checkpoint {
            version: CHECKPOINT_VERSION,
            step: 1,
            messages: vec![
                CompletionMessage::system("sys"),
                CompletionMessage::user("keep going"),
            ],
            budget: BudgetSnapshot {
                mode: BudgetMode::Off,
                turn_limit_usd: None,
                session_limit_usd: None,
                turn_spent_usd: 0.0,
                session_spent_usd: 0.0,
            },
            total_cost_usd: 0.0,
            calibration_model: None,
            loop_steered: false,
            loop_steered_pattern: Vec::new(),
            loop_steered_inputs: None,
            transcript_rewrites: 0,
        }
    }

    /// **The kill-on-flip witness (#2661).** A flip latched by one tool's
    /// result must not wait for its never-finishing sibling: the sibling is
    /// killed (its future dropped), its call answered synthetically, and the
    /// turn settles `Completed` after exactly one model call. On the old
    /// dispatch this test times out — the group is drained to the end, and
    /// `eternal` has no end.
    #[tokio::test]
    async fn a_mid_dispatch_flip_kills_the_hung_sibling_and_pairs_its_call() {
        let flag = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicU32::new(0));
        let provider = TwoParallelToolsThenText {
            calls: calls.clone(),
        };
        let tools = FlagAndHang {
            flag: flag.clone(),
            hang: true,
        };
        let config = EngineConfig {
            turn_halt: Some(Arc::new(AfterFlag { flag })),
            ..EngineConfig::default()
        };
        let mut state = TurnState::from_checkpoint(step_one(), &config);
        let engine = Engine::with_sleeper(&provider, &tools, config, &NoopSleeper);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let events = EventSender::new(tx);

        let outcome =
            tokio::time::timeout(Duration::from_secs(10), engine.drive(&mut state, &events))
                .await
                .expect("a fired flip must not wait for the hung sibling tool");

        let TurnOutcome::Completed { .. } = outcome else {
            panic!("a fired halt ends the turn as a success: {outcome:?}");
        };
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the halted turn must not pay for another model call"
        );

        drop(events);
        let mut eternal_result = None;
        while let Some(event) = rx.recv().await {
            if let AgentEvent::ToolResult {
                call_id, output, ..
            } = event
                && call_id == "call_eternal"
            {
                eternal_result = Some(output);
            }
        }
        match eternal_result {
            Some(ToolOutput::Error { message }) => assert!(
                message.contains("turn halt fired"),
                "the killed sibling's synthetic answer names the halt: {message}"
            ),
            other => panic!(
                "the killed sibling's call must still be answered on the \
                 event stream (close_open_tool_calls' contract): {other:?}"
            ),
        }
    }

    /// The control: same two tools, nothing hangs, no halt configured — both
    /// calls run to their real results and the turn ends on the second model
    /// call, exactly as before the mid-dispatch consult existed.
    #[tokio::test]
    async fn an_unfired_halt_leaves_dispatch_untouched() {
        let flag = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicU32::new(0));
        let provider = TwoParallelToolsThenText {
            calls: calls.clone(),
        };
        let tools = FlagAndHang { flag, hang: false };
        let config = EngineConfig::default();
        let mut state = TurnState::from_checkpoint(step_one(), &config);
        let engine = Engine::with_sleeper(&provider, &tools, config, &NoopSleeper);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let events = EventSender::new(tx);

        let outcome = engine.drive(&mut state, &events).await;

        let TurnOutcome::Completed { text, .. } = outcome else {
            panic!("the control turn completes: {outcome:?}");
        };
        assert_eq!(text, "finished after the halt should have fired");
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        drop(events);
        let mut eternal_output = None;
        while let Some(event) = rx.recv().await {
            if let AgentEvent::ToolResult {
                call_id, output, ..
            } = event
                && call_id == "call_eternal"
            {
                eternal_output = Some(output);
            }
        }
        assert_eq!(
            eternal_output,
            Some(ToolOutput::Ok {
                content: "eternal finished".into()
            }),
            "with no halt, the sibling's real result is untouched"
        );
    }
}
