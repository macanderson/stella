//! Speculative execution of speculation-safe read-only tool calls.
//!
//! A step's tool calls normally wait for the entire model response to
//! finish streaming before any of them run. But a call is fully known the
//! moment its own block finishes streaming — often seconds before the
//! response ends — and a *read-only* call (per `ToolSchema::read_only`) can
//! be executed early with zero observable difference to the workspace: it
//! mutates nothing, so running it during the stream instead of after
//! commutes with everything around it, and a result that ends up unused
//! (stream error, retry, input mismatch) is simply discarded work, never a
//! wrong state.
//!
//! "Safe to waste" is a second claim on top of "read-only", and the two
//! diverge (#923): a failed stream attempt re-announces its prefix on
//! retry, so a speculated call can execute twice per step. That is free
//! for a filesystem read but not for a web search that burns a metered
//! API call each run, an MCP tool whose server counts requests, or a
//! graph query that writes catch-up state to its own database on the way
//! to answering — the workspace stays correct, the user's quota does not.
//! A tool states the second claim with `ToolSchema::speculation_safe`;
//! only calls whose tools declare BOTH flags are ever run early. A
//! read-only call without it is *skipped, not fenced* — like a hook-gated
//! call, it is not a mutation, so the reads after it stay eligible.
//!
//! The flow: `Engine::run_model_call` hands the provider a
//! [`SpeculationGate`] (a `stella_protocol::ToolCallObserver`). As the
//! adapter announces finished tool-call blocks, the gate forwards the
//! speculation-safe ones over a channel to the engine's pump, which
//! executes them concurrently with the still-streaming model call and
//! collects their outputs into a [`SpeculationPool`]. Dispatch then
//! *harvests* pool entries instead of re-executing — but only when the
//! committed call is byte-identical (same id, name, and input) to what was
//! announced, so a divergent stream can never smuggle a stale result into
//! the transcript.
//!
//! # Ordering safety
//!
//! Dispatch preserves sequential semantics by running every mutating call
//! as its own barrier, in call order. Speculation must not weaken that: a
//! read-only call that appears AFTER a mutating call in the same step must
//! observe the mutation, so it cannot run early. Calls stream in order, so
//! the gate enforces this with a fence: the first non-read-only call it
//! sees permanently stops speculation for the rest of the step. Only the
//! all-read-only *prefix* of a step's calls is ever speculated — exactly
//! the calls dispatch would have started first anyway.
//!
//! # Hooks and discarded work
//!
//! Speculative execution goes through the same `execute_with_repair` path
//! as dispatch, so the registry's policy-bus gates fire exactly as they
//! would have — just earlier on the wall clock. Settings-declared
//! `PreToolUse`/`PostToolUse` hooks are the exception: a tool any such hook
//! matches is *excluded from speculation* (the gate's `hook_gated` set) and
//! runs only on the committed dispatch path, so its hooks fire exactly once
//! per committed call — never for a speculative attempt that later fails
//! and is dropped, and `PreToolUse` still gates *before* the tool executes
//! (#370). A hook-gated read-only call is skipped, not fenced: it is not a
//! mutation, so the read-only calls after it stay speculation-safe.
//!
//! What speculation cannot avoid is *wasted* read-only work: a stream that
//! fails after announcing, or a committed call that diverges from what was
//! announced, leaves a pool entry whose tool already ran real I/O but whose
//! result never reaches the transcript. That work is no longer silent — the
//! engine emits one [`stella_protocol::AgentEvent::SpeculationDiscarded`]
//! per dropped entry, so an event-log consumer can reconcile call counts
//! with what actually executed. It is the price of overlap, bounded to
//! read-only tools on purpose.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;
use stella_protocol::{AgentEvent, ToolCall, ToolCallObserver, ToolOutput};
use tokio::sync::mpsc::UnboundedSender;

use crate::event_sender::EventSender;
use crate::step::StreamProgress;

/// One speculatively-executed call's outcome, held until dispatch decides
/// whether to harvest it.
pub(crate) struct SpeculativeResult {
    /// Tool name as announced — harvest re-checks it against the committed
    /// call before trusting the output.
    pub name: String,
    /// Parsed input as announced — same re-check.
    pub input: Value,
    pub output: ToolOutput,
    /// Real execution time, which overlapped the model call instead of
    /// following it. Reported on the harvested `ToolResult` event so the
    /// timing stays honest.
    pub duration_ms: u64,
}

