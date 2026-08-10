//! The deterministic execution receipt handed to a worker revision turn.

use crate::verify::normalize_command;

/// A completed failed test execution. The runner has already bounded both
/// streams; no reviewer or guidance prose is added to this receipt.
pub(super) struct TestFailureReceipt<'a> {
    pub(super) command: &'a str,
    pub(super) exit_code: i32,
    pub(super) stdout: &'a str,
    pub(super) stderr: &'a str,
}

impl TestFailureReceipt<'_> {
    fn render(&self) -> String {
        format!(
            "command: {}\nexit_code: {}\nstdout:\n{}\nstderr:\n{}",
            normalize_command(self.command),
            self.exit_code,
            self.stdout,
            self.stderr
        )
    }
}

pub(super) enum RevisionCause<'a> {
    TestFailure(TestFailureReceipt<'a>),
}

pub(super) fn revision_prompt(cause: RevisionCause<'_>) -> String {
    let RevisionCause::TestFailure(receipt) = cause;
    format!(
        "Verification did not pass. The deterministic test execution reported:\n{}\n\n\
         Use this result to diagnose the failure, try a different solution, and run the same \
         command again.",
        receipt.render()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_failure_receipt_preserves_the_bounded_execution_result() {
        let prompt = revision_prompt(RevisionCause::TestFailure(TestFailureReceipt {
            command: "  cargo   test -p x ",
            exit_code: 101,
            stdout: "one failed",
            stderr: "assertion failed",
        }));
        assert!(prompt.contains("command: cargo test -p x"), "{prompt}");
        assert!(prompt.contains("exit_code: 101"), "{prompt}");
        assert!(prompt.contains("stdout:\none failed"), "{prompt}");
        assert!(prompt.contains("stderr:\nassertion failed"), "{prompt}");
        assert!(!prompt.contains("reviewer"), "{prompt}");
    }
}
