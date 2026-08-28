//! Push-to-talk dictation: the pure spacebar state machine.
//!
//! Hold Space in the composer and, after a warmup, the deck records the
//! microphone and pastes the transcript at the cursor (ADR 0020); or, in
//! [`VoiceMode::Tap`], tap Space on an empty composer to start and tap again
//! to stop. Both modes enter the same [`VoicePhase::Listening`] and leave it
//! the same way, so everything downstream of the gesture — the caret colour,
//! the status line, Esc, the transcript paste — is written once. This module
//! is only the *decision* half: a fold over observed key events and the deck's
//! ~30fps tick clock, held on [`crate::deck_ui::DeckUi`] like every other
//! interaction state. Capture and transcription live on the driver side of
//! the wire ([`crate::envelope::WorkspaceInput::VoiceStart`] out,
//! [`crate::envelope::Inbound::VoiceTranscript`] back) — this crate never
//! touches a microphone or the network.
//!
//! ## Why the machine consumes *observations*, not raw keys
//!
//! A bare space is a hotkey in half the deck (page-down in lists, a toggle on
//! SKILLS, a hunk mark under a pending gate) and a printable character in the
//! composer. Predicting which one a given press will be means re-implementing
//! `handle_key_inner`'s precedence chain here, which would drift. Instead the
//! shell dispatches the key normally and then reports what actually happened:
//! [`VoiceUi::typed_space`] fires only when the composer really grew by one
//! space at the cursor. A held space on a tab where space pages simply never
//! arms, and the retraction count is exact by construction — the spaces to
//! remove are precisely the ones observed in.
//!
//! ## How release is detected
//!
//! Best case the terminal reports key release (`TerminalGuard::enter` asks
//! for `REPORT_EVENT_TYPES`) and [`VoiceUi::space_release`] ends the hold on
//! the release event itself. Every other terminal ends it when the OS
//! key-repeat stream goes quiet: while a key is held the OS delivers repeats
//! every few tens of milliseconds, so a gap longer than [`LISTENING_GAP_MS`]
//! means the key is up. That fallback requires OS key-repeat to be enabled.
//!
//! ## Why tap mode exists
//!
//! With repeat disabled *and* no release reporting, a hold cannot be told
//! from a tap by any signal this machine receives, so hold mode can never
//! arm and dictation is unreachable. [`VoiceMode::Tap`] is the way in that
//! depends on neither: one observed space starts the capture and the next
//! one ends it, which every terminal can deliver. It is chosen by name
//! (`/voice tap`, persisted as `voice.mode`), never inferred — the two modes
//! read the same key, so guessing wrong would make Space do the other thing
//! at the moment a person is trying to type.
//!
//! Tap mode arms only from an **empty composer**. A space is a word gap far
//! more often than it is a gesture, and an empty composer is what separates
//! "I am starting to dictate" from "I am typing" with no warmup to wait
//! through.

/// How long Space must be held before recording starts. Long enough that
/// every ordinary use of the key — a word gap, a quick run of indentation —
/// ends first; short enough not to feel like a wait. A tap, or a hold
/// released inside the warmup, types exactly the spaces it typed.
pub const WARMUP_MS: u64 = 1_500;

/// The widest silence the *arming* phase survives between space insertions.
/// It must cover the OS's initial key-repeat delay — the one long gap between
/// the first press and the first repeat, up to ~700ms on a slow-repeat
/// setting — or a genuine hold would be dismissed as a tap before its first
/// repeat arrived.
pub const ARMING_GAP_MS: u64 = 750;

/// The repeat-stream gap that means "released" while listening, on terminals
/// that do not report key release. Inter-repeat gaps are a few tens of
/// milliseconds once repeat is underway, so this trades ~a third of a second
/// of stop latency for immunity to scheduling jitter.
pub const LISTENING_GAP_MS: u64 = 350;

/// The hard cap on one recording, matching Claude Code's two minutes. The
/// tick arm stops the capture at the cap even if the key is somehow still
/// reading as held (a stuck key, a terminal that stopped repeating).
pub const MAX_HOLD_MS: u64 = 120_000;

