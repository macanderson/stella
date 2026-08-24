// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Drawing for the **parked asks** — the question overlay (#4220) and the
//! approval card (#4240), the two surfaces that each hold a live tool call
//! open while they are up.
//!
//! The render counterpart to `deck_ui::parked`, and its own module
//! for the same reason: `deck_render.rs` is a god file closed to growth
//! (`scripts/file-size-baseline.txt`).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::deck_ui::DeckUi;
use crate::views;

/// Draw whichever parked asks are up.
///
/// Called above the floating cards and below help — a driver may need to
/// read what a key does before answering, but nothing else may cover an
/// overlay a turn is stopped on.
///
/// **The approval card draws last, so it lands on top.** That mirrors
/// `deck_ui::parked::handle_key`, which gives it the keyboard first: a card
/// taking the keys while sitting *under* another card would leave the driver
/// typing into something they cannot see.
pub(super) fn render(ui: &DeckUi, area: Rect, buf: &mut Buffer) {
    if ui.question.is_open() {
        super::guarded_overlay(buf, area, "question", |b| {
            crate::v2::question::render(&ui.question, ui.accessible, area, b)
        });
    }
    if ui.approval.is_open() {
        super::guarded_overlay(buf, area, "approval", |b| {
            crate::v2::approval::render(&ui.approval, ui.accessible, area, b)
        });
    }
}

/// Whether either parked ask is up, and so owns the keyboard.
///
/// Read by `render_deck`'s cursor-suppression rule: the hardware caret must
/// not sit in the composer while the driver is answering a card.
pub(super) fn owns_keyboard(ui: &DeckUi) -> bool {
    ui.question.is_open() || ui.approval.is_open()
}
