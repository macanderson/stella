//! Why the agent has no verb that leaves a service down (#2864).
//!
//! On `pypi-server__HRUntZQ` an agent ran `stop_process` on the server it had
//! been asked to build — twice, at steps 60 and 83 — so that its witness test
//! would fail, and restarted it at second 918 of a 921-second trial. The
//! witness contract turns inside out at that point: a proof is supposed to
//! establish that the *code* was wrong before the change, and this one
//! established only that the agent had unplugged the subject. The grader then
//! connected to a dead port and correct work scored zero. #2668 removed the
//! motive for the common case; it removed no capability, and the move stayed
//! available and stayed catastrophic.
//!
//! # The line drawn here, and why it is not a blocklist
//!
//! The capability withdrawn is not *stopping*. It is **leaving a service
//! down**. After this module there is no tool in the agent's vocabulary whose
//! postcondition is "the process the run is judged on is not running":
//!
//! * [`super::StopProcess`] refuses a live service with a named refusal
//!   ([`StopRefusal`]) that says what to assert instead. It still reaps a
//!   handle whose process has already exited, which is all it was ever doing
//!   in the honest case.
//! * [`RestartProcess`] replaces what runs under a handle — the same argv to
//!   pick up an edit, a corrected argv to fix a bad invocation — and returns
//!   only once something is running there again. Every legitimate reason to
//!   stop a service was really a reason to replace it.
//!
//! That is a structural property rather than a rule about intent: nothing has
//! to detect that a stop was *aimed* at manufacturing a baseline, because no
//! sequence of the remaining verbs reaches the state the manufacture needs.
//! A blocklist keyed on "is this the graded service?" could not have worked
//! anyway — nothing in the tree declares one, and a declaration the agent
//! makes is a declaration the agent can withhold.
//!
//! **What this costs, stated plainly:** an agent can no longer take a service
//! it started out of service and leave it that way. A process started with
//! the wrong argv is corrected through [`RestartProcess`], not killed; an
//! operator who wants the verb gone entirely still has
//! `"tools": {"stop_process": "off"}`. The one narrowing that follows from
//! it — the live-process cap can now be relieved only by a process exiting —
//! is tracked in #2999. This is a behaviour change beyond verification, and it
//! is the one #2864 asks for.
//!
//! The spawn itself lives here too, as `spawn_entry`, so `start_process` and
//! a restart cannot drift apart on env scrub, log handling or process-group
//! policy — one of them silently keeping a credential or a pipe would be the
//! whole of #2666 again.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};
use tokio::process::Command;

use super::{ProcessEntry, ProcessTableHandle, STOP_GRACE_MS, unknown_handle_error};
use crate::registry::Tool;

/// Why a process could not be started.
///
/// Named variants rather than a formatted string because the four causes have
/// genuinely different remedies — a full disk, a scratch directory that is not
/// writable, and a missing program are not one error with three spellings.
#[derive(Debug)]
pub(super) enum SpawnFailure {
    /// The log file could not be created.
    LogCreate(std::io::Error),
    /// The log file could not be persisted out of its temporary handle.
    LogPersist(std::io::Error),
    /// The log handle could not be duplicated onto stdout and stderr.
    LogDuplicate(std::io::Error),
    /// The program itself did not start.
    Spawn {
        /// The argv as displayed to the model.
        display: String,
        /// What the OS said.
        source: std::io::Error,
    },
}

impl std::fmt::Display for SpawnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LogCreate(e) => write!(f, "failed to create a log file for the process: {e}"),
            Self::LogPersist(e) => write!(f, "failed to persist the process's log file: {e}"),
            Self::LogDuplicate(e) => write!(f, "failed to duplicate the process log handle: {e}"),
            Self::Spawn { display, source } => write!(f, "failed to spawn `{display}`: {source}"),
        }
    }
}

impl std::error::Error for SpawnFailure {}

/// Why a stop was refused.
///
/// One variant today, and a type rather than a string anyway: this is the
/// discriminant a caller (or a future diagnostic record) branches on, while
/// [`Display`](std::fmt::Display) is the remediation the model reads. The
/// message names the substitute, because a refusal that only says "no" teaches
/// the model to find another way to the same state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopRefusal {
    /// The handle names a process that is still running.
    LiveService {
        /// The handle as the model spelled it.
        handle: String,
        /// The argv, for the message.
        display: String,
        /// The label `start_process` was given, if any.
        name: Option<String>,
    },
}

