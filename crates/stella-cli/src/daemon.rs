//! Process-level supervision (#1552): run a stella verb detached from the
//! terminal that launched it, so closing that terminal no longer kills the
//! work.
//!
//! # What this is, and is not
//!
//! This is the first of the two supervision levels #1552 names. The child is
//! spawned in its **own process group** with its output captured to a log
//! file, so a closed terminal (SIGHUP to the launcher's group) never reaches
//! it, and `stella daemon attach` can stream the log later from any
//! terminal. It does NOT survive a crash mid-turn — that is engine-level
//! checkpoint/resume (`stella-engine`), deliberately not blocked on here.
//!
//! # No second registry
//!
//! A detached run registers itself: every headless run already announces a
//! [`stella_store::SessionRecord`] with its own pid
//! (`agent::presence::SessionPresence`), and the registry's `list` applies
//! the pid-liveness downgrade at read time. So the supervisor writes no
//! record of its own — `stella daemon list` IS the session registry, and the
//! only state this module adds is the log file, keyed by the child's pid
//! under `~/.stella/daemon-logs/`. The launcher discovers the child's
//! session id by polling the registry for a record carrying that pid;
//! `attach`/`stop`/`logs` accept either the session id or the bare pid.
//!
//! # Stopping respects the run's own boundaries
//!
//! `stop` sends SIGTERM to the child's process group and gives it a grace
//! window to wind down before SIGKILL — the cooperative signal first, so the
//! run's own handling (budget aborts at safe boundaries, never mid-tool —
//! invariant 6) decides where to stop, and the hard kill is a last resort
//! for a wedged process, never the opener.

use std::io::{Read, Seek, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use stella_store::{SessionRecord, SessionRegistry, SessionStatus};

/// `install` / `uninstall`: the service-manager half (#1587) — what makes a
/// registered invocation come back after the logout and reboot that a
/// detached process cannot cross on its own.
mod service;

/// `stella daemon` — the supervision surface over detached runs.
#[derive(Debug, clap::Subcommand)]
pub(crate) enum DaemonCmd {
    /// List this machine's sessions, detached runs marked live
    List,
    /// Stream a detached run's output (Ctrl-C detaches; the run continues)
    Attach {
        /// Session id (`ses-…`) or the run's pid
        target: String,
    },
    /// Ask a detached run to stop (SIGTERM, then SIGKILL after a grace period)
    Stop {
        /// Session id (`ses-…`) or the run's pid
        target: String,
    },
    /// Print the path of a detached run's log file
    Logs {
        /// Session id (`ses-…`) or the run's pid
        target: String,
    },

    /// Register a stella invocation with the OS's per-user service manager
    ///
    /// A detached run survives its terminal; only the service manager
    /// survives logout and reboot. This writes a per-user definition — a
    /// launchd agent on macOS, a `systemd --user` unit on Linux — that starts
    /// the given invocation in the current directory at every login (macOS)
    /// or boot (Linux, once lingering is on), and loads it now.
    ///
    /// Registration, not resume: the command starts from scratch each time,
    /// so register standing verbs (`monitor`, a fleet watch) — a one-shot
    /// `run` would repeat its work, and its spend, on every boot. By default
    /// a command that exits stays down; `--keep-alive` opts into restarts.
    Install {
        /// Name for the service (default: the command's first word).
        /// Lowercase letters, digits and dashes.
        #[arg(long)]
        label: Option<String>,

        /// Restart the command whenever it exits, at most once a minute.
        /// Off by default: an agent that fails the moment it starts would
        /// otherwise be restarted forever, spending budget each time.
        #[arg(long)]
        keep_alive: bool,

        /// The stella invocation to register, after `--`:
        /// `stella daemon install -- monitor --interval 300`
        #[arg(last = true, required = true, value_name = "STELLA ARGS")]
        command: Vec<String>,
    },

    /// Remove a service registered by `install`
    ///
    /// Unloads it from the service manager and deletes the definition.
    /// Already-gone is reported and succeeds — uninstalling twice is the
    /// requested state, not an error.
    Uninstall {
        /// The label `install` reported.
        label: String,
    },
}

/// Where detached runs' console output lives: one `<pid>.log` per run.
///
/// Keyed by pid rather than session id because the log file must exist —
/// open, redirected into — *before* the child runs, while the session id is
/// minted by the child itself at announce time. The pid is known the moment
/// `spawn` returns, and the registry record carries it, so `attach` can join
/// the two without this module owning any registry state.
fn logs_dir() -> PathBuf {
    stella_home::data_dir().join("daemon-logs")
}

fn log_path(pid: u32) -> PathBuf {
    logs_dir().join(format!("{pid}.log"))
}

/// Re-exec the current binary with `argv`, detached: own process group, no
/// stdin, stdout+stderr appended to the pid-keyed log. Returns the child pid.
fn spawn_detached(argv: &[std::ffi::OsString]) -> Result<u32, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find own binary: {e}"))?;
    let dir = logs_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    // The log must be open before the pid exists, so it is born under a
    // launcher-unique name and renamed once `spawn` returns. Renaming an
    // open file is fine on unix — the child's fd follows the inode.
    let pending = dir.join(format!("pending-{}.log", std::process::id()));
    let log = std::fs::File::create(&pending)
        .map_err(|e| format!("cannot create {}: {e}", pending.display()))?;
    let log_err = log
        .try_clone()
        .map_err(|e| format!("cannot clone log handle: {e}"))?;

    let mut cmd = std::process::Command::new(exe);
    cmd.args(argv)
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(log_err);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // Its own group: the terminal's SIGHUP/SIGINT go to the LAUNCHER's
        // group, so the run never sees them — and `stop` has one address
        // (`kill -PGID`) that covers every process the run spawns.
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = &cmd;
        return Err("detached runs are not supported on this platform yet".to_string());
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("cannot spawn detached run: {e}"))?;
    let pid = child.id();
    // Best-effort: a failed rename leaves the log under its pending name,
    // which `logs`/`attach` cannot find — say so rather than limp.
    std::fs::rename(&pending, log_path(pid))
        .map_err(|e| format!("cannot rename log for pid {pid}: {e}"))?;
    // Deliberately no `child.wait()`: the launcher exits and the child is
    // reparented. The zombie window is the launcher's own lifetime, which
    // ends momentarily.
    std::mem::forget(child);
    Ok(pid)
}

