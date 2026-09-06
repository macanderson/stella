// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The step-boundary context re-query, remoted.
//!
//! A re-query fans out over recall, and that costs the host money. This server
//! is also allowed no rights of its own: it opens no store and reads no
//! `.stella/`. So the ask travels the way a model call and a tool call do. A
//! [`ServerFrame::RequeryRequest`] goes out, a
//! `POST /v1/turns/{id}/requery-result` comes back, and the engine step waits
//! in between.
//!
//! # What stays on this side of the wire
//!
//! The port owns two jobs, and both are here. Both exist to stop the host
//! being asked for nothing.
//!
//! - **When to ask.** A changed signal buys an ask; a counter never does. The
//!   fingerprint is the paths the turn has touched and the errors it has seen.
//!   Tool names are left out, since they change every step whether the work
//!   moved or not. [`MIN_STEPS_BETWEEN`] then spaces the asks out, so two
//!   changes one step apart cannot bill twice. The CLI's
//!   `stella_cli::memory::steering::SessionRequery` does the same thing on the
//!   other surface.
//! - **What not to send twice.** The engine puts this block in front of the
//!   model as it is, so a block that matches one in the turn already has to
//!   die here. The set starts from the recall blocks the turn opened with.
//!
//! There is a finer dedup than that: "the turn already showed that frame".
//! It stays on the CLI side. A fresh [`RequerySignal`] names only the drift,
//! so **the host owns per-handle suppression, keyed by the turn id.** An
//! answered ask also reports its cost, mirroring `SessionRequery::requery`
//! (`stella-cli/src/memory/steering.rs`): the server never sees the host's
//! own frames, only the text that came back, so it labels one frame as a
//! host answer on the turn's event stream.
//!
//! # An ask nobody answers is not a failed turn
//!
//! A quiet host fails the turn on the provider port, and hands the model an
//! error on the tool port. Here it answers `None`, and the step runs with what
//! it had. More context is a bonus, so losing it costs the turn nothing.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use stella_core::steering::TurnSignal;
use stella_protocol::{AgentEvent, ContextFrameRef, ProviderShare};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::frame::{RequerySignal, ServerFrame};
use crate::observe::event::ReverseKind;
use crate::pending::Pending;

use super::{answered, dispatched, timed_out};

/// How many steps a fresh answer waits after the last one. This spaces the
/// asks out; the fingerprint is what starts one. The CLI's `SessionRequery`
/// uses the same number, since a served turn drifts at the rate a local one
/// does.
const MIN_STEPS_BETWEEN: u32 = 2;

/// Hands out [`RemoteRequery`] tags, on the terms the provider and tool ports
/// use. A request id only has to be one of a kind inside one turn's
/// [`Pending`] map. A counter shared by the whole process does that, and no
/// call site has to be handed one.
static REQUERY_INSTANCES: AtomicU64 = AtomicU64::new(0);

/// What this turn has shown the model so far, and the last signal it
/// answered.
struct RequeryState {
    /// Blocks already in history, by exact bytes.
    produced: HashSet<String>,
    /// The last signal the plane answered. It starts empty, so a turn that
    /// never drifts never asks.
    answered_fingerprint: u64,
}

/// `stella_core::ports::SteeringRequery`, asked over the wire.
pub(crate) struct RemoteRequery {
    instance: u64,
    frames: crate::backlog::FrameSink,
    pending: Pending,
    counter: AtomicU64,
    timeout: Duration,
    state: Mutex<RequeryState>,
    /// Where this turn's telemetry goes — the same channel `run_session`
    /// folds every other `AgentEvent` through. Answering a park is spend
    /// either way, so this is never optional the way a test double's unwired
    /// channel would suggest: every real turn has one.
    events: UnboundedSender<AgentEvent>,
}

