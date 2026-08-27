// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where a plugin's panel lands on the screen — SPEC 12.2's three placements.
//!
//! [`crate::plugin_panel`] draws a frame; this decides which rectangle a frame
//! is drawn in and holds the frame between draws. The two are separate because
//! they answer different questions: the host renderer is about a plugin's
//! pixels never reaching a cell it was not leased, and this module is about the
//! deck's own layout.
//!
//! # The draw path never awaits
//!
//! A [`PanelSlot`] holds the last frame a plugin drew, and a draw renders that.
//! Asking for the next one is `stella_runtime::panel_host::ask`, which the host
//! runs as a task and lands here through [`PanelSlot::settle`] — so a repaint
//! is a pure projection of state the deck already has, and a plugin that has
//! wedged costs a stale rectangle rather than a frozen terminal.
//!
//! A slot with no frame yet draws its chrome and nothing inside it, which is
//! what the first tick after an install looks like. A slot whose tick overran
//! its budget draws the frame before it, with the host's throttle tag on the
//! border.
//!
//! # A slot exists because a grant did
//!
//! Nothing here constructs a slot from a manifest. The host composes the
//! roster, drops every package whose install grant is not in force, and hands
//! what is left to [`PanelDeck::seat`] — so "no frame before the grant" is a
//! property of the route table rather than of the renderer, and the renderer
//! cannot be the place it is undone.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::{Clear, Widget};

use stella_plugin::{PanelFrame, PanelSurface};

use crate::plugin_panel;

/// One plugin's panel on one surface, and the last thing it drew.
#[derive(Debug, Clone)]
pub struct PanelSlot {
    /// The manifest name, which is what the host stamps on the chrome.
    plugin: String,
    /// Which of SPEC 12.2's placements this slot is.
    surface: PanelSurface,
    /// The slash name this panel's popup opens under, without the leading `/`.
    ///
    /// Only ever `Some` on [`PanelSurface::Command`], and only for a name the
    /// host admitted — a name colliding with a built-in never reaches a slot.
    command: Option<String>,
    /// The last frame this panel drew, or `None` before its first tick.
    frame: Option<PanelFrame>,
    /// What the last tick cost when it overran its budget, in milliseconds.
    ///
    /// `Some` is the throttle: it survives into the next draw so the tag is on
    /// screen for as long as the stale frame it explains.
    overran_ms: Option<u64>,
    /// The budget the overran tick was leased, kept beside it because
    /// "throttled" with no numbers is a complaint rather than a report.
    budget_ms: u32,
    /// The host's frame counter for this slot, handed to each lease so a frame
    /// that arrives after the host moved on is discardable.
    tick: u64,
    /// The rectangle the last draw leased this panel, in `(cols, rows)` inside
    /// the chrome, or `None` before the first draw that had room for it.
    ///
    /// Recorded by the draw and read by [`PanelDeck::requests`], which is the
    /// whole reason the deck is what asks: the lease is the host's measurement
    /// of its own layout, and the driver has never seen the terminal.
    leased: Option<(u16, u16)>,
    /// Whether a request for this slot is out and unanswered.
    ///
    /// Without it the draw loop would ask ~30 times a second and spawn a
    /// process per ask. Exactly one of the three panel envelopes answers each
    /// request — [`crate::envelope::Inbound::PanelSilent`] exists so a failed
    /// tick still rearms — so this cannot latch shut on an error.
    awaiting: bool,
    /// When the last request went out, so a panel that answers instantly is
    /// still asked at the refresh interval rather than as fast as it replies.
    asked_at: Option<std::time::Instant>,
}

impl PanelSlot {
    /// Seat a panel that a grant admitted.
    #[must_use]
    pub fn new(plugin: impl Into<String>, surface: PanelSurface, command: Option<String>) -> Self {
        Self {
            plugin: plugin.into(),
            surface,
            command,
            frame: None,
            overran_ms: None,
            budget_ms: 0,
            tick: 0,
            leased: None,
            awaiting: false,
            asked_at: None,
        }
    }

    /// The manifest name this panel draws under.
    #[must_use]
    pub fn plugin(&self) -> &str {
        &self.plugin
    }

