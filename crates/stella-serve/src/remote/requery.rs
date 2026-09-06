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
//! It stays on the CLI side. A frame handle names a thing in a workspace, and
//! this server never sees one. The host knows what it sent.
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
}

impl RemoteRequery {
    /// One of these per turn. It starts from the recall blocks `messages`
    /// holds, so none of them can go in twice.
    pub(crate) fn new(
        messages: &[stella_protocol::CompletionMessage],
        frames: crate::backlog::FrameSink,
        pending: Pending,
        timeout: Duration,
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
    use super::*;

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
