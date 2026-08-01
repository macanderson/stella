//! The `/theme` slash command — switch the deck's colour theme and persist the
//! choice, at parity with how `/model` persists the default model. Kept out of
//! the already-large `command_deck` dispatcher: the parser, the live switch,
//! and the settings write live here; `command_deck` only wires them to `say`.
//!
//! Two themes ship. `stella-dark` (Phosphor Gold on Ink — the comet kit at
//! `docs/brand/BRAND.md`) is the default; `stella-light` is the same gold
//! darkened onto warm paper. [`blurb`] is the user-visible naming of both,
//! and it has drifted behind the identity before (it once named a terminal
//! green and an ember red-orange two recolours dead) — keep it telling the
//! truth. The switch itself is a per-frame buffer remap in
//! [`stella_tui::theme`], so it takes effect on the next frame with nothing to
//! re-lay-out — this module only flips the global and writes `ui.theme`.

use stella_tui::theme::ThemeName;

use crate::config::Config;

/// `/theme …` — the colour-theme switch.
pub enum ThemeCommand {
    /// `/theme <slug>` — a recognised theme to apply and persist.
    Set(ThemeName),
    /// `/theme <unknown>` — anything that is not a shipped theme.
    Usage(String),
}

/// Parse `trimmed` as a [`ThemeCommand`]; `None` leaves it on the normal path.
/// A bare `/theme` (no argument) never reaches here — it has no whitespace to
/// split on and is handled by the exact-match arm.
pub fn parse_theme_command(trimmed: &str) -> Option<ThemeCommand> {
    let (head, rest) = trimmed.split_once(char::is_whitespace)?;
    if head != "/theme" {
        return None;
    }
    let arg = rest.trim();
    match ThemeName::parse(arg) {
        Some(name) => Some(ThemeCommand::Set(name)),
        None => Some(ThemeCommand::Usage(arg.to_string())),
    }
}

/// The list of themes, one per line, marking the active one — shared by the
/// bare-`/theme` summary and the unknown-argument reply.
fn theme_menu() -> String {
    let active = stella_tui::theme::active_theme();
    ThemeName::ALL
        .iter()
        .map(|t| {
            let mark = if *t == active { "●" } else { "○" };
            format!("  {mark} {:<13} {}", t.slug(), blurb(*t))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A one-line description of what each theme looks like.
///
/// Printed to users by `/theme`, which has made it the most visible piece of
/// retired-identity prose in the product more than once. Both themes carry
/// the same Phosphor Gold brand hue; the ground is what differs.
fn blurb(theme: ThemeName) -> &'static str {
    match theme {
        ThemeName::StellaDark => "phosphor gold on ink (default)",
        ThemeName::StellaLight => "phosphor gold on paper",
    }
}

/// The bare-`/theme` summary: the live theme, the persisted preference (what new
/// sessions start with), and the pickable list.
pub fn current_summary(cfg: &Config) -> String {
    let active = stella_tui::theme::active_theme();
    let persisted = crate::settings::Settings::load(&cfg.workspace_root)
        .ok()
        .and_then(|s| s.ui_theme().and_then(ThemeName::parse))
        .map(|t| t.slug())
        .unwrap_or("stella-dark (default — unset)");
    format!(
        "theme (this session): {}\n\
         theme (new sessions): {persisted}\n\n\
         switch with `/theme <name>` (e.g. `/theme stella-light`):\n\n{}",
        active.slug(),
        theme_menu(),
    )
}

/// Switch the live theme AND persist it to user-scope settings (`ui.theme`),
/// through the same `save_to` the settings machinery uses elsewhere. The switch
/// is applied first, so an unwritable settings file still recolours this
/// session — the returned `Err` says exactly that rather than pretending
/// nothing happened.
pub fn set_theme(name: ThemeName) -> Result<String, String> {
    // Live switch first — the next frame is already this theme.
    stella_tui::theme::set_active_theme(name);
    let ui = crate::settings::UiSettings {
        theme: Some(name.slug().to_string()),
    };
    let Some(path) = crate::settings::user_config_path() else {
        return Err(format!(
            "theme → {} for this session, but it could not be saved for next time: \
             the user settings path is unavailable (is $HOME set?)",
            name.slug()
        ));
    };
    ui.save_to(&path).map_err(|e| {
        format!(
            "theme → {} for this session, but it could not be saved for next time: {e}",
            name.slug()
        )
    })?;
    Ok(format!(
        "theme → {} ({}). Saved — new sessions start here too. `/theme` to see both.",
        name.slug(),
        blurb(name)
    ))
}

/// The reply to `/theme <unknown>`: name the valid set rather than silently
/// no-op'ing on a typo'd slug.
pub fn usage(arg: &str) -> String {
    format!(
        "unknown theme `{arg}` — pick one of:\n\n{}\n\n(or `stella-dark`/`stella-light`)",
        theme_menu()
    )
}

/// Apply the persisted theme (`ui.theme`) at deck startup, before the first
/// frame. A missing or unrecognised value leaves the compiled default
/// (`stella-dark`) in place — the switch is best-effort chrome, never fatal.
pub fn apply_persisted(cfg: &Config) {
    if let Ok(settings) = crate::settings::Settings::load(&cfg.workspace_root)
        && let Some(name) = settings.ui_theme().and_then(ThemeName::parse)
    {
        stella_tui::theme::set_active_theme(name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_theme_command_reads_a_slug_and_flags_junk() {
        assert!(matches!(
            parse_theme_command("/theme stella-light"),
            Some(ThemeCommand::Set(ThemeName::StellaLight))
        ));
        assert!(matches!(
            parse_theme_command("/theme stella-dark"),
            Some(ThemeCommand::Set(ThemeName::StellaDark))
        ));
        // Case-insensitive, and the bare `light`/`dark` shorthands resolve.
        assert!(matches!(
            parse_theme_command("/theme LIGHT"),
            Some(ThemeCommand::Set(ThemeName::StellaLight))
        ));
        // An unknown slug is a Usage (a named error), never a wasted prompt.
        assert!(matches!(
            parse_theme_command("/theme solarized"),
            Some(ThemeCommand::Usage(s)) if s == "solarized"
        ));
        // Bare `/theme` has no whitespace to split — the exact-match arm owns it.
        assert!(parse_theme_command("/theme").is_none());
        // `/theme` must not swallow an unrelated head.
        assert!(parse_theme_command("/themes list").is_none());
    }
}
