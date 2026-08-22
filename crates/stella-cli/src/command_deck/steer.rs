// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where the deck's Esc-with-something-to-say lands, driver-side.
//!
//! Split out of [`super`] (a god file, closed to growth) following the
//! `settle`/`task_tap` pattern. The deck's half of this decision — steer or
//! stop — lives in `stella_tui::deck_ui::steer`, and the reasoning behind the
//! whole change is `stella_tui::envelope::steering`; this file is only what
//! the driver does with the texts once they arrive.

use crate::session_persist::DurableQueue;
use crate::subsession::SteeringTap;

/// Deliver a mid-turn steer to the LEAD's running turn.
///
/// The route is **not** re-derived from the text.
/// [`WorkspaceInput::Steer`](stella_tui::WorkspaceInput) already *is* the
/// intent — the deck said "steer" by choosing this message type, and strips the
/// `>` marker off every text on its way out — so asking
/// [`crate::subsession::route_mid_turn`] to classify a markerless text here
/// answered `Sidecar` and sent an Esc-steer at a live turn to a stranger
/// sub-session that could not see the conversation (#4025).
///
/// One question is left, and it is about the turn rather than the words: a turn
/// that is only *settling* is past its last model step, so it has no boundary
/// left to inject at. Its texts continue the thread as the next turn instead of
/// being dropped — the same fallback `route_mid_turn` applies for a settling
/// turn, taken here directly.
///
/// The texts arrive as one message precisely so that decision is taken once
/// for all of them. Drained as N separate inputs, a second Esc landing between
/// two of them would steer half the backlog and cancel the turn holding the
/// rest — the same lost-work failure the change exists to end.
pub(super) fn steer_lead(tap: &SteeringTap, queue: &mut DurableQueue, texts: Vec<String>) {
    // Branch once, on the turn — never per text, or a `mark_settling` landing
    // mid-batch would split one steer across both destinations.
    let settling = tap.is_settling();
    for text in texts {
        // Defensive: the deck already strips the marker, and a stray one is a
        // prefix rather than a word the user meant to send either way.
        let text = match text.trim_start().strip_prefix('>') {
            Some(rest) => rest.trim_start().to_string(),
            None => text,
        };
        if settling {
            queue.push_back(text);
        } else {
            tap.push(text);
        }
    }
}

/// Deliver a steer aimed at a WORKER lane.
///
/// A worker has a steering tap of its own, but the driver holds no handle to
/// it — `SubSessions` tracks stop and pause channels and nothing else — so
/// there is no way to inject at that lane's boundary today (#2899).
///
/// The words are not dropped for it: they go to the backlog, which is where a
/// prompt typed at a busy lane already goes. That is a weaker answer than the
/// lead gets, and it is the honest one — silently discarding a correction
/// would be the failure this whole change exists to fix, in a quieter costume.
pub(super) fn steer_worker(queue: &mut DurableQueue, texts: Vec<String>) {
    for text in texts {
        queue.push_back(text);
    }
}

/// A steer whose turn ended before the idle arm read it: the first text runs
/// NOW and the rest keep their order behind it.
///
/// The channel is FIFO, so this is exactly where a steer aimed at a turn that
/// has just finished lands — and the user pressed Esc precisely because they
/// had something to say, so the words become the next turn rather than a
/// no-op. `None` only for an empty steer, which the deck does not send.
pub(super) fn steer_at_rest(queue: &mut DurableQueue, mut texts: Vec<String>) -> Option<String> {
    if texts.is_empty() {
        return None;
    }
    let first = texts.remove(0);
    // Front-inserted in reverse so the tail keeps its own order ahead of
    // whatever was already parked.
    for rest in texts.into_iter().rev() {
        queue.push_front(rest);
    }
    Some(first)
}