    /// Which placement this slot is.
    #[must_use]
    pub fn surface(&self) -> PanelSurface {
        self.surface
    }

    /// The slash name this panel's popup opens under, if it has one.
    #[must_use]
    pub fn command(&self) -> Option<&str> {
        self.command.as_deref()
    }

    /// The tick number of the request currently out, or of the last one.
    #[must_use]
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// The rectangle the last draw leased this panel, inside the chrome.
    #[must_use]
    pub fn leased(&self) -> Option<(u16, u16)> {
        self.leased
    }

    /// Land a frame the plugin drew inside its budget.
    ///
    /// The frame is not checked here: `PanelLease::admits` is what refuses a
    /// frame answering another surface or a tick the host has moved past, and
    /// it is asked on the task's side, where the lease still exists. Landing a
    /// frame clears any standing throttle, because the tag describes the frame
    /// on screen and this is a new one.
    pub fn settle(&mut self, frame: PanelFrame) {
        self.frame = Some(frame);
        self.overran_ms = None;
        self.awaiting = false;
    }

    /// Record a tick that overran, keeping whatever frame is already on screen.
    pub fn overran(&mut self, elapsed_ms: u64, budget_ms: u32) {
        self.overran_ms = Some(elapsed_ms);
        self.budget_ms = budget_ms;
        self.awaiting = false;
    }

    /// Record a tick that produced nothing, keeping the frame and saying so
    /// nowhere. Rearms the seat, which is the whole point of the envelope.
    pub fn silent(&mut self) {
        self.awaiting = false;
    }

    /// Whether this slot has drawn anything yet.
    #[must_use]
    pub fn has_frame(&self) -> bool {
        self.frame.is_some()
    }

    /// Draw this panel into `area`: the host's chrome, the last good frame
    /// inside it, and the throttle tag when the last tick overran.
    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        let lease = plugin_panel::chrome(area, &self.plugin, buf);
        // The measurement the next lease states, taken here because this is
        // the only place that knows it: `chrome` decides how much of `area`
        // survives its own border.
        self.leased = (lease.width > 0 && lease.height > 0).then_some((lease.width, lease.height));
        if lease.width == 0 || lease.height == 0 {
            return;
        }
        if let Some(frame) = &self.frame {
            plugin_panel::blit(frame, lease, buf);
        }
        if let Some(elapsed) = self.overran_ms {
            plugin_panel::throttle_tag(area, elapsed, self.budget_ms, buf);
        }
    }
}

/// Every panel the deck is currently permitted to draw.
///
/// Ordered by seating order, which the host makes name order, so two runs of
/// the same workspace put the same pane in the same place on the SETTINGS nav.
#[derive(Debug, Clone, Default)]
pub struct PanelDeck {
    slots: Vec<PanelSlot>,
    /// Which command panel's popup is open, as an index into `slots`.
    open_popup: Option<usize>,
}

impl PanelDeck {
    /// Seat one admitted panel. The host calls this once per route.
    pub fn seat(&mut self, slot: PanelSlot) {
        self.slots.push(slot);
    }

    /// Replace every seat with the ones the driver admitted.
    ///
    /// Wholesale rather than per-seat, because the seat list is the grant's
    /// shadow: a plugin retracted between two of these must lose its rectangle,
    /// and merging would leave it drawing from a slot nobody renewed.
    pub fn reseat(&mut self, seats: &[crate::envelope::PanelSeat]) {
        self.open_popup = None;
        self.slots = seats
            .iter()
            .map(|seat| PanelSlot::new(&seat.plugin, seat.surface, seat.command.clone()))
            .collect();
    }

    /// Every seated panel, in seating order.
    #[must_use]
    pub fn slots(&self) -> &[PanelSlot] {
        &self.slots
    }

    /// Mutable access to one slot, for the task that landed its frame.
    pub fn slot_mut(&mut self, index: usize) -> Option<&mut PanelSlot> {
        self.slots.get_mut(index)
    }

