//! The production [`HookRunner`] — the real-I/O half of the hooks framework
//! (`stella_core::hooks` owns the matching/blocking logic; this crate owns
//! process spawning, mirroring how `ToolRegistry` implements the engine's
//! `ToolExecutor` port).
//!
//! Each hook action runs in the workspace root, with the event payload piped
//! in as JSON on stdin, bounded by the action's clamped timeout. The spawn is
//! detached into its own group — a `setsid` session on unix, a Job Object on
//! Windows (#3550) — so a timeout, or a dropped future, kills the whole tree
//! ([`crate::exec::GroupKillGuard`]) and not just the process that fronts it;
//! `kill_on_drop` backs that up for the direct child. A hung hook can stall
//! its own timeout window, never the session, and nothing it spawned outlives
//! that window.
//!
//! # Two spawn shapes, decided by the action and never by the caller
//!
//! An **operator's** hook is `bash -c <command>` over a scrubbed inherited
//! environment: it is the user's own shell, written in their own settings
//! file, and the shell is the point.
//!
//! A **plugin's** hook — an action carrying a
//! [`stella_core::hooks::PluginHookOrigin`], assembled by
//! the host from an installed manifest — is its declared argv, executed
//! directly, from an environment cleared and then refilled with the names the
//! manifest declared and nothing else. Two differences from the operator's
//! shape, both structural:
//!
//! - **No shell.** The argv is a third party's, and handing it to `bash -c`
//!   would make its own quoting an injection surface — a plugin whose
//!   `argv = ["python3", "${plugin_dir}/main.py"]` sits in a directory with a
//!   space in it would either break or run something else.
//! - **Default-deny on the environment**, not the denylist
//!   [`crate::subprocess_env::scrub_spawn_env`] applies. A plugin is a third
//!   party and the set of variables it needs is knowable only to its author,
//!   so it declares them and the install consent shows them
//!   (`stella_plugin::Runtime::child_env`). A declared name this crate grades
//!   as a model credential is withheld even so, which is the same judgement
//!   `stella_runtime::wrapper::SubprocessWrapper::declare` makes at the
//!   socket, reached through the same
//!   [`crate::subprocess_env::is_sensitive_env_name`] — one decision, two
//!   transports, never two answers (#3512).

use std::ffi::OsStr;
use std::process::Stdio;

use async_trait::async_trait;
use stella_core::hooks::{HookAction, HookExecError, HookExecResult, HookRunner, PluginHookOrigin};
use tokio::io::AsyncWriteExt;

/// The [`HookRunner`] every shipping door installs.
///
/// Named for the *host*, not for the shell, because the shell is only one of
/// the two shapes it spawns — see the module doc.
pub struct HostHookRunner;

/// The child process one plugin route runs as: its argv, and an environment
/// built from empty.
///
/// `lookup` is the parent environment, injected so the default-deny rule is
/// testable without mutating the process's own variables.
fn plugin_command<F>(
    origin: &PluginHookOrigin,
    mut lookup: F,
) -> Result<tokio::process::Command, HookExecError>
where
    F: FnMut(&str) -> Option<String>,
{
    let Some((program, args)) = origin.argv.split_first() else {
        // Unreachable through a manifest — `Runtime::validate` refuses an
        // empty argv at load — but this is a port taking plain data, and a
        // host that built the origin some other way gets a named failure
        // rather than a panic (invariant 5).
        return Err(HookExecError::SpawnFailed {
            command: origin.plugin.clone(),
            message: format!("plugin `{}` declared no program to run", origin.plugin),
        });
    };
    let mut command = tokio::process::Command::new(program);
    command.args(args);
    // Cleared FIRST: every `env` below adds to an empty map, so a variable the
    // operator's shell picked up after the install is simply not there.
    command.env_clear();
    for name in &origin.env_allowlist {
        if crate::subprocess_env::is_sensitive_env_name(OsStr::new(name.as_str())) {
            continue;
        }
        if let Some(value) = lookup(name) {
            command.env(name, value);
        }
    }
    Ok(command)
}

