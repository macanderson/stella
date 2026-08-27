// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What the `/` palette remembers: the deck's half of SPEC 10's `recent`
//! section (#5048). The list itself is an input — the driver reads and writes
//! `.stella/private/palette-recents.json` and pushes it in over
//! `Inbound::PaletteRecents` — so all that happens here is noticing that a
//! command ran, updating the local copy so the section reorders on the
//! keystroke, and telling the driver.

use crate::deck_ui::DeckUi;
use crate::envelope::WorkspaceInput;

/// Record that a slash command ran here: locally, so the palette's `recent`
/// section reorders on this keystroke, and on the driver, which is the only
/// side that can make the entry outlive the session (#5048).
///
/// Taken in `deck_ui::submit_prompt` because that is the ONE place every
/// submission passes through, and "recent" means the commands you ran — not
/// the ones you happened to run a particular way. Two routes would otherwise
/// be missed:
///
/// - **Commands run with arguments.** `⇥` completes a row into the buffer and
///   the popup closes the moment a space is typed, so `/budget 5.00` submits
///   through the plain composer path and never reaches the popup's own
///   handler.
/// - **Commands typed out in full**, which never opened the popup at all.
///
/// And a history folded from the *prompt stream* instead would miss a third
/// of the vocabulary in the other direction: `/files`, `/diff`, `/graph` and
/// the rest of the tab switches are consumed deck-side by [`super::local`]
/// and never leave for the driver.
///
/// Only a name the current vocabulary actually offers is recorded. A prompt
/// that merely opens with a slash — `/tmp/foo.rs is broken` — is prose, and
/// letting it in would spend a five-entry history on names no row can ever
/// match.
///
/// The input to the driver is advisory: it reorders a menu and does nothing
/// else, so a driver that drops it costs an ordering, never an action.
pub(super) fn remember_command_run(ui: &mut DeckUi, text: &str) {
    // The name alone, never the typed line — `/budget 5.00` is a run of
    // `/budget`, and one entry per argument would spend the whole history on
    // one command.
    let name = text.split_whitespace().next().unwrap_or_default();
    if !ui.slash_commands.iter().any(|c| c.name == name) {
        return;
    }
    crate::composer::palette::remember(&mut ui.palette_recent, name);
    ui.pending_inputs.push(WorkspaceInput::PaletteRan {
        name: name.to_string(),
    });
}
