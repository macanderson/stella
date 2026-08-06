// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! `stella daemon install` / `uninstall`: registration with the OS's
//! per-user service manager (#1587).
//!
//! A supervised run (the parent module, #1552) survives its terminal, which
//! is as far as a process can go on its own behalf: nothing a process does
//! to itself survives a reboot, and on a machine configured to reap user
//! processes at logout, nothing keeps it registered either.
//! Surviving those is the service manager's job — launchd on macOS,
//! `systemd --user` on Linux — and these two verbs are the explicit,
//! reversible handshake with it. Explicit and reversible are requirements,
//! not adjectives: writing under `~/Library/LaunchAgents` and calling
//! `launchctl` is a side effect on the operator's machine outside any
//! workspace, so it happens only when `install` is typed and is undone
//! completely by `uninstall`.
//!
//! # What the installed service does — a decision, recorded
//!
//! It starts **one stella invocation the operator explicitly registered**, in
//! the directory `install` was run from, at every login (launchd) or boot
//! (systemd, once lingering is on). This is *service registration*: a service
//! manager re-**starts**, so a command launched at boot begins a fresh turn
//! and a fresh spend. That makes standing verbs — `monitor`, a fleet watch, a
//! deliberately perpetual run — the fit, and nothing is ever registered
//! implicitly: an ordinary terminal run is supervised, never installed.
//!
//! The one command for which restarting is the *wrong* verb has its own flag:
//! `--resume-all` registers [`super::boot`]'s sweep (#1627), which continues
//! the turns the reboot killed from their last committed step boundary
//! instead of starting anything over. It is still explicit, still one
//! registered invocation, and still removed by `uninstall`.
//!
//! Two consequences of the same budget concern, both deliberate:
//!
//! - **No restart unless asked.** The default definition starts the command
//!   at load and lets it stay down when it exits; only `--keep-alive` adds
//!   `KeepAlive` / `Restart=always`, and then throttled and (on systemd)
//!   start-limited. An agent that fails the moment it starts, under an
//!   unconditional restart policy, is a spend loop with no human in it.
//! - **The binary is resolved at load, loudly.** A definition naming this
//!   binary by absolute path breaks the day `brew upgrade` moves it, so the
//!   service execs through a `/bin/sh` shim that tries the recorded path,
//!   falls back to a `PATH` lookup widened with the usual install prefixes,
//!   and otherwise writes one line naming exactly what is missing to the
//!   service log and exits 127 — a failure the operator can read, not a
//!   silent no-show at boot.
//!
//! A service-run stella has no controlling terminal, so
//! [`crate::daemon::should_supervise`] (correctly) says no and the command runs in
//! this process — the service manager *is* its supervisor, and wrapping a
//! second one inside it would give launchd a child that exits the moment it
//! hands the work off. It follows that a registered run is not a *supervised*
//! run: `stella daemon list` lists supervised runs and will not show it. Its
//! console is the service log, and the manager's own query (`launchctl
//! print`, `systemctl --user status`) is the service's status surface.
//!
//! # Where things land
//!
//! The definition goes where the platform's manager reads —
//! `~/Library/LaunchAgents/sh.oxagen.stella.<label>.plist`,
//! `~/.config/systemd/user/stella-<label>.service` — resolved through
//! `stella-home` like every other home-anchored path. The console goes to
//! `~/.stella/services/<label>.log` on both platforms, under a directory only
//! the owner can list — a service's stdout is model output, not something
//! other local users get to read.

use std::path::{Path, PathBuf};

use colored::Colorize;

/// The seconds launchd waits between `--keep-alive` restarts
/// (`ThrottleInterval`), and systemd's `RestartSec`.
///
/// launchd's default is 10, which turns an instantly-failing command into six
/// starts a minute. One a minute is still visibly alive in the log while
/// being slow enough for an operator to notice and uninstall before the
/// spend matters.
const RESTART_SECS: u32 = 60;

/// systemd's brake that launchd lacks: after this many starts inside
/// [`START_LIMIT_WINDOW_SECS`], the unit enters the failed state and stays
/// down until a human looks. Five starts in ten minutes is a command that is
/// not going to succeed on the sixth.
const START_LIMIT_BURST: u32 = 5;
/// See [`START_LIMIT_BURST`].
const START_LIMIT_WINDOW_SECS: u32 = 600;

