//! Stop the execute turn the moment the tracked test goes fail→pass.
//!
//! # Why this exists
//!
//! Every other way a Stella turn ends is a *limit* being reached: the model
//! stops asking for tools, the budget bites, the step cap trips, the harness
//! kills the process. None of them is "the work is done", so an agent that
//! has already succeeded keeps spending until one of them fires.
//!
//! Measured on Terminal-Bench 2.1, run `or1`, task `pypi-server`: Stella wrote
//! a correct solution, then spent the rest of its wall clock re-litigating
//! whether the grader could see its commits, and was killed mid-sentence. Its
//! run envelope reads `"status": "interrupted"` with the final text
//!
//! > "The push tool expects a pre-configured remote that doesn't exist — this
//! > suggests my manual `git init` created a disconnected repo that the
//! > verification system's 'diff' doesn't see."
//!
//! The trial still scored 1.0, but only because the grader reads files off
//! disk *after* the agent phase. The same behaviour on a task where the late
//! flailing edits the answer turns a pass into a failure — and on this panel
//! the clock is already the dominant failure mode.
//!
//! # Why the oracle is the right signal
//!
//! [`crate::verify::FlipOracle`] already encodes what "done" means, and does
//! it conservatively: it locks onto the first *failing* run of a normalized
//! command and only reports flipped when that same command later passes. A
//! command that was passing all along never flips, so this can never end a
//! turn on a test that was green before the agent touched anything.
//!
//! Before this module the oracle was consulted at exactly two moments — a
//! pre-execute baseline and post-execute verification — so the agent's own
//! test runs, the ones that tell it the work is finished, were invisible to
//! it. This watches those runs as they happen.
//!
//! # What it deliberately does not do
//!
//! It does not decide the verdict. The authoritative oracle in
//! `CandidateState` still runs the whole ladder afterwards, including the
//! confirmation re-run (#859), the lint regression veto (#861) and the
//! mutation check (#870). This only decides when to stop *working*; whether
//! the work is credited is a separate question with its own, stricter,
//! machinery.

use std::sync::atomic::{AtomicBool, Ordering};

use stella_core::driver::TurnHalt;

use crate::verify::normalize_command;

/// The marker the bash tool appends to a command's output.
///
/// Parsed rather than plumbed because the exit status genuinely is not
/// carried anywhere else: `ToolOutput::Ok` holds only the text the model
/// reads, and its `Error` arm means the *tool* failed (spawn, timeout), which
/// is not the same as the command exiting non-zero. A test that fails is a
/// perfectly successful tool call.
const EXIT_MARKER: &str = "[exit code:";

/// Reads an exit status out of a bash tool result's content.
///
/// Returns `None` when no marker is present, which is the honest answer for
/// every tool that is not the shell — and `None` never advances the oracle,
/// so an unrecognised result can only fail to stop a turn, never stop one
/// early.
#[must_use]
pub fn exit_status(content: &str) -> Option<i32> {
    // Last marker, not first: a command whose own output quotes the phrase
    // must not outrank the one the harness appended at the end.
    let start = content.rfind(EXIT_MARKER)? + EXIT_MARKER.len();
    let rest = &content[start..];
    let end = rest.find(']')?;
    rest[..end].trim().parse().ok()
}

/// The bash-ish argument key a shell tool carries its command line under.
///
/// A list rather than one name because the shell tool has been spelled more
/// than one way across hosts and MCP servers; an unknown shape yields `None`
/// and simply never contributes an observation.
const COMMAND_KEYS: &[&str] = &["command", "cmd", "script"];

/// Pull a shell command line out of a tool call's arguments.
#[must_use]
pub fn command_of(input: &serde_json::Value) -> Option<&str> {
    COMMAND_KEYS
        .iter()
        .find_map(|key| input.get(*key).and_then(serde_json::Value::as_str))
}

/// Latches when the tracked test is observed going fail→pass mid-turn, and
/// reports that as a reason for the engine to stop at its next step boundary.
///
/// Latched, never re-read from live state: a flip that happened cannot
/// un-happen, and a predicate that flickered would let the turn continue past
/// the moment it should have ended.
#[derive(Debug)]
pub struct FlipHalt {
    /// The tracked command in normalized form — compared, and quoted back in
    /// the halt reason so a reader can see which command ended the turn.
    tracked: String,
    flipped: AtomicBool,
}

