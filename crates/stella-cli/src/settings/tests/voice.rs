// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The `[voice]` section's writer: what `/voice` persists, and what it must
//! not destroy on the way (#5347).
//!
//! A sibling file rather than more lines in `settings/tests.rs`, which sits at
//! the 1500-line ratchet.

use super::*;
// The gesture slug these tests round-trip is parsed where it is read, the
// house convention for a slug in settings (`VoiceSettings::mode`).
use stella_tui::voice::VoiceMode;

/// **Witness (#5347).** `/voice tap` persists both fields and the next
/// session reads them back — the whole point of the command, since dictation
/// was previously reachable only by hand-editing this file.
///
/// The section write is the part that can silently go wrong: `[voice]` also
/// carries the transcription endpoint, so a writer that serialized a fresh
/// struct would switch dictation on and drop the provider it dictates
/// through. That is asserted here rather than argued.
#[test]
fn voice_mode_save_preserves_the_transcription_endpoint_and_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "settings.json",
        r#"{"providers": {"zai": {"default_model": "glm-5.2"}},
            "voice": {"provider": "groq", "model": "whisper-large-v3"},
            "future_key": {"anything": true}}"#,
    );

    // What `/voice tap` writes: read-modify-write on the loaded section.
    let mut voice = Settings::load_from(std::slice::from_ref(&path))
        .unwrap()
        .voice
        .unwrap_or_default();
    voice.enabled = Some(true);
    voice.mode = Some("tap".to_string());
    voice.save_to(&path).unwrap();

    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(raw["voice"]["enabled"], true);
    assert_eq!(raw["voice"]["mode"], "tap");
    // The endpoint the user configured is still there…
    assert_eq!(raw["voice"]["provider"], "groq");
    assert_eq!(raw["voice"]["model"], "whisper-large-v3");
    // …as is every sibling key, at the value level.
    assert_eq!(raw["providers"]["zai"]["default_model"], "glm-5.2");
    assert_eq!(raw["future_key"]["anything"], true);

    // The next session reads it back through the normal load path and
    // resolves the gesture the deck will bind to Space.
    let merged = Settings::load_from(std::slice::from_ref(&path)).unwrap();
    let loaded = merged.voice.as_ref().unwrap();
    assert_eq!(loaded.enabled, Some(true));
    assert_eq!(
        loaded.mode.as_deref().and_then(VoiceMode::parse),
        Some(VoiceMode::Tap)
    );

    // `/voice off` leaves the gesture and the endpoint recorded, so switching
    // back on does not ask for either again.
    let mut off = loaded.clone();
    off.enabled = Some(false);
    off.save_to(&path).unwrap();
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(raw["voice"]["enabled"], false);
    assert_eq!(raw["voice"]["mode"], "tap");
    assert_eq!(raw["voice"]["provider"], "groq");

    // An empty section is dropped rather than written as `{}`.
    VoiceSettings::default().save_to(&path).unwrap();
    let raw: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(raw.as_object().unwrap().get("voice").is_none());
    assert_eq!(raw["providers"]["zai"]["default_model"], "glm-5.2");
}

/// An absent or unreadable `voice.mode` resolves to the gesture that shipped
/// first, so an existing `voice.enabled` behaves exactly as it did before
/// this field existed — a typo costs the mode, never dictation itself.
#[test]
fn an_absent_or_junk_voice_mode_falls_back_to_hold() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "settings.json",
        r#"{"voice": {"enabled": true}}"#,
    );
    let merged = Settings::load_from(std::slice::from_ref(&path)).unwrap();
    let voice = merged.voice.as_ref().unwrap();
    assert_eq!(voice.enabled, Some(true));
    assert_eq!(voice.mode, None);
    assert_eq!(
        voice
            .mode
            .as_deref()
            .and_then(VoiceMode::parse)
            .unwrap_or_default(),
        VoiceMode::Hold
    );

    let path = write(
        dir.path(),
        "junk.json",
        r#"{"voice": {"enabled": true, "mode": "push"}}"#,
    );
    let merged = Settings::load_from(std::slice::from_ref(&path)).unwrap();
    let voice = merged.voice.as_ref().unwrap();
    assert_eq!(
        voice
            .mode
            .as_deref()
            .and_then(VoiceMode::parse)
            .unwrap_or_default(),
        VoiceMode::Hold,
        "an unrecognised mode must not disable dictation"
    );
}
