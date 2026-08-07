//! The CLI's terminal failure: the message `main` prints and the exit code
//! the process ends with.
//!
//! The whole error chain used to be `Result<(), String>`, which collapses
//! every unhappy ending into `ExitCode::FAILURE` — so a correct loop-abort
//! (the engine detecting a stuck loop, warning, escalating, stopping) exited
//! identically to a segfault, and Harbor scored it as a crash (#1524).
//! [`CliFailure`] keeps the message and adds the one bit the boundary needs.
//! `From<String>` lets the existing string-error helpers keep flowing through
//! `?` unchanged: an unannotated string failure stays a plain failure.
//!
//! The exit-code taxonomy of `stella run`, in one place:
//!
//! | code | meaning |
//! |---|---|
//! | 0 | completed (including an honest abstain) |
//! | 1 | failure: a crash, a hard error, or a failed verification verdict |
//! | 2 | CLI usage error (clap's convention) |
//! | 3 | deliberate stop: the engine (or its user) chose to end the run |
//! | 130/143 | cut short by SIGINT/SIGTERM (`128 + signal`) |

use std::fmt;
use std::process::ExitCode;

use stella_core::AbortKind;

/// Exit code of a deliberate stop ([`AbortKind::DeliberateStop`]): a stuck
/// loop escalated past its warning, an enforced budget or deadline, the step
/// cap, a scope review the user ended, a confident-zero close. Distinct from
/// `1` so a wrapper can tell "the agent chose to stop" from "the agent fell
/// over" without parsing anything — `bench/harbor_adapter` reads exactly
/// this code to score a deliberate stop as a stop rather than a crash.
pub(crate) const DELIBERATE_STOP_EXIT_CODE: u8 = 3;

/// A terminal CLI failure: what to print, and how to exit.
#[derive(Debug)]
pub(crate) struct CliFailure {
    message: String,
    deliberate_stop: bool,
}

impl CliFailure {
    /// A plain failure — exits `1`, exactly as every `Err(String)` always has.
    pub(crate) fn error(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            deliberate_stop: false,
        }
    }

    /// A deliberate stop — exits [`DELIBERATE_STOP_EXIT_CODE`].
    pub(crate) fn deliberate_stop(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            deliberate_stop: true,
        }
    }

    /// The failure an aborted run reports, split by the abort's own typed
    /// kind — the one place the engine's `AbortKind` becomes an exit code.
    pub(crate) fn from_abort(reason: String, kind: AbortKind) -> Self {
        match kind {
            AbortKind::DeliberateStop => Self::deliberate_stop(reason),
            AbortKind::Failure => Self::error(reason),
        }
    }

    /// Whether this failure is a deliberate stop — the same bit
    /// [`Self::exit_code`] reads, exposed so the session registry can record
    /// a policy stop distinctly from a crash (#1653).
    pub(crate) fn is_deliberate_stop(&self) -> bool {
        self.deliberate_stop
    }

    pub(crate) fn exit_code(&self) -> ExitCode {
        if self.deliberate_stop {
            ExitCode::from(DELIBERATE_STOP_EXIT_CODE)
        } else {
            ExitCode::FAILURE
        }
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    /// Whether the run *chose* to end rather than fell over — the same bit
    /// [`Self::exit_code`] turns into `3`, asked directly.
    ///
    /// The terminal-status writers need it too (#1653): a policy stop and a
    /// crash are one `SessionStatus` apart, and without this accessor the
    /// distinction is thrown away one line before the registry write.
    pub(crate) fn is_deliberate_stop(&self) -> bool {
        self.deliberate_stop
    }
}

impl From<String> for CliFailure {
    fn from(message: String) -> Self {
        Self::error(message)
    }
}

impl fmt::Display for CliFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #1524 witness: a deliberate stop is distinguishable at the process
    /// boundary. On the old `Result<(), String>` chain this distinction did
    /// not exist — every abort exited `1`.
    #[test]
    fn a_deliberate_stop_exits_with_its_own_code() {
        let failure = CliFailure::from_abort(
            "stuck-loop detected (persisted after a steering warning): …".into(),
            AbortKind::DeliberateStop,
        );
        assert_eq!(
            failure.exit_code(),
            ExitCode::from(DELIBERATE_STOP_EXIT_CODE)
        );
    }

    #[test]
    fn a_failed_run_keeps_the_generic_failure_exit() {
        let failure = CliFailure::from_abort("model call failed: 500".into(), AbortKind::Failure);
        assert_eq!(failure.exit_code(), ExitCode::FAILURE);
    }

    #[test]
    fn a_bare_string_error_stays_a_plain_failure() {
        let failure = CliFailure::from(String::from("reading TASK.md: not found"));
        assert_eq!(failure.exit_code(), ExitCode::FAILURE);
        assert_eq!(failure.message(), "reading TASK.md: not found");
    }
}