#[async_trait]
impl HookRunner for HostHookRunner {
    async fn run(
        &self,
        action: &HookAction,
        payload_json: &str,
        cwd: &str,
    ) -> Result<HookExecResult, HookExecError> {
        let mut command = match &action.plugin {
            Some(origin) => plugin_command(origin, |name| std::env::var(name).ok())?,
            None => {
                let mut command = tokio::process::Command::new("bash");
                command.arg("-c").arg(&action.command);
                // Full spawn policy: a hook running from inside a git hook
                // must not inherit the outer repo's GIT_DIR, and its stdout
                // is parsed (block decisions), so forced-color overrides go
                // too — same families `exec::drive` removes. The plugin arm
                // needs none of this: it cleared the environment instead.
                crate::subprocess_env::scrub_spawn_env(&mut command);
                command
            }
        };
        command
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Own process group, exactly like every other spawn in this crate: a
        // hook that backgrounds work (`some-watcher &`) leaves grandchildren
        // that `kill_on_drop` cannot reach, and a timed-out hook must not
        // leak them into the rest of the session.
        crate::exec::detach_into_own_process_group(&mut command);
        let mut child = command.spawn().map_err(|e| HookExecError::SpawnFailed {
            command: action.command.clone(),
            message: e.to_string(),
        })?;
        // Capture the pid before the capped wait takes ownership.
        let pid = child.id().unwrap_or(0) as i32;
        // Cancellation backstop: a dropped future (the session ending mid-hook)
        // must not leave the detached group running.
        let mut guard = crate::exec::GroupKillGuard::arm(pid);

        // Feed the payload on a DETACHED task and let the timeout-bounded
        // capped wait below drain stdout concurrently. Writing inline
        // before the wait was a hang: a hook that never reads stdin blocks the
        // write once the payload exceeds the OS pipe buffer (~64 KiB), so the
        // session hung forever, before the timeout window even opened. Writing
        // concurrently — and dropping stdin to signal EOF for hooks that DO
        // read (`cat`, `jq`) — means a non-reading hook is instead bounded by
        // the timeout; a timed-out kill drops the pipe and the write EPIPEs
        // and the task ends (no leak). `kill_on_drop` reaps the child.
        if let Some(mut stdin) = child.stdin.take() {
            let payload = payload_json.to_string(); // owned: the task outlives the borrow
            tokio::spawn(async move {
                let _ = stdin.write_all(payload.as_bytes()).await;
                // `stdin` drops here → EOF for a hook reading to end-of-input.
            });
        }

        let timeout_ms = action.effective_timeout_ms();
        // Capped capture: a hook is repository- or operator-authored shell, so
        // the volume it prints is not Stella's to trust — `wait_with_output`
        // would hold all of it (see `crate::exec::MAX_CAPTURE_BYTES`).
        let waited = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            crate::exec::wait_with_capped_output(child, crate::exec::MAX_CAPTURE_BYTES),
        )
        .await;
        match waited {
            // Timeout elapsed: kill the whole group, then let dropping the
            // in-flight wait future reap the direct child via
            // `kill_on_drop`.
            Err(_) => {
                // Disarms and kills the group in one step, guarding on a real
                // pid: kill(-0, …) would hit Stella's OWN group.
                guard.kill_now();
                Err(HookExecError::TimedOut {
                    command: action.command.clone(),
                    timeout_ms,
                })
            }
            // Wait failure leaves the child's state unknown — the still-armed
            // guard kills the group on return rather than leak it.
            Ok(Err(e)) => Err(HookExecError::SpawnFailed {
                command: action.command.clone(),
                message: format!("could not collect hook output: {e}"),
            }),
            Ok(Ok(output)) => {
                guard.disarm();
                Ok(HookExecResult {
                    exit_code: output.status.code().unwrap_or(-1),
                    stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                    stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(command: &str) -> HookAction {
        HookAction::new(command)
    }

    fn plugin_action(argv: &[&str], env: &[&str]) -> HookAction {
        HookAction::from_plugin(
            PluginHookOrigin {
                plugin: "fixture".to_string(),
                argv: argv.iter().map(|arg| (*arg).to_string()).collect(),
                env_allowlist: env.iter().map(|name| (*name).to_string()).collect(),
            },
            5_000,
        )
    }

    /// **Witness: a plugin's argv is executed, not interpreted.**
    ///
    /// The argument is shell metacharacters end to end. Run through
    /// `bash -c`, `$HOME` would expand and the `;` would start a second
    /// command; run as argv it is one literal string on stdout. This is the
    /// property that makes it safe to dispatch a third party's `[runtime]`
    /// argv at all, so it is asserted rather than argued.
    #[tokio::test]
    async fn a_plugin_action_runs_its_argv_and_never_a_shell() {
        let dir = tempfile::tempdir().expect("tempdir");
        let hostile = "$HOME; echo second";
        let out = HostHookRunner
            .run(
                &plugin_action(&["/bin/echo", hostile], &["PATH"]),
                "{}",
                &dir.path().display().to_string(),
            )
            .await
            .expect("hook runs");
        assert_eq!(out.exit_code, 0);
        assert_eq!(
            out.stdout.trim(),
            hostile,
            "the argument reached the program byte for byte"
        );
    }

    /// **Witness: default-deny.** The child sees the declared names and
    /// nothing else — not the parent's `HOME`, which every test process has.
    #[tokio::test]
    async fn a_plugin_child_starts_from_an_empty_environment() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(
            std::env::var("HOME").is_ok(),
            "the fixture needs an undeclared variable to be missing FROM"
        );
        let out = HostHookRunner
            .run(
                &plugin_action(&["/usr/bin/env"], &["PATH"]),
                "{}",
                &dir.path().display().to_string(),
            )
            .await
            .expect("hook runs");
        assert!(
            out.stdout.lines().any(|line| line.starts_with("PATH=")),
            "the declared name is present: {}",
            out.stdout
        );
        assert!(
            !out.stdout.lines().any(|line| line.starts_with("HOME=")),
            "an undeclared name is absent, not merely scrubbed: {}",
            out.stdout
        );
    }

    /// **Witness: the manifest cannot ask its way to a model credential.**
    ///
    /// The same refusal `SubprocessWrapper::declare` makes at the socket,
    /// through the same `is_sensitive_env_name`, so the hook transport and
    /// the wrapper transport cannot give a plugin two different answers about
    /// the key that pays for the agent (invariant 3, #3512).
    #[test]
    fn a_declared_model_credential_is_withheld_from_a_plugin_hook() {
        let origin = PluginHookOrigin {
            plugin: "greedy".to_string(),
            argv: vec!["/bin/echo".to_string()],
            env_allowlist: vec![
                "PLUGIN_MODE".to_string(),
                "ANTHROPIC_API_KEY".to_string(),
                "GITHUB_TOKEN".to_string(),
            ],
        };
        // Answers for every name, so absence below is the refusal and not a
        // variable that happened to be unset on this machine.
        let command = plugin_command(&origin, |_| Some("value".to_string())).expect("a program");
        let names: Vec<String> = command
            .as_std()
            .get_envs()
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec!["PLUGIN_MODE".to_string()],
            "a credential is withheld even though the manifest declared it"
        );
    }

    /// An origin with no program is a named failure, never a panic — the
    /// port takes plain data and invariant 5 governs it.
    #[test]
    fn an_origin_with_no_program_is_a_named_spawn_failure() {
        let origin = PluginHookOrigin {
            plugin: "empty".to_string(),
            argv: Vec::new(),
            env_allowlist: Vec::new(),
        };
        let err = plugin_command(&origin, |_| None).expect_err("no program to run");
        assert!(matches!(err, HookExecError::SpawnFailed { .. }));
    }

    #[tokio::test]
    async fn runs_the_command_in_the_given_cwd_and_captures_stdout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = HostHookRunner
            .run(&action("pwd"), "{}", &dir.path().display().to_string())
            .await
            .expect("hook runs");
        assert_eq!(out.exit_code, 0);
        // Canonicalized comparison: macOS tempdirs live behind /private.
        let reported = std::path::Path::new(out.stdout.trim())
            .canonicalize()
            .expect("canonical stdout path");
        assert_eq!(reported, dir.path().canonicalize().expect("canonical dir"));
    }

    #[tokio::test]
    async fn pipes_the_payload_json_on_stdin() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = HostHookRunner
            .run(
                &action("cat"),
                r#"{"event":"PreToolUse"}"#,
                &dir.path().display().to_string(),
            )
            .await
            .expect("hook runs");
        assert!(out.stdout.contains("PreToolUse"));
    }