/// The longest the deck waits for the driver to answer a finished capture
/// before releasing the gesture.
///
/// Every other phase here defends itself with a deadline — [`ARMING_GAP_MS`],
/// [`LISTENING_GAP_MS`], [`MAX_HOLD_MS`]. [`VoicePhase::Transcribing`] had
/// none, and it is the one phase whose exit belongs to *another crate*: only
/// `Inbound::VoiceTranscript` / `VoiceFailed` clear it. `cancel` refuses
/// (it acts only while [`VoiceUi::recording`]) and a fresh space press is a
/// no-op, so an answer that never arrives disabled dictation for the rest of
/// the session, showing `◌ transcribing…` forever.
///
/// Set **above** the driver's own 60s HTTP timeout
/// (`command_deck::voice::transcribe`): whenever that call can fail on its
/// own it answers with a reason, and the reason is worth more than this
/// release. This covers what the request timeout cannot — a capture whose
/// blocking `finish()` never returns on a wedged audio device, and any future
/// path that forgets to answer.
pub const TRANSCRIBE_WAIT_MS: u64 = 90_000;

/// The timeout `command_deck::voice::transcribe` builds its client with.
const DRIVER_HTTP_TIMEOUT_MS: u64 = 60_000;

/// Releasing before the driver's own request timeout would replace a real
/// failure reason with silence. A build error rather than a test, because
/// tuning either number without reading the other is the whole hazard.
const _: () = assert!(TRANSCRIBE_WAIT_MS > DRIVER_HTTP_TIMEOUT_MS);

/// The shortest a [`VoiceMode::Tap`] capture can be, and the window in which
/// a space cannot end the capture it just started.
///
/// Tap mode reads one key event as "start" and the next as "stop", which is
/// exactly what a *held* space looks like once OS key-repeat delivers its
/// first repeat. There is no signal that separates the two — that is the
/// premise tap mode exists under — so the floor is a clock instead: it is set
/// at [`ARMING_GAP_MS`], which is already this module's measure of the
/// slowest initial key-repeat delay a terminal will hand us. A space held in
/// tap mode therefore records for this long and stops, rather than opening
/// and closing a capture on one keypress and posting silence to a
/// transcription provider.
///
/// The cost is that a double-tap faster than this does not stop the capture.
/// Nobody dictates for under three quarters of a second, and the status line
/// says the recording is live throughout.
pub const TAP_MIN_MS: u64 = ARMING_GAP_MS;

/// Which gesture starts a dictation. Both modes record the same way and end
/// in the same [`VoicePhase`]s; they differ only in what begins and ends the
/// capture.
///
/// Chosen by name and persisted (`voice.mode`, `/voice hold` / `/voice tap`),
/// never inferred from terminal capabilities: both modes read Space, so a
/// wrong guess makes the spacebar do the other thing without being asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VoiceMode {
    /// Hold Space through the warmup to record; release to stop. Needs
    /// either release reporting or OS key-repeat (see the module docs).
    #[default]
    Hold,
    /// Tap Space on an empty composer to record; tap again to stop. Needs
    /// neither, which is the point.
    Tap,
}

impl VoiceMode {
    /// Every mode, in the order `/voice` lists them.
    pub const ALL: &'static [VoiceMode] = &[VoiceMode::Hold, VoiceMode::Tap];

    /// The persisted spelling (`voice.mode`) and the `/voice` argument.
    pub fn slug(self) -> &'static str {
        match self {
            VoiceMode::Hold => "hold",
            VoiceMode::Tap => "tap",
        }
    }

    /// Parse a persisted or typed mode. Case-insensitive and space-trimmed;
    /// `None` for anything else, so an unreadable `voice.mode` falls back to
    /// the default rather than disabling dictation.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "hold" => Some(VoiceMode::Hold),
            "tap" => Some(VoiceMode::Tap),
            _ => None,
        }
    }
}

/// Where the dictation gesture currently stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VoicePhase {
    /// No gesture in progress.
    #[default]
    Idle,
    /// Space is (apparently) held, warmup running. The spaces it has typed
    /// so far stay in the composer — a hold abandoned here was just typing.
    Arming {
        /// When the first space of this run was observed.
        since_ms: u64,
        /// When the most recent space was observed (the gap clock).
        last_ms: u64,
        /// How many spaces this run has inserted — the retraction count if
        /// the warmup completes.
        typed: usize,
    },
    /// Recording. Space events are swallowed; release (or a quiet repeat
    /// stream) ends it.
    Listening { since_ms: u64, last_ms: u64 },
    /// Capture ended; the driver is transcribing. The composer works
    /// normally — the transcript pastes at the cursor when it arrives.
    Transcribing { since_ms: u64 },
}

