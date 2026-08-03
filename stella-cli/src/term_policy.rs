// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! What this terminal can support: colour, animation, and the Command Deck.
//!
//! Three decisions that look unrelated in a match arm but are one question —
//! *is there a human at a capable terminal on the other end of this stream?* —
//! answered from the same inputs (`TERM`, `NO_COLOR`, `CLICOLOR_FORCE`,
//! `STELLA_NO_ANIM`, `STELLA_PLAIN`, and whether stdin/stdout are ttys).
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

/// Which of the three chat surfaces this invocation gets.
///
/// They are three, not two, because "not the deck" covers two different
/// answers to two different questions. `--plain` answers *there is no usable
/// terminal here* (a pipe, a CI log, `TERM=dumb`); `--simple` answers *there
/// is a terminal, but the thing reading it is not a pair of eyes*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatSurface {
    /// The tabbed Command Deck — the default on a capable terminal.
    Deck,
    /// The single-pane accessible surface (#936): inline, one column,
    /// scrollback-backed. See `stella_tui::RunOptions::screen_reader`.
    Simple,
    /// The line REPL, with the reason it was chosen.
    Plain(PlainReason),
}

/// The surface decision, kept pure so all three branches are testable without
/// a pty and without touching the process's real streams.
///
/// The ladder, in order, and each rung is a deliberate choice:
///
/// 1. `--plain` on the command line. The most conservative thing anyone can
///    ask for, asked for explicitly; nothing overrides it.
/// 2. `--simple` on the command line — but only if there is a terminal to
///    draw on. It is an *inline* TUI, so it still needs raw mode and a real
///    stdout; asked for over a pipe it degrades to the REPL and says which
///    stream was the problem, rather than failing.
/// 3. `STELLA_PLAIN`. Below both flags: an environment variable is usually
///    inherited rather than typed, and a user who typed `--simple` because
///    they need it should not be silently overruled by a `STELLA_PLAIN`
///    exported for some other tool's benefit.
/// 4. `STELLA_SIMPLE`, on the same terminal condition as rung 2. This is how
///    a screen-reader user makes the accessible surface their default without
///    typing a flag every time.
/// 5. Stream shape: no tty, no full-screen anything.
/// 6. Otherwise the deck.
pub(crate) fn chat_surface(
    plain_flag: bool,
    simple_flag: bool,
    plain_env: bool,
    simple_env: bool,
    stdin_tty: bool,
    stdout_tty: bool,
) -> ChatSurface {
    // What a full-screen (or inline-TUI) surface needs from the streams. Both
    // the deck and `--simple` enter raw mode, so both fail the same way.
    let stream_fault = if !stdin_tty {
        Some(PlainReason::StdinNotTty)
    } else if !stdout_tty {
        Some(PlainReason::StdoutNotTty)
    } else {
        None
    };
    if plain_flag {
        return ChatSurface::Plain(PlainReason::Flag);
    }
    if simple_flag {
        return match stream_fault {
            Some(reason) => ChatSurface::Plain(reason),
            None => ChatSurface::Simple,
        };
    }
    if plain_env {
        return ChatSurface::Plain(PlainReason::Env);
    }
    if simple_env {
        return match stream_fault {
            Some(reason) => ChatSurface::Plain(reason),
            None => ChatSurface::Simple,
        };
    }
    match stream_fault {
        Some(reason) => ChatSurface::Plain(reason),
        None => ChatSurface::Deck,
    }
}

/// [`chat_surface`] applied to this process's real environment and streams.
pub(crate) fn resolve_chat_surface(plain_flag: bool, simple_flag: bool) -> ChatSurface {
    chat_surface(
        plain_flag,
        simple_flag,
        env_flag("STELLA_PLAIN"),
        env_flag("STELLA_SIMPLE"),
        std::io::stdin().is_terminal(),
        std::io::stdout().is_terminal(),
    )
}

/// This CLI's house convention for a boolean env var: on when present and not
/// `0` (see the module docs).
fn env_flag(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty() && v != "0")
}

/// Whether `chat` should launch the Command Deck specifically — the question
/// `stella resume` asks, because a durable session is a deck feature and
/// neither `--simple` nor `--plain` can reopen one.
pub(crate) fn use_deck(plain_flag: bool, simple_flag: bool) -> bool {
    resolve_chat_surface(plain_flag, simple_flag) == ChatSurface::Deck
}
