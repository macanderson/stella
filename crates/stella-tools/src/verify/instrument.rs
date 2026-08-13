// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Whether the witness command **ran at all** — the question that separates a
//! fact about the test from a fact about the code.
//!
//! # The failure this module exists to end
//!
//! `verify_done` had one channel for both answers. A witness whose command
//! could not execute — a malformed argv, a missing interpreter, a shell that
//! could not parse it — exits non-zero exactly like a witness whose assertion
//! failed, so the working-tree half answered `NOT DONE — the witness test
//! fails on your NEW code. Fix the implementation (or the test) and retry.`
//! That sentence is an accusation against code the run never reached, and it
//! is advice that cannot terminate: no edit to the implementation can make a
//! command run that the shell never parsed.
//!
//! On a 2026-08-11 Terminal-Bench trial an agent spent **fifteen consecutive
//! `verify_done` calls** chasing an argv bug in a witness it had authored
//! itself — `fatal: ambiguous argument 'AKIA1234567890123456': unknown
//! revision or path not in the working tree`, git refusing to run the search
//! at all — and read every one of them as evidence about its change. It then
//! contaminated the control branch trying to make the accusation go away. 38
//! of that trial's 82 tool calls were spent after the task was already done,
//! and it scored 0 (#2872).
//!
//! An agent cannot repair what it cannot name. So the tool names it: an exit
//! that arrived without the command ever running is reported as a broken
//! **instrument**, and the message withholds — explicitly — the inference the
//! old one invited.
//!
//! # The discriminator, and why it is this one
//!
//! Not "did it fail twice": every armed witness is red on the baseline by
//! construction, so failing on both trees discriminates nothing (the reasoning
//! `stella-pipeline`'s `WitnessUnsatisfiable` doc comment puts on the record).
//! The discriminator here is **whether the command ran** — an exit the shell
//! itself produced instead of running the program ([`Instrument::NotFound`],
//! [`Instrument::NotExecutable`]), an exit from a parser that never reached
//! the first statement ([`Instrument::ShellSyntax`]), or a program that
//! refused its own arguments and did no work ([`Instrument::Argv`]). None of
//! those is an assertion about anything.
//!
//! # Why only the working-tree half
//!
//! `verify_done` runs the baseline half only once the working-tree half has
//! passed, so any breakage that is a property of the *command* or of the
//! staged witness text is caught on the first run — the shadow sees the same
//! command and the same bytes. What can still fail at the baseline alone is
//! tree-dependent: a program that does not exist there yet, which on a
//! creation task is the deliverable itself and the whole proof.
//! [`creation::confirm`](super::creation::confirm) already decides that one
//! from git rather than from text, and widening this classifier over the
//! baseline would take those verdicts away from it.
//!
//! The case this module therefore does not cover is the pipeline's: an
//! author-written witness whose baseline run is its *first* run, where a
//! shell that could not parse it reads as "the test fails on the old code"
//! and arms the flip oracle. That screen belongs in
//! `stella-pipeline`'s witness stage, which has the separate stdout this tool
//! does not, and is tracked as the second half of #2872.
//!
//! # Style
//!
//! [`import_failure_signature`] lives here for the same reason and is the
//! shape the rest of the module copies: a deliberately narrow vocabulary in
//! which every entry is justified by why an author can always act on it, and
//! every exclusion is named. Every string below is a pure function of the
//! call's inputs and observations — no timings, no counters — because
//! `ToolOutput` is what `stella-core`'s loop detector keys on (see
//! [`phases`](super::phases)).