impl std::fmt::Display for StopRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self::LiveService {
            handle,
            display,
            name,
        } = self;
        let label = name.as_ref().map(|n| format!(" ({n})")).unwrap_or_default();
        write!(
            f,
            "refused: {handle}{label} is still running `{display}`, and stopping a running \
             service is not available. A service is usually the deliverable, and taking it \
             down to make a test fail proves the test can detect an absent server, not that \
             the code was wrong — the grader then finds a dead port and correct work scores \
             zero. Assert against the RUNNING service instead (connect to it, drive it, check \
             its output with read_output). To pick up an edit, or to correct a bad argv, use \
             restart_process — it replaces what runs under {handle} and leaves it running."
        )
    }
}

impl std::error::Error for StopRefusal {}

/// Spawn one managed process. The single spawn policy both `start_process`
/// and [`RestartProcess`] go through.
///
/// stdout and stderr are interleaved into a private file rather than a pipe
/// this process owns the read end of: a pipe faults the child with `SIGPIPE`
/// when Stella exits, which is what #2666 was.
pub(super) fn spawn_entry(
    argv: &[String],
    name: Option<String>,
    cwd: &Path,
    scratch: Option<&Path>,
) -> Result<ProcessEntry, SpawnFailure> {
    let log_dir = scratch
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let named = tempfile::Builder::new()
        .prefix("stella-proc-")
        .suffix(".log")
        .tempfile_in(&log_dir)
        .map_err(SpawnFailure::LogCreate)?;
    let (log_file, log_path) = named
        .keep()
        .map_err(|e| SpawnFailure::LogPersist(e.error))?;
    let stdout_stdio = match log_file.try_clone() {
        Ok(f) => std::process::Stdio::from(f),
        Err(e) => {
            let _ = std::fs::remove_file(&log_path);
            return Err(SpawnFailure::LogDuplicate(e));
        }
    };
    let stderr_stdio = std::process::Stdio::from(log_file);

    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    cmd.current_dir(cwd);
    // Full spawn policy: git-repo retargeting and forced-color overrides are
    // scrubbed along with credentials, exactly like `exec::drive` — a
    // server's output lands in its log file, never a terminal.
    crate::subprocess_env::scrub_spawn_env(&mut cmd);
    crate::subprocess_env::inject_scratch_env(&mut cmd, scratch);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(stdout_stdio);
    cmd.stderr(stderr_stdio);
    // No `kill_on_drop`: a started process is deliberately left running when
    // this Rust value drops — see the module doc of `super`.
    // Own process group so a stop can take down grandchildren.
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(source) => {
            let _ = std::fs::remove_file(&log_path);
            return Err(SpawnFailure::Spawn {
                display: argv.join(" "),
                source,
            });
        }
    };
    let pid = child.id().unwrap_or(0) as i32;
    let stdin = child.stdin.take();
    Ok(ProcessEntry {
        name,
        display: argv.join(" "),
        argv: argv.to_vec(),
        cwd: cwd.to_path_buf(),
        child,
        stdin,
        stdin_closed: false,
        log_path,
        read_pos: 0,
        pid,
        exit_code: None,
    })
}

/// How a termination ended.
pub(super) enum Terminated {
    /// It exited within the SIGTERM grace window.
    Exited(i32),
    /// It ignored SIGTERM and was killed.
    Killed(Option<i32>),
    /// The entry left the table mid-stop.
    Vanished,
}

