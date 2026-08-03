// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What this terminal can support: colour, animation, accessibility, and the
//! Command Deck.
//!
//! Four decisions that look unrelated in a match arm but are one question —
//! *is there a human at a capable terminal on the other end of this stream, and
//! how are they reading it?* — answered from the same inputs (`TERM`,
//! `NO_COLOR`, `CLICOLOR_FORCE`, `STELLA_NO_ANIM`, `STELLA_ACCESSIBLE`,
//! `STELLA_PLAIN`, and whether stdin/stdout are ttys).
//!
//! Each is a pure function of its inputs with the process-global side effect
//! isolated in [`apply_dumb_terminal_policy`], so the decisions are testable
//! without `colored`'s override — which every other test in this binary renders
//! through, and which is therefore the one thing a test must not disturb.
//!
//! The house convention for the boolean env vars is deliberately uniform: a
//! variable is on when present and non-`0`, except `NO_COLOR`, which follows
//! its own published rule (present and non-empty).

use std::io::IsTerminal;

/// Honour `TERM=dumb` by switching ANSI output off process-wide.
///
/// The `colored` crate already respects `NO_COLOR`, `CLICOLOR*`, and a
/// non-tty stream, but not `TERM` — so a dumb terminal (Emacs `M-x shell`,
/// a bare serial console, an editor's build pane, `TERM=dumb` in CI) got the
/// full escape-sequence treatment and rendered it literally. Every other Unix
/// tool that colours output treats `dumb` as "this terminal cannot", so
/// stella does too. An explicit `CLICOLOR_FORCE` still wins: it is the
/// documented way to say "I know what my terminal is", and `colored`'s own
/// override resolution keeps honouring it because this only sets the default
/// when the user has not forced anything.
pub(crate) fn apply_dumb_terminal_policy() {
    if dumb_terminal(
        std::env::var_os("TERM").as_deref(),
        std::env::var_os("CLICOLOR_FORCE").as_deref(),
    ) {
        colored::control::set_override(false);
    }
}

/// The decision behind [`apply_dumb_terminal_policy`], kept pure so it can be
/// tested without reaching for `colored`'s process-global override (which
/// every other test in this binary renders through).
pub(crate) fn dumb_terminal(
    term: Option<&std::ffi::OsStr>,
    clicolor_force: Option<&std::ffi::OsStr>,
) -> bool {
    let forced = clicolor_force.is_some_and(|v| !v.is_empty() && v != "0");
    !forced && term.is_some_and(|term| term == "dumb")
}

/// Whether deck animation is off for this invocation: the `--no-anim` flag,
/// or either environment signal its own help text promises.
///
/// The promise was only half kept. `init_fx::animation_enabled` folded
/// `STELLA_NO_ANIM` and `NO_COLOR` in for the `stella init` cinematic, but the
/// Command Deck — the surface with the shimmering progress bar and the
/// blinking caret, and the one an asciinema recording or a CI log actually
/// captures — was handed the bare flag. So `NO_COLOR=1 stella chat` kept
/// animating, and the flag's documentation was simply wrong about it.
///
/// `NO_COLOR` follows its published rule (present and non-empty);
/// `STELLA_NO_ANIM` follows this CLI's own house convention for boolean env
/// vars (`--plain`'s `STELLA_PLAIN`, `env_files`' `STELLA_NO_ENV_FILE`), where
/// an explicit `0` means off.
pub(crate) fn animation_disabled(no_anim_flag: bool) -> bool {
    let stella = std::env::var_os("STELLA_NO_ANIM").is_some_and(|v| !v.is_empty() && v != "0");
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    no_anim_flag || stella || no_color
}

/// How the deck presents itself, as distinct from what it runs.
///
/// Grouped rather than passed as two adjacent `bool`s: they are the same kind
/// of thing, they are both resolved here, and two positional booleans in a row
/// is exactly the signature a caller silently transposes.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DeckPresentation {
    /// Motion frozen to a static frame ([`animation_disabled`]). Implied by
    /// [`Self::accessible`].
    pub(crate) no_anim: bool,
    /// Accessible mode ([`accessible_mode`], #1258): the same deck, drawn on
    /// the user's own screen with settled messages moving into scrollback.
    /// See `stella_tui::DeckOptions::accessible`.
    pub(crate) accessible: bool,
}