/// Output signatures proving the previous-code run died while *loading* the
/// test or its imports — no behavioural assertion ever executed, so the
/// failure is not evidence of a flip (#2067). The live false positive: a
/// compiled `.so` present in the working tree but absent from a fresh shadow
/// checkout fails `import` on the old side and fakes a confirmation.
///
/// Deliberately narrow, and deliberately NOT the compile-error family:
/// a witness that fails to *build* on the old code (rustc `error[E…]`) is
/// the canonical missing-API witness shape and stays credited with the
/// existing weaker-evidence warning (#1790). The import/loader family is
/// different: the author can always convert module absence into an
/// assertion (`try: import x … except ImportError: ok = False`), so refusal
/// is actionable, while crediting it lets a stale artifact decide the
/// verdict. `SyntaxError` and "No such file or directory" are also absent —
/// both can BE the witnessed behaviour (a fix-the-parse-error task; a shell
/// witness asserting a file the change creates).
///
/// A match here is a *candidate* refusal, not the verdict: on a creation
/// task the module that would not load can be the deliverable itself, absent
/// at the baseline by construction. [`creation::confirm`](super::creation::confirm)
/// decides that from git — never from the text this function reads, which
/// cannot tell the two apart (#2668).
pub(super) fn import_failure_signature(output: &str) -> Option<&'static str> {
    const SIGNATURES: &[(&str, &str)] = &[
        ("modulenotfounderror", "a Python `ModuleNotFoundError`"),
        ("no module named", "a missing Python module"),
        ("importerror", "a Python `ImportError`"),
        (
            "error while loading shared libraries",
            "a missing shared library",
        ),
        ("cannot find module", "a missing Node module"),
        ("error collecting", "a pytest collection error"),
        ("errors during collection", "a pytest collection error"),
    ];
    let lower = output.to_ascii_lowercase();
    SIGNATURES
        .iter()
        .find(|(needle, _)| lower.contains(needle))
        .map(|(_, label)| *label)
}

/// A witness command that did not run. Each variant is one way an exit code
/// can arrive with nothing having been asserted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Instrument {
    /// Exit 127: the shell looked the command up and found nothing. Read as a
    /// code and not as a message because POSIX reserves the code for exactly
    /// this, while the wording differs per shell and per locale.
    NotFound,
    /// Exit 126: the file was found and could not be executed — a missing
    /// exec bit, or a `#!` line naming an interpreter that is not installed.
    NotExecutable,
    /// The shell could not parse the command or the script, so no statement
    /// in it ever ran.
    ShellSyntax,
    /// The program refused its own arguments and did no work — git's
    /// revision-versus-path ambiguity, the shape that cost fifteen calls.
    Argv,
}

impl Instrument {
    /// What the call observed, as a clause completing "the witness command
    /// did not run: …".
    pub(super) const fn observed(self) -> &'static str {
        match self {
            Self::NotFound => "the shell could not find the command (exit 127)",
            Self::NotExecutable => {
                "the command was found but could not be executed — a missing exec bit, or a \
                 `#!` interpreter that is not installed (exit 126)"
            }
            Self::ShellSyntax => {
                "the shell could not parse it, so no statement in it ever ran (syntax error)"
            }
            Self::Argv => {
                "the program rejected its own arguments and performed no work (git could not \
                 tell a revision from a path — the missing `--` separator)"
            }
        }
    }

    /// The repair, stated as one action on the *command*. Advice that can
    /// terminate; in particular, never "strengthen the test".
    pub(super) const fn repair(self) -> &'static str {
        match self {
            Self::NotFound => {
                "install the tool, or call it by a path that exists in this workspace"
            }
            Self::NotExecutable => {
                "run it through its interpreter explicitly (`bash x.sh`, `python3 x.py`), or \
                 `chmod +x` it"
            }
            Self::ShellSyntax => {
                "fix the quoting or the shell dialect — a `#!/bin/sh` script may not use bash \
                 arrays, and `bash x.sh` runs the same file under a parser that accepts them"
            }
            Self::Argv => {
                "separate revisions from paths with `--` (`git grep -e PATTERN --`), or use a \
                 tool that takes no revision argument at all (`grep -r PATTERN .`)"
            }
        }
    }
}

/// Shell parse-error signatures. Narrow by construction: each is text a
/// *parser* emits about text it refused, never text a program emits about a
/// value it compared.
///
/// Python's `SyntaxError` is absent for the reason
/// [`import_failure_signature`] excludes it — a fix-the-parse-error task
/// witnesses one, and a classifier must not swallow a task's whole subject.
/// The three below name a *shell* parser, which the witness command always
/// passes through and a task's subject essentially never is.
const SYNTAX_SIGNATURES: &[&str] = &[
    // bash
    "syntax error near unexpected token",
    // bash, an unterminated construct
    "unexpected end of file",
    // dash / ash: `Syntax error: "(" unexpected`
    "syntax error:",
];

