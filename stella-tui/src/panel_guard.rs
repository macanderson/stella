//! The panel panic boundary (L-T7) — the crate's only one.
//!
//! A *band* is any rectangle of the frame drawn by one function: a single-session
//! panel ([`crate::render::guarded_panel`]), a deck chrome row, the deck's
//! active-tab content area, or a deck overlay. Each is drawn into its own
//! scratch [`Buffer`] inside `catch_unwind`; on a panic the scratch is dropped
//! and a red error card is painted in its place. Nothing unwinds out of
//! `terminal.draw`, so the surviving bands render, the event loop keeps its
//! keyboard, and the session continues.
//!
//! The scratch starts as a **copy of the frame region**, not an empty buffer.
//! That is what lets an overlay be guarded over the whole frame without
//! erasing the bands drawn beneath it, and it is why the deck's ground fill
//! (`deck_render::render_deck` fills the frame with `theme::GROUND` before any
//! band draws) survives a guarded band unchanged. An empty scratch would blit
//! `Reset` cells back and the band would render on the terminal's own
//! background — invisible in a snapshot test, wrong on a light terminal.
//!
//! # What a caught panic can leave behind
//!
//! `catch_unwind` is entered through `AssertUnwindSafe`, and the deck's draw
//! closures really do capture `&mut DeckUi`. `UnwindSafe` is a lint, not a
//! memory-safety invariant — no arrangement of safe code here can produce UB —
//! so the question the assertion has to answer is the narrower, answerable
//! one: if a band panics halfway through, what is left in `DeckUi`, and does
//! the next frame recover from it?
//!
//! Every `&mut DeckUi` write reachable from `render_deck` falls into one of
//! three classes. The list is exhaustive as of this module's introduction; a
//! new view that adds a write outside classes 1–2 has to extend class 3 here.
//!
//! 1. **Measurements.** `ui.metrics.{session,trace,files_diff,help}_*`
//!    (`views::session`, `views::traces`, `views::files`,
//!    `deck_render::render_help`). Plain `usize`s, recomputed from scratch
//!    every frame and read only by the *next* keypress's scroll clamp. A
//!    missed write leaves the previous frame's measurement, which the next
//!    frame overwrites.
//!
//! 2. **Scroll and cursor clamps.** `ui.graph_cursor`, `ui.context_scroll`,
//!    `ui.inspect_scroll`, the skills preview's `scroll`, the MCP inspector's
//!    `scroll`, the installed panel's `created_scroll`. Every one is
//!    `x = x.min(max_measured_this_frame)` — idempotent, and re-applied on
//!    every frame the owning panel is drawn. A missed clamp leaves an offset
//!    that is at worst too large for one frame; `Paragraph::scroll` saturates
//!    past the content instead of panicking, and the next frame clamps again.
//!
//! 3. **Take-and-restore.** These are the only writes that could persist a
//!    *torn* value, so they are enumerated rather than summarised:
//!    - `ui.session_plan.take()` and its restore (`views::session::render`) —
//!      a pure memo of `FoldPlan::build` keyed on `FoldPlanKey`. A panic
//!      between the take and the restore loses the memo; the next frame's key
//!      lookup misses and rebuilds it. Cost: one whole-transcript walk.
//!    - `ui.session_pending_scroll.take()` (`views::session::render`) — one
//!      queued "scroll the selection into view" nudge. Losing it leaves the
//!      selection highlighted but possibly off-screen until the next selection
//!      move re-arms it. No state is inconsistent, one nudge is dropped.
//!    - `SessionFold::refresh` (`views::session`) — an *incremental* cache,
//!      and the one place where a torn value used to survive the frame. It
//!      committed its cache key **before** the fold loop, so a panic inside
//!      `entry_lines` left `prefix` extended with no matching `entry_rows`
//!      range and no `settled` bump — and the next frame, key still matching,
//!      resumed at the same index and double-appended those lines. `refresh`
//!      now commits the key only after the loop completes, so a panic leaves
//!      `key = None` and the next frame rebuilds the cache from zero.
//!    - `ui.inspect_view.take()` and its restore (`deck_render`) — deleted.
//!      The overlay reads the view through a shared borrow, which costs no
//!      clone (the reason the move existed) and cannot lose it.
//!
//! With those two changes, no `DeckUi` field can hold a value the next frame
//! does not either overwrite or rebuild. That is the recoverability claim the
//! `AssertUnwindSafe` rests on — recoverability, not soundness.
//!
//! # Three mechanical notes
//!
//! A guarded band costs one scratch copy in and one `blit` out per frame, so
//! the price is paid per *guarded rectangle*, not per frame. The deck's chrome
//! rows are one or two rows each; the content band is the real cost, and the
//! deck already walks the whole buffer twice per frame (`theme::apply_theme`,
//! `theme::degrade_buffer`) at the same 33 ms tick. Overlays are guarded only
//! on the frames they are actually up — including the startup notice, whose
//! visibility is asked at the call site rather than inside its renderer.
//!
//! [`crate::term::PanelBoundary`] is a `thread_local!`, and tokio tasks can
//! migrate between workers — but `terminal.draw(..)` is fully synchronous with
//! no `await` inside it, so the marker is entered and dropped on the same
//! thread. Nesting is safe: the guard restores the previous flag rather than
//! clearing it.
//!
//! Release builds set `panic = "abort"` (`[profile.release]`), where
//! `catch_unwind` never runs its handler — the process dies inside the panic
//! hook. Everything here is therefore effective in unwind (dev/test) builds
//! only, exactly as the single-session boundary always has been. See
//! [`crate::term`] for what the hook does on each side of that split.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

