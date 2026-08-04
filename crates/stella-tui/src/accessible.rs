//! Accessible mode — the **deck**, made readable by a screen reader.
//!
//! This is deliberately not a second surface. `stella --accessible` runs the
//! same [`crate::deck_shell::run_deck`] with every tab, every gate and every
//! key it always had; what changes is how that program talks to the terminal.
//! A separate accessible surface is a permanent second-class tier — every deck
//! feature shipped afterwards becomes a new gap it never closes — so the real
//! product has to be the accessible one (#1258, which reverses #1237's
//! `--simple`).
//!
//! Four mechanisms, in the order they matter:
//!
//! 1. **The normal screen.** The alternate screen (`term::Screen`) is
//!    a separate grid the deck rewrites wholesale ~30 times a second, and it
//!    vanishes on exit. A reader is handed cell churn with no notion of "a new
//!    line appeared", and quitting takes the conversation with it.
//! 2. **Scrollback.** With an inline viewport, [`Scrollback`] moves each
//!    settled transcript entry out of the repainting pane and into ordinary
//!    terminal output via `Terminal::insert_before` — announced once as it
//!    arrives, reachable with the reader's review cursor, and still there
//!    afterwards.
//! 3. **Announcements.** Tab switches, overlays and lane focus only repaint,
//!    which on the normal screen is silent. They emit a line instead.
//! 4. **Linear layout.** The remaining side-by-side splits stack, and the grid
//!    views get a label-value text rendering (`ui.accessible`, read by the
//!    view modules).
//!
//! The load-bearing invariant lives here rather than in the shell, because it
//! is the one that is easy to get wrong and impossible to see: **the flush
//! counter may only advance when `insert_before` actually wrote.** That call
//! is a silent no-op on any non-inline viewport, so advancing the counter
//! against one drops entries from the live pane without writing them anywhere.
//! [`Scrollback::plan`] and [`Scrollback::record`] are therefore two steps, and
//! only the shell — which holds the `io::Result` of the write — may take the
//! second.

use std::collections::HashMap;

use ratatui::text::Line;

use crate::deck::WorkspaceModel;
use crate::envelope::AgentId;
use crate::model::FileState;
use crate::term::Screen;

/// Rows the deck's inline viewport claims at most.
///
/// The deck's own chrome is ~11 rows (tab bar 3, trace strip 2, progress 1,
/// composer 1, footer 1, statline 3), so a viewport sized for the
/// single-pane surface #1237 shipped — 14 rows — would leave three rows of
/// content and no usable table. This is a live working area, not the
/// session's history: everything older has already moved into scrollback.
const DECK_INLINE_VIEWPORT_ROWS: u16 = 30;

/// How tall the inline viewport should be on a terminal of `terminal_rows`.
///
/// One row is always left above it whenever the terminal has one to spare:
/// the viewport is anchored to the cursor's row, and claiming the entire
/// screen would leave `insert_before` nowhere to put a line — every flush
/// would scroll the whole screen instead of appending above a stable region.
///
/// There is deliberately no lower bound beyond one row. A floor could only be
/// honoured by claiming rows the terminal does not have, which is the one
/// outcome this must never produce; a terminal too short for the full working
/// area gets a cramped one, not a broken flush.
pub(crate) fn inline_viewport_rows(terminal_rows: u16) -> u16 {
    terminal_rows
        .saturating_sub(1)
        .clamp(1, DECK_INLINE_VIEWPORT_ROWS)
}

/// Which screen a deck session draws on.
///
/// An accessible session is never allowed the alternate one: it is the whole
/// reason the mode exists, so it is not a preference the caller can combine
/// away.
pub(crate) fn screen_for(accessible: bool) -> Screen {
    if accessible {
        Screen::Normal
    } else {
        Screen::Alternate
    }
}

/// Whether this session captures the mouse.
///
/// `requested` is [`crate::deck_shell::DeckOptions::mouse_capture`], which is
/// a request rather than a decision: an accessible session refuses it. Mouse
/// capture disables the terminal's own selection, and selection is how several
/// assistive technologies read a terminal — so honouring the request would
/// take away the thing the mode exists to provide.
pub(crate) fn mouse_capture_enabled(requested: bool, accessible: bool) -> bool {
    requested && !accessible
}