/// Labels stay boring on purpose: they become file names, launchd labels and
/// unit names, three grammars with three escaping stories that all accept
/// this intersection unescaped.
const LABEL_MAX: usize = 32;

/// Which per-user service manager this build can talk to.
///
/// A value, not a `cfg` fence, so every planning and rendering function below
/// compiles and is tested on every platform — the one `cfg` decision is
/// [`ServiceManager::current`], and everything downstream of it is data.
///
/// That design is exactly what `dead_code` misreads: [`ServiceManager::current`]
/// is the only constructor, and it is `cfg`-selected, so on any one target the
/// *other* platform's variant is never constructed even though every function
/// below still matches on it. The exemption is per-variant and points at the
/// target that cannot construct it, so each variant stays lint-checked on the
/// platform that can — a variant that went genuinely dead on its own target
/// would still be caught.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ServiceManager {
    /// macOS: a launchd agent in the user's `gui` domain.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Launchd,
    /// Linux: a `systemd --user` unit.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    SystemdUser,
}

impl ServiceManager {
    /// The manager for the platform this binary is running on.
    fn current() -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        {
            Ok(Self::Launchd)
        }
        #[cfg(target_os = "linux")]
        {
            Ok(Self::SystemdUser)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            Err(
                "stella daemon install/uninstall needs a per-user service manager, and this \
                 platform has neither launchd (macOS) nor systemd --user (Linux)"
                    .to_string(),
            )
        }
    }

    /// The definition's file name for `label`.
    fn file_name(self, label: &str) -> String {
        match self {
            // The `sh.oxagen.*` reverse-domain prefix matches the launchd
            // agents this project already ships (scripts/fullauto.sh).
            Self::Launchd => format!("sh.oxagen.stella.{label}.plist"),
            Self::SystemdUser => format!("stella-{label}.service"),
        }
    }

    /// Where the manager reads per-user definitions from, or `None` when no
    /// home directory is discoverable — which means "cannot install", never
    /// a fallback location, because the manager only reads the real one.
    fn definition_dir(self) -> Option<PathBuf> {
        match self {
            Self::Launchd => stella_home::launch_agents_dir(),
            Self::SystemdUser => stella_home::systemd_user_unit_dir(),
        }
    }
}

/// What the operator asked to register, fully resolved — the one bundle of
/// facts every rendering function below is a pure function of.
struct ServiceSpec<'a> {
    label: &'a str,
    /// The running binary, recorded as the shim's first candidate.
    exe: &'a Path,
    /// The registered stella argv (everything after `--`).
    args: &'a [String],
    /// The directory the service runs in — where `install` was typed.
    workdir: &'a Path,
    /// Where the service's stdout and stderr land.
    log_path: &'a Path,
    /// Whether the manager restarts the command when it exits.
    keep_alive: bool,
}

/// Everything `install` will do, computed before anything is touched, so the
/// side effects reduce to one file write and the loader calls — and so tests
/// can hold the whole outcome in their hands without a service manager.
struct ServicePlan {
    manager: ServiceManager,
    label: String,
    /// The definition file to write.
    path: PathBuf,
    /// Its full content.
    content: String,
    /// Where the service's stdout and stderr will land.
    log_path: PathBuf,
}

/// `stella daemon install [--label L] [--keep-alive] -- <stella args…>`, or
/// `stella daemon install --resume-all`.
pub(crate) fn install(
    label: Option<&str>,
    keep_alive: bool,
    resume_all: bool,
    command: &[String],
) -> Result<(), String> {
    let manager = ServiceManager::current()?;
    let command = resolve_command(resume_all, keep_alive, command)?;
    let command = command.as_slice();
    let label = match (label, resume_all) {
        (Some(label), _) => label.to_string(),
        (None, true) => super::boot::BOOT_LABEL.to_string(),
        (None, false) => derive_label(command)?,
    };
    validate_label(&label)?;

    let exe = std::env::current_exe()
        .map_err(|e| format!("cannot locate the stella binary to register: {e}"))?;
    let workdir = std::env::current_dir()
        .map_err(|e| format!("cannot resolve the directory the service should run in: {e}"))?;
    let definition_dir = manager
        .definition_dir()
        .ok_or("no home directory, so nowhere the service manager would read a definition from")?;
    let log_path = ensure_service_log_dir()?.join(format!("{label}.log"));

    let plan = plan(
        manager,
        &ServiceSpec {
            label: &label,
            exe: &exe,
            args: command,
            workdir: &workdir,
            log_path: &log_path,
            keep_alive,
        },
        &definition_dir,
    )?;
    write_definition(&plan)?;
    println!("{} wrote {}", "✓".green(), plan.path.display());
    load(&plan);
    if resume_all {
        println!(
            "  at every boot it {} the turns this machine interrupted — it never restarts one",
            "continues".green()
        );
        println!(
            "  preview what it would do: {}",
            "stella daemon resume-all --dry-run".cyan()
        );
    }
    println!(
        "  console: {}",
        plan.log_path.display().to_string().dimmed()
    );
    println!(
        "  remove with: {}",
        format!("stella daemon uninstall {label}").cyan()
    );
    Ok(())
}

