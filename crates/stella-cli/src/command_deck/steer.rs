// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where the deck's Esc-with-something-to-say lands, driver-side.
//!
//! Split out of [`super`] (a god file, closed to growth) following the
//! `settle`/`task_tap` pattern. The deck's half of this decision — steer or
//! stop — lives in `stella_tui::deck_ui::steer`, and the reasoning behind the
//! whole change is `stella_tui::envelope::steering`; this file is only what
//! the driver does with the texts once they arrive.

use tokio::sync::mpsc::UnboundedSender;

use crate::config::Config;
use crate::session_persist::DurableQueue;
use crate::subsession::SteeringTap;
use stella_tui::Inbound;

/// The persisted `ui.mid_turn_prompt` policy, or the deck's default (`queue`)
/// when the key is absent, unreadable, or names a slug the deck does not know
/// — chrome, never fatal, like `ui.theme`.
pub(super) fn mid_turn_prompt_policy(cfg: &Config) -> stella_tui::deck_ui::MidTurnPrompt {
    crate::settings::Settings::load(&cfg.workspace_root)
        .ok()
        .and_then(|s| {
            s.ui_mid_turn_prompt()
                .and_then(stella_tui::deck_ui::MidTurnPrompt::parse)
        })
        .unwrap_or_default()
}

/// One leading `>` marker off a text, and nothing else.
///
/// The deck strips the marker before sending (`stella_tui::deck_ui::steer`),
/// so a prompt parked as `> fix the tests` arrives here as `fix the tests`.
/// Both sides of a backlog claim go through this, or the queued copy would
/// never match the text it was delivered as.
fn unmarked(text: &str) -> &str {
    match text.trim_start().strip_prefix('>') {
        Some(rest) => rest.trim_start(),
        None => text,
    }
}

/// Take one delivered prompt out of the driver's backlog.
///
/// The backlog lives in two places: the deck's mirror, which the deck clears
/// as it sends the steer ("leaving the rows up would show prompts as 'waiting'
/// that are already in the model's hands"), and this `DurableQueue`, which is
/// what dispatch actually pops. Handing a held prompt to the turn without
/// claiming it here is how the same words ran twice, with `queue.json`
/// recording the doubled order for the next resume to replay (#4026).
///
/// Matching is on the words, because the two sides share no indices, and
/// oldest-first, so N copies of one text claim N entries rather than the same
/// one N times. **A miss is the ordinary case, not an error**: the composer
/// draft rides in the same message and was never queued.
///
/// A wholesale `clear()` would be wrong in the other direction — the driver's
/// backlog can hold prompts restored from a previous session that the deck's
/// mirror never showed, and destroying those is the same lost-work failure
/// this whole path exists to prevent.
fn claim(queue: &mut DurableQueue, text: &str) {
    let wanted = unmarked(text);
    let found = queue.iter().position(|queued| unmarked(queued) == wanted);
    if let Some(index) = found {
        // `remove` is the write-through path; `items` is never touched directly.
        queue.remove(index);
    }
}

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
        let text = unmarked(&text).to_string();
        // Delivered means delivered: whichever destination it takes below, this
        // prompt must stop being a parked one (#4026).
        claim(queue, &text);
        if settling {
            queue.push_back(text);
        } else {
            tap.push(text);
        }
    }
}

/// A plain stop (the first Esc) at a lead with prompts still parked: deliver
/// the backlog into the running turn and keep it going. `false` when there
/// was nothing parked, so the caller's soft stop still runs.
///
/// The deck already steers when *its* mirror of the queue is non-empty, and
/// sends a stop only when it believes there is nothing to say. Its mirror is
/// not the backlog: prompts restored from a previous session, and prompts a
/// double-Esc hold returned, live in this `DurableQueue` and nowhere the deck
/// can see. A stop that reached here with those parked soft-stopped the turn,
/// truncated its messages to the last boundary, and auto-dispatched the first
/// parked prompt into a model that had just forgotten what it was doing —
/// which from the chair reads as "Esc blew up my turn". Esc with something
/// queued means *send it*; the stop is what an empty queue means.
pub(super) fn stop_steers_backlog(
    tap: &SteeringTap,
    queue: &mut DurableQueue,
    in_tx: &UnboundedSender<Inbound>,
) -> bool {
    let parked: Vec<String> = queue.iter().cloned().collect();
    if parked.is_empty() {
        return false;
    }
    let count = parked.len();
    steer_lead(tap, queue, parked);
    let _ = in_tx.send(super::chrome_note(format!(
        "steering {count} queued prompt{} into the running turn — Esc again to stop",
        if count == 1 { "" } else { "s" }
    )));
    true
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
        // Claim before re-parking, or a held backlog handed back here lands on
        // top of the copy it was handed from and dispatches twice (#4026).
        claim(queue, &text);
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
    // The whole batch is claimed before any of it is re-parked: this is the
    // sharpest form of the double-dispatch, since a double-Esc hold parks the
    // backlog and the Esc-steer then front-inserts the tail straight back on
    // top of it (#4026). Claiming in delivery order keeps "oldest entry first"
    // meaningful for duplicated texts.
    for text in &texts {
        claim(queue, text);
    }
    let first = texts.remove(0);
    // Front-inserted in reverse so the tail keeps its own order ahead of
    // whatever was already parked.
    for rest in texts.into_iter().rev() {
        queue.push_front(rest);
    }
    Some(first)
}
