//! **Whether a human is present to see and answer a prompt this process
//! might print, mid-run.** The single pure derivation of that fact, so two
//! call sites can never disagree about whether somebody is there to ask.
//!
//! See README.md for why this is a separate crate with no dependencies.
//!
//! A run has a human exactly when all three hold:
//!
//! - `interactive_output` — this run would render interactive text at all.
//!   `false` for a machine output format (`--output-format json`) or an
//!   invocation that has otherwise forbidden prompting.
//! - `stdin_is_terminal` — the human can answer. A piped or redirected stdin
//!   can accept bytes but never a keystroke meant for this question.
//! - `prompt_is_visible` — the human can see the question, on whichever
//!   stream this caller's prompt actually renders on (stdout, stderr, or
//!   whatever the caller checks).
//!
//! `stella-model`'s credential prompt goes through `rpassword`, which writes
//! to `/dev/tty` directly on Unix instead of stdout. Checking
//! `stdout().is_terminal()` there is only a close stand-in, not an exact
//! match (tracked separately: #3052).
#[must_use]
pub fn human_can_answer(
    interactive_output: bool,
    stdin_is_terminal: bool,
    prompt_is_visible: bool,
) -> bool {
    interactive_output && stdin_is_terminal && prompt_is_visible
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full truth table: every one of the three inputs is load-bearing on
    /// its own, so a caller cannot drop one and still get the right answer by
    /// accident.
    #[test]
    fn all_three_conditions_are_independently_required() {
        assert!(
            human_can_answer(true, true, true),
            "every condition met has a human"
        );
        assert!(
            !human_can_answer(false, true, true),
            "a machine output format has nobody to ask, whatever the terminal looks like"
        );
        assert!(
            !human_can_answer(true, false, true),
            "a piped stdin cannot answer even if the prompt is visible"
        );
        assert!(
            !human_can_answer(true, true, false),
            "a prompt nobody can see must not be sent, even with a live stdin"
        );
    }
}