/// Whether the Command Deck runs in accessible mode: the `--accessible` flag
/// or `STELLA_ACCESSIBLE` (#1258).
///
/// A **mode on the deck**, not a surface beside it — so it composes with
/// everything and takes no part in the deck-or-REPL decision below. It follows
/// this CLI's house convention for boolean env vars (`--plain`'s
/// `STELLA_PLAIN`, `env_files`' `STELLA_NO_ENV_FILE`), where an explicit `0`
/// means off.
///
/// The env var exists because a screen-reader user should be able to set this
/// once, in their shell profile, rather than remember a flag on every
/// invocation — the flag is the exception, not the habit.
pub(crate) fn accessible_mode(accessible_flag: bool) -> bool {
    accessible_flag || accessible_env(std::env::var_os("STELLA_ACCESSIBLE").as_deref())
}

/// The env half of [`accessible_mode`], kept pure so the convention is
/// testable without touching the process environment.
pub(crate) fn accessible_env(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| !v.is_empty() && v != "0")
}

/// Why `chat` fell back to the line REPL instead of the Command Deck.
///
/// Carried rather than collapsed to a bool because the two surfaces are not
/// the same program with different paint. The REPL has no prompt queue and no
/// mid-turn steering, and it exits the moment stdin reaches EOF — so a user
/// who is silently downgraded sees follow-up prompts vanish, a queue that
/// never drains, and an exit the instant a turn ends, with nothing anywhere
/// saying why. Naming the reason is what lets the caller say it out loud.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlainReason {
    /// `--plain` was passed.
    Flag,
    /// `STELLA_PLAIN` is set to something other than `0`.
    Env,
    /// stdin is a pipe or a file, so there are no keystrokes to read.
    StdinNotTty,
    /// stdout is redirected, so raw mode and the alternate screen are moot.
    StdoutNotTty,
}

impl PlainReason {
    /// One line naming the cause, for the notice printed before the REPL
    /// banner. Phrased as the condition observed, not as an instruction —
    /// `--plain` and a piped stdin are both legitimate ways to run.
    pub(crate) fn explain(self) -> &'static str {
        match self {
            Self::Flag => "--plain was passed",
            Self::Env => "STELLA_PLAIN is set",
            Self::StdinNotTty => "stdin is not a terminal (piped or redirected)",
            Self::StdoutNotTty => "stdout is not a terminal (piped or redirected)",
        }
    }
}

/// The deck-or-REPL decision, kept pure so both branches are testable without
/// a pty and without touching the process's real streams. `None` means the
/// Command Deck runs.
///
/// Explicit opt-outs are reported ahead of stream shape: someone who passed
/// `--plain` does not need to be told about their tty.
pub(crate) fn deck_decision(
    plain_flag: bool,
    plain_env: bool,
    stdin_tty: bool,
    stdout_tty: bool,
) -> Option<PlainReason> {
    if plain_flag {
        Some(PlainReason::Flag)
    } else if plain_env {
        Some(PlainReason::Env)
    } else if !stdin_tty {
        Some(PlainReason::StdinNotTty)
    } else if !stdout_tty {
        Some(PlainReason::StdoutNotTty)
    } else {
        None
    }
}

/// Whether `chat` should launch the Command Deck: an explicit `--plain` or
/// STELLA_PLAIN=1 opts out, and both stdin and stdout must be real terminals
/// (raw mode + the alternate screen are meaningless on a pipe).
///
/// [`deck_decision`] is the same question with the reason kept; this wrapper
/// exists for the call sites that only branch on it.
pub(crate) fn use_deck(plain_flag: bool) -> bool {
    plain_fallback(plain_flag).is_none()
}

/// [`deck_decision`] applied to this process's real environment and streams.
pub(crate) fn plain_fallback(plain_flag: bool) -> Option<PlainReason> {
    deck_decision(
        plain_flag,
        std::env::var_os("STELLA_PLAIN").is_some_and(|v| !v.is_empty() && v != "0"),
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    )
}