/// git's revision-versus-path fatal, which is a usage error and not a result.
/// Both halves are required: `ambiguous argument` alone also appears in advice
/// text printed alongside runs that did the work.
const ARGV_SIGNATURES: &[(&str, &str)] = &[(
    "fatal: ambiguous argument",
    "unknown revision or path not in the working tree",
)];

/// Classify the working-tree half's failure: did the witness command run and
/// fail, or never run?
///
/// `None` means it ran — the ordinary case, which keeps its `NOT DONE`
/// verdict, because a witness that runs and fails on the new code is exactly
/// the evidence that verdict claims it is.
pub(super) fn working_tree_failure(exit: i32, output: &str) -> Option<Instrument> {
    // Codes first: they come from the outer `bash -c` and say what happened to
    // the command itself, independent of wording a locale could change.
    match exit {
        127 => return Some(Instrument::NotFound),
        126 => return Some(Instrument::NotExecutable),
        _ => {}
    }
    let lower = output.to_ascii_lowercase();
    if SYNTAX_SIGNATURES
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return Some(Instrument::ShellSyntax);
    }
    if ARGV_SIGNATURES
        .iter()
        .any(|(head, tail)| lower.contains(head) && lower.contains(tail))
    {
        return Some(Instrument::Argv);
    }
    None
}

