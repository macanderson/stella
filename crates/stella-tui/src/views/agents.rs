// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! AGENTS tab — the agents installed on disk at the user (`~/.stella/agents`)
//! and project (`.stella/agents`) levels, and nothing else.
//!
//! It used to be two panes behind a secondary nav: an `htop`-style executions
//! dashboard of the live lanes, and this list. The dashboard is gone (owner's
//! call, 2026-08-22): the live lanes are the SESSION tab's nested rows and the
//! status bar, and a tab named AGENTS that opened on a process table was the
//! wrong first screen for "which agents do I have". The list, its editor,
//! its create flow and its version picker are [`crate::views::installed`].
//!
//! What the dashboard carried that has no home yet — stop / pause / restart
//! of a worker lane by key, and focusing a worker's transcript — is tracked
//! in #4334.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::deck::WorkspaceModel;
use crate::deck_ui::DeckUi;

pub fn render(model: &WorkspaceModel, ui: &mut DeckUi, area: Rect, buf: &mut Buffer) {
    crate::views::installed::render(ui, model.now_ms, area, buf);
}

/// Bytes → `"212M"` style, binary (1024) units, whole numbers only.
pub(crate) fn humanize_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut val = bytes as f64;
    let mut unit = 0;
    while val >= 1024.0 && unit < UNITS.len() - 1 {
        val /= 1024.0;
        unit += 1;
    }
    format!("{val:.0}{}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_bytes_rounds_to_whole_binary_units() {
        assert_eq!(humanize_bytes(0), "0B");
        assert_eq!(humanize_bytes(1023), "1023B");
        assert_eq!(humanize_bytes(1024), "1K");
        assert_eq!(humanize_bytes(148 * 1024 * 1024), "148M");
    }
}
