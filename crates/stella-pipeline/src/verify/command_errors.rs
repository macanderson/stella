//! The errored-command census (#2125): the deterministic channel that lets the
//! verify stage weigh a measurement standing on a broken command chain.
//!
//! # The hole this closes
//!
//! #1957 landed the worker-side half as a prompt rule
//! (`measurement_discipline!`, `stella-cli/src/agent/prompt.rs`): a number is
//! VOID if any command in the chain that produced it reported an error, even
//! when the overall exit code is 0 — a shell pipeline's status is its last
//! command's, and a failed command substitution does not propagate. That
//! steers the worker. Nothing caught the worker that cited the number anyway.
//!
//! The verify stage structurally could not catch it either, and that is
//! **deliberate architecture, not a gap**: the verifier sees the goal, the
//! deterministic evidence summary and the bounded diff — never the worker's
//! tool transcript (verifier ≠ worker, L-E11, hardened by #1795). The stderr
//! blocks that void a measurement live only in that transcript. On the
//! Terminal-Bench 2.1 `git-multibranch` trace behind #1957 they read
//! `bc: command not found` under an empty timing capture, and
//! `fatal: detected dubious ownership` under a cited "70 ms"; the claim was
//! unfalsifiable at verdict time.
//!
//! # Why a census rather than the transcript
//!
//! Feeding the transcript to the verifier would buy the signal at the price of
//! the independence the whole stage rests on. So the fact travels as a
//! **count**, produced by the pipeline's own observation of the tool stream and
//! carried in the trusted zone of the prompt exactly like `mutating_actions`:
//! no worker-authored byte, no stderr text, nothing a change under review can
//! author. The verifier learns *that* commands errored under a zero exit, never
//! *what they said* — which is all a weighing needs and all invariant 3's
//! content-free discipline permits.
//!
//! # What is counted, and what deliberately is not
//!
//! Only the silent shape: a shell result that **exited 0** while its captured
//! stderr named a failed command. A command that exits non-zero is already
//! loud — the worker saw it, the ladder's other channels can see its effects,
//! and healthy runs are full of them (the failing baseline a flip is measured
//! from is one). Counting those would put a large, meaningless number in front
//! of a verifier on every good run, and a number a reader must learn to ignore
//! is worse than no number.
//!
//! The census also **never decides anything**. It is not read by
//! [`crate::verify::ladder_decision`] and cannot withhold a pass: an errored
//! probe makes a cited quantity *unsubstantiated*, not *disproven*, and the
//! verifier instructions say so in those words. Its absence is likewise a dark
//! channel — never evidence that a run's commands were clean.

use stella_protocol::ToolOutput;

use crate::flip_halt::exit_status;

/// The evidence-summary key this census travels under.
///
/// One constant because two readers must agree on it: the per-call summary
/// (`crate::pipeline::evidence`) emits it, and the verifier's fixed
/// instruction block defines it. The vocabulary the instructions define and
/// the vocabulary the summary emits have drifted apart before — the guard
/// outlives the thing it guards when each side spells its own copy.
pub const EVIDENCE_KEY: &str = "errored_commands";

/// The marker the shell tool prefixes a command's captured stderr with
/// (`stella-tools/src/bash.rs` renders `stdout`, then `[stderr]`, then
/// `[exit code: N]`).
///
/// Parsed rather than plumbed for the same reason
/// [`crate::flip_halt::exit_status`] parses its own marker: `ToolOutput::Ok`
/// carries only the text the model reads, and the stream/exit split is not
/// carried anywhere else. Gating on this marker is also the false-positive
/// defense — a command whose *stdout* happens to print `command not found`
/// (a grep over a log, a test asserting the message) is not stderr and is not
/// counted.
const STDERR_MARKER: &str = "[stderr]";

/// Whether one line of captured stderr reports a failed command.
///
/// A closed, case-sensitive vocabulary rather than a general "looks like an
/// error" heuristic, in the spirit of
/// [`crate::witness::warrant::diff_accountable_mutator`]: only spellings this
/// pipeline can vouch for are listed, each is a diagnostic no ordinary
/// successful command emits, and the cost of a missing entry is a census that
/// undercounts — never one that invents a failure the run did not have.
///
/// * `command not found` — bash/zsh (`bash: line 1: bc: command not found`).
/// * a line ending `: not found` — dash/sh/busybox (`sh: 1: bc: not found`).
/// * a line opening `fatal:` — git and its family
///   (`fatal: detected dubious ownership in repository at '/app'`).
fn line_reports_a_failed_command(line: &str) -> bool {
    let line = line.trim();
    line.contains("command not found") || line.ends_with(": not found") || line.starts_with("fatal:")
}

/// Whether a shell result's captured stderr block names a failed command.
///
/// The **last** marker, not the first, mirroring
/// [`crate::flip_halt::exit_status`]: the harness appends this block after the
/// command's stdout, so a command whose own stdout quotes the marker must not
/// redirect the scan onto its own output.
fn stderr_reports_a_failed_command(text: &str) -> bool {
    let Some(marker) = text.rfind(STDERR_MARKER) else {
        return false;
    };
    // `skip(1)` drops the marker's own line; the trailing `[exit code: N]`
    // line matches no signature and needs no special case.
    text[marker..]
        .lines()
        .skip(1)
        .any(line_reports_a_failed_command)
}