/// `stella daemon uninstall <label>`.
///
/// Unloads first, removes the file second: the reverse order leaves the
/// manager running a definition that no longer exists on disk. Idempotent on
/// purpose — uninstalling something already gone reports it and succeeds,
/// because the state the operator asked for is the state they have.
pub(crate) fn uninstall(label: &str) -> Result<(), String> {
    let manager = ServiceManager::current()?;
    validate_label(label)?;
    let definition_dir = manager
        .definition_dir()
        .ok_or("no home directory, so nowhere a service definition could have been written")?;
    let path = definition_dir.join(manager.file_name(label));

    unload(manager, label);
    match remove_definition(&path)? {
        RemoveOutcome::Removed => println!("{} removed {}", "✓".green(), path.display()),
        RemoveOutcome::NothingInstalled => {
            eprintln!("{} nothing installed at {}", "▸".dimmed(), path.display());
        }
    }
    if manager == ServiceManager::SystemdUser {
        // Re-read now that the file is gone; stale in-memory units otherwise
        // survive until the next reload for an unrelated reason. Lingering is
        // deliberately left as the operator set it — other services may
        // depend on it, and `install` says so when it turns it on.
        run_tool(&["systemctl", "--user", "daemon-reload"], Quiet::Yes);
    }
    Ok(())
}

/// The argv to register, given the two mutually exclusive ways of asking.
///
/// `--resume-all` is not sugar over a command the operator could have typed
/// after `--`: it is the one registration whose whole point is that it does
/// *not* restart, so it names its own verb and refuses to be combined with
/// either an explicit command (which of the two would boot run?) or
/// `--keep-alive` (a sweep restarted every minute would re-enter its own
/// attempt bound forever — see [`super::boot`]).
fn resolve_command(
    resume_all: bool,
    keep_alive: bool,
    command: &[String],
) -> Result<Vec<String>, String> {
    if !resume_all {
        if command.is_empty() {
            return Err("nothing to register — give the stella invocation after `--`, or pass \
                        --resume-all to register the boot-time resume sweep"
                .to_string());
        }
        return Ok(command.to_vec());
    }
    if !command.is_empty() {
        return Err(
            "--resume-all registers `stella daemon resume-all`; drop the command after `--` \
             (install it as a second service if you want both)"
                .to_string(),
        );
    }
    if keep_alive {
        return Err(
            "--keep-alive cannot be combined with --resume-all: the sweep resumes what a boot \
             interrupted and exits, and restarting it every minute would spend against its own \
             attempt bound until it is exhausted"
                .to_string(),
        );
    }
    Ok(super::boot::registered_argv())
}

/// Compute the whole installation for `manager` from resolved inputs.
///
/// Pure over its arguments — no environment reads, no filesystem — which is
/// what lets the tests pin every byte of both definition formats on any
/// development platform.
fn plan(
    manager: ServiceManager,
    spec: &ServiceSpec<'_>,
    definition_dir: &Path,
) -> Result<ServicePlan, String> {
    let content = match manager {
        ServiceManager::Launchd => launchd_plist(spec)?,
        ServiceManager::SystemdUser => systemd_unit(spec)?,
    };
    Ok(ServicePlan {
        manager,
        label: spec.label.to_string(),
        path: definition_dir.join(manager.file_name(spec.label)),
        content,
        log_path: spec.log_path.to_path_buf(),
    })
}

