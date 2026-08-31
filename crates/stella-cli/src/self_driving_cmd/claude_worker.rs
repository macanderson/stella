//! The claude worker — one issue through Claude Code, in the same worktree.
//!
//! Separated from the parent so each worker's child process is a file rather
//! than a pair of arms in one function. What this module owns is everything
//! specific to claude: how its command line is built, how its output is read,
//! and the one thing it cannot honour. The parent still decides which worker
//! runs, and still measures the outcome from the tree either way.

use std::path::Path;
use std::process::{Command, Stdio};

/// Why a dollar cap and a claude worker cannot be combined.
///
/// The cap is refused, not ignored. Claude Code reports no cost the run budget
/// can read. Every turn would be charged nothing, so the ceiling would never
/// be reached. Ask for a $5 run and you would get an unbounded one, with
/// nothing said. A cap that is quietly infinite is worse than no cap at all.
pub(super) fn uncappable(cap: f64) -> String {
    format!(
        "a claude worker cannot be held to --spend-limit ${cap:.2}: claude does \
         not report what a turn cost in the form this loop reads. Set \
         worker.max_turns, or drop the dollar cap."
    )
}

/// The Claude Code child, built but not spawned.
///
/// Building and spawning are split so the whole command is a test seam. A
/// test can then read what the settings put on the command line. It does not
/// have to run an agent to find out.
fn claude_command(
    dir: &Path,
    prompt: &str,
    worker: &crate::settings::toml_config::WorkerSection,
) -> Command {
    let mut cmd = Command::new(&worker.command);
    cmd.current_dir(dir)
        .arg("-p")
        .arg("--output-format")
        .arg("json");
    if let Some(model) = worker.model.as_deref() {
        cmd.arg("--model").arg(model);
    }
    if let Some(max_turns) = worker.max_turns {
        cmd.arg("--max-turns").arg(max_turns.to_string());
    }
    // Only when the operator asked for it in as many words. Choosing a worker
    // and widening what that worker may do are two decisions, and this is the
    // second one.
    if worker.dangerously_skip_permissions {
        cmd.arg("--dangerously-skip-permissions");
    }
    cmd.arg(prompt);
    cmd
}

/// Run Claude Code non-interactively in one issue's isolated worktree.
///
/// The prompt rides as an argument, and stdin is closed. Nobody is watching
/// this loop. A worker that stops to ask a question has to fail, rather than
/// wait for a person who is not there.
pub(super) fn run_claude(
    dir: &Path,
    prompt: &str,
    worker: &crate::settings::toml_config::WorkerSection,
) -> Result<String, String> {
    let out = claude_command(dir, prompt, worker)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| {
            format!(
                "could not start claude (`{}`): {error}. Set worker.command to \
                 the executable's name on PATH, or an absolute path to it.",
                worker.command
            )
        })?;

    let summary = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if out.status.success() {
        Ok(summary)
    } else {
        Err(format!(
            "claude exited {}{}",
            out.status.code().unwrap_or(-1),
            match claude_reason(&summary) {
                Some(reason) => format!(" — {reason}"),
                None => String::new(),
            }
        ))
    }
}