/// Close stdin, SIGTERM the group, wait out [`STOP_GRACE_MS`], then SIGKILL.
///
/// Factored out of `stop_process` so a restart takes the identical path: the
/// grace window is what lets a REPL or a server flush and exit cleanly, and a
/// restart that skipped it would corrupt exactly the state a stop preserves.
/// The table lock is released across every sleep — a two-second stop must not
/// freeze `read_output` on unrelated handles.
pub(super) async fn terminate(table: &ProcessTableHandle, handle: &str) -> Terminated {
    let pid = {
        let mut guard = table.lock().unwrap_or_else(|p| p.into_inner());
        let Some(entry) = guard.entries.get_mut(handle) else {
            return Terminated::Vanished;
        };
        entry.stdin = None;
        // Latch the close so a send_stdin holding the handle out for an
        // in-flight write cannot put it back afterwards.
        entry.stdin_closed = true;
        if let Some(code) = entry.poll_exit() {
            return Terminated::Exited(code);
        }
        #[cfg(unix)]
        if entry.pid > 0 {
            // SAFETY: signalling the child's own process group; the > 0
            // guard means we never signal our own group.
            unsafe {
                libc::kill(-entry.pid, libc::SIGTERM);
            }
        }
        entry.pid
    };
    let mut waited = 0u64;
    loop {
        {
            let mut guard = table.lock().unwrap_or_else(|p| p.into_inner());
            let Some(entry) = guard.entries.get_mut(handle) else {
                return Terminated::Vanished;
            };
            if let Some(code) = entry.poll_exit() {
                return Terminated::Exited(code);
            }
        }
        if waited >= STOP_GRACE_MS {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        waited += 100;
    }
    #[cfg(unix)]
    if pid > 0 {
        // SAFETY: same guarded group signal as above.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
    let mut guard = table.lock().unwrap_or_else(|p| p.into_inner());
    match guard.entries.get_mut(handle) {
        Some(entry) => {
            let _ = entry.child.start_kill();
            Terminated::Killed(entry.poll_exit())
        }
        None => Terminated::Vanished,
    }
}

/// `restart_process` — replace what runs under a handle, leaving it running.
pub struct RestartProcess {
    /// The shared process table.
    pub handle: ProcessTableHandle,
    /// Session scratch, for the replacement's log file and `STELLA_SCRATCH`.
    pub scratch: Option<PathBuf>,
}

#[async_trait]
impl Tool for RestartProcess {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "restart_process".into(),
            description: "Replace what runs under a started process handle: stops the current \
                          process (SIGTERM, brief grace, then SIGKILL) and immediately starts it \
                          again under the same handle. Use it to pick up an edit, or pass argv \
                          to correct a bad invocation. The handle is always left running."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "handle": { "type": "string", "description": "Handle returned by start_process" },
                    "argv": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Replacement argv (argv[0] is the program). Omit to restart the same command."
                    }
                },
                "required": ["handle"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, _root: &std::path::Path) -> ToolOutput {
        let handle = match crate::input::required_str(input, "handle") {
            Ok(v) => v.to_string(),
            Err(err) => {
                return ToolOutput::error(err.to_string());
            }
        };
        let override_argv: Option<Vec<String>> =
            input.get("argv").and_then(|v| v.as_array()).map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            });
        if override_argv.as_ref().is_some_and(Vec::is_empty) {
            return ToolOutput::error(
                "`argv`, when given, must be a non-empty array of strings (argv[0] is \
                          the program) — omit it to restart the same command",
            );
        }

        // Read the replacement's shape before anything is signalled, so a
        // handle that cannot be restarted is reported without first taking
        // down the process it names.
        let (argv, name, cwd) = {
            let table = self.handle.lock().unwrap_or_else(|p| p.into_inner());
            match table.entries.get(&handle) {
                Some(entry) => (
                    override_argv.unwrap_or_else(|| entry.argv.clone()),
                    entry.name.clone(),
                    entry.cwd.clone(),
                ),
                None => {
                    if table.tombstones.contains_key(&handle) {
                        return ToolOutput::error(format!(
                            "{handle} was reaped and its command is no longer recorded — \
                                 start the replacement with start_process"
                        ));
                    }
                    return unknown_handle_error(&table, &handle);
                }
            }
        };

        let stopped = match terminate(&self.handle, &handle).await {
            Terminated::Exited(code) => format!("exited (code {code})"),
            Terminated::Killed(Some(code)) => format!("killed after SIGTERM grace (code {code})"),
            Terminated::Killed(None) => "killed after SIGTERM grace".to_string(),
            Terminated::Vanished => {
                return ToolOutput::error(format!("{handle} disappeared while restarting"));
            }
        };

        let replacement = match spawn_entry(&argv, name.clone(), &cwd, self.scratch.as_deref()) {
            Ok(entry) => entry,
            Err(failure) => {
                // The old process is already down and the new one did not
                // start. Say both halves: a message that reported only the
                // spawn failure would leave the model believing the service
                // it asked to restart is still up.
                return ToolOutput::error(format!(
                    "{handle} was stopped ({stopped}) but its replacement did not start: \
                         {failure} — nothing is running under {handle} now; fix the command and \
                         call restart_process again with argv"
                ));
            }
        };
        let display = replacement.display.clone();
        let pid = replacement.pid;
        // Replacing the entry does not grow the table, so a restart is never
        // refused by the live-process cap the way stop-then-start would be.
        let previous = {
            let mut table = self.handle.lock().unwrap_or_else(|p| p.into_inner());
            table.entries.insert(handle.clone(), replacement)
        };
        // The replaced process's log is discarded with it: `read_output` on
        // this handle now reports the new process, and keeping the old file
        // would leak one log per restart for output nothing can attribute.
        if let Some(previous) = previous {
            let _ = std::fs::remove_file(&previous.log_path);
        }
        ToolOutput::Ok {
            content: format!(
                "{handle} restarted: previous process {stopped}; now running `{display}`{} \
                 (pid {pid}) — poll it with read_output",
                name.map(|n| format!(" ({n})")).unwrap_or_default()
            ),
        }
    }
}
