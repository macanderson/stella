// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `--detach`: hand the run to the supervisor and give the shell back (#1607).
//!
//! Supervision (`super`) already makes a run outlive its terminal. What it
//! does not do is give the *prompt* back: the launching process stays for the
//! whole run, streaming the two console files, and `Ctrl-C` there is a stop
//! rather than a detach — deliberately, because that is what the key means
//! everywhere else. So on today's surface there is no way to start a run and
//! keep using the terminal you started it from, and no way at all to start one
//! from a script that then exits.
//!
//! That gap is what `--detach` closes, and it is the surviving half of #1594's
//! intent. Everything else #1594 proposed — a `stella daemon` command group, a
//! console in the session sidecar, `setsid`, the liveness lock, stop
//! escalation, a single registry rather than a second one — landed instead
//! through #1591/#1603/#1640 and is reused here verbatim. This module is
//! therefore very small on purpose: **a detached launch is an attached launch
//! that does not stay**, and any second implementation of the launch would be
//! the divergence that supervision's own `watch` doc warns about.
//!
//! # Two differences from an attached launch, both deliberate
//!
//! 1. **A terminal is not required.** [`super::should_supervise`] answers "no"
//!    without a controlling terminal, because a run with no terminal to lose
//!    is already immune to the hangup supervision exists to prevent. `--detach`
//!    is asked for a different property — *this process returns now* — which a
//!    pipe, a CI step, and a `cron` job want exactly as much as a terminal
//!    does. So the flag forces supervision rather than merely permitting it.
//! 2. **The exit code is the launch's, not the run's.** An attached
//!    supervisor forwards the child's status
//!    ([`super::forwarded_exit_code`]); a detached one has returned long
//!    before there is a status to forward, so `stella run --detach; echo $?`
//!    answers "did the launch succeed". The run's own answer arrives in
//!    `stella daemon list`.
//!
//! # Why `--foreground` wins over `--detach` instead of conflicting with it
//!
//! Clap could reject the pair outright, and that would be wrong here: the
//! supervised child re-parses its parent's argv **verbatim** (`--detach`
//! included) and is told to do the work through `STELLA_FOREGROUND=1` in its
//! environment — which clap reads as the `--foreground` flag. A declared
//! conflict fires on env-sourced values too, so every detached child would die
//! in argument parsing before it ran a turn. Precedence in [`posture`] is what
//! keeps that path open, and it also happens to be the safer reading of a
//! contradictory command line: the flag that asks for *less* machinery wins.

use super::Supervised;

/// What this invocation does with the supervisor.
///
/// Three states rather than the `bool` #1591 used, because "supervise" and
/// "stay and stream it" stopped being the same question once `--detach`
/// existed. Computed once, from flags and the terminal, by [`posture`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Posture {
    /// Do the work in this process. Older releases' only shape.
    Foreground,
    /// Hand the work to a supervised child and stream it back here until it
    /// ends — the default for a run typed at a terminal.
    Attached,
    /// Hand the work to a supervised child and return immediately.
    Detached,
}

impl Posture {
    /// Whether the work goes to a supervised child at all.
    pub(crate) fn supervises(self) -> bool {
        !matches!(self, Self::Foreground)
    }
}

/// Resolve the posture from the two flags, the recursion backstop, and the
/// terminal.
///
/// Pure over already-observed booleans for the same reason
/// [`super::should_supervise`] is — the caller does the observing, so the
/// decision is testable as arithmetic. It is layered *on* that function rather
/// than beside it so the "which invocations are supervised" rule keeps exactly
/// one owner.
///
/// Order matters and encodes three rules:
///
/// - `--foreground` (or `STELLA_FOREGROUND`) wins over everything, including
///   `--detach`. See the module docs for why this is precedence, not a clap
///   conflict.
/// - A process that is already the supervised child never supervises again,
///   `--detach` or not. This is the recursion backstop, and `--detach` riding
///   in the child's verbatim argv is precisely the case it has to survive.
/// - `--detach` then forces supervision, terminal or no terminal.
pub(crate) fn posture(
    foreground: bool,
    detach: bool,
    already_supervised: bool,
    has_controlling_terminal: bool,
) -> Posture {
    if foreground || already_supervised {
        return Posture::Foreground;
    }
    if detach {
        return Posture::Detached;
    }
    if super::should_supervise(foreground, already_supervised, has_controlling_terminal) {
        Posture::Attached
    } else {
        Posture::Foreground
    }
}

/// Let go of a just-spawned supervised run and tell the terminal how to find
/// it again.
///
/// Taking [`Supervised`] **by value** is the whole mechanism: dropping the
/// handle closes this process's ends of the two console tails and drops the
/// `Child`, which on Unix neither signals nor reaps — the child is already a
/// session leader with its own console files and its own liveness lock, so it
/// notices nothing. The launcher then returns, and the run is reparented to
/// init.
///
/// The announcement is printed on **stderr** for the same reason the attached
/// banner is: `--output-format json` writes the machine-readable answer to
/// stdout, and a launcher that garnished it would break every script that
/// pipes one.
pub(crate) fn release(run: Supervised) -> Result<(), String> {
    use colored::Colorize;

    eprintln!(
        "{} {} — running in the background",
        "▸ detached".green().bold(),
        run.id.dimmed()
    );
    eprintln!(
        "  follow it with {}",
        format!("stella daemon attach {}", run.id).cyan()
    );
    eprintln!(
        "  stop it with   {}",
        format!("stella daemon stop {}", run.id).cyan()
    );
    drop(run);
    Ok(())
}

#[cfg(test)]
mod tests;