/// Write the planned definition, refusing to replace one that exists.
///
/// Replacing would silently re-point a service the operator installed on
/// purpose, possibly while the manager still runs the old definition —
/// `uninstall` then `install` keeps both the human and the manager in the
/// loop. The atomic owner-only writer is deliberate: the definition embeds
/// the registered argv, which for an agent command can be a prompt.
fn write_definition(plan: &ServicePlan) -> Result<(), String> {
    if plan.path.exists() {
        return Err(format!(
            "{} already exists — `stella daemon uninstall {}` first, or pick another --label",
            plan.path.display(),
            plan.label
        ));
    }
    if let Some(parent) = plan.path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    stella_store::write_sensitive_file_atomic(&plan.path, plan.content.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", plan.path.display()))
}

/// What `uninstall` found on disk. Both are successes: the operator asked
/// for the service to be gone, and it is.
#[derive(PartialEq, Eq, Debug)]
enum RemoveOutcome {
    Removed,
    NothingInstalled,
}

/// Remove the definition file, distinguishing "removed it" from "was
/// already gone" by the removal's own result rather than a look-then-remove
/// race.
fn remove_definition(path: &Path) -> Result<RemoveOutcome, String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(RemoveOutcome::Removed),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RemoveOutcome::NothingInstalled),
        Err(e) => Err(format!("cannot remove {}: {e}", path.display())),
    }
}

/// The directory service consoles land in, created owner-only.
///
/// Owner-only because the log is the run's stdout, which is model output.
/// The manager creates the log files itself with default modes, so the
/// directory is what carries the restriction.
pub(super) fn ensure_service_log_dir() -> Result<PathBuf, String> {
    let dir = stella_home::data_dir().join("services");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("cannot restrict {}: {e}", dir.display()))?;
    }
    Ok(dir)
}

/// Hand the written definition to the service manager, falling back to
/// printing the exact command when the call fails — the fullauto pattern: a
/// failed `launchctl` must not strand the operator with a file and no next
/// step.
fn load(plan: &ServicePlan) {
    match plan.manager {
        ServiceManager::Launchd => {
            let domain = format!("gui/{}", current_uid());
            let path = plan.path.display().to_string();
            if run_tool(&["launchctl", "bootstrap", &domain, &path], Quiet::No) {
                println!(
                    "{} loaded — starts at every login, including after a reboot",
                    "✓".green()
                );
            } else {
                eprintln!(
                    "{} wrote the definition but launchctl bootstrap failed; load it with:",
                    "⚠".yellow()
                );
                eprintln!("    launchctl bootstrap {domain} {path}");
            }
        }
        ServiceManager::SystemdUser => {
            let unit = plan.manager.file_name(&plan.label);
            run_tool(&["systemctl", "--user", "daemon-reload"], Quiet::Yes);
            if run_tool(
                &["systemctl", "--user", "enable", "--now", &unit],
                Quiet::No,
            ) {
                println!("{} enabled and started", "✓".green());
            } else {
                eprintln!(
                    "{} wrote the definition but systemctl failed; enable it with:",
                    "⚠".yellow()
                );
                eprintln!(
                    "    systemctl --user daemon-reload && systemctl --user enable --now {unit}"
                );
            }
            // Without lingering, a user manager starts at login and is torn
            // down at logout — the exact boundary this issue exists to cross.
            if run_tool(&["loginctl", "enable-linger"], Quiet::No) {
                println!(
                    "{} lingering on — survives logout, starts at boot",
                    "✓".green()
                );
            } else {
                eprintln!(
                    "{} could not enable lingering; without it the service starts at login and \
                     stops at logout. Turn it on with: loginctl enable-linger",
                    "⚠".yellow()
                );
            }
        }
    }
}

/// Ask the manager to stop and forget the service, best-effort.
///
/// Quiet because the common uninstall is of a service that already exited or
/// was never loaded, and "no such service" from the manager is that path
/// working, not a problem to surface.
fn unload(manager: ServiceManager, label: &str) {
    match manager {
        ServiceManager::Launchd => {
            let target = format!("gui/{}/sh.oxagen.stella.{label}", current_uid());
            run_tool(&["launchctl", "bootout", &target], Quiet::Yes);
        }
        ServiceManager::SystemdUser => {
            let unit = manager.file_name(label);
            run_tool(
                &["systemctl", "--user", "disable", "--now", &unit],
                Quiet::Yes,
            );
        }
    }
}