/// Completed speculative executions for one committed step, keyed by
/// `call_id`. Dropped wholesale when a stream attempt fails — read-only
/// work is safe to waste for the *workspace*, but the I/O still ran, so the
/// driver accounts for every dropped entry with a `SpeculationDiscarded`
/// event rather than losing it silently (#370).
pub(crate) type SpeculationPool = HashMap<String, SpeculativeResult>;

/// The observer handed to `Provider::complete_observed` — the engine's one
/// stream-side seam. Tool calls: filtered down to the speculation-safe
/// prefix (read-only, well-formed, before any mutating call) and forwarded
/// to the engine's pump. Answer text: each fragment is forwarded to the
/// turn's event channel as a best-effort `TextDelta` preview (the step's
/// eventual `Text` event stays authoritative — see its protocol docs).
pub(crate) struct SpeculationGate {
    /// Tools whose schemas declare `read_only` — the FENCE set. The first
    /// announced call outside it is a mutation, which permanently stops
    /// speculation for the step (ordering safety, module docs). Broader
    /// than `speculation_safe` on purpose: a read-only tool that opted out
    /// of speculation still must not fence the reads behind it.
    read_only_tools: HashSet<String>,
    /// Tools whose schemas declare `speculation_safe` as well — the
    /// ELIGIBILITY set, always a subset of `read_only_tools`. A read-only
    /// call outside it (web search, MCP-provided tool) is skipped, not
    /// fenced: running it twice would bill the user or the remote, but it
    /// mutates no workspace state (#923).
    speculation_safe: HashSet<String>,
    /// Read-only tools that must NOT be speculated because a configured
    /// `PreToolUse`/`PostToolUse` hook matches them: those hooks fire
    /// exactly once, on the committed dispatch path, never for a
    /// speculative attempt that may fail and be dropped (`PreToolUse` also
    /// gates *before* execution there). Not a mutation — so unlike a fenced
    /// call it does not stop speculation of later read-only calls; it is
    /// simply skipped.
    hook_gated: HashSet<String>,
    /// Set on the first non-read-only announcement; never cleared. See the
    /// module docs' ordering-safety section.
    fenced: AtomicBool,
    tx: UnboundedSender<ToolCall>,
    /// The turn's event stream, for live `TextDelta` previews. Deltas from
    /// an attempt that later fails have already been emitted by design —
    /// no reset marker exists; consumers replace the preview when the
    /// authoritative `Text` lands.
    events: EventSender,
    /// The attempt's idle clock ([`crate::step::bounded_generation`]).
    /// Ticked by EVERY observer method — text, reasoning, a whole streamed
    /// call, an argument fragment — because each is a fragment that arrived,
    /// and the deadline's question is only "is anything arriving". Ticked
    /// FIRST in `tool_call_streamed`, before the fence/eligibility
    /// early-returns: a stream of mutating calls (`bash`, `edit` — the
    /// common case) announces work the gate declines to speculate, but the
    /// provider is plainly answering and must never read as stalled.
    progress: StreamProgress,
}

impl SpeculationGate {
    pub(crate) fn new(
        read_only_tools: HashSet<String>,
        speculation_safe: HashSet<String>,
        hook_gated: HashSet<String>,
        tx: UnboundedSender<ToolCall>,
        events: impl Into<EventSender>,
        progress: StreamProgress,
    ) -> Self {
        Self {
            read_only_tools,
            speculation_safe,
            hook_gated,
            fenced: AtomicBool::new(false),
            tx,
            events: events.into(),
            progress,
        }
    }
}

impl ToolCallObserver for SpeculationGate {
    fn text_delta(&self, delta: &str) {
        self.progress.record();
        if delta.is_empty() {
            return;
        }
        // A send after the renderer hung up is fine — previews are lossy by
        // contract.
        let _ = self.events.send(AgentEvent::TextDelta {
            text: delta.to_string(),
        });
    }

