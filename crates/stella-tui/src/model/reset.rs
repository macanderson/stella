// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What `/clear` keeps and what it throws away.
//!
//! A sibling of `model.rs` rather than lines in it: that file sits against the
//! 1500-line ceiling AGENTS.md holds it to, and the rule there is that new
//! logic lands beside a god file, never inside one.

use super::SessionModel;

impl SessionModel {
    /// Rewind the **conversation** to seq-0 while keeping the record of what
    /// this session did to the tree — what `/clear` means
    /// (`Inbound::SessionReset`; `command_deck::session_clear`).
    ///
    /// Everything conversational resets: transcript, HUD, plan, pending
    /// gates, streaming preview. The file-touch half
    /// ([`Self::files`], [`Self::files_evicted`], `file_touch_seq`) survives,
    /// because those bytes are still on the user's disk after a clear and
    /// `/clear` changes no identity — the session, its store row, its sidecar
    /// dir and its worker lanes all continue.
    ///
    /// # Why this is not `*self = Self::new()`
    ///
    /// It was, and that made the Files tab lie. The tab's ROWS come from
    /// `deck::WorkspaceModel::ledger`, which `/clear` leaves alone because the
    /// bytes are still on disk; its diff TEXT is looked up here (L-T5 — no second data
    /// path for diffs). Wholesale replacement cut one of those two and not the
    /// other, so every row survived with its counts intact and every diff pane
    /// went to `(no diff captured)`: an accurate `+64 -6` beside a claim that
    /// nothing was captured.
    ///
    /// Carrying `file_touch_seq` across is required, not tidiness. It
    /// stamps [`super::FileState::touched_seq`], the recency key
    /// [`super::MAX_TRACKED_FILES`] eviction orders by; restarting it at 0 under
    /// retained files would rank every surviving path above every new one and
    /// evict newest-first.
    ///
    /// `diff_budget` crosses for the same reason and is the sharper case: it
    /// is the accounting of the text `files` still holds, so resetting it
    /// under a retained ledger would leave the session believing it holds
    /// nothing while holding everything — a bound that reads as satisfied
    /// because it forgot what it was bounding.
    ///
    /// [`super::FileState::touched_turn`] is the one field on a retained row that is
    /// cleared rather than carried: it is a value in [`Self::turns_completed`]'s
    /// numbering, and that counter does reset (the field's own doc has the
    /// failure). The touch survives, so the row stays `● hot`; only its turn goes.
    ///
    /// Written as a destructure-and-restore so the default for a field added
    /// later is to RESET — new conversation state is the common case, and it
    /// then needs no edit here; a new *file-ledger* field is what has to be
    /// named.
    pub fn reset_conversation(&mut self) {
        let Self {
            mut files,
            files_evicted,
            file_touch_seq,
            diff_budget,
            per_call_producer_seen,
            ..
        } = std::mem::take(self);
        for file in &mut files {
            file.touched_turn = None;
        }
        self.files = files;
        self.files_evicted = files_evicted;
        self.file_touch_seq = file_touch_seq;
        self.diff_budget = diff_budget;
        self.per_call_producer_seen = per_call_producer_seen;
    }
}