impl RemoteRequery {
    /// One of these per turn. It starts from the recall blocks `messages`
    /// holds, so none of them can go in twice.
    pub(crate) fn new(
        messages: &[stella_protocol::CompletionMessage],
        frames: crate::backlog::FrameSink,
        pending: Pending,
        timeout: Duration,
        events: UnboundedSender<AgentEvent>,
    ) -> Self {
        let produced = messages
            .iter()
            .filter(|m| {
                m.role == stella_protocol::MessageRole::User
                    && m.content.starts_with(stella_core::receipts::RECALL_MARKER)
            })
            .map(|m| m.content.clone())
            .collect();
        Self {
            instance: REQUERY_INSTANCES.fetch_add(1, Ordering::Relaxed),
            frames,
            pending,
            counter: AtomicU64::new(0),
            timeout,
            state: Mutex::new(RequeryState {
                produced,
                answered_fingerprint: fingerprint(&[], &[]),
            }),
            events,
        }
    }

    /// Ask the host, and wait for its answer up to this turn's deadline.
    /// Every way the ask can fail gives `None`. The module header says why
    /// that is safe here and nowhere else.
    async fn ask(&self, signal: &TurnSignal<'_>) -> Option<String> {
        let request_id = format!(
            "requery-{}-{}",
            self.instance,
            self.counter.fetch_add(1, Ordering::Relaxed)
        );
        let (tx, rx) = oneshot::channel();
        // Add the entry before the frame goes out, so it is there by the time
        // the host can answer. A refused add means the turn was cancelled.
        if !self.pending.register_requery(request_id.clone(), tx) {
            return None;
        }
        if self
            .frames
            .send(ServerFrame::RequeryRequest {
                request_id: request_id.clone(),
                signal: RequerySignal::from_signal(signal),
            })
            .is_err()
        {
            self.pending.abandon(&request_id);
            return None;
        }
        let started = dispatched(&self.pending, &request_id, ReverseKind::Requery);
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(context)) => {
                answered(&self.pending, &request_id, ReverseKind::Requery, started);
                context
            }
            // The sender went away: a cancel, or the turn shutting down.
            // `Pending` reports the dropped work itself.
            Ok(Err(_)) => None,
            Err(_) => {
                self.pending.abandon(&request_id);
                timed_out(&self.pending, &request_id, ReverseKind::Requery, started);
                None
            }
        }
    }
}

/// A digest of the drift marks that ignores their order. `BTreeSet` is what
/// makes two signals with the same facts agree.
fn fingerprint(paths: &[String], errors: &[&str]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    let paths: std::collections::BTreeSet<&str> = paths.iter().map(String::as_str).collect();
    let errors: std::collections::BTreeSet<&str> = errors.iter().copied().collect();
    (paths, errors).hash(&mut hasher);
    hasher.finish()
}

/// The telemetry for an answered ask.
///
/// The server witnesses one thing about a re-query: the block of text that
/// came back, and what putting it in front of the model will cost. It never
/// sees the host's own recall — which providers ran, which frames won fusion
/// — so this reports one synthetic frame labelled `"served-requery"` rather
/// than inventing provenance the host never sent. Mirrors
/// `stella_protocol::Recall::telemetry_event`'s shape without its per-frame
/// detail: the CLI's port runs the recall itself, this one only relays the
/// host's answer to it.
fn recall_event(block: &str) -> AgentEvent {
    let tokens = stella_protocol::estimate_token_cost(block);
    AgentEvent::ContextRecall {
        tokens,
        frames: vec![ContextFrameRef {
            id: None,
            citation_label: "host re-query answer".to_string(),
            provider: "host".to_string(),
            source: "served-requery".to_string(),
            kind: "host".to_string(),
            uri: None,
            method: None,
            token_cost: tokens,
            block_id: None,
            content_digest: None,
        }],
        provider_mix: vec![ProviderShare {
            provider: "host".to_string(),
            frames: 1,
        }],
        usage: None,
        // The host's own fan-out latency never crosses the wire — `0` is
        // this event's ordinary "not measured", the same reading a port
        // with no CGP host behind it reports.
        latency_ms: 0,
        used_ann_index: None,
    }
}