    fn reasoning_delta(&self, delta: &str) {
        self.progress.record();
        if delta.is_empty() {
            return;
        }
        // `Reasoning`, never `TextDelta`: the transcript folds this into its
        // own collapsible entry, keeping thinking visible but plainly
        // distinct from the answer.
        let _ = self.events.send(AgentEvent::Reasoning {
            delta: delta.to_string(),
        });
    }

    fn tool_input_delta(&self) {
        // Pure liveness: the fragment's bytes are partial JSON only
        // `tool_call_streamed` may deliver whole. Without this, a generation
        // whose entire output is one large tool call (a `write_file` of a
        // whole document) streams in observer silence and the idle deadline
        // kills a healthy, paying call as "stalled".
        self.progress.record();
    }

    fn tool_call_streamed(&self, call: &ToolCall) {
        self.progress.record();
        if self.fenced.load(Ordering::Relaxed) {
            return;
        }
        if !self.read_only_tools.contains(&call.name) {
            self.fenced.store(true, Ordering::Relaxed);
            return;
        }
        // Read-only but not speculation-safe (a metered web call, an MCP
        // tool, a read that writes internal state): a retry would run it
        // twice, so it only ever executes on the committed dispatch path.
        // Skipped, not fenced — the workspace is untouched, so the reads
        // that follow stay eligible (#923).
        if !self.speculation_safe.contains(&call.name) {
            return;
        }
        // Read-only but hook-gated: never speculated (its hooks must fire
        // once, at dispatch — #370), yet not a mutation, so it does not
        // fence the read-only calls that follow it.
        if self.hook_gated.contains(&call.name) {
            return;
        }
        // Adapters never announce a call whose input failed to parse, but
        // the `Null` repair sentinel is load-bearing enough to re-check:
        // a malformed call belongs to dispatch's repair path, not to
        // execution of any kind.
        if call.input.is_null() {
            return;
        }
        // A send after the pump stopped (receiver dropped) is fine — the
        // announcement is simply lost, and dispatch executes normally.
        let _ = self.tx.send(call.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc::unbounded_channel;

    fn call(name: &str, id: &str) -> ToolCall {
        ToolCall {
            call_id: id.to_string(),
            name: name.to_string(),
            input: serde_json::json!({"path": "src/lib.rs"}),
        }
    }

    fn gate_with(
        names: &[&str],
    ) -> (
        SpeculationGate,
        tokio::sync::mpsc::UnboundedReceiver<ToolCall>,
        tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    ) {
        gate_with_gated(names, &[])
    }

    fn gate_with_gated(
        names: &[&str],
        gated: &[&str],
    ) -> (
        SpeculationGate,
        tokio::sync::mpsc::UnboundedReceiver<ToolCall>,
        tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    ) {
        // Every read-only name doubles as speculation-safe here; tests about
        // the two flags diverging use `gate_full` directly.
        gate_full(names, names, gated)
    }

    fn gate_full(
        read_only: &[&str],
        speculation_safe: &[&str],
        gated: &[&str],
    ) -> (
        SpeculationGate,
        tokio::sync::mpsc::UnboundedReceiver<ToolCall>,
        tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
    ) {
        let (tx, rx) = unbounded_channel();
        let (events_tx, events_rx) = unbounded_channel();
        let read_only: HashSet<String> = read_only.iter().map(|s| s.to_string()).collect();
        let speculation_safe: HashSet<String> =
            speculation_safe.iter().map(|s| s.to_string()).collect();
        let hook_gated: HashSet<String> = gated.iter().map(|s| s.to_string()).collect();
        (
            SpeculationGate::new(
                read_only,
                speculation_safe,
                hook_gated,
                tx,
                events_tx,
                StreamProgress::default(),
            ),
            rx,
            events_rx,
        )
    }

    #[test]
    fn text_deltas_forward_to_the_event_stream_skipping_empty_fragments() {
        let (gate, _rx, mut events_rx) = gate_with(&[]);
        gate.text_delta("Hel");
        gate.text_delta("");
        gate.text_delta("lo");

        let forwarded: Vec<String> = std::iter::from_fn(|| events_rx.try_recv().ok())
            .map(|e| match e {
                AgentEvent::TextDelta { text } => text,
                other => panic!("unexpected event: {other:?}"),
            })
            .collect();
        assert_eq!(forwarded, vec!["Hel".to_string(), "lo".to_string()]);
    }

    /// Thinking rides `Reasoning`, not `TextDelta` — the transcript renders
    /// the two differently (collapsed and dimmed vs. the reply), and merging
    /// them is what once published chain-of-thought as the answer.
    #[test]
    fn reasoning_deltas_forward_as_reasoning_never_as_text() {
        let (gate, _rx, mut events_rx) = gate_with(&[]);
        gate.reasoning_delta("weigh");
        gate.reasoning_delta("");
        gate.reasoning_delta("ing");

        let forwarded: Vec<String> = std::iter::from_fn(|| events_rx.try_recv().ok())
            .map(|e| match e {
                AgentEvent::Reasoning { delta } => delta,
                other => panic!("thinking must never ride another event: {other:?}"),
            })
            .collect();
        assert_eq!(forwarded, vec!["weigh".to_string(), "ing".to_string()]);
    }

    #[test]
    fn forwards_read_only_calls_and_drops_everything_after_a_mutating_one() {
        let (gate, mut rx, _events) = gate_with(&["read_file", "grep"]);
        gate.tool_call_streamed(&call("read_file", "c1"));
        gate.tool_call_streamed(&call("grep", "c2"));
        // The barrier: nothing after this may run early, including reads.
        gate.tool_call_streamed(&call("edit_file", "c3"));
        gate.tool_call_streamed(&call("read_file", "c4"));

        let forwarded: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|c| c.call_id)
            .collect();
        assert_eq!(
            forwarded,
            vec!["c1".to_string(), "c2".to_string()],
            "only the all-read-only prefix is speculation-safe"
        );
    }

    #[test]
    fn null_input_never_reaches_the_pump_but_does_not_fence() {
        let (gate, mut rx, _events) = gate_with(&["read_file"]);
        gate.tool_call_streamed(&ToolCall {
            call_id: "bad".to_string(),
            name: "read_file".to_string(),
            input: Value::Null,
        });
        gate.tool_call_streamed(&call("read_file", "good"));

        let forwarded: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|c| c.call_id)
            .collect();
        assert_eq!(
            forwarded,
            vec!["good".to_string()],
            "a malformed call belongs to the repair path; a read-only call \
             after it is still safe (nothing mutated)"
        );
    }