/// The `stella run --detach` path: rebuild this invocation's argv without
/// `--detach`, hand the *resolved* prompt to the child explicitly (its stdin
/// is `/dev/null`, so a piped prompt cannot be re-read there), spawn it
/// detached, and print how to follow it.
///
/// Note the prompt becomes a process argument, visible in `ps`, exactly as
/// an inline `stella run "…"` already is — only a stdin-piped prompt gains
/// that visibility by detaching.
pub(crate) fn detach_run(resolved_prompt: &str, prompt_was_inline: bool) -> Result<(), String> {
    let argv = child_argv(
        std::env::args_os().skip(1),
        prompt_was_inline,
        resolved_prompt,
    );
    let pid = spawn_detached(&argv)?;
    // The child mints its session id at announce time; give it a moment so
    // the printed hint can name the id rather than the pid. Either works.
    let handle = match discover_session(pid, Duration::from_secs(3)) {
        Some(record) => record.id,
        None => pid.to_string(),
    };
    println!("detached: pid {pid}");
    println!("  follow: stella daemon attach {handle}");
    println!("  stop:   stella daemon stop {handle}");
    Ok(())
}

/// This invocation's argv, rewritten for the detached child: `--detach`
/// itself is stripped (before any `--` separator, so a prompt that literally
/// reads `--detach` survives), and a prompt that arrived on stdin is
/// delivered as an explicit trailing argument instead — the child's stdin is
/// `/dev/null`, so nothing can be re-read there. The `-` read-stdin marker
/// goes with it.
fn child_argv(
    args: impl Iterator<Item = std::ffi::OsString>,
    prompt_was_inline: bool,
    resolved_prompt: &str,
) -> Vec<std::ffi::OsString> {
    let mut argv: Vec<std::ffi::OsString> = Vec::new();
    let mut past_separator = false;
    let mut detach_stripped = false;
    for arg in args {
        if !past_separator {
            if arg == "--" {
                past_separator = true;
            } else if !detach_stripped && arg == "--detach" {
                detach_stripped = true;
                continue;
            } else if !prompt_was_inline && arg == "-" {
                continue;
            }
        }
        argv.push(arg);
    }
    if !prompt_was_inline {
        if !past_separator {
            argv.push("--".into());
        }
        argv.push(resolved_prompt.into());
    }
    argv
}

