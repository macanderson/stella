//! The `/voice` slash command — switch dictation on, off, or between
//! gestures, and persist the choice.
//!
//! Dictation shipped switched off, reachable only by hand-editing
//! `voice.enabled` in a settings file (ADR 0020), so the feature was out of
//! reach for anyone who had not read the ADR. This is the one command that
//! turns it on: `/voice tap`, `/voice hold`, `/voice off`.
//!
//! Kept out of the `command_deck` dispatcher for the reason
//! [`super::theme_cmd`] is — the parser, the settings write and the reply
//! text live together, and the dispatcher only wires them to `say`.
//!
//! Unlike `/theme`, the live half is not a global this module can flip: the
//! gesture lives on the deck's own `stella_tui::voice::VoiceUi`, on the other
//! side of the envelope. So the caller follows a successful write with an
//! `Inbound::VoiceConfig`. Persisting alone would make `/voice tap` a command
//! that reports success and changes nothing until the next session.

use stella_tui::voice::VoiceMode;

use crate::settings::{Settings, VoiceSettings};

/// `/voice …` — the dictation switch.
pub enum VoiceCommand {
    /// `/voice hold` or `/voice tap` — switch on, in that gesture.
    On(VoiceMode),
    /// `/voice off` — switch dictation off, leaving the provider settings
    /// (`voice.provider`, `voice.model`, `voice.language`) in place so
    /// switching back on does not ask for them again.
    Off,
    /// Anything else after `/voice`.
    Usage(String),
}

/// Parse `trimmed` as a [`VoiceCommand`]; `None` leaves it on the normal
/// path. A bare `/voice` (no argument) never reaches here — it has no
/// whitespace to split on and is handled by the exact-match arm, exactly as
/// `/theme` is.
pub fn parse_voice_command(trimmed: &str) -> Option<VoiceCommand> {
    let (head, rest) = trimmed.split_once(char::is_whitespace)?;
    if head != "/voice" {
        return None;
    }
    let arg = rest.trim();
    if arg.eq_ignore_ascii_case("off") {
        return Some(VoiceCommand::Off);
    }
    match VoiceMode::parse(arg) {
        Some(mode) => Some(VoiceCommand::On(mode)),
        None => Some(VoiceCommand::Usage(arg.to_string())),
    }
}

/// What each gesture is, in one line — the text `/voice` prints.
fn blurb(mode: VoiceMode) -> &'static str {
    match mode {
        VoiceMode::Hold => "hold space through the warmup, release to stop",
        VoiceMode::Tap => "tap space on an empty prompt to start, tap again to stop",
    }
}

/// The persisted state, resolved the way the deck resolves it at startup:
/// `(enabled, mode)`, so the summary and the writer agree on what
/// "currently" means rather than each reading settings its own way.
pub fn persisted(workspace_root: &std::path::Path) -> (bool, VoiceMode) {
    let voice = Settings::load(workspace_root).ok().and_then(|s| s.voice);
    let enabled = voice.as_ref().and_then(|v| v.enabled).unwrap_or(false);
    let mode = voice
        .as_ref()
        .and_then(|v| v.mode.as_deref())
        .and_then(VoiceMode::parse)
        .unwrap_or_default();
    (enabled, mode)
}

