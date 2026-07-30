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

/// Whether `chat` should launch the Command Deck: an explicit `--plain` or
/// STELLA_PLAIN=1 opts out, and both stdin and stdout must be real terminals
/// (raw mode + the alternate screen are meaningless on a pipe).
pub(crate) fn use_deck(plain_flag: bool) -> bool {
    let plain_env = std::env::var_os("STELLA_PLAIN").is_some_and(|v| !v.is_empty() && v != "0");
    !plain_flag && !plain_env && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}