/// Whether one finished tool call is a command chain that reported an error
/// while exiting 0 — the shape a measurement claim can silently stand on.
///
/// Deliberately blind to which [`ToolOutput`] arm carries the text. The shell
/// tool puts the same rendered capture in both (`Ok` on a zero exit, `Error`
/// otherwise), and an MCP shell may split them differently; the exit marker is
/// the authority on the status either way, and a result with no marker is not
/// a command chain at all and says nothing.
#[must_use]
pub fn exited_zero_with_a_failed_command(output: &ToolOutput) -> bool {
    let text = match output {
        ToolOutput::Ok { content } => content,
        ToolOutput::Error { message } => message,
    };
    exit_status(text) == Some(0) && stderr_reports_a_failed_command(text)
}

/// The trusted-zone clause for a census, or `None` when nothing was observed.
///
/// `None` rather than `errored_commands=0` on purpose, matching every other
/// conditional channel in the summary: silence means the probe had nothing to
/// say. Printing a zero would read as "this run's commands were verified
/// clean", which is a claim this channel is not built to make — the vocabulary
/// above is closed, so an unlisted spelling passes unseen.
#[must_use]
pub fn evidence_clause(errored_commands: u32) -> Option<String> {
    (errored_commands > 0).then(|| {
        format!(
            "; {EVIDENCE_KEY}={errored_commands} (command chain(s) that exited 0 while \
             stderr reported a failed command)"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(content: &str) -> ToolOutput {
        ToolOutput::Ok {
            content: content.to_string(),
        }
    }

    /// The #1957 trace's own two shapes, verbatim from the `git-multibranch`
    /// evidence: a missing `bc` under an empty timing capture, and git
    /// refusing a repository under a cited duration. Both exit 0.
    #[test]
    fn the_measured_failure_shapes_are_counted() {
        assert!(exited_zero_with_a_failed_command(&shell(
            "\n[stderr]\nbash: line 1: bc: command not found\n[exit code: 0]"
        )));
        assert!(exited_zero_with_a_failed_command(&shell(
            "70\n[stderr]\nfatal: detected dubious ownership in repository at '/app'\n\
             [exit code: 0]"
        )));
        // dash/busybox spelling of the same failure.
        assert!(exited_zero_with_a_failed_command(&shell(
            "\n[stderr]\nsh: 1: bc: not found\n[exit code: 0]"
        )));
    }

    /// A clean run must stay silent: stderr that carries an ordinary progress
    /// line is not a failed command, and a result with no stderr block at all
    /// is not one either.
    #[test]
    fn ordinary_output_is_not_a_failed_command() {
        assert!(!exited_zero_with_a_failed_command(&shell(
            "3 passed\n[exit code: 0]"
        )));
        assert!(!exited_zero_with_a_failed_command(&shell(
            "ok\n[stderr]\n   Compiling stella-pipeline v0.7.9\n[exit code: 0]"
        )));
    }

    /// The false-positive defense: the phrase in a command's own STDOUT — a
    /// grep over a log, a test asserting the message — is not stderr, and a
    /// census that counted it would void honest measurements.
    #[test]
    fn the_phrase_in_stdout_is_not_counted() {
        assert!(!exited_zero_with_a_failed_command(&shell(
            "log.txt:14: bash: bc: command not found\n[exit code: 0]"
        )));
        // ...and a quoted marker inside stdout must not redirect the scan onto
        // stdout when a real stderr block follows it.
        assert!(!exited_zero_with_a_failed_command(&shell(
            "printing [stderr] and bc: command not found\n[stderr]\nwarning: slow\n\
             [exit code: 0]"
        )));
    }

    /// A non-zero exit is the loud case this census deliberately excludes: the
    /// worker saw it, and counting the failing baseline every flip is measured
    /// from would put a meaningless number in front of the verifier on every
    /// healthy run.
    #[test]
    fn a_loud_failure_is_not_the_silent_shape() {
        assert!(!exited_zero_with_a_failed_command(&ToolOutput::Error {
            message: "\n[stderr]\nbash: line 1: bc: command not found\n[exit code: 127]".into(),
        }));
    }

    /// A result that is not a command chain at all — no exit marker — says
    /// nothing in either direction. Read/write tools land here.
    #[test]
    fn a_non_shell_result_is_never_a_command_error() {
        assert!(!exited_zero_with_a_failed_command(&shell(
            "wrote 3 lines to src/lib.rs"
        )));
        assert!(!exited_zero_with_a_failed_command(&ToolOutput::Error {
            message: "no such file: src/nope.rs".into(),
        }));
    }

    /// The clause is emitted only when something was observed — a printed
    /// `=0` would read as a clean bill of health this channel cannot issue.
    #[test]
    fn the_clause_is_silent_when_nothing_was_observed() {
        assert_eq!(evidence_clause(0), None);
        let clause = evidence_clause(2).expect("a non-zero census is stated");
        assert!(clause.contains("errored_commands=2"), "{clause}");
        assert!(clause.contains("exited 0"), "{clause}");
    }
}
