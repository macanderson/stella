// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Where a one-shot prompt comes from (#1261).
//!
//! `stella run` and `stella goal` used to take their prompt as a *required*
//! positional, which made the whole family of shell idioms inexpressible:
//!
//! ```sh
//! cat spec.md | stella run
//! stella run < spec.md
//! stella run - <<'PROMPT'
//! …
//! PROMPT
//! ```
//!
//! That is the shape most non-interactive automation takes, and it is exactly
//! what a one-shot subcommand exists to serve, so the gap sat squarely on the
//! surface built for it.
//!
//! # The rule
//!
//! Three inputs decide, in this order:
//!
//! 1. a positional that is not `-` — use it verbatim, read no stdin;
//! 2. a positional of exactly `-` — read stdin, **even on a TTY** (the
//!    conventional explicit form, and the only way to force it interactively);
//! 3. no positional — read stdin when it is piped, and otherwise fail with a
//!    named error rather than hanging on a terminal nobody is typing into.
//!
//! Case 3's TTY check is the whole reason this is a pure function taking
//! `stdin_is_tty` rather than calling `IsTerminal` itself: "no prompt and no
//! pipe" must be a *fast, named* error, and the difference between that and a
//! silent hang is not something a test should need a pty to pin.
//!
//! # What this deliberately does not do
//!
//! It does not touch attachment extraction. A stdin-sourced prompt goes
//! through [`crate::attachments::user_message_in`] exactly as an argument-sourced
//! one does — where the bytes came from must not change what the prompt
//! *means*. See that module for what a path-shaped token in prompt text does.

/// Why no prompt could be resolved. Carries its own message because every
/// caller does the same thing with it: surface it and exit non-zero.
const NO_PROMPT: &str = "no prompt: pass one as an argument, pipe it on stdin, or use `-` to \
                         read stdin explicitly";

/// An empty prompt is a mistake worth naming rather than a turn worth running:
/// an empty pipe is nearly always a failed upstream command, and sending it
/// costs a model call to be told nothing was asked.
const EMPTY_PROMPT: &str = "empty prompt: stdin produced no text (an upstream command in the \
                            pipeline may have failed)";

/// Resolve the prompt from the positional argument and, when called for,
/// stdin.
///
/// `read_stdin` is only invoked in the two cases that need it, so the ordinary
/// argument path never touches the handle — which matters because reading a
/// terminal's stdin blocks until EOF.
pub(crate) fn resolve(
    positional: Option<String>,
    stdin_is_tty: bool,
    read_stdin: impl FnOnce() -> std::io::Result<String>,
) -> Result<String, String> {
    let from_stdin = match positional.as_deref() {
        // A real prompt. Note this is checked BEFORE the `-` case, so a
        // prompt that merely starts with `-` is still a prompt.
        Some(text) if text != "-" => return non_empty(text.to_string()),
        // Explicit stdin, honoured even on a TTY.
        Some(_) => true,
        // Implicit: only when something is actually piped in.
        None => !stdin_is_tty,
    };

    if !from_stdin {
        return Err(NO_PROMPT.to_string());
    }
    let text = read_stdin().map_err(|e| format!("could not read the prompt from stdin: {e}"))?;
    non_empty(text)
}

/// Trim and reject the empty case. Trailing newlines are an artefact of every
/// heredoc and every `echo`, and a prompt is not whitespace-sensitive at its
/// edges.
fn non_empty(text: String) -> Result<String, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(EMPTY_PROMPT.to_string());
    }
    Ok(trimmed.to_string())
}

/// Read all of stdin to a string — the real reader passed to [`resolve`].
pub(crate) fn read_stdin_to_string() -> std::io::Result<String> {
    use std::io::Read;
    let mut buf = String::new();
    std::io::stdin().lock().read_to_string(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reader must not run on the argument path: on a TTY it would block
    /// until the user hit Ctrl-D, turning every ordinary `stella run "…"` into
    /// a hang.
    #[test]
    fn an_argument_prompt_never_reads_stdin() {
        let got = resolve(Some("do the thing".into()), true, || {
            panic!("stdin must not be read when a prompt argument is present")
        });
        assert_eq!(got.unwrap(), "do the thing");
    }

    /// The defect this file exists for: a piped prompt was simply unreachable.
    #[test]
    fn a_piped_prompt_is_read_when_no_argument_is_given() {
        let got = resolve(None, false, || Ok("piped prompt\n".into()));
        assert_eq!(got.unwrap(), "piped prompt");
    }

    /// Without the TTY check this is a silent hang on an interactive terminal,
    /// which reads to a user as "stella froze on startup".
    #[test]
    fn no_argument_and_no_pipe_is_a_named_error_not_a_hang() {
        let err = resolve(None, true, || {
            panic!("stdin must not be read when it is a terminal and no `-` was given")
        })
        .unwrap_err();
        assert!(err.contains("no prompt"), "got {err}");
    }

    /// `-` is the only way to force a stdin read interactively, so it must
    /// win over the TTY check rather than be filtered by it.
    #[test]
    fn a_bare_dash_reads_stdin_even_on_a_tty() {
        let got = resolve(Some("-".into()), true, || Ok("from a heredoc".into()));
        assert_eq!(got.unwrap(), "from a heredoc");
    }

    /// A prompt beginning with `-` is a prompt, not a request for stdin —
    /// only the exact string `-` means stdin. (Reaching this function at all
    /// still requires `--` at the shell, which is clap's business, not this
    /// module's.)
    #[test]
    fn a_leading_dash_prompt_is_a_prompt_not_a_stdin_request() {
        let got = resolve(Some("--fix the dash handling".into()), true, || {
            panic!("a leading-dash prompt must not be read as `-`")
        });
        assert_eq!(got.unwrap(), "--fix the dash handling");
    }

    /// An empty pipe is nearly always a failed upstream command; running a
    /// turn on it spends a model call to be told nothing was asked.
    #[test]
    fn an_empty_pipe_is_refused_rather_than_sent_to_the_model() {
        let err = resolve(None, false, || Ok("   \n\t\n".into())).unwrap_err();
        assert!(err.contains("empty prompt"), "got {err}");
    }

    #[test]
    fn a_stdin_read_failure_is_reported_rather_than_swallowed() {
        let err = resolve(None, false, || Err(std::io::Error::other("pipe broke"))).unwrap_err();
        assert!(err.contains("pipe broke"), "got {err}");
    }
}