/// Give the host's block the mark the engine's turn-window rule reads.
///
/// The block goes in as a user message, word for word.
/// `driver::loop_evidence::turn_start_index` tells added context from a real
/// user turn by this prefix alone. A host that left it off would move the turn
/// window, so the server adds it.
fn marked(block: String) -> String {
    if block.starts_with(stella_core::receipts::RECALL_MARKER) {
        return block;
    }
    format!("{}\n{block}", stella_core::receipts::RECALL_MARKER)
}

#[async_trait::async_trait]
impl stella_core::ports::SteeringRequery for RemoteRequery {
    async fn requery(&self, signal: &TurnSignal<'_>) -> Option<String> {
        if signal.since_last_query < MIN_STEPS_BETWEEN {
            return None;
        }
        // Never park a cancelled turn. `Pending::cancel` has woken everyone
        // already, so a park now would wait out the whole deadline with
        // nobody left to answer it.
        if self.pending.is_cancelled() {
            return None;
        }
        let current = fingerprint(signal.touched_paths, signal.errors_seen);
        // The guard is a `std::sync::Mutex`, so it is never held over the
        // await below. That is what keeps this future `Send`.
        if self
            .state
            .lock()
            .expect("requery state")
            .answered_fingerprint
            == current
        {
            return None;
        }
        let context = self.ask(signal).await;
        // The spend happened in `ask` above, so it is reported here — before
        // the dedup below can discard the text while the host's fan-out is
        // already paid for. Mirrors `SessionRequery::requery`
        // (`stella-cli/src/memory/steering.rs`), which reports for the same
        // reason before its own dedup runs.
        if let Some(block) = &context {
            let _ = self.events.send(recall_event(block));
        }
        let mut state = self.state.lock().expect("requery state");
        // The signal counts as answered either way. A drift the host had
        // nothing for must not be asked again on every later step.
        state.answered_fingerprint = current;
        let block = marked(context?);
        state.produced.insert(block.clone()).then_some(block)
    }
}

#[cfg(test)]
mod tests {
    use stella_core::ports::SteeringRequery;

    use super::*;

