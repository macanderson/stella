//! Out-of-memory recognition (#1294): telling "the machine ran out of memory"
//! apart from "the test failed".
//!
//! # Why they cannot share an outcome
//!
//! The two mean opposite things. A failing test is a statement about the
//! *code* — fix it. An OOM kill is a statement about the *machine*: the run
//! observed no assertion at all, so nothing was learned about the code. Report
//! the second as the first and the pipeline tells a model its correct work was
//! wrong; the model then "fixes" something that was never broken, spending a
//! revision to make working code worse. This is the same class of confusion
//! [`crate::ports::CmdKind::TimedOut`] closed in #860, one cause further out.
//!
//! # What is actually observable
//!
//! On Linux the OOM killer's only instrument is `SIGKILL`, delivered to the
//! process it chose. That leaves three observable traces, in descending order
//! of how much they prove:
//!
//! 1. **The signal itself.** `WIFSIGNALED` with `SIGKILL` — what
//!    [`std::os::unix::process::ExitStatusExt::signal`] reports.
//! 2. **`128 + 9 = 137`**, which is how a shell (and anything that reports a
//!    child's death as an exit code) spells the same fact one indirection out.
//! 3. **The runtime's own message.** A managed runtime often notices first and
//!    dies on its own terms — `MemoryError`, `std::bad_alloc`,
//!    `OutOfMemoryError`, `JavaScript heap out of memory` — with an ordinary
//!    non-zero exit code that carries no signal at all.
//!
//! # What is deliberately not read
//!
//! The kernel *also* logs the kill (`Out of memory: Killed process …`), and
//! the issue that prompted this names the log as a source. This module does
//! not read it, and that is a decision rather than an omission: `dmesg` is
//! restricted on hardened kernels (`kernel.dmesg_restrict`), the log is not
//! namespaced, so inside a container it shows kills belonging to other
//! workloads, and attributing a line to *this* run means matching a pid that
//! the kernel prints for the process it killed — which may be a grandchild the
//! runner never knew about. A wrong attribution here is worse than a missed
//! one: it converts a genuine test failure into "we learned nothing" and
//! retries it forever. What is honored is the log *text* when it reaches this
//! run's own output (some CI harnesses splice it in) — see [`OOM_MARKERS`].
//!
//! # Failing open, deliberately
//!
//! Everything here degrades toward "an ordinary completed run". An
//! unrecognized OOM is the status quo — a test failure the worker revises,
//! which is at worst a wasted revision. A *fabricated* OOM would be worse: a
//! genuine, reproducible failure re-run until the retry budget is gone and
//! then handed to the verifier as inconclusive, hiding a real defect behind
//! "nothing was learned". So the markers are distinctive runtime phrases, a
//! passing run is never OOM whatever it printed, and no heuristic here fires
//! on a bare non-zero exit.

/// The signal the kernel's OOM killer sends. Not read from `libc` because
/// this crate holds no platform dependency (invariant #2: no I/O in the
/// engine) — the number is fixed by POSIX, and the runner that observes it
/// passes it in.
pub const SIGKILL: i32 = 9;

/// How a shell reports a `SIGKILL`ed child: `128 + SIGKILL`.
pub const SIGKILL_EXIT_CODE: i32 = 128 + SIGKILL;

/// Runtime-authored out-of-memory messages, lowercase, matched as substrings
/// against a failing run's combined output.
///
/// Every entry names a *runtime dying of allocation failure*, never a generic
/// word like "memory": the phrases are long enough that a test asserting on
/// them is writing about OOM handling on purpose. That is the trade — a suite
/// whose own fixtures print `std::bad_alloc` will have a failing run read as
/// OOM and retried once — and it is the cheap direction, because the retry
/// reproduces the failure and the second observation stands.
pub const OOM_MARKERS: &[&str] = &[
    // Linux kernel's own line, when a harness splices it into the run's
    // output. Also covers `oom-killer: ...` invocation lines.
    "out of memory",
    "oom-killer",
    "oom_kill",
    "memory cgroup out of memory",
    // Java / Kotlin / Scala.
    "outofmemoryerror",
    // libc / C++.
    "cannot allocate memory",
    "std::bad_alloc",
    // Rust's allocation-failure abort.
    "memory allocation of",
    // Python.
    "memoryerror",
    // Go.
    "runtime: out of memory",
    // Cargo/libtest reporting a signal-killed child process, which is how a
    // `cargo test` run whose harness was the OOM victim surfaces: the parent
    // exits 101 having printed the child's signal.
    "signal: 9, sigkill",
];

/// What a runner observed about how one process ended, as the facts a
/// platform actually reports — the input to [`killed_by_oom`].
///
/// Deliberately not `std::process::ExitStatus`: this crate must stay free of
/// platform APIs (invariant #2), and the runner is the only party that can
/// read a Unix signal anyway. Passing the extracted facts keeps the decision
/// pure and directly testable, which matters more here than anywhere — an OOM
/// kill is famously hard to trigger on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ExitFacts<'a> {
    /// The Unix termination signal, when the process was killed by one.
    /// `None` on a normal exit, and on every non-Unix platform.
    pub signal: Option<i32>,
    /// The process's real exit code, when it exited normally. `None` when it
    /// was signalled (there is no code in that case) or the platform could
    /// not report one.
    pub exit_code: Option<i32>,
    /// The run's combined stdout+stderr tail, as the runner captured it.
    pub output: &'a str,
}

