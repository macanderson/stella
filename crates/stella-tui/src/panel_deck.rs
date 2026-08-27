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
//! roster, drops every package whose install grant is not in force and every
//! panel the operator has not allowed, and hands what is left to
//! [`PanelDeck::reseat`] — so "no frame before the grant" is a property of the
//! route table rather than of the renderer, and the renderer cannot be the
//! place it is undone.
//!
//! It hands them over **again** whenever the roster could have changed, which
//! is what makes a retraction take effect inside a live session: a seat is the
//! grant's shadow, so the list is replaced whole and the seating it belongs to
//! is counted, and a frame request raised against an older one names a seating
//! that no longer exists (#5253).

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
    /// the chrome, or `None` when the last draw did not draw it.
    ///
    /// Written by the draw and **taken** by [`PanelDeck::requests`], which is
    /// what makes "asked for a frame" mean "on screen": a closed popup and a
    /// pane on a tab nobody is looking at both go undrawn, so neither leaves a
    /// rectangle behind and neither spawns a process for a panel no one can
    /// see. It is also the whole reason the deck is what asks — the lease is
    /// the host's measurement of its own layout, and the driver has never seen
    /// the terminal.
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

    /// Land a frame the plugin drew inside its budget, or refuse it.
    ///
    /// **`PanelLease::admits`'s rule, applied where the frame lands.** The
    /// driver already asks it against the lease it issued, and that is not
    /// enough on its own: the answer travels back as a slot *index*, and an
    /// index means whatever the seat list says it means when the answer
    /// arrives. Reseating between the ask and the answer — a plugin installed
    /// or retracted mid-session ([#5253]) — makes slot 0 a different plugin,
    /// and a frame landed by index alone would render one plugin's pixels
    /// inside another plugin's chrome. That is the wide-glyph breach by a
    /// different door: a rectangle carrying a name its author did not earn.
    ///
    /// So a frame is accepted only when this seat asked for one, the frame
    /// answers this seat's surface, and it carries the tick this seat is
    /// waiting on. No seating generation is needed for it, because the frame
    /// already names both things a stale answer gets wrong — a third
    /// identifier would only be a second way to ask the same question.
    ///
    /// Landing a frame clears any standing throttle, because the tag describes
    /// the frame on screen and this is a new one.
    ///
    /// [#5253]: https://github.com/macanderson/stella/issues/5253
    pub fn settle(&mut self, frame: PanelFrame) -> bool {
        if !self.awaiting || frame.surface != self.surface || frame.tick != self.tick {
            return false;
        }
        self.frame = Some(frame);
        self.overran_ms = None;
        self.awaiting = false;
        true
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
    /// Which composition of the roster these seats came from, echoed onto
    /// every request raised against them (#5253).
    generation: u64,
}

impl PanelDeck {
    /// Replace every seat with the ones the driver admitted, as of
    /// `generation`.
    ///
    /// **The only way seats are made**, which is what the seating generation
    /// needs to be true. A `seat(one)` that pushed a single slot lived here
    /// with no caller; it is gone rather than left, because it would add a slot
    /// the deck then stamps with a generation that slot was never part of, and
    /// a request raised against it would name a seating the driver's route list
    /// does not agree with. Deletion rather than a comment, on CLAUDE.md's
    /// reasoning about an unused item that asserts something the code does not.
    ///
    /// Wholesale rather than per-seat, because the seat list is the grant's
    /// shadow: a plugin retracted between two of these must lose its rectangle,
    /// and merging would leave it drawing from a slot nobody renewed.
    ///
    /// The open popup closes with them. It is an index into a list that has
    /// just been replaced, so keeping it would leave whichever plugin now
    /// holds that index open on the screen without anybody having typed its
    /// name.
    pub fn reseat(&mut self, generation: u64, seats: &[crate::envelope::PanelSeat]) {
        self.open_popup = None;
        self.generation = generation;
        self.slots = seats
            .iter()
            .map(|seat| PanelSlot::new(&seat.plugin, seat.surface, seat.command.clone()))
            .collect();
    }

    /// Which composition these seats came from.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
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
    /// A slot is asked when the last draw drew it (so there is a rectangle to
    /// lease, and something is on screen to refresh), it has no request
    /// outstanding, and it has not been asked inside the refresh interval.
    /// Nothing here starts anything — a request is a message, and a seat that
    /// no grant produced has no slot to be asked about.
    pub fn requests(&mut self) -> Vec<crate::envelope::WorkspaceInput> {
        self.requests_at(std::time::Instant::now())
    }