use crate::theme;

/// Widest error card painted over an overlay, in columns.
const CARD_W: u16 = 56;
/// Tallest error card painted over an overlay, in rows.
const CARD_H: u16 = 7;

/// Draw one band under the panic boundary. On a panic the error card fills
/// `area`, which is what a band wants: the band's whole rectangle is the thing
/// that failed to render.
pub(crate) fn guarded_band<F>(dst: &mut Buffer, area: Rect, label: &str, draw: F)
where
    F: FnOnce(&mut Buffer),
{
    guarded(dst, area, area, label, draw);
}

/// Draw a floating overlay under the panic boundary.
///
/// Overlays compute their own centred popup rect internally, so the guard is
/// taken over the whole frame; the scratch-is-a-copy discipline means the deck
/// underneath is preserved either way. On a panic the error card is a small
/// centred card rather than a full-frame wipe — the overlay is what broke, and
/// the deck behind it is still worth looking at while the user presses `esc`.
pub(crate) fn guarded_overlay<F>(dst: &mut Buffer, area: Rect, label: &str, draw: F)
where
    F: FnOnce(&mut Buffer),
{
    guarded(dst, area, centered(area), label, draw);
}

/// The boundary itself: draw into a copy of `area`, and on a panic paint the
/// error card into a *pristine* copy at `card` instead. Either way exactly one
/// infallible `blit` reaches the real frame buffer, after the band has fully
/// drawn or fully failed — a half-written band can never be shown.
fn guarded<F>(dst: &mut Buffer, area: Rect, card: Rect, label: &str, draw: F)
where
    F: FnOnce(&mut Buffer),
{
    if area.width == 0 || area.height == 0 {
        return;
    }
    let drawn = {
        // Tells the panic hook this panic is rendered in place, so it must
        // neither restore the terminal nor stash a report for the exit replay.
        let _boundary = crate::term::PanelBoundary::enter();
        let mut scratch = snapshot(dst, area);
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            #[cfg(test)]
            fail_if_armed(label);
            draw(&mut scratch);
            scratch
        }))
    };
    let buf = match drawn {
        Ok(buf) => buf,
        Err(payload) => {
            let mut buf = snapshot(dst, area);
            paint_error_card(&mut buf, card, label, &panic_message(&*payload));
            buf
        }
    };
    blit(dst, &buf, area);
}

/// A standalone buffer holding whatever `src` currently shows in `area`.
fn snapshot(src: &Buffer, area: Rect) -> Buffer {
    let mut out = Buffer::empty(area);
    blit(&mut out, src, area);
    out
}

/// Copy every cell of `src` in `area` into `dst`. Infallible: cells outside
/// either buffer are skipped rather than indexed.
fn blit(dst: &mut Buffer, src: &Buffer, area: Rect) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = src.cell((x, y)).cloned();
            if let (Some(cell), Some(slot)) = (cell, dst.cell_mut((x, y))) {
                *slot = cell;
            }
        }
    }
}

/// A card-sized rect centred in `area`, never larger than it — so the `Clear`
/// below always indexes inside the scratch buffer.
fn centered(area: Rect) -> Rect {
    let width = CARD_W.min(area.width);
    let height = CARD_H.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

/// Extract a human message from a panic payload.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panicked with a non-string payload".to_string()
    }
}

/// Paint a visible red error card standing in for a band that panicked.
/// `card` must lie inside `buf`'s area — [`Clear`] indexes rather than checks.
fn paint_error_card(buf: &mut Buffer, card: Rect, label: &str, message: &str) {
    if card.width == 0 || card.height == 0 {
        return;
    }
    Clear.render(card, buf);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme::DANGER))
        .title(format!(" ⚠ panel '{label}' panicked "));
    let body = format!("{message}\n\nthe rest of the TUI is still running");
    Paragraph::new(body)
        .block(block)
        .wrap(Wrap { trim: true })
        .style(Style::new().fg(theme::DANGER).add_modifier(Modifier::BOLD))
        .render(card, buf);
}

