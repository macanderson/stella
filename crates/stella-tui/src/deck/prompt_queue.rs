//! The prompt queue: never blocks input. Split out of `deck.rs` when #1742's
//! cross-store comment pushed it past its file-size ceiling — the
//! `settlement.rs` pattern: relieve the ratchet by extracting a coherent
//! cluster, never by raising it. Re-exported from [`super`], so consumers
//! keep addressing `deck::PromptQueue`.

use std::collections::VecDeque;

#[derive(Clone, Debug, PartialEq)]
pub struct QueuedPrompt {
    pub text: String,
    pub ts: u64,
}

/// The non-blocking prompt queue. Submitting a prompt always enqueues and
/// returns; the deck never gates typing on a busy agent. Dispatch REMOVES
/// the prompt (`take_next`), so the queue holds only the waiting backlog —
/// no dispatched history is retained, keeping memory proportional to what is
/// actually queued (the same discipline as the capped `RouteLog`/`TraceLog`).
#[derive(Clone, Debug, Default)]
pub struct PromptQueue {
    pub items: VecDeque<QueuedPrompt>,
}

impl PromptQueue {
    pub fn enqueue(&mut self, text: String, ts: u64) {
        self.items.push_back(QueuedPrompt { text, ts });
    }
    /// Insert at the FRONT: a double-Esc requeue (the interrupted prompt
    /// runs before the rest of the backlog) or the first submission after a
    /// hold (the user's new prompt runs before even that).
    pub fn enqueue_front(&mut self, text: String, ts: u64) {
        self.items.push_front(QueuedPrompt { text, ts });
    }
    /// Number of not-yet-dispatched prompts.
    pub fn pending(&self) -> usize {
        self.items.len()
    }
    /// Remove the oldest pending prompt for dispatch, returning its text.
    pub fn take_next(&mut self) -> Option<String> {
        self.items.pop_front().map(|p| p.text)
    }
    /// Remove one queued prompt by position (0 = oldest), returning its text.
    /// The queue is a *list* the user edits — deleting or pulling a prompt
    /// back out for editing must never require dispatching it first.
    pub fn remove(&mut self, index: usize) -> Option<String> {
        self.items.remove(index).map(|p| p.text)
    }
    /// Drop every pending prompt (the deck gates this behind a confirm).
    pub fn clear(&mut self) {
        self.items.clear();
    }
}