    /// The frame requests this draw owes the driver, marking each as out.
    ///
    /// **The deck asks and the driver spawns**, which is how SPEC 12.4's "off
    /// the draw path" is kept: only the deck knows the rectangle, only the
    /// driver may block on a process, and this is the sentence between them.
    ///
    /// A slot is asked when it has been drawn at least once (so there is a
    /// rectangle to lease), has no request outstanding, and has not been asked
    /// inside the refresh interval. Nothing here starts anything — a request is a
    /// message, and a seat that no grant produced has no slot to be asked
    /// about.
    pub fn requests(&mut self) -> Vec<crate::envelope::WorkspaceInput> {
        let now = std::time::Instant::now();
        let mut out = Vec::new();
        for (slot, panel) in self.slots.iter_mut().enumerate() {
            let Some((cols, rows)) = panel.leased else {
                continue;
            };
            if panel.awaiting {
                continue;
            }
            if panel
                .asked_at
                .is_some_and(|at| now.duration_since(at) < REFRESH)
            {
                continue;
            }
            panel.tick = panel.tick.saturating_add(1);
            panel.awaiting = true;
            panel.asked_at = Some(now);
            out.push(crate::envelope::WorkspaceInput::PanelFrameWanted {
                slot,
                tick: panel.tick,
                cols,
                rows,
            });
        }
        out
    }

    /// The indices of every panel drawing on `surface`, in seating order.
    #[must_use]
    pub fn on(&self, surface: PanelSurface) -> Vec<usize> {
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.surface == surface)
            .map(|(index, _)| index)
            .collect()
    }

    /// The command panel registered under the bare slash name `name`.
    #[must_use]
    pub fn command_index(&self, name: &str) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.command() == Some(name))
    }

    /// Open the popup of the command panel named `name`, or report that no
    /// panel answers to it.
    pub fn open_command(&mut self, name: &str) -> bool {
        match self.command_index(name) {
            Some(index) => {
                self.open_popup = Some(index);
                true
            }
            None => false,
        }
    }

    /// Close whatever popup is open. Esc closes every overlay (SPEC 13).
    pub fn close_popup(&mut self) {
        self.open_popup = None;
    }

    /// The slot whose popup is open, if any.
    #[must_use]
    pub fn open_popup(&self) -> Option<usize> {
        self.open_popup
    }
}

/// How often a seated panel is asked for a new frame.
///
/// A rate, not a budget: `PanelLease::budget_ms` is the deadline one frame has,
/// and this is how often the host wants one. They are different numbers because
/// a panel is a process — asking at the budget's own rate would spawn thirty a
/// second for a rectangle that changes when its subject does.
const REFRESH: std::time::Duration = std::time::Duration::from_millis(1000);

/// Apply a panel envelope to the deck's panel state, reporting whether it was
/// one.
///
/// The whole of the deck's panel ingest, so `deck_ui`'s dispatcher carries one
/// arm rather than four — that module is a grandfathered god file closed to
/// growth (AGENTS.md § "God files"), and panel state belongs beside the panels
/// anyway.
pub fn ingest(deck: &mut PanelDeck, inbound: &crate::envelope::Inbound) -> bool {
    use crate::envelope::Inbound;
    match inbound {
        Inbound::PanelsSeated(seats) => deck.reseat(seats),
        Inbound::PanelFrame { slot, frame } => {
            if let Some(slot) = deck.slot_mut(*slot) {
                slot.settle(frame.as_ref().clone());
            }
        }
        Inbound::PanelSilent { slot } => {
            if let Some(slot) = deck.slot_mut(*slot) {
                slot.silent();
            }
        }
        Inbound::PanelThrottled {
            slot,
            elapsed_ms,
            budget_ms,
        } => {
            if let Some(slot) = deck.slot_mut(*slot) {
                slot.overran(*elapsed_ms, *budget_ms);
            }
        }
        _ => return false,
    }
    true
}

/// Close an open command popup on Esc, reporting whether it took the key.
///
/// SPEC 13's "all overlays closable with esc" is the whole of the keyboard a
/// panel gets: SPEC 12 leases it a rectangle, never a keystroke, so every other
/// key goes on to do its normal job with the popup still up. Here rather than
/// in `deck_ui` for [`ingest`]'s reason — that module is closed to growth, and
/// this is panel state.
pub fn esc_closes_popup(deck: &mut PanelDeck, is_esc: bool) -> bool {
    let closing = is_esc && deck.open_popup.is_some();
    if closing {
        deck.close_popup();
    }
    closing
}