/// Whether to surface a tool's failure output.
#[derive(PartialEq, Eq)]
enum Quiet {
    Yes,
    No,
}

/// Run a service-manager command and answer whether it succeeded.
///
/// Success output is dropped — `launchctl` and `systemctl` are silent when
/// they work — and failure output is surfaced (unless `Quiet::Yes`) because
/// the manager's own message is the actionable half of any failure here.
fn run_tool(argv: &[&str], quiet: Quiet) -> bool {
    let Some((program, args)) = argv.split_first() else {
        return false;
    };
    match std::process::Command::new(program).args(args).output() {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            if quiet == Quiet::No {
                let err = String::from_utf8_lossy(&out.stderr);
                let err = err.trim();
                if !err.is_empty() {
                    eprintln!("  {}", err.dimmed());
                }
            }
            false
        }
        Err(e) => {
            if quiet == Quiet::No {
                eprintln!("  {} {}", program, e.to_string().dimmed());
            }
            false
        }
    }
}

/// The real uid, for launchd's `gui/<uid>` domain target.
#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: `getuid` cannot fail and touches no memory.
    unsafe { libc::getuid() }
}

/// Non-unix never reaches a loader — [`ServiceManager::current`] errors
/// first — but the un-`cfg`ed callers above must still compile.
#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

/// A label the operator did not pass: the registered command's first word.
fn derive_label(command: &[String]) -> Result<String, String> {
    let first = command
        .first()
        .ok_or("nothing to register — give the stella invocation after `--`")?;
    let label: String = first
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let label = label.trim_matches('-');
    if label.is_empty() {
        return Err(format!(
            "cannot derive a service label from `{first}` — pass one with --label"
        ));
    }
    Ok(label.chars().take(LABEL_MAX).collect())
}

/// A label becomes a file name, a launchd label, and a systemd unit name —
/// this is the intersection all three accept without escaping.
fn validate_label(label: &str) -> Result<(), String> {
    let shaped = !label.is_empty()
        && label.len() <= LABEL_MAX
        && label
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !label.starts_with('-');
    if shaped {
        Ok(())
    } else {
        Err(format!(
            "service label `{label}` must be 1–{LABEL_MAX} lowercase letters, digits or dashes"
        ))
    }
}

/// The `/bin/sh` shim both definitions exec through — the resolve-at-load
/// answer to a binary that moves.
///
/// One line, because a systemd `ExecStart=` value is one line. The registered
/// argv is *not* in here: it travels as positional parameters (`"$@"`), so
/// this string needs no per-argument quoting and the arguments need no
/// shell escaping at all.
fn resolver_script(exe: &str, label: &str) -> String {
    format!(
        "bin=\"{exe}\"; \
         if ! [ -x \"$bin\" ]; then \
         bin=\"$(PATH=\"$PATH:/opt/homebrew/bin:/usr/local/bin:$HOME/.cargo/bin:$HOME/.local/bin\" \
         command -v stella || true)\"; fi; \
         if [ -z \"$bin\" ]; then \
         echo \"stella service {label}: no stella binary at {exe} or on PATH\" >&2; exit 127; fi; \
         exec \"$bin\" \"$@\""
    )
}

/// A path this module embeds inside the shim's double quotes and, on
/// systemd, inside a single-quoted `ExecStart=` word.
///
/// Rejecting rather than escaping, because a path spelled with shell
/// metacharacters has no single spelling that all three consumers (plist
/// XML, POSIX sh, systemd's quoting layer) agree on — and refusing loudly at
/// install time beats a definition that misfires at boot.
fn embeddable_path(path: &Path) -> Result<String, String> {
    let text = path.display().to_string();
    if text
        .chars()
        .any(|c| matches!(c, '"' | '\'' | '$' | '`' | '\\' | '\n' | '\r'))
    {
        return Err(format!(
            "cannot register a service from `{text}` — the path contains shell metacharacters \
             (quotes, $, backslash or a newline); rename the offending component or run install \
             from another path"
        ));
    }
    Ok(text)
}