/// Said out loud when the inline viewport could not be anchored.
///
/// Anchoring means writing a Device Status Report and blocking until the
/// terminal answers with its cursor position. Every real emulator answers;
/// some minimal ones and most test harnesses do not, and the read times out.
/// That degrades to a full-viewport draw on the user's *own* screen — never
/// the alternate one — with the scrollback flush disabled. The promise of the
/// mode is that finished messages become durable output; silently not
/// delivering it is worse than not offering it.
const DEGRADE_NOTICE: &str = "accessible mode: this terminal did not report its cursor position, so messages stay in the \
     deck pane instead of moving into scrollback";

/// What a session must say out loud about the scrollback it is not getting.
///
/// `inline` is whether the viewport was actually anchored, not whether one was
/// asked for. A session that got what it asked for says nothing, and an
/// ordinary (non-accessible) session never promised scrollback in the first
/// place — announcing a missing viewport there would be chatter about a
/// feature the user did not request.
pub(crate) fn degrade_notice(accessible: bool, inline: bool) -> Option<&'static str> {
    (accessible && !inline).then_some(DEGRADE_NOTICE)
}

/// The marker every line the *program* speaks opens with.
///
/// [`stella_protocol::AgentEvent`] has no system-notice variant, so driver and
/// chrome messages ride `Text` — which renders on the **agent** rail. The rail
/// glyph is a visual distinction, so it is exactly the one that does not
/// survive being read aloud: unmarked, the transcript asserts that the model
/// said "conversation cleared". Same `▸` #1243 used, and the same one the CLI
/// already uses on the normal screen for its own voice.
pub const NOTICE_MARKER: &str = "▸ ";

/// One lane's newly-settled entries, ready to be written into scrollback.
///
/// Produced by [`Scrollback::plan`] and consumed by the shell, which writes it
/// and only then calls [`Scrollback::record`]. The split is the invariant: a
/// block that was planned but not written must leave the counter alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushBlock {
    /// The lane these entries belong to.
    pub agent: AgentId,
    /// First transcript index not yet in scrollback.
    pub from: usize,
    /// One past the last settled index.
    pub to: usize,
    /// Whether this block opens with a lane header. True only when the lane
    /// differs from the one written last — consecutive blocks from the same
    /// agent read as one voice, and re-announcing it every flush would be
    /// noise a reader cannot skip.
    pub header: Option<String>,
}

/// Per-lane scrollback bookkeeping for an accessible deck session.
///
/// Lives in [`crate::deck_ui::DeckUi`] because it is ephemeral interaction
/// state, not part of the event fold — nothing here is reconstructible by
/// replaying the log, and nothing here may influence what the model says.
#[derive(Debug, Clone, Default)]
pub struct Scrollback {
    /// Entries already written into the terminal's scrollback, per lane.
    ///
    /// Per-lane because the deck has several. #1237's rule — "flush everything
    /// but the trailing entry" — assumed one conversation; with lanes it has
    /// to be answered per lane, and the counter has to survive a tab or focus
    /// change (which touch neither this map nor the transcript).
    flushed: HashMap<AgentId, usize>,
    /// The lane whose block was written last, so a header is emitted only when
    /// the voice actually changes.
    last_lane: Option<AgentId>,
    /// Whether the inline viewport was actually obtained.
    ///
    /// The load-bearing flag, and deliberately not "is accessible mode on":
    /// `insert_before` is a silent no-op on any other viewport, so a session
    /// that asked for accessible mode and did not get an inline viewport must
    /// flush nothing at all rather than advance a counter against a write that
    /// never happened.
    live: bool,
    /// Lines queued by the pure layer for the next flush — tab switches,
    /// overlay open/close, focus changes. Drained by the shell.
    announcements: Vec<String>,
}

impl Scrollback {
    /// Arm (or disarm) the whole mechanism. Called once by the shell, with
    /// whether the inline viewport was obtained.
    pub fn set_live(&mut self, live: bool) {
        self.live = live;
    }

    /// Whether settled entries are actually reaching the terminal's scrollback.
    pub fn is_live(&self) -> bool {
        self.live
    }

    /// How many of `agent`'s entries are already in scrollback.
    pub fn flushed_for(&self, agent: &str) -> usize {
        self.flushed.get(agent).copied().unwrap_or(0)
    }

    /// Queue a line for the next flush.
    ///
    /// Dropped outright when the mechanism is not live: with no inline
    /// viewport there is nowhere for it to go, and stashing it would grow an
    /// unbounded list nobody ever reads.
    pub fn announce(&mut self, text: impl Into<String>) {
        if !self.live {
            return;
        }
        self.announcements.push(text.into());
    }

