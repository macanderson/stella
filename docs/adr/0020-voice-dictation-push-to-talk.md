---
id: adr/0020-voice-dictation-push-to-talk
title: "ADR 0020: Voice dictation is a held spacebar, and transcription is BYOK"
status: proposed
---

# ADR 0020: Voice dictation is a held spacebar, and transcription is BYOK

- Status: **Proposed** — awaiting ratification by the repository owner.
- Date: 2026-08-27
- Deciders: repository owner (pending)

## Context

Stella's interactive prompt is typed. Claude Code ships dictation — hold the
spacebar, speak, release, and the transcript lands in the prompt — and Stella
should meet that bar. Three constraints shape how it can land here:

**A terminal does not report key release.** Without the kitty keyboard
protocol's `REPORT_EVENT_TYPES` flag, a held key arrives as a stream of
repeated Press events and its release arrives as nothing at all.
`TerminalGuard::enter` today pushes only `DISAMBIGUATE_ESCAPE_CODES`, and both
key loops filter `KeyEventKind::Release` out defensively.

**The spacebar is a printable character.** `Composer::insert_char` receives it
mid-prompt, so any hold gesture must leave a plain tap meaning "a space",
including the taps that begin an aborted hold.

**Stella is BYOK with zero telemetry egress by default** (architecture rule 3).
Claude Code transcribes on Anthropic's servers against a claude.ai account;
Stella has no such account to lean on, and microphone audio leaving the
machine is exactly the kind of egress a user must opt into by name.

## Decision

**The gesture is Claude Code's.** Hold Space in the composer; a warmup phase
("keep holding…") absorbs the taps that were just typing, then recording
starts ("listening…") and the caret changes colour. Release stops it; the
transcript is inserted at the cursor. The hold is detected two ways, best
first: `TerminalGuard::enter` now also requests `REPORT_EVENT_TYPES`, so a
terminal that reports releases ends the recording on the release event; every
other terminal ends it when the OS key-repeat stream goes quiet
(`stella_tui::voice` holds the gap thresholds and their rationale). Spaces
typed during warmup are retracted only when recording actually starts, so an
aborted hold has typed exactly what the user typed.

**The state machine is pure and lives in `stella-tui`** (`src/voice.rs`):
a fold over key events and the deck's existing 33ms tick clock, held on
`DeckUi` like every other interaction state, unit-tested with an injected
clock. The shell (`deck_shell::run_deck`) only wires its commands to the
driver, the same shape as the `⌃V` clipboard lane.

**Capture and transcription live on the driver side** (`stella-cli`,
`command_deck/voice.rs`), behind the deck's existing wire:
`WorkspaceInput::{VoiceStart, VoiceStop, VoiceCancel}` out,
`Inbound::{VoiceTranscript, VoiceFailed}` back. The interactive shell (`stella-tui`) never touches a
microphone or the network (its README's engine-access boundary). Capture is
`cpal` on macOS and Windows — both are OS frameworks, no system packages —
and an `arecord`/`sox` subprocess on Linux, where a `cpal`/ALSA build
dependency would break `install.sh` source builds on any box without ALSA
headers. WAV encoding and the multipart request body are written in-tree
(each is a page of well-specified bytes with a unit test), so `cpal` is the
feature's only new dependency.

**Transcription is an OpenAI-compatible `audio/transcriptions` call.** The
`voice` settings section names a provider id, a model slug, and a language —
never an endpoint or a command, which keeps the section outside the
project-trust boundary, like `ui`. The endpoint derives from the provider's
existing configuration and the key resolves through the existing credential
chain (`stella_model::credential::ApiKey`), so there is no second secret
store and any OpenAI-compatible transcription server (OpenAI, Groq, a local
Whisper server declared as a provider) works unmodified.

**Dictation is off until the user turns it on.** Holding a key is too easy to
do by accident for it to stream microphone audio to a provider by default;
architecture rule 3's "network exception selected by the user" means
selected by name, so `voice.enabled` defaults to false and the failure notice says how to
enable it.

## Consequences

- A terminal with `REPORT_EVENT_TYPES` gets crisp release detection; every
  other terminal inherits the repeat-gap heuristic, which requires OS
  key-repeat to be enabled. A user with key-repeat disabled and no kitty
  protocol cannot hold-to-talk; the tap-to-toggle mode Claude Code also
  ships is the remedy and is tracked as a follow-up issue.
- Recording is bounded (a hard cap, and the elapsed time is drawn from the
  deck's tick clock), and audio is written to a temporary file that is
  removed after transcription — nothing about a dictation persists except
  the text the user then sees in the composer.
- Providers whose transcription API is not OpenAI-shaped (e.g. Anthropic,
  which has none) are simply not usable as `voice.provider`; the notice
  names the setting. Promoting transcription to a declared per-provider
  capability axis (architecture rule 8) waits until a second wire dialect
  exists to declare.