    #[tokio::test]
    async fn hook_scrubs_inherited_credentials_but_keeps_benign_env() {
        let _fixture = crate::subprocess_env::test_support::InheritedCredentialFixture::install();
        let dir = tempfile::tempdir().expect("tempdir");
        let out = HostHookRunner
            .run(
                &action(crate::subprocess_env::test_support::PROBE_COMMAND),
                "{}",
                &dir.path().display().to_string(),
            )
            .await
            .expect("hook runs");
        crate::subprocess_env::test_support::assert_scrubbed(&out.stdout);
    }

    /// The hook runner used to apply only the credential scrub — a hook
    /// spawned from inside a git hook inherited the outer repo's GIT_DIR.
    #[tokio::test]
    async fn hook_scrubs_git_repo_and_forced_color_env() {
        let _fixture = crate::subprocess_env::test_support::SpawnHygieneFixture::install();
        let dir = tempfile::tempdir().expect("tempdir");
        let out = HostHookRunner
            .run(
                &action(crate::subprocess_env::test_support::SPAWN_HYGIENE_PROBE_COMMAND),
                "{}",
                &dir.path().display().to_string(),
            )
            .await
            .expect("hook runs");
        crate::subprocess_env::test_support::assert_spawn_hygiene_scrubbed(&out.stdout);
    }