/// The mode list, one per line, marking the active one.
fn mode_menu(active: VoiceMode, enabled: bool) -> String {
    VoiceMode::ALL
        .iter()
        .map(|m| {
            let mark = if enabled && *m == active {
                "●"
            } else {
                "○"
            };
            format!("  {mark} {:<5} {}", m.slug(), blurb(*m))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The bare-`/voice` summary: whether dictation is on, which gesture it is
/// bound to, and how to change either.
pub fn current_summary(cfg: &crate::config::Config) -> String {
    let (enabled, mode) = persisted(&cfg.workspace_root);
    let state = if enabled {
        format!("on ({})", mode.slug())
    } else {
        "off".to_string()
    };
    format!(
        "dictation: {state}\n\n{}\n\n\
         `/voice tap` or `/voice hold` switches on; `/voice off` switches off.\n\
         Tap mode is the one that works when your terminal reports no key\n\
         releases and OS key repeat is off — there a hold cannot be told from\n\
         a tap, and hold mode never starts.",
        mode_menu(mode, enabled),
    )
}

/// Switch dictation on in `mode` and persist it (`voice.enabled`,
/// `voice.mode`) to user-scope settings.
pub fn set_mode(mode: VoiceMode) -> Result<String, String> {
    write_section(Some(true), Some(mode), || {
        format!(
            "dictation → on, {} mode ({}). Saved — new sessions start here too.",
            mode.slug(),
            blurb(mode)
        )
    })
}

/// Switch dictation off and persist it, leaving the gesture and the provider
/// settings recorded so `/voice` can switch back on without asking again.
pub fn set_off() -> Result<String, String> {
    write_section(Some(false), None, || {
        "dictation → off. Saved — `/voice tap` or `/voice hold` switches it back on.".to_string()
    })
}

/// The shared write: load the user-scope `[voice]` section, apply the fields
/// this command owns, save it back, and render `ok` on success.
///
/// Read-modify-write on the section, not the file: `[voice]` also carries
/// `provider`, `model` and `language`, and a fresh struct here would erase a
/// configured local Whisper endpoint.
///
/// A failure names what did *not* happen. Unlike `/theme`, nothing has taken
/// effect at this point — the caller sends the live update only on `Ok` — so
/// a failed write leaves the session exactly as it was, and the message says
/// that rather than `/theme`'s "it worked for now but was not saved".
fn write_section(
    enabled: Option<bool>,
    mode: Option<VoiceMode>,
    ok: impl Fn() -> String,
) -> Result<String, String> {
    let mut voice: VoiceSettings = Settings::load_user_scope(&mut Vec::new())
        .ok()
        .and_then(|s| s.voice)
        .unwrap_or_default();
    if let Some(enabled) = enabled {
        voice.enabled = Some(enabled);
    }
    if let Some(mode) = mode {
        voice.mode = Some(mode.slug().to_string());
    }
    let Some(path) = crate::settings::user_config_path() else {
        return Err(
            "dictation was not changed: the user settings path is unavailable (is $HOME set?)"
                .to_string(),
        );
    };
    voice
        .save_to(&path)
        .map_err(|e| format!("dictation was not changed: {e}"))?;
    Ok(ok())
}

/// The reply to `/voice <unknown>`: name the valid set rather than silently
/// no-op'ing on a typo.
pub fn usage(arg: &str) -> String {
    format!(
        "unknown voice mode `{arg}` — try `/voice hold`, `/voice tap`, or `/voice off`:\n\n{}",
        mode_menu(VoiceMode::default(), false)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_voice_command_reads_each_mode_and_flags_junk() {
        assert!(matches!(
            parse_voice_command("/voice tap"),
            Some(VoiceCommand::On(VoiceMode::Tap))
        ));
        assert!(matches!(
            parse_voice_command("/voice hold"),
            Some(VoiceCommand::On(VoiceMode::Hold))
        ));
        // Case-insensitive, like every other slug argument in the deck.
        assert!(matches!(
            parse_voice_command("/voice TAP"),
            Some(VoiceCommand::On(VoiceMode::Tap))
        ));
        assert!(matches!(
            parse_voice_command("/voice Off"),
            Some(VoiceCommand::Off)
        ));
        // An unknown argument is a named error, never a silent no-op.
        assert!(matches!(
            parse_voice_command("/voice push"),
            Some(VoiceCommand::Usage(s)) if s == "push"
        ));
        // Bare `/voice` has no whitespace to split — the exact-match arm owns it.
        assert!(parse_voice_command("/voice").is_none());
        // `/voice` must not swallow an unrelated head.
        assert!(parse_voice_command("/voices list").is_none());
    }

    #[test]
    fn the_usage_and_menu_name_every_shipped_mode() {
        let text = usage("push");
        for mode in VoiceMode::ALL {
            assert!(text.contains(mode.slug()), "usage omits {}", mode.slug());
        }
    }
}