    /// [`PanelDeck::requests`] against a stated clock.
    ///
    /// The seam exists because a witness for "a panel that went undrawn is not
    /// asked again" needs a second draw, and a second draw inside the refresh
    /// interval reports "not asked" for the wrong reason — while a test that
    /// slept out a real second would be a second added to every run.
    pub fn requests_at(&mut self, now: std::time::Instant) -> Vec<crate::envelope::WorkspaceInput> {
        let mut out = Vec::new();
        for (slot, panel) in self.slots.iter_mut().enumerate() {
            let Some((cols, rows)) = panel.leased.take() else {
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
                generation: self.generation,
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
        Inbound::PanelsSeated { generation, seats } => deck.reseat(*generation, seats),
        Inbound::PanelFrame { slot, frame } => {
            if let Some(slot) = deck.slot_mut(*slot) {
                // Refused frames are dropped rather than reported: a frame for
                // a seat that has been reseated names nothing a reader could
                // act on, and the seat asks again on the next draw.
                let _ = slot.settle(frame.as_ref().clone());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Inbound, PanelSeat, WorkspaceInput};
    use stella_plugin::{PanelLine, PanelPaint, PanelSpan, PanelStyle, PanelText};

    fn seat(plugin: &str, surface: PanelSurface) -> PanelSeat {
        PanelSeat {
            plugin: plugin.to_string(),
            surface,
            command: None,
        }
    }

    fn frame(surface: PanelSurface, tick: u64, text: &str) -> PanelFrame {
        PanelFrame::new(
            surface,
            tick,
            PanelPaint::Lines(vec![PanelLine::new(vec![PanelSpan::new(
                PanelText::new(text).expect("plain text"),
                PanelStyle::plain(),
            )])]),
        )
    }

    /// Draw one seat and read back what its rectangle holds.
    fn painted(deck: &mut PanelDeck, index: usize) -> String {
        let area = Rect::new(0, 0, 40, 5);
        let mut buf = Buffer::empty(area);
        deck.slot_mut(index).expect("a seat").render(area, &mut buf);
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "))
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Draw every seat, then ask, and report the `(slot, tick)` pairs the deck
    /// opened — so a test answers with the tick the deck actually issued rather
    /// than one it guessed.
    fn drawn_then_asked(deck: &mut PanelDeck) -> Vec<(usize, u64)> {
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 40));
        for index in 0..deck.slots().len() {
            let y = u16::try_from(index).unwrap_or(0) * 5;
            let area = Rect::new(0, y, 40, 5);
            deck.slot_mut(index).expect("a seat").render(area, &mut buf);
        }
        deck.requests()
            .into_iter()
            .filter_map(|input| match input {
                WorkspaceInput::PanelFrameWanted { slot, tick, .. } => Some((slot, tick)),
                _ => None,
            })
            .collect()
    }

    /// **The breach witness.** A frame in flight when the seats are replaced
    /// does not land in whatever plugin now holds its slot index.
    ///
    /// The answer travels back as an index, and an index means whatever the
    /// seat list says it means when it arrives. Landing it by index alone puts
    /// one plugin's pixels inside another plugin's chrome — the label a reader
    /// trusts naming a rectangle its author did not draw, which is the
    /// wide-glyph breach reached by a different door.
    #[test]
    fn a_frame_in_flight_across_a_reseat_lands_nowhere() {
        let mut deck = PanelDeck::default();
        deck.reseat(
            1,
            &[
                seat("alpha", PanelSurface::Settings),
                seat("beta", PanelSurface::Overlay),
            ],
        );
        let (slot, tick) = drawn_then_asked(&mut deck)
            .into_iter()
            .find(|(slot, _)| *slot == 0)
            .expect("alpha asked for a frame");

        // alpha is uninstalled while its frame is in flight, so slot 0 is beta.
        // A second seating, numbered as the driver numbers it — which the frame
        // does not carry and `settle` never reads. That is the point: this rule
        // holds on the frame's own surface and tick alone, so it is still the
        // last line of defence for a host that got the seating wrong.
        deck.reseat(2, &[seat("beta", PanelSurface::Overlay)]);
        assert_eq!(deck.slots()[0].plugin(), "beta", "slot 0 changed hands");

        let landed = deck.slot_mut(slot).expect("a seat").settle(frame(
            PanelSurface::Settings,
            tick,
            "alphas pixels",
        ));

        // The breach itself first, so a flip fails on the pixels rather than
        // on the return value that is only evidence about them.
        let shown = painted(&mut deck, 0);
        assert!(
            !shown.contains("alphas pixels"),
            "alpha did not draw inside beta's chrome:\n{shown}"
        );
        assert!(
            shown.contains("panel · beta"),
            "and the chrome is still beta's:\n{shown}"
        );
        assert!(!landed, "and the frame said so");
    }

    /// Anti-vacuity: a frame answering the seat that asked for it, on the tick
    /// it asked on, is drawn.
    #[test]
    fn a_frame_answering_its_own_lease_is_drawn() {
        let mut deck = PanelDeck::default();
        deck.reseat(1, &[seat("alpha", PanelSurface::Settings)]);
        let (slot, tick) = drawn_then_asked(&mut deck)[0];

        assert!(
            deck.slot_mut(slot).expect("a seat").settle(frame(
                PanelSurface::Settings,
                tick,
                "alphas pixels"
            )),
            "its own frame is accepted"
        );
        let shown = painted(&mut deck, 0);
        assert!(shown.contains("alphas pixels"), "{shown}");
    }

    /// A frame for the wrong surface is refused with no reseat in sight: one
    /// plugin drawing several surfaces gets several leases per tick, alike in
    /// everything but this.
    #[test]
    fn a_frame_answering_another_surface_is_refused() {
        let mut deck = PanelDeck::default();
        deck.reseat(1, &[seat("alpha", PanelSurface::Settings)]);
        let (slot, tick) = drawn_then_asked(&mut deck)[0];

        assert!(
            !deck.slot_mut(slot).expect("a seat").settle(frame(
                PanelSurface::Overlay,
                tick,
                "wrong rectangle"
            )),
            "a frame for another surface is refused"
        );
    }

    /// A frame arriving for a tick the host has moved past is refused, so a
    /// slow answer cannot overwrite a newer one.
    #[test]
    fn a_frame_carrying_a_stale_tick_is_refused() {
        let mut deck = PanelDeck::default();
        deck.reseat(1, &[seat("alpha", PanelSurface::Settings)]);
        let (slot, tick) = drawn_then_asked(&mut deck)[0];

        assert!(
            !deck.slot_mut(slot).expect("a seat").settle(frame(
                PanelSurface::Settings,
                tick.saturating_sub(1),
                "stale"
            )),
            "a frame for an older tick is refused"
        );
    }

    /// A seat that asked for nothing accepts nothing — an unsolicited frame is
    /// an answer to a question the deck never put.
    #[test]
    fn a_seat_that_asked_for_nothing_accepts_nothing() {
        let mut deck = PanelDeck::default();
        deck.reseat(1, &[seat("alpha", PanelSurface::Settings)]);
        assert!(
            !deck
                .slot_mut(0)
                .expect("a seat")
                .settle(frame(PanelSurface::Settings, 0, "unasked")),
            "nothing was asked for"
        );
    }

    /// Every panel envelope reaches the deck through one ingest arm, a refused
    /// frame is dropped there, and nothing else is claimed.
    #[test]
    fn ingest_routes_every_panel_envelope_and_claims_nothing_else() {
        let mut deck = PanelDeck::default();
        assert!(ingest(
            &mut deck,
            &Inbound::PanelsSeated {
                generation: 1,
                seats: vec![seat("alpha", PanelSurface::Settings)],
            }
        ));
        assert!(ingest(&mut deck, &Inbound::PanelSilent { slot: 0 }));
        assert!(ingest(
            &mut deck,
            &Inbound::PanelThrottled {
                slot: 0,
                elapsed_ms: 91,
                budget_ms: 33,
            }
        ));
        assert!(ingest(
            &mut deck,
            &Inbound::PanelFrame {
                slot: 0,
                frame: Box::new(frame(PanelSurface::Settings, 7, "unasked")),
            }
        ));
        assert!(
            !deck.slots()[0].has_frame(),
            "the unasked frame was dropped"
        );
        assert!(!ingest(&mut deck, &Inbound::Notice("not a panel".into())));
    }
}
