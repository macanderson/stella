// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The push-to-talk golden: the SESSION tab while a dictation is listening.
//!
//! A submodule of `deck_render_snapshots` rather than more lines in it, like
//! [`super::mcp`]: the parent sits at the 1500-line ceiling.
//!
//! The voice line claims the air row above the composer while the microphone
//! is live (`stella_tui::voice`), and SESSION is where that matters — the
//! pulse otherwise leaves the row blank there, so without this frame nothing
//! pins that a recording is visible on the tab the dictating user is looking
//! at. What the golden cannot pin is the caret's colour change (styling is
//! stripped — see the parent's module docs); `deck_render`'s
//! `the_caret_recolours_while_the_microphone_is_live` holds that half.

use stella_tui::deck::DeckTab;
use stella_tui::voice::VoiceCmd;

use super::{H, W, assert_golden, fixture_model, render_frame, ui_for};

#[test]
fn deck_render_snapshots_pin_the_voice_recording_row() {
    let model = fixture_model();
    let mut ui = ui_for(DeckTab::Session);
    ui.voice.enabled = true;
    // Drive the machine through its own API — the phase is private, so no
    // fixture can fake a recording the machine would refuse. The hold starts
    // 2s before the fixture clock's "now": press, repeats every 50ms, the
    // warmup completes mid-run, and the frame renders at `model.now_ms`, so
    // the elapsed time it shows is fixture data.
    let start = model.now_ms - 2_000;
    ui.voice.typed_space(start);
    let mut t = start;
    let mut started = false;
    while t < model.now_ms {
        t += 50;
        // The same routing the shell applies: once recording, a space event
        // is "still held" rather than a character.
        if ui.voice.swallows_space() {
            ui.voice.space_repeat(t);
        } else {
            ui.voice.typed_space(t);
        }
        if matches!(ui.voice.tick(t), VoiceCmd::Start { .. }) {
            started = true;
        }
    }
    assert!(started, "the hold must cross the warmup");
    assert!(ui.voice.recording(), "the frame is of a live recording");

    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "session_voice_recording",
        "the SESSION tab while push-to-talk is listening",
        W,
        H,
        &frame,
    );
}