/// The registry record announcing `pid`, if the child has gotten that far.
fn discover_session(pid: u32, patience: Duration) -> Option<SessionRecord> {
    let registry = SessionRegistry::open_default();
    let deadline = Instant::now() + patience;
    loop {
        if let Some(record) = registry.list().into_iter().find(|r| r.pid == pid) {
            return Some(record);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `target` as a registry record: a session id first, a bare pid second.
fn resolve(registry: &SessionRegistry, target: &str) -> Result<SessionRecord, String> {
    let records = registry.list();
    if let Some(record) = records.iter().find(|r| r.id == target) {
        return Ok(record.clone());
    }
    if let Ok(pid) = target.parse::<u32>()
        && let Some(record) = records.iter().find(|r| r.pid == pid)
    {
        return Ok(record.clone());
    }
    Err(format!(
        "no session matches `{target}` — `stella daemon list` shows what exists"
    ))
}

/// `stella daemon <cmd>` dispatch.
pub(crate) fn run(cmd: DaemonCmd) -> Result<(), String> {
    let registry = SessionRegistry::open_default();
    match cmd {
        DaemonCmd::List => {
            let records = registry.list();
            if records.is_empty() {
                println!("no sessions.");
                return Ok(());
            }
            for record in records {
                let log = log_path(record.pid);
                let detached = if log.exists() { " [detached]" } else { "" };
                println!(
                    "{}  {}  pid {}{}  {}",
                    record.id,
                    record.status.label(),
                    record.pid,
                    detached,
                    record.title,
                );
            }
            Ok(())
        }
        DaemonCmd::Attach { target } => attach(&registry, &target),
        DaemonCmd::Stop { target } => stop(&registry, &target),
        DaemonCmd::Logs { target } => {
            let record = resolve(&registry, &target)?;
            let log = log_path(record.pid);
            if !log.exists() {
                return Err(format!(
                    "session {} has no daemon log — it was not started with --detach",
                    record.id
                ));
            }
            println!("{}", log.display());
            Ok(())
        }
        DaemonCmd::Install {
            label,
            keep_alive,
            command,
        } => service::install(label.as_deref(), keep_alive, &command),
        DaemonCmd::Uninstall { label } => service::uninstall(&label),
    }
}

/// Stream `target`'s log to stdout until the run ends. Ctrl-C interrupts
/// only this attach — the run is in its own process group and never sees it.
fn attach(registry: &SessionRegistry, target: &str) -> Result<(), String> {
    let record = resolve(registry, target)?;
    let log = log_path(record.pid);
    let mut file = std::fs::File::open(&log).map_err(|e| {
        format!(
            "session {} has no readable daemon log ({e}) — it was not started with --detach",
            record.id
        )
    })?;
    // Start from the beginning: an attach is "show me this run", and the
    // interesting part of a short run is usually already written.
    let mut offset = 0u64;
    let stdout = std::io::stdout();
    loop {
        file.seek(std::io::SeekFrom::Start(offset))
            .map_err(|e| format!("cannot seek log: {e}"))?;
        let mut chunk = Vec::new();
        file.read_to_end(&mut chunk)
            .map_err(|e| format!("cannot read log: {e}"))?;
        if !chunk.is_empty() {
            offset += chunk.len() as u64;
            let mut out = stdout.lock();
            let _ = out.write_all(&chunk);
            let _ = out.flush();
        }
        // Liveness through the registry's own downgrade rule, not a second
        // opinion: a live-status record whose pid is gone reads as Error.
        let live = registry
            .list()
            .into_iter()
            .find(|r| r.id == record.id)
            .is_some_and(|r| r.status.is_live());
        if !live {
            // One last drain — the run may have written between the read
            // above and its exit.
            file.seek(std::io::SeekFrom::Start(offset))
                .map_err(|e| format!("cannot seek log: {e}"))?;
            let mut tail = Vec::new();
            let _ = file.read_to_end(&mut tail);
            if !tail.is_empty() {
                let mut out = stdout.lock();
                let _ = out.write_all(&tail);
                let _ = out.flush();
            }
            eprintln!("— run ended ({}) —", record.id);
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// How long `stop` waits between the cooperative TERM and the hard KILL.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// TERM the run's process group, wait for it to wind down, KILL if it
/// will not, and record the outcome as `Cancelled` — a stop somebody asked
/// for, never `Error`.
fn stop(registry: &SessionRegistry, target: &str) -> Result<(), String> {
    let record = resolve(registry, target)?;
    if !record.status.is_live() {
        return Err(format!(
            "session {} is not running (status: {})",
            record.id,
            record.status.label()
        ));
    }
    signal_group(record.pid, Signal::Term)?;
    let deadline = Instant::now() + STOP_GRACE;
    let died = loop {
        let live = registry
            .list()
            .into_iter()
            .find(|r| r.id == record.id)
            .is_some_and(|r| r.status.is_live());
        if !live {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    if !died {
        signal_group(record.pid, Signal::Kill)?;
    }
    // Stamp the intent: without this the pid-liveness downgrade would show
    // the session as `Error` (crashed), when it was stopped on request.
    let _ = registry.set_status(&record.id, SessionStatus::Cancelled);
    println!(
        "stopped {} (pid {}){}",
        record.id,
        record.pid,
        if died {
            ""
        } else {
            " — killed after grace period"
        }
    );
    Ok(())
}

enum Signal {
    Term,
    Kill,
}

/// Send `signal` to the process GROUP `pgid` — the whole detached run,
/// including anything its shell tools spawned.
#[cfg(unix)]
fn signal_group(pgid: u32, signal: Signal) -> Result<(), String> {
    let signo = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // The child was spawned with `process_group(0)`, so its pgid equals its
    // pid; a pid too large for pid_t is corrupt and must not be negated into
    // an arbitrary group.
    let pid = libc::pid_t::try_from(pgid).map_err(|_| format!("pid {pgid} is not addressable"))?;
    let rc = unsafe { libc::kill(-pid, signo) };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!(
            "cannot signal process group {pgid}: {}",
            std::io::Error::last_os_error()
        ))
    }
}

#[cfg(not(unix))]
fn signal_group(_pgid: u32, _signal: Signal) -> Result<(), String> {
    Err("stopping detached runs is not supported on this platform yet".to_string())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// The witness #1552 asked for, at the supervision layer: a run spawned
    /// detached lives in its own process group with its output landing in a
    /// log a later process can stream, and `stop` ends it on request.
    ///
    /// The child here is a shell writing then sleeping — the supervision
    /// contract (detach, log, group signal) is identical for the real
    /// binary, whose spawn path differs only in the executable name.
    #[test]
    fn a_detached_run_outlives_its_launcher_context_and_stops_on_request() {
        // Not `spawn_detached` itself, which re-execs the CURRENT binary —
        // under `cargo test` that is the test harness. Same mechanics, pinned
        // executable.
        let dir = tempfile::tempdir().unwrap();
        let log_file = dir.path().join("run.log");
        let log = std::fs::File::create(&log_file).unwrap();
        let err = log.try_clone().unwrap();
        let mut cmd = std::process::Command::new("/bin/sh");
        {
            use std::os::unix::process::CommandExt as _;
            cmd.process_group(0);
        }
        let mut child = cmd
            .args(["-c", "echo supervised-output; exec sleep 30"])
            .stdin(std::process::Stdio::null())
            .stdout(log)
            .stderr(err)
            .spawn()
            .unwrap();
        let pid = child.id();

        // The output reaches the log without any terminal attached.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let text = std::fs::read_to_string(&log_file).unwrap_or_default();
            if text.contains("supervised-output") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "detached output never reached the log"
            );
            std::thread::sleep(Duration::from_millis(50));
        }

        // Its own process group: the group id is the child's pid, so a group
        // signal addresses the run without touching the launcher (this test).
        let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
        assert_eq!(pgid, pid as libc::pid_t, "child must lead its own group");

        // Reattach shape: a *different* handle on the log sees the content —
        // there is no live pipe to lose with the launching terminal.
        let reread = std::fs::read_to_string(&log_file).unwrap();
        assert!(reread.contains("supervised-output"));

        // And the group signal ends it. Reaped through the handle this test
        // deliberately kept — a zombie still answers a liveness probe, so
        // `try_wait` is the honest "it ended".
        signal_group(pid, Signal::Term).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(child.try_wait(), Ok(Some(_))) {
            assert!(Instant::now() < deadline, "TERM did not end the run");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// `attach`/`stop`/`logs` address a run by session id or bare pid; a
    /// target that is neither says so and names the tool that lists them.
    #[test]
    fn resolve_finds_by_id_and_by_pid_and_reports_misses() {
        let dir = tempfile::tempdir().unwrap();
        let registry = SessionRegistry::open(dir.path());
        // This test process's own pid, so the liveness downgrade keeps the
        // record listable.
        let mut record = SessionRecord::new("/tmp/ws", "ws");
        record.pid = std::process::id();
        registry.upsert(&record).unwrap();

        assert_eq!(resolve(&registry, &record.id).unwrap().id, record.id);
        assert_eq!(
            resolve(&registry, &record.pid.to_string()).unwrap().id,
            record.id
        );
        let miss = resolve(&registry, "ses-nonexistent").unwrap_err();
        assert!(miss.contains("stella daemon list"), "{miss}");
    }

    /// The argv rebuild: `--detach` is stripped, a stdin prompt is delivered
    /// as an explicit trailing argument (the child's stdin is null), and a
    /// prompt that literally IS `--detach` — after `--` — survives.
    #[test]
    fn child_argv_strips_the_flag_and_carries_a_resolved_prompt() {
        let rebuild = |args: &[&str], inline: bool, prompt: &str| -> Vec<String> {
            child_argv(args.iter().map(std::ffi::OsString::from), inline, prompt)
                .into_iter()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };

        assert_eq!(
            rebuild(&["run", "--detach", "fix the bug"], true, "fix the bug"),
            vec!["run", "fix the bug"]
        );
        // Piped prompt: the `-` marker goes, the resolved text arrives.
        assert_eq!(
            rebuild(&["run", "--detach", "-"], false, "from stdin"),
            vec!["run", "--", "from stdin"]
        );
        // A prompt that IS `--detach`, protected by the separator.
        assert_eq!(
            rebuild(&["run", "--detach", "--", "--detach"], true, "--detach"),
            vec!["run", "--", "--detach"]
        );
    }
}