// The test-only injection seam.
//
// The boundary needs a witness that panics inside a *real* `render_deck` band,
// and no crafted `WorkspaceModel`/`DeckUi` produces one: every view indexes
// through `get`/`saturating_*`, and `tests/render_robustness.rs` already sweeps
// all nine tabs and every overlay across 121 geometries without a panic. So the
// panic is injected at the one place a view's panic would surface — inside the
// guarded closure, before the draw — and compiled out of every non-test build.
//
// It is one seam, not one per caller: `SessionFold::refresh` calls
// [`fail_if_armed`] from inside its fold loop too, which is how the "commit the
// cache key only after the loop" reordering gets a witness of its own.

#[cfg(test)]
thread_local! {
    static ARMED_PANIC: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
}

/// Arms the band labelled `label` to panic mid-draw until the returned guard
/// drops. Test-only; see the note above.
#[cfg(test)]
pub(crate) fn arm_panic(label: &'static str) -> ArmedPanic {
    ARMED_PANIC.with(|c| c.set(Some(label)));
    ArmedPanic
}

/// Disarms [`arm_panic`] on drop, so one test cannot leak an injection into
/// the next test sharing the thread.
#[cfg(test)]
pub(crate) struct ArmedPanic;

#[cfg(test)]
impl Drop for ArmedPanic {
    fn drop(&mut self) {
        ARMED_PANIC.with(|c| c.set(None));
    }
}

/// Panics if [`arm_panic`] armed this exact `label`. Test-only.
#[cfg(test)]
pub(crate) fn fail_if_armed(label: &str) {
    if ARMED_PANIC.with(std::cell::Cell::get) == Some(label) {
        panic!("injected panic in band {label}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn filled(w: u16, h: u16) -> Buffer {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        buf.set_style(area, Style::new().bg(Color::Rgb(1, 2, 3)));
        for y in 0..h {
            for x in 0..w {
                buf[(x, y)].set_symbol("·");
            }
        }
        buf
    }

    #[test]
    fn a_panicking_band_leaves_every_cell_outside_its_area_untouched() {
        // The blast radius of a caught panic is exactly `area` — this is what
        // makes "one bad view, one error card" true rather than aspirational.
        let mut buf = filled(50, 12);
        let before = buf.clone();
        let band = Rect::new(10, 3, 30, 6);
        guarded_band(&mut buf, band, "boom", |_| panic!("kaboom in a band"));
        for y in 0..12u16 {
            for x in 0..50u16 {
                if band.contains((x, y).into()) {
                    continue;
                }
                assert_eq!(
                    buf[(x, y)],
                    before[(x, y)],
                    "cell ({x},{y}) is outside the band and must be byte-identical"
                );
            }
        }
        let card: String = (3..9)
            .flat_map(|y| (10..40).map(move |x| (x, y)))
            .map(|p| buf[p].symbol().to_string())
            .collect();
        assert!(card.contains("panicked"), "error card is visible: {card}");
        assert!(card.contains("kaboom"), "carries the message: {card}");
    }

    #[test]
    fn a_band_that_draws_nothing_preserves_what_was_already_there() {
        // The scratch is a copy of the frame, not an empty buffer: a band that
        // paints only part of itself must not blit `Reset` cells back over the
        // deck's ground fill. An `Buffer::empty` scratch fails this.
        let mut buf = filled(20, 5);
        let before = buf.clone();
        guarded_band(&mut buf, Rect::new(0, 0, 20, 5), "quiet", |_| {});
        assert_eq!(buf, before, "a no-op draw is a no-op frame");
    }

    #[test]
    fn a_panicking_overlay_keeps_the_frame_around_its_card() {
        // Overlays are guarded over the whole frame (they own their popup rect
        // internally), so their error card must be a centred card — not a
        // full-frame wipe of the deck the user still needs to see.
        let mut buf = filled(80, 24);
        let area = Rect::new(0, 0, 80, 24);
        guarded_overlay(&mut buf, area, "help", |_| panic!("overlay blew up"));
        assert_eq!(
            buf[(0, 0)].symbol(),
            "·",
            "the frame outside the card survives an overlay panic"
        );
        let text: String = (0..24)
            .flat_map(|y| (0..80u16).map(move |x| (x, y)))
            .map(|p| buf[p].symbol().to_string())
            .collect();
        assert!(text.contains("panicked"), "error card is visible");
    }

    #[test]
    fn a_degenerate_band_is_a_no_op() {
        let mut buf = filled(10, 3);
        let before = buf.clone();
        guarded_band(&mut buf, Rect::new(0, 0, 0, 3), "zero", |_| {
            panic!("must never run")
        });
        assert_eq!(buf, before);
    }
}