    /// A signal that clears the fingerprint gate and the step-spacing floor —
    /// enough to buy an ask on a fresh [`RemoteRequery`]. `touched_paths` is
    /// non-empty so the fingerprint differs from the constructor's `([], [])`
    /// starting point; an empty signal would never clear the gate.
    fn asking_signal(touched: &[String]) -> TurnSignal<'_> {
        TurnSignal {
            prompt: "fix the failing test",
            recent_tool_calls: &[],
            touched_paths: touched,
            active_domains: &[],
            step: 5,
            since_last_query: MIN_STEPS_BETWEEN,
            errors_seen: &[],
        }
    }

    /// A fresh port, a channel to watch the frames it sends, and a channel to
    /// watch the telemetry it reports.
    fn wired_port() -> (
        RemoteRequery,
        tokio::sync::mpsc::UnboundedReceiver<ServerFrame>,
        tokio::sync::mpsc::UnboundedReceiver<AgentEvent>,
        Pending,
    ) {
        let (frame_tx, frame_rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = crate::backlog::FrameSink::new(
            frame_tx,
            crate::backlog::FrameBacklog::new(crate::backlog::DEFAULT_MAX_QUEUED_FRAMES),
        );
        let pending = Pending::new(
            crate::observe::null_observer(),
            crate::observe::event::TurnRef::new("turn-requery-events"),
        );
        let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();
        let port = RemoteRequery::new(
            &[],
            sink,
            pending.clone(),
            Duration::from_millis(200),
            events_tx,
        );
        (port, frame_rx, events_rx, pending)
    }

    /// **Witness.** This test fails to compile without the `events` field on
    /// `RemoteRequery` and its extra constructor argument. With them, it
    /// checks that an answered ask is reported, matching
    /// `SessionRequery::requery`'s contract: the host's fan-out already
    /// spent money, so the turn must show it.
    #[tokio::test]
    async fn an_answered_requery_reports_its_recall_cost() {
        let (port, mut frame_rx, mut events_rx, pending) = wired_port();

        // The host side: see the request, answer it. Off the requery
        // future's own task, the way a real HTTP handler would run.
        let host = tokio::spawn(async move {
            let ServerFrame::RequeryRequest { request_id, .. } =
                frame_rx.recv().await.expect("a request frame")
            else {
                panic!("expected a RequeryRequest frame");
            };
            pending
                .resolve_requery(&request_id, Some("see src/a.rs".to_string()))
                .expect("resolve");
        });

        let touched = vec!["src/a.rs".to_string()];
        let block = port.requery(&asking_signal(&touched)).await;
        host.await.expect("host task");

        assert!(block.is_some(), "an answered ask returns a block");
        match events_rx.try_recv().expect("a recall event was sent") {
            AgentEvent::ContextRecall { frames, tokens, .. } => {
                assert_eq!(frames.len(), 1, "one synthetic host frame: {frames:?}");
                assert!(tokens > 0, "the block costs something: {tokens}");
            }
            other => panic!("expected ContextRecall, got {other:?}"),
        }
    }

    /// **Witness, the declined half.** A quiet host is the ordinary answer.
    /// It must cost the event stream nothing. Else a host with nothing new
    /// would still show as recalling on every step.
    #[tokio::test]
    async fn a_declined_requery_reports_nothing() {
        let (port, mut frame_rx, mut events_rx, pending) = wired_port();

        let host = tokio::spawn(async move {
            let ServerFrame::RequeryRequest { request_id, .. } =
                frame_rx.recv().await.expect("a request frame")
            else {
                panic!("expected a RequeryRequest frame");
            };
            pending.resolve_requery(&request_id, None).expect("resolve");
        });

        let touched = vec!["src/a.rs".to_string()];
        let block = port.requery(&asking_signal(&touched)).await;
        host.await.expect("host task");

        assert!(block.is_none(), "a declined ask returns nothing");
        assert!(
            events_rx.try_recv().is_err(),
            "a declined ask reports no recall cost"
        );
    }

    /// **Witness.** The wire carries no per-request answer history. So the
    /// host must own per-handle suppression, and that must be written down,
    /// not just true. This checks the module header and the spec row for
    /// the same phrase. The phrase is built here from two halves, so the
    /// check cannot pass by matching its own source code.
    #[test]
    fn the_module_header_and_the_spec_both_say_who_suppresses_a_seen_handle() {
        // Built from two halves so this check cannot pass by matching its
        // own source: the contiguous phrase exists only in the module
        // header above and in the spec row, never in this line.
        let phrase = format!(
            "{}{}",
            "the host owns per-handle", " suppression, keyed by the turn id"
        );
        let this_module = include_str!("requery.rs");
        assert!(
            this_module.contains(phrase.as_str()),
            "the module header must name who owns cross-ask handle suppression"
        );
        let spec = include_str!("../../../../docs/spec/serve-surface.md");
        assert!(
            spec.contains(phrase.as_str()),
            "docs/spec/serve-surface.md's requery-result row must state the \
             same contract"
        );
    }

    /// A changed signal is what starts an ask. Two boundaries that saw the
    /// same paths and errors in a different order agree, and buy no second
    /// ask.
    #[test]
    fn the_fingerprint_ignores_the_order_facts_arrived_in() {
        let a = vec!["src/b.rs".to_string(), "src/a.rs".to_string()];
        let b = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        assert_eq!(fingerprint(&a, &["timeout"]), fingerprint(&b, &["timeout"]));
        assert_ne!(fingerprint(&a, &["timeout"]), fingerprint(&a, &[]));
        assert_ne!(fingerprint(&a, &[]), fingerprint(&[], &[]));
    }

    /// A host that sends plain text gets the mark added. One that wrote the
    /// mark keeps its own bytes, so no block is marked twice.
    #[test]
    fn a_block_without_the_recall_marker_is_given_one() {
        let marker = stella_core::receipts::RECALL_MARKER;
        let added = marked("see src/adapter.rs".to_string());
        assert!(added.starts_with(marker), "{added}");
        assert!(added.ends_with("see src/adapter.rs"), "{added}");

        let already = format!("{marker}\nsee src/adapter.rs");
        assert_eq!(marked(already.clone()), already);
    }
}