/// What Claude Code said about a run that ended badly.
///
/// Its `--output-format json` is not the shape the stella worker's parser
/// reads, so that parser is not reused. Aimed at this document it finds none
/// of its keys. It then falls back to the last line, and for one line of JSON
/// that is the whole document.
///
/// `result` carries the text a person reads, in both the success and the error
/// shape. `error` appears when a run failed before there was any result.
fn claude_reason(summary: &str) -> Option<String> {
    let condense = |text: &str| -> Option<String> {
        let condensed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if condensed.is_empty() {
            return None;
        }
        let clipped: String = condensed.chars().take(600).collect();
        Some(if condensed.chars().count() > 600 {
            format!("{clipped}…")
        } else {
            clipped
        })
    };

    let Ok(value) = serde_json::from_str::<serde_json::Value>(summary) else {
        return condense(summary.lines().last().unwrap_or(summary));
    };
    ["result", "error"]
        .iter()
        .find_map(|key| value.get(key).and_then(serde_json::Value::as_str))
        .and_then(condense)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::toml_config::WorkerKind;

    /// The claude child's argv, as strings.
    fn claude_args(worker: &crate::settings::toml_config::WorkerSection) -> Vec<String> {
        claude_command(Path::new("/tmp/worktree"), "fix #1", worker)
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    /// A worker section with claude selected and nothing else said.
    fn claude_worker() -> crate::settings::toml_config::WorkerSection {
        crate::settings::toml_config::WorkerSection {
            kind: WorkerKind::Claude,
            ..Default::default()
        }
    }

    /// **Witness.** Selecting claude does not also hand it permission bypass.
    ///
    /// The two are separate decisions. This flag lets the worker write files
    /// and run commands without asking. It appears only when someone wrote it
    /// down.
    #[test]
    fn a_claude_worker_does_not_skip_permissions_unless_asked() {
        let args = claude_args(&claude_worker());
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--dangerously-skip-permissions"),
            "the default claude worker must not bypass permissions: {args:?}"
        );

        let asked = crate::settings::toml_config::WorkerSection {
            dangerously_skip_permissions: true,
            ..claude_worker()
        };
        assert!(
            claude_args(&asked)
                .iter()
                .any(|arg| arg == "--dangerously-skip-permissions"),
            "asking for the bypass must put it on the command line"
        );
    }

    /// **Witness.** The worker's settings reach claude's command line, and the
    /// ones that were not set put nothing there.
    #[test]
    fn the_claude_worker_settings_reach_the_command_line() {
        let bare = claude_args(&claude_worker());
        assert_eq!(
            bare,
            ["-p", "--output-format", "json", "fix #1"],
            "an unconfigured claude worker sends only the print-mode contract \
             and the prompt"
        );

        let configured = crate::settings::toml_config::WorkerSection {
            model: Some("opus".to_owned()),
            max_turns: Some(40),
            ..claude_worker()
        };
        assert_eq!(
            claude_args(&configured),
            [
                "-p",
                "--output-format",
                "json",
                "--model",
                "opus",
                "--max-turns",
                "40",
                "fix #1"
            ]
        );
    }

    /// **Witness.** A dollar cap against a claude worker is refused, and the
    /// refusal says what to reach for instead.
    ///
    /// The alternative is the dangerous one. The run budget charges what a
    /// turn reports. Claude reports nothing it can read, so every turn would
    /// cost zero, and a $5 run would quietly become an unbounded one.
    #[test]
    fn a_dollar_cap_against_a_claude_worker_is_refused() {
        let refusal = uncappable(5.0);
        assert!(
            refusal.contains("--spend-limit $5.00"),
            "the refusal names the cap that was asked for: {refusal}"
        );
        assert!(
            refusal.contains("worker.max_turns"),
            "a refusal that names no alternative is a dead end: {refusal}"
        );
    }

    /// **Witness.** Claude's own JSON is read by its own keys.
    ///
    /// The stella worker's parser reads a different shape. Aimed at this
    /// document it matches nothing and falls back to the last line. For one
    /// line of JSON that is the whole document, quoted at a person as if it
    /// were a diagnosis.
    #[test]
    fn a_failed_claude_run_reports_what_claude_said() {
        let summary =
            r#"{"type":"result","is_error":true,"result":"I need write access to continue."}"#;
        assert_eq!(
            claude_reason(summary).as_deref(),
            Some("I need write access to continue.")
        );

        assert_eq!(
            claude_reason(r#"{"error":"credit balance is too low"}"#).as_deref(),
            Some("credit balance is too low")
        );

        // Not JSON at all — a crash, a shell error — still yields the last
        // line, so a missing executable reads as one.
        assert_eq!(
            claude_reason("boom\ncommand not found: claude").as_deref(),
            Some("command not found: claude")
        );

        // JSON this parser does not recognise reports nothing, so the caller
        // prints the bare exit code rather than a whole document.
        assert_eq!(claude_reason(r#"{"type":"result"}"#), None);
    }
}
