//! The one dispatch site's grouped tool execution —
//! [`Engine::execute_tool_calls`], the scheduler that turns one step's tool
//! calls into barrier-separated concurrent groups. A child module of `driver`
//! so the engine internals stay reachable, split out to keep `driver.rs`
//! under the size gate.

use std::collections::HashSet;

use futures_util::StreamExt;
use stella_protocol::{AgentEvent, ToolCall, ToolResult};

use super::{Engine, MAX_CONCURRENT_TOOL_CALLS, SPECULATION_DISCARD_HARVEST_MISMATCH};
use crate::event_sender::EventSender;
use crate::speculation::SpeculationPool;

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
    pub(super) async fn execute_tool_calls(
        &self,
        calls: &[ToolCall],
        dispatch_safe_tools: &HashSet<String>,
        mut speculation: SpeculationPool,
        events: &EventSender,
    ) -> Vec<ToolResult> {
        let mut indexed: Vec<(usize, ToolResult)> = Vec::with_capacity(calls.len());
        let mut i = 0;
        while i < calls.len() {
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
            }
            drop(in_flight);

            i = group_end;
        }
        // Any pool entry no committed call ever claimed ran real I/O that
        // never reached the transcript — account for it, don't drop it (#370).
        self.discard_speculation_pool(speculation, SPECULATION_DISCARD_HARVEST_MISMATCH, events);
        indexed.sort_by_key(|(index, _)| *index);
        indexed.into_iter().map(|(_, result)| result).collect()
    }
}
