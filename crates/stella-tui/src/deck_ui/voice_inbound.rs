// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The three dictation envelopes, folded into the deck's UI state.
//!
//! `crate::voice` is the pure decision half and stays that way — it never
//! learns what a composer or a notice is. This is the other side of that
//! line: the driver's answers arrive as [`Inbound`] and have to *do*
//! something to a [`DeckUi`], so the doing lives here.
//!
//! A sibling file rather than more arms in `deck_ui.rs`, which is a
//! grandfathered god file closed to growth (AGENTS.md § "God files").

use crate::deck_ui::DeckUi;
use crate::envelope::Inbound;

/// Fold a dictation envelope. `true` means it was one and the caller is done
/// with it — the same "recognized and handled" contract the rest of
/// `ingest_inbound`'s chain uses.
pub(super) fn ingest(inbound: &Inbound, ui: &mut DeckUi) -> bool {
    match inbound {
        // A dictation's answer is keyboard input by another route: it lands
        // in the composer at the cursor, never in the record.
        Inbound::VoiceTranscript { text } => {
            ui.voice.settled();
            ui.paste(text);
            true
        }
        // No text to paste — say why instead, as a transient notice.
        Inbound::VoiceFailed { reason } => {
            ui.voice.settled();
            ui.notice.push(reason.clone());
            true
        }
        // `/voice` switched dictation on, off, or between gestures and has
        // already persisted it. Any gesture in flight is returned to rest
        // first: the new mode reads the spacebar differently, and a capture
        // folded half under one set of rules and half under the other has no
        // key left that ends it.
        Inbound::VoiceConfig { enabled, mode } => {
            ui.voice.settled();
            ui.voice.enabled = *enabled;
            ui.voice.mode = *mode;
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::{VoiceCmd, VoiceMode};

    /// **Witness (#5347).** `/voice tap` takes effect in the session that ran
    /// it, not only in the next one.
    ///
    /// Without the envelope the deck keeps the enablement and mode it read at
    /// startup, so the command reports success and the spacebar keeps doing
    /// the old thing — which is the opposite of "first use is one command".
    #[test]
    fn a_voice_config_envelope_rebinds_the_gesture_in_this_session() {
        let mut ui = DeckUi::default();
        assert!(!ui.voice.enabled, "dictation ships off");
        assert_eq!(ui.voice.mode, VoiceMode::Hold);

        assert!(ingest(
            &Inbound::VoiceConfig {
                enabled: true,
                mode: VoiceMode::Tap,
            },
            &mut ui,
        ));
        assert!(ui.voice.enabled);
        assert_eq!(ui.voice.mode, VoiceMode::Tap);

        // And the machine is live in the new mode straight away: one space on
        // an empty composer opens a capture.
        assert_eq!(
            ui.voice.typed_space(0, true),
            VoiceCmd::Start { retract: 1 }
        );
    }

    /// Switching mode mid-gesture returns the machine to rest. A capture
    /// started under hold-mode rules has no stop key once tap mode is bound,
    /// so leaving it running would wedge dictation for the session.
    #[test]
    fn switching_mode_mid_capture_returns_the_machine_to_rest() {
        let mut ui = DeckUi::default();
        ui.voice.enabled = true;
        ui.voice.mode = VoiceMode::Tap;
        ui.voice.typed_space(0, true);
        assert!(ui.voice.recording());

        ingest(
            &Inbound::VoiceConfig {
                enabled: true,
                mode: VoiceMode::Hold,
            },
            &mut ui,
        );
        assert!(!ui.voice.recording());
        assert_eq!(ui.voice.mode, VoiceMode::Hold);
    }

    #[test]
    fn a_non_voice_envelope_is_handed_back() {
        let mut ui = DeckUi::default();
        assert!(!ingest(&Inbound::ShowHelp, &mut ui));
    }
}