/// What the shell must do about a transition, beyond updating the phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceCmd {
    /// Nothing — the common case.
    None,
    /// Warmup crossed: remove the `retract` warmup spaces from the composer
    /// and send [`crate::envelope::WorkspaceInput::VoiceStart`].
    Start { retract: usize },
    /// The hold ended: send [`crate::envelope::WorkspaceInput::VoiceStop`].
    Stop,
    /// Esc while listening: send
    /// [`crate::envelope::WorkspaceInput::VoiceCancel`].
    Cancel,
}

/// The deck's dictation state: capability bits set once at startup, and the
/// phase the events below fold.
#[derive(Debug, Clone, Default)]
pub struct VoiceUi {
    /// `voice.enabled` from settings, threaded in via `DeckOptions`. Off by
    /// default: holding a key is too easy to do by accident for it to stream
    /// microphone audio to a provider unasked (ADR 0020).
    pub enabled: bool,
    /// Whether the terminal reports key release (the kitty keyboard protocol
    /// was pushed — `TerminalGuard::kitty`). With it, release ends the hold
    /// on the event; without it, the repeat-gap fallback does. Inert in
    /// [`VoiceMode::Tap`], which ends a capture on the next space instead.
    pub release_events: bool,
    /// `voice.mode` from settings, threaded in via `DeckOptions` and
    /// switchable mid-session by `/voice` ([`crate::envelope::Inbound::VoiceConfig`]).
    pub mode: VoiceMode,
    phase: VoicePhase,
}

impl VoiceUi {
    pub fn phase(&self) -> VoicePhase {
        self.phase
    }

    /// Whether the microphone is (or should be) live — the caret recolours
    /// on exactly this.
    pub fn recording(&self) -> bool {
        matches!(self.phase, VoicePhase::Listening { .. })
    }

    /// Whether the shell should swallow plain-space key events instead of
    /// dispatching them (recording: a repeat is "still held", not a
    /// character).
    pub fn swallows_space(&self) -> bool {
        self.recording()
    }

    /// Whether Esc currently means "abandon the recording".
    pub fn esc_cancels(&self) -> bool {
        self.recording()
    }

    /// A plain space was dispatched normally and the composer really grew by
    /// one space at the cursor. `composer_was_empty` reports whether that
    /// space landed into an empty composer — the shell knows, and only the
    /// shell can know, because dispatch is what decides where a key goes.
    ///
    /// In [`VoiceMode::Hold`] this starts or extends the arming run and the
    /// warmup completes on [`Self::tick`], so it returns nothing. In
    /// [`VoiceMode::Tap`] it *is* the start: there is no warmup to wait
    /// through, so the capture opens here and the one space it typed is
    /// retracted immediately.
    pub fn typed_space(&mut self, now_ms: u64, composer_was_empty: bool) -> VoiceCmd {
        if !self.enabled {
            return VoiceCmd::None;
        }
        if self.mode == VoiceMode::Tap {
            // Only from rest and only from an empty composer: mid-sentence a
            // space is a word gap, and a gesture that fires there would make
            // the spacebar unusable for typing.
            if self.phase == VoicePhase::Idle && composer_was_empty {
                self.phase = VoicePhase::Listening {
                    since_ms: now_ms,
                    last_ms: now_ms,
                };
                return VoiceCmd::Start { retract: 1 };
            }
            return VoiceCmd::None;
        }
        self.phase = match self.phase {
            VoicePhase::Idle => VoicePhase::Arming {
                since_ms: now_ms,
                last_ms: now_ms,
                typed: 1,
            },
            VoicePhase::Arming {
                since_ms, typed, ..
            } => VoicePhase::Arming {
                since_ms,
                last_ms: now_ms,
                typed: typed + 1,
            },
            other => other,
        };
        VoiceCmd::None
    }