/// The rows an overlay panel claims in the SESSION transcript, chrome included.
///
/// Zero when nothing draws there, so the band collapses exactly the way the
/// gate cards above it do rather than leaving an empty border in the flow of a
/// turn.
#[must_use]
pub fn overlay_height(deck: &PanelDeck, area_height: u16) -> u16 {
    if deck.on(PanelSurface::Overlay).is_empty() {
        return 0;
    }
    // A third of the tab, bounded so the transcript is never squeezed out of
    // its own tab by somebody else's rectangle, and never smaller than the
    // chrome plus one interior row.
    OVERLAY_ROWS.min(area_height.saturating_sub(MIN_TRANSCRIPT_ROWS))
}

/// The rows the overlay band claims: the host's two border rows and six of
/// plugin, which is a block in the flow of a turn rather than a second tab.
const OVERLAY_ROWS: u16 = 8;

/// The transcript keeps at least this many rows whatever a plugin asks for.
/// SPEC 12 leases a plugin a rectangle; it does not lease it the tab.
const MIN_TRANSCRIPT_ROWS: u16 = 6;

/// Draw every overlay panel stacked in `area`, sharing it evenly.
pub fn render_overlay(deck: &mut PanelDeck, area: Rect, buf: &mut Buffer) {
    let indices = deck.on(PanelSurface::Overlay);
    if indices.is_empty() || area.height == 0 || area.width == 0 {
        return;
    }
    let each = area.height / u16::try_from(indices.len()).unwrap_or(1).max(1);
    if each < 3 {
        // Too little for chrome and one interior row: draw the first alone
        // rather than a stack of borders with nothing in them.
        if let Some(slot) = indices.first().and_then(|index| deck.slots.get_mut(*index)) {
            slot.render(area, buf);
        }
        return;
    }
    for (row, index) in indices.iter().enumerate() {
        let Ok(row) = u16::try_from(row) else {
            return;
        };
        let y = area.y + row * each;
        if y >= area.y + area.height {
            return;
        }
        let Some(slot) = deck.slots.get_mut(*index) else {
            continue;
        };
        slot.render(Rect::new(area.x, y, area.width, each), buf);
    }
}

/// The rectangle a command panel's popup occupies: centred, and bounded so it
/// reads as a card over the deck rather than as a new full-screen tab.
#[must_use]
pub fn popup_rect(area: Rect) -> Rect {
    let width = area.width.saturating_mul(3) / 4;
    let height = area.height.saturating_mul(3) / 5;
    let width = width.clamp(POPUP_MIN_COLS.min(area.width), area.width);
    let height = height.clamp(POPUP_MIN_ROWS.min(area.height), area.height);
    Rect::new(
        area.x + (area.width.saturating_sub(width)) / 2,
        area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    )
}

/// The smallest popup worth opening — chrome plus enough interior for a line
/// of the plugin's own words.
const POPUP_MIN_COLS: u16 = 24;
/// The popup's floor in rows, on the same reasoning as [`POPUP_MIN_COLS`].
const POPUP_MIN_ROWS: u16 = 6;

/// Draw the open command popup over `area`, if one is open.
///
/// [`Clear`] first, because a popup is drawn over a tab that has already
/// painted itself and a panel must not composite with whatever was underneath
/// it — a plugin's rectangle reading as half of Stella's own surface is the
/// spoofing SPEC 12.3 forbids arriving by a different door.
pub fn render_command_popup(deck: &mut PanelDeck, area: Rect, buf: &mut Buffer) {
    let rect = popup_rect(area);
    let Some(slot) = deck.open_popup.and_then(|index| deck.slots.get_mut(index)) else {
        return;
    };
    if rect.width < 3 || rect.height < 3 {
        return;
    }
    Clear.render(rect, buf);
    slot.render(rect, buf);
}
