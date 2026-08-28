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
use stella_tui::voice::{VoiceCmd, VoiceMode};

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
    ui.voice.typed_space(start, true);
    let mut t = start;
    let mut started = false;
    while t < model.now_ms {
        t += 50;
        // The same routing the shell applies: once recording, a space event
        // is swallowed rather than typed — in hold mode that reads as "still
        // held".
        if ui.voice.swallows_space() {
            ui.voice.swallowed_space(t);
        } else {
            ui.voice.typed_space(t, false);
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

/// The same row in tap mode, which has to say a different thing: nothing is
/// being held, so "release to stop" would be advice a person cannot follow.
///
/// A second golden rather than an assertion on the string, for the reason the
/// parent's module docs give — a `contains` check cannot see a row that moved
/// or a column that shifted.
#[test]
fn deck_render_snapshots_pin_the_tap_mode_recording_row() {
    let model = fixture_model();
    let mut ui = ui_for(DeckTab::Session);
    ui.voice.enabled = true;
    ui.voice.mode = VoiceMode::Tap;
    // One tap on an empty composer, 2s before the fixture clock's "now": the
    // capture opens on the space itself, with no warmup to run through.
    let start = model.now_ms - 2_000;
    assert!(matches!(
        ui.voice.typed_space(start, true),
        VoiceCmd::Start { retract: 1 }
    ));
    // The tick clock runs on with no repeats arriving: hold mode would have
    // called that a release, and tap mode keeps recording.
    let mut t = start;
    while t < model.now_ms {
        t += 50;
        assert_eq!(ui.voice.tick(t), VoiceCmd::None);
    }
    assert!(ui.voice.recording(), "the frame is of a live recording");

    let frame = render_frame(&model, &mut ui, W, H);
    assert_golden(
        "session_voice_recording_tap",
        "the SESSION tab while tap-mode dictation is listening",
        W,
        H,
        &frame,
    );
}