    #[tokio::test]
    async fn nonzero_exit_is_a_result_not_an_error_with_stderr_captured() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = HostHookRunner
            .run(
                &action("echo blocked 1>&2; exit 3"),
                "{}",
                &dir.path().display().to_string(),
            )
            .await
            .expect("ran to completion");
        assert_eq!(out.exit_code, 3);
        assert!(out.stderr.contains("blocked"));
    }

    #[tokio::test]
    async fn a_hung_hook_times_out_with_the_named_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut hung = action("sleep 30");
        hung.timeout_ms = Some(150);
        let started = std::time::Instant::now();
        let err = HostHookRunner
            .run(&hung, "{}", &dir.path().display().to_string())
            .await
            .expect_err("times out");
        assert!(matches!(err, HookExecError::TimedOut { .. }));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "timeout enforced promptly"
        );
    }

    #[tokio::test]
    async fn a_hook_that_ignores_stdin_still_completes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let out = HostHookRunner
            .run(
                &action("echo ok"),
                "{\"big\":\"payload\"}",
                &dir.path().display().to_string(),
            )
            .await
            .expect("hook runs");
        assert_eq!(out.exit_code, 0);
        assert!(out.stdout.contains("ok"));
    }

    #[tokio::test]
    async fn a_non_reading_hook_with_a_large_payload_times_out_not_hangs() {
        // `sleep 30` never reads stdin. With a payload larger than the OS
        // pipe buffer (~64 KiB), the old inline `write_all` blocked forever
        // BEFORE the timeout window opened — the session hung. The write is
        // now detached, so the timeout still bounds the run.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut hung = action("sleep 30");
        hung.timeout_ms = Some(150);
        let big_payload = format!("{{\"blob\":\"{}\"}}", "x".repeat(256 * 1024));
        let started = std::time::Instant::now();
        let err = HostHookRunner
            .run(&hung, &big_payload, &dir.path().display().to_string())
            .await
            .expect_err("must time out, not hang on the stdin write");
        assert!(matches!(err, HookExecError::TimedOut { .. }));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "timeout must still be enforced despite the unread large payload"
        );
    }
}