/// The refusal: what was observed, what it is not evidence of, and the one
/// repair. Terminal in the honest direction — the change stays UNPROVEN, and
/// no path here can produce a confirmation.
pub(super) fn refusal(broken: Instrument, test_cmd: &str, exit: i32, tail: &str) -> String {
    format!(
        "WITNESS UNRUNNABLE — the witness command did not run: {}. This is a fact about the \
         TEST COMMAND, not about your change: no assertion was reached, so exit {exit} is NOT \
         evidence that your implementation is wrong, and editing working code in response to \
         it is how a correct change gets undone.\n\
         Repair the instrument once — {} — then call verify_done again. If the command cannot \
         be made to run, report the change as UNPROVEN and stop: repeating it proves nothing, \
         and no number of retries turns an unrun command into a witness.\n\
         - command:       `{test_cmd}`\n\
         - previous code: not attempted — the witness never ran on the new code, so there is \
         nothing to compare it against\n\
         --- output tail ---\n{tail}",
        broken.observed(),
        broken.repair()
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use stella_protocol::tool::ToolOutput;

    use super::*;
    use crate::registry::Tool;
    use crate::verify::VerifyDone;
    use crate::verify::tests::scaffold;

    /// The import/loader vocabulary is deliberately narrow: behavioural
    /// failures — assertions, files a shell witness checks for, and the Rust
    /// missing-API compile shape (#1790) — stay credited.
    ///
    /// Moved here with [`import_failure_signature`] itself, unchanged.
    #[test]
    fn import_failure_signatures_are_narrow() {
        assert!(import_failure_signature("ModuleNotFoundError: No module named 'x'").is_some());
        assert!(import_failure_signature("ImportError: undefined symbol: foo").is_some());
        assert!(
            import_failure_signature("./app: error while loading shared libraries: libz.so.1")
                .is_some()
        );
        assert!(import_failure_signature("Error: Cannot find module 'left-pad'").is_some());
        assert!(import_failure_signature("assertion failed: `(left == right)`").is_none());
        assert!(
            import_failure_signature("cat: /etc/nginx/ssl/cert.pem: No such file or directory")
                .is_none()
        );
        assert!(
            import_failure_signature("error[E0425]: cannot find function `retry_delays`").is_none()
        );
    }

    /// The exact argv bug from the trace: git refuses to tell a revision from
    /// a path, searches nothing, and exits non-zero.
    #[test]
    fn the_git_argv_fatal_is_an_instrument_failure() {
        let observed = "fatal: ambiguous argument 'AKIA1234567890123456': unknown revision or \
                        path not in the working tree.\n";
        assert_eq!(working_tree_failure(128, observed), Some(Instrument::Argv));
    }

    /// The half of the vocabulary that must stay narrow: an assertion that ran
    /// and failed is not an instrument failure, however loud it is. Excusing
    /// one would be the mirror defect — a broken change reported as a broken
    /// test.
    #[test]
    fn an_assertion_failure_is_not_an_instrument_failure() {
        for output in [
            "FAILED tests/test_api.py::test_rate_limit - assert 0 == 1\n1 failed in 0.42s\n",
            "thread 'main' panicked at src/lib.rs:9:5:\nassertion `left == right` failed\n",
            "expected 'gcov' in the output, got ''\n",
            // git ran, searched, and found nothing: exit 1 with no fatal.
            "",
        ] {
            assert_eq!(working_tree_failure(1, output), None, "{output}");
        }
    }

    #[test]
    fn a_missing_command_and_a_missing_exec_bit_are_read_from_the_exit_code() {
        assert_eq!(
            working_tree_failure(127, "witness.sh: line 1: pytest: command not found\n"),
            Some(Instrument::NotFound)
        );
        assert_eq!(
            working_tree_failure(126, "bash: ./witness.sh: Permission denied\n"),
            Some(Instrument::NotExecutable)
        );
    }

    #[test]
    fn a_shell_that_could_not_parse_the_witness_is_an_instrument_failure() {
        for output in [
            "bash: -c: line 1: syntax error near unexpected token `('\n",
            "witness.sh: 3: Syntax error: \"(\" unexpected\n",
            "bash: witness.sh: line 9: unexpected end of file\n",
        ] {
            assert_eq!(
                working_tree_failure(2, output),
                Some(Instrument::ShellSyntax),
                "{output}"
            );
        }
    }

    /// #2872 witness.
    ///
    /// Fails on `main`, where a witness that cannot run returns `NOT DONE —
    /// the witness test fails on your NEW code. Fix the implementation (or the
    /// test) and retry.` — an accusation against code the command never
    /// reached, and the sentence that cost fifteen consecutive calls on the
    /// 2026-08-11 panel.
    ///
    /// The *pair* is the assertion: one witness broken as an instrument and
    /// one that runs and legitimately fails, same tool and same workspace,
    /// must not come back as the same kind of answer. Asserting only the first
    /// half would pass on a tool that called everything a broken instrument.
    #[tokio::test]
    async fn a_witness_that_cannot_run_is_not_reported_as_failing_code() {
        let root = scaffold("instrument_working_tree").await;
        std::fs::write(root.join("impl.txt"), "new behavior\n").unwrap();

        // Broken as an instrument: git refuses the argv and searches nothing.
        std::fs::write(
            root.join("witness.sh"),
            "git grep -q AKIA1234567890123456 AKIA1234567890123456\n",
        )
        .unwrap();
        let broken = VerifyDone::default()
            .execute(
                &json!({
                    "test_cmd": "bash witness.sh",
                    "test_files": ["witness.sh"],
                    "timeout_secs": 60
                }),
                &root,
            )
            .await;
        let broken = match &broken {
            ToolOutput::Error { message, .. } => message.clone(),
            other => panic!("expected a refusal, got {other:?}"),
        };
        assert!(
            broken.contains("WITNESS UNRUNNABLE"),
            "an instrument failure must be named as one: {broken}"
        );
        assert!(
            !broken.contains("NOT DONE"),
            "the tool accused the implementation for a command that never ran: {broken}"
        );
        assert!(
            broken.contains("NOT evidence that your implementation is wrong"),
            "the refusal must withhold the inference the old message invited: {broken}"
        );
        assert!(
            broken.contains("UNPROVEN"),
            "a refusal must never read as a pass: {broken}"
        );

        // Ran, and legitimately failed: the assertion is false on this tree.
        std::fs::write(
            root.join("witness.sh"),
            "grep -q 'behaviour that does not exist' impl.txt\n",
        )
        .unwrap();
        let genuine = VerifyDone::default()
            .execute(
                &json!({
                    "test_cmd": "bash witness.sh",
                    "test_files": ["witness.sh"],
                    "timeout_secs": 60
                }),
                &root,
            )
            .await;
        let genuine = match &genuine {
            ToolOutput::Error { message, .. } => message.clone(),
            other => panic!("expected a refusal, got {other:?}"),
        };
        assert!(
            genuine.contains("NOT DONE"),
            "a witness that ran and failed is still evidence about the code: {genuine}"
        );
        assert!(
            !genuine.contains("WITNESS UNRUNNABLE"),
            "a real assertion failure must not be excused as a broken instrument: {genuine}"
        );

        std::fs::remove_dir_all(&root).ok();
    }
}