    /// Take the queued announcements, leaving the queue empty.
    pub fn take_announcements(&mut self) -> Vec<String> {
        std::mem::take(&mut self.announcements)
    }

    /// What is safe to move into scrollback right now, lane by lane.
    ///
    /// `include_trailing` is false for every draw and true exactly once, at
    /// teardown. [`crate::model::SessionModel`] coalesces streaming deltas into
    /// the **trailing** entry, so it is the only one that can still grow;
    /// flushing it mid-session would put half a sentence into scrollback and
    /// then, once it finished growing, put the whole sentence there again — a
    /// reader would hear the fragment and the sentence as two utterances. By
    /// teardown nothing can grow.
    ///
    /// Returns an empty plan when the mechanism is not live, so a caller that
    /// forgets the gate still cannot advance a counter against a no-op write.
    pub fn plan(&self, model: &WorkspaceModel, include_trailing: bool) -> Vec<FlushBlock> {
        if !self.live {
            return Vec::new();
        }
        let multi_lane = model.agents.len() > 1;
        let mut last = self.last_lane.clone();
        let mut plan = Vec::new();
        for entry in &model.agents {
            let id = &entry.meta.id;
            let settled = settled_entry_count(entry.model.transcript.len(), include_trailing);
            let from = self.flushed_for(id);
            if settled <= from {
                continue;
            }
            // A single-lane session never names its lane: there is only one
            // voice, and prefixing every block with it is noise.
            let header = (multi_lane && last.as_deref() != Some(id.as_str()))
                .then(|| format!("{NOTICE_MARKER}{} · {}", entry.meta.title, id));
            last = Some(id.clone());
            plan.push(FlushBlock {
                agent: id.clone(),
                from,
                to: settled,
                header,
            });
        }
        plan
    }

    /// Record that `block` reached the terminal.
    ///
    /// **Only** call this once the write returned `Ok`. Everything in the
    /// module docs about the counter comes down to this one call site.
    pub fn record(&mut self, block: &FlushBlock) {
        self.flushed.insert(block.agent.clone(), block.to);
        self.last_lane = Some(block.agent.clone());
    }

    /// Shift a lane's counter down after front-eviction dropped `slots`
    /// leading entries.
    ///
    /// The retention cap drains the oldest chunk and stands one marker in its
    /// place, so every retained index moves down by the number of slots the
    /// pass removed. The counter is an index into that list; left alone it
    /// would suppress entries from the live pane that were never written
    /// anywhere. Under-counting is the safe direction and `saturating_sub`
    /// takes it.
    pub fn shift_after_eviction(&mut self, agent: &str, slots: usize) {
        if let Some(flushed) = self.flushed.get_mut(agent) {
            *flushed = flushed.saturating_sub(slots);
        }
    }

    /// Drop a lane's bookkeeping — for a lane the model no longer carries.
    pub fn forget(&mut self, agent: &str) {
        self.flushed.remove(agent);
        if self.last_lane.as_deref() == Some(agent) {
            self.last_lane = None;
        }
    }
}

/// How many leading transcript entries are safe to move into scrollback.
///
/// Everything but the last (see [`Scrollback::plan`]), or all of them at
/// teardown.
fn settled_entry_count(transcript_len: usize, include_trailing: bool) -> usize {
    if include_trailing {
        transcript_len
    } else {
        transcript_len.saturating_sub(1)
    }
}

/// The rendered lines for one [`FlushBlock`], wrapped to `width`.
///
/// `live` is `false` for every entry: liveness is the tail-follow affordance
/// for a thought still being written, and nothing in this range is still being
/// written — that is what makes it flushable at all.
pub fn block_lines(
    model: &WorkspaceModel,
    block: &FlushBlock,
    expand_thinking: bool,
    width: usize,
) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let Some(entry) = model.agents.iter().find(|a| a.meta.id == block.agent) else {
        return out;
    };
    if let Some(header) = &block.header {
        out.push(Line::from(header.clone()));
    }
    let transcript = &entry.model.transcript;
    let files: &[FileState] = &entry.model.files;
    let to = block.to.min(transcript.len());
    for item in &transcript[block.from.min(to)..to] {
        crate::render::entry_lines(
            item,
            files,
            expand_thinking,
            expand_thinking,
            false,
            width,
            &mut out,
        );
    }
    out
}

pub(crate) mod announce;

#[cfg(test)]
mod tests;
