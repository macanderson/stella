//! **Whether a human is present to see and answer a prompt this process
//! might print, mid-run.** The single pure derivation of that fact, so two
//! call sites can never disagree about whether somebody is there to ask.
//!
//! # Why this is its own crate
//!
//! This predicate has two consumers on opposite sides of a dependency
//! direction invariant 1 (`AGENTS.md`) forbids: `stella-cli`'s own
//! scope-review and approval prompts, and `stella-model`'s interactive
//! credential prompt (`credential.rs`) — which must never depend on
//! `stella-cli`, the binary above it (#3036). `stella-home` is the
//! obvious-looking home (both crates already depend on it), but its own
//! README draws the boundary explicitly: "this crate owns one decision: what
//! path a process should treat as the stella home... everything else is
//! out." A human-presence fact is not a path. `stella-home`'s README also
//! names the rule this crate satisfies: a new leaf is warranted when
//! functionality needs a dependency direction the current graph forbids —
//! exactly this shape, and the same one `stella-diff` and `stella-embed`
//! were split out for (#1139, #1511).
//!
//! # The three inputs, and why they stay three separate booleans
//!
//! Asking costs a human two capabilities, and both are needed: they must SEE
//! the prompt and they must be able to ANSWER it. So a run has a human
//! exactly when:
//!
//! - `interactive_output` — this run would render interactive text at all.
//!   `false` for a machine output format (`--output-format json`) or an
//!   invocation that has otherwise forbidden prompting, regardless of what
//!   the terminal looks like.
//! - `stdin_is_terminal` — the human can answer. A piped or redirected stdin
//!   can accept bytes but never a keystroke meant for this question.
//! - `prompt_is_visible` — the human can see the question in the first
//!   place, on whichever stream this caller's prompt actually renders on.
//!   `stella-cli`'s approval responder prints via `println!`, so its
//!   callers pass `stdout().is_terminal()`. The daemon
//!   console's scope-review prompt is deliberately stderr-only — stdout there
//!   is the run's own byte-verbatim relay, and folding a prompt into it would
//!   corrupt a machine-format caller reading that stream — so its callers
//!   pass `stderr().is_terminal()` instead. `stella-model`'s credential
//!   prompt goes through `rpassword`, whose default configuration writes to
//!   `/dev/tty` directly on Unix rather than stdout; checking
//!   `stdout().is_terminal()` there is a proxy for that, not an exact match
//!   (tracked as its own gap, not fixed here: #3052).
//!
//! Deliberately pure over already-observed booleans rather than reading
//! `std::io` itself: each consumer's three inputs come from a genuinely
//! different place (a supervised child's stdin is a parked file with no real
//! terminal to query; a JSON-format run's `interactive_output` is `false`
//! independent of any terminal at all), so computing them here would either
//! narrow this crate to one caller's shape or grow an API this crate has no
//! business owning. Purity is also what makes the condition directly
//! unit-testable without faking a terminal.
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