/// Whether this run was killed for running out of memory (#1294).
///
/// The three rules, in the order they are checked and with what each one
/// costs if it is wrong:
///
/// 1. **`SIGKILL`.** On Linux the OOM killer sends nothing else, and a
///    runner's *own* deadline kill is classified [`crate::ports::CmdKind::TimedOut`]
///    before this is ever consulted — so a `SIGKILL` arriving here was not
///    ours. An operator manually killing a test run would land here too, and
///    "we learned nothing, try again" is the right answer for that as well.
/// 2. **Exit `137`.** The same fact, reported by a shell or a wrapper that
///    turned the signal into a code. A test suite that deliberately exits
///    `137` for an unrelated reason would be misread; no runner in the
///    supported dialects does.
/// 3. **A runtime marker in the output of a run that FAILED.** A passing run
///    is never an OOM, whatever it printed — that guard is what keeps a suite
///    with an OOM-handling test from having its green run thrown away.
///
/// `exit_code: Some(0)` with no signal is always `false`, which is the one
/// case worth stating outright: a run that succeeded observed its assertions,
/// and no amount of scary text in its log changes that.
#[must_use]
pub fn killed_by_oom(facts: ExitFacts<'_>) -> bool {
    if facts.signal == Some(SIGKILL) {
        return true;
    }
    if facts.exit_code == Some(SIGKILL_EXIT_CODE) {
        return true;
    }
    // A run that passed observed its assertions; nothing in its output can
    // turn that into "we learned nothing".
    if facts.exit_code == Some(0) {
        return false;
    }
    let haystack = facts.output.to_ascii_lowercase();
    OOM_MARKERS.iter().any(|marker| haystack.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts<'a>(signal: Option<i32>, exit_code: Option<i32>, output: &'a str) -> ExitFacts<'a> {
        ExitFacts {
            signal,
            exit_code,
            output,
        }
    }

    /// Rule 1: the signal itself, which is the only thing the kernel's OOM
    /// killer ever does.
    #[test]
    fn a_sigkilled_process_is_an_oom() {
        assert!(killed_by_oom(facts(Some(SIGKILL), None, "")));
        // Any other signal is not: SIGSEGV and SIGABRT are the code's own
        // problem and must stay ordinary failures.
        for signal in [1, 2, 6, 11, 15] {
            assert!(
                !killed_by_oom(facts(Some(signal), None, "")),
                "signal {signal} is not an out-of-memory kill"
            );
        }
    }

    /// Rule 2: the same fact one indirection out, as a shell spells it.
    #[test]
    fn exit_137_is_the_shells_spelling_of_the_same_kill() {
        assert!(killed_by_oom(facts(None, Some(137), "")));
        assert!(!killed_by_oom(facts(None, Some(1), "")));
        assert!(!killed_by_oom(facts(None, Some(101), "")));
    }

    /// Rule 3: a runtime that noticed first and died on its own terms, with an
    /// ordinary exit code and no signal at all.
    #[test]
    fn a_runtime_that_reports_its_own_allocation_failure_is_an_oom() {
        for output in [
            "MemoryError",
            "terminate called after throwing an instance of 'std::bad_alloc'",
            "java.lang.OutOfMemoryError: Java heap space",
            "FATAL ERROR: Reached heap limit — JavaScript heap out of memory",
            "memory allocation of 68719476736 bytes failed",
            "fatal error: runtime: out of memory",
            "fork: Cannot allocate memory",
            "error: test failed: process didn't exit successfully (signal: 9, SIGKILL: kill)",
            "Out of memory: Killed process 4242 (pytest) total-vm:8000000kB",
        ] {
            assert!(
                killed_by_oom(facts(None, Some(1), output)),
                "unrecognized out-of-memory report: {output}"
            );
        }
    }

    /// The guard that keeps a green suite green: a run that PASSED observed
    /// its assertions, whatever its log says.
    #[test]
    fn a_passing_run_is_never_an_oom_however_it_reads() {
        assert!(!killed_by_oom(facts(
            None,
            Some(0),
            "test out_of_memory_is_reported ... ok\nMemoryError handling verified"
        )));
    }

    /// The default direction: an ordinary failure with nothing OOM-shaped in
    /// it stays an ordinary failure, which is what the worker revises.
    #[test]
    fn an_ordinary_failure_stays_an_ordinary_failure() {
        assert!(!killed_by_oom(facts(
            None,
            Some(101),
            "assertion `left == right` failed\n  left: 1\n right: 2"
        )));
        assert!(!killed_by_oom(ExitFacts::default()));
    }
}