    #[test]
    fn a_hook_gated_read_is_skipped_but_does_not_fence_later_reads() {
        // `grep` is read-only but matched by a configured hook, so it must
        // never be speculated — yet it is not a mutation, so the read-only
        // `read_file` calls on either side of it stay speculation-safe.
        let (gate, mut rx, _events) = gate_with_gated(&["read_file", "grep"], &["grep"]);
        gate.tool_call_streamed(&call("read_file", "c1"));
        gate.tool_call_streamed(&call("grep", "c2"));
        gate.tool_call_streamed(&call("read_file", "c3"));

        let forwarded: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|c| c.call_id)
            .collect();
        assert_eq!(
            forwarded,
            vec!["c1".to_string(), "c3".to_string()],
            "a hook-gated read is skipped, never fences the reads around it"
        );
    }

    #[test]
    fn a_read_only_but_speculation_unsafe_call_is_skipped_not_fenced() {
        // `web_search` is read-only (mutates no workspace state) but NOT
        // speculation-safe: a retried stream would announce it twice and
        // each run burns a metered API call (#923). It must never reach the
        // pump — and since it is not a mutation, the reads on either side
        // of it must stay eligible.
        let (gate, mut rx, _events) = gate_full(&["read_file", "web_search"], &["read_file"], &[]);
        gate.tool_call_streamed(&call("read_file", "c1"));
        gate.tool_call_streamed(&call("web_search", "c2"));
        gate.tool_call_streamed(&call("read_file", "c3"));

        let forwarded: Vec<String> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|c| c.call_id)
            .collect();
        assert_eq!(
            forwarded,
            vec!["c1".to_string(), "c3".to_string()],
            "a speculation-unsafe read is skipped, never fences the reads around it"
        );
    }

    #[test]
    fn send_after_receiver_dropped_is_silently_lost() {
        let (gate, rx, _events) = gate_with(&["read_file"]);
        drop(rx);
        // Must not panic — the announcement is simply lost and dispatch
        // executes the call normally.
        gate.tool_call_streamed(&call("read_file", "c1"));
    }
}