    /// A plain space arrived while recording, so the shell swallowed it
    /// rather than typing it ([`Self::swallows_space`]).
    ///
    /// The two modes read the same event as opposite things, which is why
    /// this is named for what was *observed* rather than for either reading:
    /// under [`VoiceMode::Hold`] it is a key-repeat saying the key is still
    /// down, and under [`VoiceMode::Tap`] it is the second tap, ending the
    /// capture once [`TAP_MIN_MS`] has passed.
    pub fn swallowed_space(&mut self, now_ms: u64) -> VoiceCmd {
        let VoicePhase::Listening { since_ms, .. } = self.phase else {
            return VoiceCmd::None;
        };
        if self.mode == VoiceMode::Tap {
            if now_ms.saturating_sub(since_ms) >= TAP_MIN_MS {
                self.phase = VoicePhase::Transcribing { since_ms: now_ms };
                return VoiceCmd::Stop;
            }
            return VoiceCmd::None;
        }
        self.phase = VoicePhase::Listening {
            since_ms,
            last_ms: now_ms,
        };
        VoiceCmd::None
    }

    /// The terminal reported Space released (only ever arrives when
    /// [`Self::release_events`]).
    ///
    /// Inert in [`VoiceMode::Tap`]: the release being reported there is the
    /// release of the tap that *started* the capture, and ending on it would
    /// collapse tap mode back into hold mode on exactly the terminals that
    /// can report releases.
    pub fn space_release(&mut self, now_ms: u64) -> VoiceCmd {
        if self.mode == VoiceMode::Tap {
            return VoiceCmd::None;
        }
        match self.phase {
            // Released inside the warmup: it was a tap (or a short hold);
            // the spaces it typed stay typed.
            VoicePhase::Arming { .. } => {
                self.phase = VoicePhase::Idle;
                VoiceCmd::None
            }
            VoicePhase::Listening { .. } => {
                self.phase = VoicePhase::Transcribing { since_ms: now_ms };
                VoiceCmd::Stop
            }
            _ => VoiceCmd::None,
        }
    }

    /// Any non-space key (or a paste) landed while arming: the user is
    /// typing, not holding. The spaces already typed stay typed.
    pub fn interrupt(&mut self) {
        if matches!(self.phase, VoicePhase::Arming { .. }) {
            self.phase = VoicePhase::Idle;
        }
    }

    /// Esc while recording: abandon the capture without transcribing.
    pub fn cancel(&mut self) -> VoiceCmd {
        if self.recording() {
            self.phase = VoicePhase::Idle;
            VoiceCmd::Cancel
        } else {
            VoiceCmd::None
        }
    }

    /// The deck's heartbeat (~30fps): where the warmup completes and where a
    /// quiet repeat stream (or the [`MAX_HOLD_MS`] cap) ends a recording.
    pub fn tick(&mut self, now_ms: u64) -> VoiceCmd {
        match self.phase {
            VoicePhase::Arming {
                since_ms,
                last_ms,
                typed,
            } => {
                if now_ms.saturating_sub(last_ms) > ARMING_GAP_MS {
                    // The stream went quiet before the warmup: a tap.
                    self.phase = VoicePhase::Idle;
                    VoiceCmd::None
                } else if now_ms.saturating_sub(since_ms) >= WARMUP_MS {
                    self.phase = VoicePhase::Listening {
                        since_ms: now_ms,
                        last_ms: now_ms,
                    };
                    VoiceCmd::Start { retract: typed }
                } else {
                    VoiceCmd::None
                }
            }
            VoicePhase::Listening { since_ms, last_ms } => {
                // Hold mode only. A tap-mode capture has no repeat stream to
                // go quiet — nothing is being held — so this rule would end
                // every tap recording a third of a second after it started.
                let released = self.mode == VoiceMode::Hold
                    && !self.release_events
                    && now_ms.saturating_sub(last_ms) > LISTENING_GAP_MS;
                if released || now_ms.saturating_sub(since_ms) >= MAX_HOLD_MS {
                    self.phase = VoicePhase::Transcribing { since_ms: now_ms };
                    VoiceCmd::Stop
                } else {
                    VoiceCmd::None
                }
            }
            // The driver owes an answer here; this is the deadline for it.
            // Releasing to Idle restores the gesture rather than reporting a
            // failure the deck cannot describe — the transcript is already
            // lost, and a feature that works again is the recoverable half.
            VoicePhase::Transcribing { since_ms } => {
                if now_ms.saturating_sub(since_ms) >= TRANSCRIBE_WAIT_MS {
                    self.phase = VoicePhase::Idle;
                }
                VoiceCmd::None
            }
            _ => VoiceCmd::None,
        }
    }