impl FlipHalt {
    /// Watch `tracked`, the command whose baseline the candidate already
    /// observed failing.
    #[must_use]
    pub fn new(tracked: &str) -> Self {
        Self {
            tracked: normalize_command(tracked),
            flipped: AtomicBool::new(false),
        }
    }

    /// The command being watched, normalized.
    #[must_use]
    pub fn tracked(&self) -> &str {
        &self.tracked
    }

    /// Whether the flip has latched.
    #[must_use]
    pub fn is_flipped(&self) -> bool {
        self.flipped.load(Ordering::SeqCst)
    }

    /// Feed one finished shell call. Returns `true` if this observation
    /// latched the flip.
    ///
    /// Requires BOTH that the command normalizes to the tracked one and that
    /// it exited zero. Anything else — a different command, a failure, output
    /// with no exit marker — leaves the latch exactly as it was.
    pub fn observe(&self, command: &str, content: &str) -> bool {
        if normalize_command(command) != self.tracked {
            return false;
        }
        if exit_status(content) != Some(0) {
            return false;
        }
        // `swap` rather than `store`: the caller emits a proof step on the
        // transition, and two concurrent tool results finishing together must
        // not both claim to be the one that flipped it.
        !self.flipped.swap(true, Ordering::SeqCst)
    }
}

impl TurnHalt for FlipHalt {
    fn halt_reason(&self) -> Option<String> {
        self.is_flipped().then(|| {
            format!(
                "the tracked test now passes (`{}` went fail→pass), so the goal is met — stopping \
                 here rather than spending the rest of the turn against a solved task",
                self.tracked
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_trailing_exit_marker() {
        assert_eq!(exit_status("all good\n[exit code: 0]"), Some(0));
        assert_eq!(exit_status("boom\n[exit code: 1]"), Some(1));
        assert_eq!(exit_status("no marker here"), None);
    }

    #[test]
    fn a_quoted_marker_does_not_outrank_the_real_one() {
        // A test that prints the phrase must not be read as its own status.
        let content = "checking [exit code: 1] handling\n[exit code: 0]";
        assert_eq!(exit_status(content), Some(0));
    }

    #[test]
    fn flips_only_on_the_tracked_command_passing() {
        let halt = FlipHalt::new("pytest -q");
        assert!(halt.halt_reason().is_none());

        // A different command passing is not the signal.
        assert!(!halt.observe("cargo test", "ok\n[exit code: 0]"));
        assert!(halt.halt_reason().is_none());

        // The tracked command still failing is not the signal.
        assert!(!halt.observe("pytest -q", "1 failed\n[exit code: 1]"));
        assert!(halt.halt_reason().is_none());

        // The tracked command passing is.
        assert!(halt.observe("pytest -q", "3 passed\n[exit code: 0]"));
        assert!(halt.halt_reason().is_some());
    }

    #[test]
    fn the_latch_fires_once_and_stays_set() {
        let halt = FlipHalt::new("pytest -q");
        assert!(halt.observe("pytest -q", "ok\n[exit code: 0]"));
        // Second observation does not re-announce the transition...
        assert!(!halt.observe("pytest -q", "ok\n[exit code: 0]"));
        // ...but the latch — and therefore the stop — survives.
        assert!(halt.is_flipped());
        assert!(halt.halt_reason().is_some());
    }

    #[test]
    fn a_later_failure_cannot_unlatch_a_flip() {
        // The turn must not resume because a subsequent unrelated run failed;
        // the ladder, not this, decides whether the pass is trustworthy.
        let halt = FlipHalt::new("pytest -q");
        assert!(halt.observe("pytest -q", "ok\n[exit code: 0]"));
        assert!(!halt.observe("pytest -q", "1 failed\n[exit code: 1]"));
        assert!(halt.is_flipped());
    }

    #[test]
    fn output_without_an_exit_marker_never_stops_a_turn() {
        let halt = FlipHalt::new("pytest -q");
        assert!(!halt.observe("pytest -q", "3 passed"));
        assert!(!halt.is_flipped());
    }

    #[test]
    fn a_shell_command_is_read_from_any_known_argument_key() {
        let bash = serde_json::json!({"command": "pytest -q"});
        assert_eq!(command_of(&bash), Some("pytest -q"));
        let alt = serde_json::json!({"cmd": "make test"});
        assert_eq!(command_of(&alt), Some("make test"));
        let unrelated = serde_json::json!({"path": "src/lib.rs"});
        assert_eq!(command_of(&unrelated), None);
    }
}