/// Escape for a plist `<string>` element.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The launchd agent definition.
fn launchd_plist(spec: &ServiceSpec<'_>) -> Result<String, String> {
    let ServiceSpec {
        label,
        exe,
        args,
        workdir,
        log_path,
        keep_alive,
    } = *spec;
    let script = resolver_script(&embeddable_path(exe)?, label);
    let mut argv = String::new();
    for arg in args {
        argv.push_str(&format!("    <string>{}</string>\n", xml_escape(arg)));
    }
    let keep = if keep_alive {
        format!(
            "  <key>KeepAlive</key><true/>\n  <key>ThrottleInterval</key><integer>{RESTART_SECS}</integer>\n"
        )
    } else {
        String::new()
    };
    let log = xml_escape(&log_path.display().to_string());
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20 <key>Label</key><string>sh.oxagen.stella.{label}</string>\n\
         \x20 <key>ProgramArguments</key>\n\
         \x20 <array>\n\
         \x20   <string>/bin/sh</string>\n\
         \x20   <string>-c</string>\n\
         \x20   <string>{script}</string>\n\
         \x20   <string>stella-{label}</string>\n\
         {argv}\
         \x20 </array>\n\
         \x20 <key>WorkingDirectory</key><string>{workdir}</string>\n\
         \x20 <key>RunAtLoad</key><true/>\n\
         {keep}\
         \x20 <key>StandardOutPath</key><string>{log}</string>\n\
         \x20 <key>StandardErrorPath</key><string>{log}</string>\n\
         </dict>\n\
         </plist>\n",
        script = xml_escape(&script),
        workdir = xml_escape(&embeddable_path(workdir)?),
    ))
}

/// Escape a value for a non-`Exec` systemd directive: `%` specifiers expand
/// everywhere in a unit file, and a newline would end the directive and
/// start injecting new ones — which is why it is rejected upstream by
/// [`embeddable_path`] and re-checked here for arguments.
fn systemd_value(text: &str) -> Result<String, String> {
    if text.contains('\n') || text.contains('\r') {
        return Err("a service definition value cannot contain a newline".to_string());
    }
    Ok(text.replace('%', "%%"))
}

/// Quote one `ExecStart=` word: systemd's double-quote grammar (backslash
/// escapes), plus the line-level `$` and `%` expansions that apply inside
/// quotes too.
fn systemd_word(arg: &str) -> Result<String, String> {
    if arg.chars().any(|c| c.is_control()) {
        return Err(format!(
            "cannot register an argument containing control characters: {arg:?}"
        ));
    }
    let escaped = arg
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "$$")
        .replace('%', "%%");
    Ok(format!("\"{escaped}\""))
}

/// The `systemd --user` unit definition.
fn systemd_unit(spec: &ServiceSpec<'_>) -> Result<String, String> {
    let ServiceSpec {
        label,
        exe,
        args,
        workdir,
        log_path,
        keep_alive,
    } = *spec;
    let script = resolver_script(&embeddable_path(exe)?, label);
    // The shim reaches sh verbatim: `$` and `%` are line-level expansions in
    // `ExecStart=`, applying inside the single quotes as well.
    let script = script.replace('$', "$$").replace('%', "%%");
    let mut exec = format!("/bin/sh -c '{script}' stella-{label}");
    for arg in args {
        exec.push(' ');
        exec.push_str(&systemd_word(arg)?);
    }
    let start_limit = if keep_alive {
        format!(
            "StartLimitIntervalSec={START_LIMIT_WINDOW_SECS}\nStartLimitBurst={START_LIMIT_BURST}\n"
        )
    } else {
        String::new()
    };
    let restart = if keep_alive {
        format!("Restart=always\nRestartSec={RESTART_SECS}\n")
    } else {
        String::new()
    };
    let log = systemd_value(&log_path.display().to_string())?;
    Ok(format!(
        "# Written by `stella daemon install`; `stella daemon uninstall {label}` removes it.\n\
         [Unit]\n\
         Description=stella service {label}\n\
         {start_limit}\
         \n\
         [Service]\n\
         Type=exec\n\
         WorkingDirectory={workdir}\n\
         ExecStart={exec}\n\
         StandardOutput=append:{log}\n\
         StandardError=append:{log}\n\
         {restart}\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        workdir = systemd_value(&embeddable_path(workdir)?)?,
    ))
}

#[cfg(test)]
mod tests;