    /// The driver answered ([`crate::envelope::Inbound::VoiceTranscript`] or
    /// [`crate::envelope::Inbound::VoiceFailed`]): the gesture is over,
    /// whatever phase it was in — a failed start can answer while still
    /// listening.
    pub fn settled(&mut self) {
        self.phase = VoicePhase::Idle;
    }

    /// The pulse-row line for an in-progress gesture, or `None` at rest.
    /// Deterministic over `(mode, phase, now_ms)` — the goldens render it.
    pub fn status_line(&self, now_ms: u64) -> Option<String> {
        match self.phase {
            VoicePhase::Idle => None,
            VoicePhase::Arming { .. } => Some("◌ keep holding to dictate…".to_string()),
            VoicePhase::Listening { since_ms, .. } => {
                let s = now_ms.saturating_sub(since_ms) / 1000;
                let stop = match self.mode {
                    VoiceMode::Hold => "release to stop",
                    VoiceMode::Tap => "tap space to stop",
                };
                Some(format!(
                    "● listening {}:{:02} · {stop} · esc cancels",
                    s / 60,
                    s % 60
                ))
            }
            VoicePhase::Transcribing { .. } => Some("◌ transcribing…".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    /// **The witness.** An answer that never arrives releases the gesture
    /// instead of disabling dictation for the session.
    ///
    /// `Transcribing` was the one phase with no deadline, and the one whose
    /// exit belongs to another crate. `cancel` acts only while `recording()`,
    /// and a fresh space press maps `Transcribing -> Transcribing`, so before
    /// this there was no way out of it from inside the deck.
    #[test]
    fn a_transcription_that_never_answers_releases_the_gesture() {
        let mut v = enabled();
        // Hold through warmup into a recording, then stop.
        assert!(matches!(
            hold_through_warmup(&mut v),
            VoiceCmd::Start { .. }
        ));
        assert!(v.recording());
        // The stream goes quiet: the listening gap ends the recording.
        let stopped_at = WARMUP_MS + LISTENING_GAP_MS + 100;
        assert_eq!(v.tick(stopped_at), VoiceCmd::Stop);
        assert!(matches!(v.phase(), VoicePhase::Transcribing { .. }));
        let waiting_since = stopped_at;

        // The driver says nothing. Just under the deadline the deck still waits
        // — the driver's own 60s failure must win when it can produce a reason.
        v.tick(waiting_since + TRANSCRIBE_WAIT_MS - 1);
        assert!(
            matches!(v.phase(), VoicePhase::Transcribing { .. }),
            "the deck must not give up before the driver's own timeout can answer"
        );

        // Past it, the gesture is released and dictation works again.
        v.tick(waiting_since + TRANSCRIBE_WAIT_MS);
        assert_eq!(v.phase(), VoicePhase::Idle);
        assert_eq!(v.status_line(waiting_since + TRANSCRIBE_WAIT_MS), None);

        // And a new gesture can start, which is the whole point.
        v.typed_space(waiting_since + TRANSCRIBE_WAIT_MS, true);
        assert!(matches!(v.phase(), VoicePhase::Arming { .. }));
    }

    use super::*;

    fn armed(v: &mut VoiceUi, start_ms: u64) {
        v.typed_space(start_ms, true);
        assert!(matches!(v.phase(), VoicePhase::Arming { .. }));
    }

    fn hold_through_warmup(v: &mut VoiceUi) -> VoiceCmd {
        // A held key: press at t=0, repeats every 50ms past the warmup.
        armed(v, 0);
        let mut typed = 1;
        let mut t = 0;
        while t < WARMUP_MS {
            t += 50;
            // Only the first space of the run met an empty composer; hold
            // mode ignores the flag either way.
            v.typed_space(t, false);
            typed += 1;
            let cmd = v.tick(t);
            if let VoiceCmd::Start { retract } = cmd {
                assert_eq!(retract, typed, "every warmup space is retracted");
                return cmd;
            }
        }
        panic!("warmup never completed: {:?}", v.phase());
    }

    fn enabled() -> VoiceUi {
        VoiceUi {
            enabled: true,
            ..VoiceUi::default()
        }
    }

    #[test]
    fn disabled_never_arms() {
        let mut v = VoiceUi::default();
        assert_eq!(v.typed_space(0, true), VoiceCmd::None);
        assert_eq!(v.phase(), VoicePhase::Idle);
    }

    #[test]
    fn a_tap_stays_a_tap() {
        let mut v = enabled();
        armed(&mut v, 0);
        // No repeats follow; the gap clock dismisses the run.
        assert_eq!(v.tick(ARMING_GAP_MS + 1), VoiceCmd::None);
        assert_eq!(v.phase(), VoicePhase::Idle);
    }

    #[test]
    fn typing_a_word_after_spaces_aborts_the_arming() {
        let mut v = enabled();
        armed(&mut v, 0);
        v.typed_space(100, false);
        v.interrupt(); // any other key
        assert_eq!(v.phase(), VoicePhase::Idle);
    }

    #[test]
    fn a_hold_crosses_the_warmup_and_retracts_exactly_what_it_typed() {
        let mut v = enabled();
        let cmd = hold_through_warmup(&mut v);
        assert!(matches!(cmd, VoiceCmd::Start { .. }));
        assert!(v.recording());
        assert!(v.swallows_space());
    }

    #[test]
    fn a_slow_initial_repeat_delay_survives_the_arming_gap() {
        // Press, then nothing for 700ms (a slow OS initial delay), then
        // repeats: the run must still be alive when the first repeat lands.
        let mut v = enabled();
        armed(&mut v, 0);
        assert_eq!(v.tick(700), VoiceCmd::None);
        assert!(
            matches!(v.phase(), VoicePhase::Arming { .. }),
            "700ms without a repeat is inside ARMING_GAP_MS"
        );
        v.typed_space(700, false);
        let mut t = 700;
        loop {
            t += 50;
            v.typed_space(t, false);
            if let VoiceCmd::Start { .. } = v.tick(t) {
                break;
            }
            assert!(t < 3 * WARMUP_MS, "warmup must complete");
        }
        assert!(v.recording());
    }

    #[test]
    fn a_quiet_repeat_stream_stops_the_recording_without_release_events() {
        let mut v = enabled();
        hold_through_warmup(&mut v);
        let VoicePhase::Listening { last_ms, .. } = v.phase() else {
            panic!("recording");
        };
        assert_eq!(v.tick(last_ms + LISTENING_GAP_MS), VoiceCmd::None);
        assert_eq!(v.tick(last_ms + LISTENING_GAP_MS + 1), VoiceCmd::Stop);
        assert!(matches!(v.phase(), VoicePhase::Transcribing { .. }));
    }

    #[test]
    fn with_release_events_the_release_stops_it_and_the_gap_does_not() {
        let mut v = enabled();
        v.release_events = true;
        hold_through_warmup(&mut v);
        let VoicePhase::Listening { last_ms, .. } = v.phase() else {
            panic!("recording");
        };
        // The gap clock is inert: a terminal that reports releases may
        // legitimately stop repeating (some do while other keys interleave).
        assert_eq!(v.tick(last_ms + 10 * LISTENING_GAP_MS), VoiceCmd::None);
        assert!(v.recording());
        assert_eq!(
            v.space_release(last_ms + 10 * LISTENING_GAP_MS + 1),
            VoiceCmd::Stop
        );
        assert!(matches!(v.phase(), VoicePhase::Transcribing { .. }));
    }

    #[test]
    fn a_release_inside_the_warmup_is_a_tap_that_keeps_its_spaces() {
        let mut v = enabled();
        v.release_events = true;
        armed(&mut v, 0);
        assert_eq!(v.space_release(200), VoiceCmd::None);
        assert_eq!(v.phase(), VoicePhase::Idle);
    }

    #[test]
    fn esc_cancels_a_recording_without_transcribing() {
        let mut v = enabled();
        hold_through_warmup(&mut v);
        assert!(v.esc_cancels());
        assert_eq!(v.cancel(), VoiceCmd::Cancel);
        assert_eq!(v.phase(), VoicePhase::Idle);
    }

    #[test]
    fn the_hard_cap_stops_a_recording_the_repeats_keep_alive() {
        let mut v = enabled();
        hold_through_warmup(&mut v);
        let VoicePhase::Listening { since_ms, .. } = v.phase() else {
            panic!("recording");
        };
        // Keep the repeat stream alive right up to the cap.
        let mut t = since_ms;
        while t < since_ms + MAX_HOLD_MS {
            t += 100;
            v.swallowed_space(t);
        }
        assert_eq!(v.tick(t), VoiceCmd::Stop);
    }

    #[test]
    fn settled_returns_to_rest_from_any_phase() {
        let mut v = enabled();
        hold_through_warmup(&mut v);
        // A driver that could not start answers VoiceFailed while the deck
        // is still listening.
        v.settled();
        assert_eq!(v.phase(), VoicePhase::Idle);
    }

    fn tapping() -> VoiceUi {
        VoiceUi {
            enabled: true,
            mode: VoiceMode::Tap,
            ..VoiceUi::default()
        }
    }

    /// **The witness.** Tap mode starts and stops a recording on a terminal
    /// that reports no key releases and whose OS delivers no key-repeat —
    /// the configuration in which hold mode can never arm, and the reason
    /// #5347 exists.
    ///
    /// Every signal hold mode relies on is withheld here: `release_events`
    /// is false, and not one repeat is delivered between the two taps. Under
    /// `VoiceMode::Hold` the same script records nothing at all, which
    /// `the_same_script_records_nothing_in_hold_mode` below asserts directly.
    #[test]
    fn tap_mode_records_with_no_key_repeat_and_no_release_events() {
        let mut v = tapping();
        assert!(!v.release_events, "the terminal reports no releases");

        // One tap on an empty composer opens the capture immediately — no
        // warmup — and retracts the single space it typed.
        assert_eq!(v.typed_space(0, true), VoiceCmd::Start { retract: 1 });
        assert!(v.recording());
        assert!(v.swallows_space());

        // The tick clock runs on with no repeats arriving. Hold mode would
        // have called that a release long ago; tap mode keeps recording.
        let mut t = 0;
        while t < 10 * LISTENING_GAP_MS {
            t += 50;
            assert_eq!(v.tick(t), VoiceCmd::None);
        }
        assert!(
            v.recording(),
            "a silent repeat stream must not end a tap capture"
        );

        // The second tap ends it.
        assert_eq!(v.swallowed_space(t), VoiceCmd::Stop);
        assert!(matches!(v.phase(), VoicePhase::Transcribing { .. }));
    }

    /// The control for the witness above: the identical script under
    /// `VoiceMode::Hold` never reaches a recording, so the test proves tap
    /// mode rather than proving the harness.
    #[test]
    fn the_same_script_records_nothing_in_hold_mode() {
        let mut v = enabled();
        assert_eq!(v.typed_space(0, true), VoiceCmd::None);
        let mut t = 0;
        while t < 10 * LISTENING_GAP_MS {
            t += 50;
            v.tick(t);
        }
        assert!(!v.recording());
        assert_eq!(
            v.phase(),
            VoicePhase::Idle,
            "the arming gap dismissed it as a tap"
        );
    }

    #[test]
    fn tap_mode_ignores_a_space_typed_into_a_non_empty_composer() {
        let mut v = tapping();
        // Mid-sentence, a space is a word gap and must stay one.
        assert_eq!(v.typed_space(0, false), VoiceCmd::None);
        assert_eq!(v.phase(), VoicePhase::Idle);
        // The empty composer is what arms it.
        assert_eq!(v.typed_space(10, true), VoiceCmd::Start { retract: 1 });
    }

    #[test]
    fn tap_mode_never_arms_while_disabled() {
        let mut v = VoiceUi {
            mode: VoiceMode::Tap,
            ..VoiceUi::default()
        };
        assert_eq!(v.typed_space(0, true), VoiceCmd::None);
        assert_eq!(v.phase(), VoicePhase::Idle);
    }

    /// A space held in tap mode delivers repeats that are indistinguishable
    /// from a second tap. `TAP_MIN_MS` is what stops the first of them ending
    /// the capture it started — the capture survives to the floor and then
    /// ends, rather than posting silence to a provider.
    #[test]
    fn a_held_space_in_tap_mode_cannot_stop_the_capture_it_started() {
        let mut v = tapping();
        assert_eq!(v.typed_space(0, true), VoiceCmd::Start { retract: 1 });
        // Every repeat arriving strictly inside the floor is refused.
        let mut t = 50;
        while t < TAP_MIN_MS {
            assert_eq!(v.swallowed_space(t), VoiceCmd::None, "at {t}ms");
            assert!(v.recording());
            t += 50;
        }
        // At the floor, the next one ends the capture.
        assert_eq!(v.swallowed_space(TAP_MIN_MS), VoiceCmd::Stop);
    }

    /// The release of the *starting* tap must not end the capture: a terminal
    /// that reports releases would otherwise collapse tap mode into hold
    /// mode, which is the mode the user explicitly did not pick.
    #[test]
    fn a_reported_release_does_not_end_a_tap_capture() {
        let mut v = tapping();
        v.release_events = true;
        assert_eq!(v.typed_space(0, true), VoiceCmd::Start { retract: 1 });
        assert_eq!(v.space_release(20), VoiceCmd::None);
        assert!(v.recording(), "the tap's own release is not the stop");
        assert_eq!(v.swallowed_space(TAP_MIN_MS), VoiceCmd::Stop);
    }

    #[test]
    fn esc_cancels_a_tap_recording_and_the_cap_still_binds() {
        let mut v = tapping();
        v.typed_space(0, true);
        assert!(v.esc_cancels());
        assert_eq!(v.cancel(), VoiceCmd::Cancel);
        assert_eq!(v.phase(), VoicePhase::Idle);

        // And the hard cap ends a tap capture nobody stops.
        let mut v = tapping();
        v.typed_space(0, true);
        assert_eq!(v.tick(MAX_HOLD_MS), VoiceCmd::Stop);
    }

    #[test]
    fn the_tap_status_line_names_the_key_that_stops_it() {
        let mut v = tapping();
        v.typed_space(0, true);
        let line = v.status_line(65_000).unwrap();
        assert!(line.contains("listening 1:05"), "{line}");
        assert!(line.contains("tap space to stop"), "{line}");
        // Hold mode still says what it always said — the goldens render it.
        let mut h = enabled();
        hold_through_warmup(&mut h);
        let VoicePhase::Listening { since_ms, .. } = h.phase() else {
            panic!("recording");
        };
        assert!(
            h.status_line(since_ms).unwrap().contains("release to stop"),
            "hold mode's wording is unchanged"
        );
    }

    #[test]
    fn every_mode_round_trips_through_its_slug() {
        for mode in VoiceMode::ALL {
            assert_eq!(VoiceMode::parse(mode.slug()), Some(*mode));
        }
        // Case and surrounding space are forgiven; junk is not.
        assert_eq!(VoiceMode::parse(" TAP "), Some(VoiceMode::Tap));
        assert_eq!(VoiceMode::parse("hold"), Some(VoiceMode::Hold));
        assert_eq!(VoiceMode::parse("push"), None);
        // The default is the mode that shipped first, so an existing
        // `voice.enabled` with no `voice.mode` behaves exactly as before.
        assert_eq!(VoiceMode::default(), VoiceMode::Hold);
    }

    #[test]
    fn the_status_line_tracks_the_phase() {
        let mut v = enabled();
        assert_eq!(v.status_line(0), None);
        armed(&mut v, 0);
        assert!(v.status_line(0).unwrap().contains("keep holding"));
        // A fresh machine for the hold: the helper counts its own spaces.
        let mut v = enabled();
        hold_through_warmup(&mut v);
        let VoicePhase::Listening { since_ms, .. } = v.phase() else {
            panic!("recording");
        };
        let line = v.status_line(since_ms + 65_000).unwrap();
        assert!(line.contains("listening 1:05"), "{line}");
        v.tick(since_ms + MAX_HOLD_MS);
        assert!(
            v.status_line(since_ms + MAX_HOLD_MS)
                .unwrap()
                .contains("transcribing")
        );
    }
}
