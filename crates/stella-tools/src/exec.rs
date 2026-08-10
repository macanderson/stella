//! Shared subprocess runner for tools that spawn commands: process-group
//! spawn, hard timeout with a group kill, combined output, middle-out
//! truncation. Two entry points: `run` shells out via `bash -c`
//! (`verify_done`, `build_project`, `run_tests`, `run_script`, `screenshot`);
//! `run_argv` execs an argv vector directly with NO shell anywhere
//! (`run_lint`, `format_code`, `diagnostics`, the `repo_*` tools).
//!
//! The `bash` tool (registered by default, switchable off) does NOT come
//! through here — it spawns its own child so it can own its output cap and
//! process-group policy (see `crate::bash`).

use std::collections::VecDeque;
use std::time::Duration;

use tokio::process::Command;

pub(crate) const MAX_OUTPUT_BYTES: usize = 30_000;

/// Per-stream in-memory ceiling for a captured subprocess.
///
/// `Child::wait_with_output` buffers whatever the child writes, so every
/// payload cap in this crate (`truncate_middle`, `bash`'s own middle cut,
/// `diagnostics`' parse) only ever applied AFTER the whole stream was
/// already resident. `yes`, `cat /dev/urandom | base64`, or a build stuck in
/// a warning loop therefore grew Stella's RSS without bound until the OOM
/// killer or the timeout arrived — whichever came first, and the OOM killer
/// usually won. 8 MiB per stream sits far above any real build or test log,
/// so nothing observable changes, while turning an unbounded allocation into
/// a bounded one.
pub(crate) const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

/// Ceiling on any model-supplied `timeout_secs`. The timeout is the hang
/// backstop for commands the model itself launches — accepting an arbitrary
/// u64 lets one tool call disable the backstop entirely (u64::MAX ≈ never).
pub(crate) const MAX_TIMEOUT_SECS: u64 = 600;

/// Parse the optional `timeout_secs` field of a tool input: absent or 0
/// means `default` (a literal 0 would otherwise time out every invocation
/// instantly — custom.rs's convention), clamped to [`MAX_TIMEOUT_SECS`] so
/// no exec-style tool can disable its own hang backstop.
pub(crate) fn timeout_from(input: &serde_json::Value, default: u64) -> u64 {
    input
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .filter(|&t| t > 0)
        .unwrap_or(default)
        .min(MAX_TIMEOUT_SECS)
}

// The canonical spawn-env policy (git-repo retargeting, forced color, and
// the credential scrub) lives in `subprocess_env` so every spawn path shares
// one helper; these re-exports keep the historical `exec::` paths valid for
// the callers that scrub ad-hoc `Command`s themselves.
pub use crate::subprocess_env::{FORCED_COLOR_ENV_VARS, GIT_REPO_ENV_VARS};

/// Compatibility facade for callers added on `main`; the canonical policy
/// lives in `subprocess_env` so every subprocess shares one monotonic registry.
pub fn register_sensitive_env_names<I, S>(names: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    crate::subprocess_env::register_sensitive_env_names(names);
}

/// Whether a name is currently classified as a host credential.
pub fn is_sensitive_env_name(name: &str) -> bool {
    crate::subprocess_env::is_sensitive_env_name(std::ffi::OsStr::new(name))
}

/// Remove every registered host credential from a model-controlled child.
pub fn scrub_sensitive_env(cmd: &mut Command) {
    crate::subprocess_env::scrub_sensitive_env(cmd);
}

/// Synchronous-command counterpart used by fixed helper probes.
pub fn scrub_sensitive_std_env(cmd: &mut std::process::Command) {
    crate::subprocess_env::scrub_sensitive_std_env(cmd);
}

/// Run `command` via `bash -c` in `dir`. Returns `(exit_code, combined
/// stdout+stderr)`; `Err` is spawn failure or timeout (the process group is
/// killed on timeout so nothing lingers).
pub(crate) async fn run(
    command: &str,
    dir: &std::path::Path,
    timeout_secs: u64,
) -> Result<(i32, String), String> {
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(command);
    drive(cmd, command, dir, timeout_secs, &[])
        .await
        .map(|(code, output)| (code, truncate_middle(output)))
}

/// Run a repository-owned GitHub CLI command while preserving only GitHub
/// CLI's exact documented authentication variables. The command text must be
/// constructed by Stella code with user values shell-quoted; model-authored
/// arbitrary shell commands must use `run` and receive no credential.
pub(crate) async fn run_github(
    command: &str,
    dir: &std::path::Path,
    timeout_secs: u64,
) -> Result<(i32, String), String> {
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(command);
    drive(
        cmd,
        command,
        dir,
        timeout_secs,
        crate::subprocess_env::GITHUB_CLI_AUTH_ENV_VARS,
    )
    .await
    .map(|(code, output)| (code, truncate_middle(output)))
}

/// Run `program` with `args` DIRECTLY — argv exec, no shell anywhere — in
/// `dir`. The runner for the argv-exec tools (`run_lint`, `format_code`,
/// `diagnostics`, the `repo_*` family — `run_script` composes shell text and
/// goes through [`run`] instead): arguments reach the child exactly as given, so no
/// model-supplied string is ever shell-interpreted. Same process-group
/// spawn, timeout kill, and truncation as `run`.
pub(crate) async fn run_argv(
    program: &str,
    args: &[String],
    dir: &std::path::Path,
    timeout_secs: u64,
) -> Result<(i32, String), String> {
    run_argv_untruncated(program, args, dir, timeout_secs)
        .await
        .map(|(code, output)| (code, truncate_middle(output)))
}

/// `run_argv` without the middle-out truncation — for callers that PARSE
/// the full output into a bounded structure of their own (`diagnostics`
/// consuming a `--message-format=json` stream: truncating the raw stream
/// would sever JSON lines and silently drop the very records the parse
/// exists to keep). "Untruncated" means the model-facing [`truncate_middle`]
/// cut is not applied; the runner's own [`MAX_CAPTURE_BYTES`] memory ceiling
/// still holds, and a stream that reaches it carries a loud marker line the
/// line-oriented parsers skip like any other non-record line.
pub(crate) async fn run_argv_untruncated(
    program: &str,
    args: &[String],
    dir: &std::path::Path,
    timeout_secs: u64,
) -> Result<(i32, String), String> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    let display = std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    drive(cmd, &display, dir, timeout_secs, &[]).await
}

/// Outcome of [`run_captured`].
///
/// The three arms exist because the search tools must tell them apart:
/// "not installed" selects a fallback program, "timed out" must NOT — retrying
/// a walk that just hung under a slower, non-gitignore-aware tool doubles the
/// stall instead of recovering from it.
pub(crate) enum Captured {
    /// Ran to completion inside the deadline.
    Done(std::process::Output),
    /// Could not be spawned — in practice the program is absent. Also covers
    /// a post-spawn pipe/wait failure, which callers treat the same way
    /// (falling back to the next program).
    Unavailable,
    /// Still running at the deadline; the child has been killed.
    TimedOut,
}

/// Run `cmd` under a wall-clock deadline, keeping stdout and stderr SEPARATE.
///
/// [`drive`] is the richer runner, but it merges the two streams — which the
/// search tools cannot accept, because they discriminate a real pattern error
/// from a benign unreadable-directory warning by pairing exit code 2 with the
/// content of stderr alone. This keeps that discrimination and adds the two
/// things every ad-hoc `.output().await` was missing: a deadline, and
/// `kill_on_drop` so a cancelled turn does not leave the child walking the
/// tree.
pub(crate) async fn run_captured(mut cmd: Command, timeout_secs: u64) -> Captured {
    cmd.kill_on_drop(true);
    // Set here, not left to the caller: [`wait_with_capped_output`] can only
    // bound a stream it owns a pipe for, and an inherited stdout would send
    // a search's results to the user's terminal instead.
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    // `Command::output` closed stdin for us; spawning by hand does not, and
    // an inherited stdin is the TUI's terminal — a `grep` handed no path
    // would silently eat the user's keystrokes.
    cmd.stdin(std::process::Stdio::null());
    let Ok(child) = cmd.spawn() else {
        return Captured::Unavailable;
    };
    // Capped, not `output()`: `rg --max-count` is PER FILE, so a recursive
    // walk of a large tree emits an unbounded number of lines and the tool's
    // own caps only ran once the whole thing was already in memory.
    match tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        wait_with_capped_output(child, MAX_CAPTURE_BYTES),
    )
    .await
    {
        Ok(Ok(output)) => Captured::Done(output),
        Ok(Err(_)) => Captured::Unavailable,
        // Dropping the wait future closes the child handle, and
        // `kill_on_drop` reaps the process.
        Err(_) => Captured::TimedOut,
    }
}

/// Head-plus-tail byte accumulator for one child stream: keeps the first and
/// last `cap / 2` bytes and counts what fell out of the middle.
///
/// Head AND tail because a failing command's information sits at both ends —
/// the first error and the final summary — which is the same reason
/// [`truncate_middle`] exists. This is that policy moved to *ingest* time, so
/// the bytes in between are never held at all.
struct CappedStream {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    dropped: u64,
    cap: usize,
    half: usize,
}

impl CappedStream {
    fn new(cap: usize) -> Self {
        Self {
            head: Vec::new(),
            tail: VecDeque::new(),
            dropped: 0,
            cap,
            half: cap.max(2) / 2,
        }
    }

    fn push(&mut self, mut chunk: &[u8]) {
        if self.head.len() < self.half {
            let take = (self.half - self.head.len()).min(chunk.len());
            self.head.extend_from_slice(&chunk[..take]);
            chunk = &chunk[take..];
        }
        if chunk.is_empty() {
            return;
        }
        self.tail.extend(chunk.iter().copied());
        if self.tail.len() > self.half {
            let excess = self.tail.len() - self.half;
            self.tail.drain(..excess);
            self.dropped += excess as u64;
        }
    }

    /// Head, a loud marker when anything was dropped, then tail. The marker
    /// is bytes rather than a post-hoc annotation because the caller decodes
    /// this as the process's output and must not be able to mistake a capped
    /// stream for a complete one.
    fn into_bytes(mut self) -> Vec<u8> {
        if self.dropped == 0 {
            self.head.extend(self.tail);
            return self.head;
        }
        let cap = if self.cap >= 1024 * 1024 {
            format!("{} MiB", self.cap / (1024 * 1024))
        } else {
            format!("{}-byte", self.cap)
        };
        let marker = format!(
            "\n[… {} bytes dropped: output exceeded the {cap} in-memory capture cap …]\n",
            self.dropped,
        );
        self.head.extend_from_slice(marker.as_bytes());
        self.head.extend(self.tail);
        self.head
    }
}

/// Drain `reader` to EOF holding at most `cap` bytes (see [`CappedStream`]).
async fn read_capped<R>(mut reader: R, cap: usize) -> std::io::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;
    let mut acc = CappedStream::new(cap);
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        acc.push(&buf[..n]);
    }
    Ok(acc.into_bytes())
}

/// [`tokio::process::Child::wait_with_output`] with a per-stream memory
/// ceiling — see [`MAX_CAPTURE_BYTES`] for why the unbounded version is not
/// safe on a path where the model chooses the command.
///
/// Both pipes and the exit wait are polled concurrently, exactly as
/// `wait_with_output` does, so a child that fills one pipe while the other is
/// idle cannot deadlock.
pub(crate) async fn wait_with_capped_output(
    mut child: tokio::process::Child,
    cap: usize,
) -> std::io::Result<std::process::Output> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out = async {
        match stdout {
            Some(reader) => read_capped(reader, cap).await,
            None => Ok(Vec::new()),
        }
    };
    let err = async {
        match stderr {
            Some(reader) => read_capped(reader, cap).await,
            None => Ok(Vec::new()),
        }
    };
    let (status, stdout, stderr) = tokio::try_join!(child.wait(), out, err)?;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// SIGKILLs `pid`'s process group on drop unless disarmed — the
/// cancellation backstop for this module's shared runner and for the tools
/// that spawn their own child instead of coming through it ([`crate::bash`],
/// [`crate::custom`]): when the future driving a tool call is dropped
/// mid-wait (Esc cancels the turn), the detached process group must not keep
/// running — and mutating the tree — after the user believes the turn
/// stopped. Normal exit and the timeout path disarm it.
///
/// It is deliberately `pub`: every `pre_exec(setsid)` spawn site in the
/// workspace must use *this* guard rather than grow a second one. A `setsid`
/// child is in its own session, so Ctrl-C's SIGINT — delivered only to the
/// tty's foreground process group — never reaches it; this guard is the only
/// thing that reaps the tree, and it fires because the CLI drops the work
/// future on a signal instead of calling `exit` (`stella-cli/src/signals.rs`).
///
/// Never `tokio::spawn` teardown from a `Drop` instead: during runtime
/// shutdown the spawn silently does nothing, which is precisely the case
/// being handled. `kill(2)` is synchronous, so this guard needs no runtime.
#[cfg(unix)]
pub struct GroupKillGuard {
    pid: i32,
    armed: bool,
}

#[cfg(unix)]
impl GroupKillGuard {
    /// Arm a guard over the process group led by `pid` (a child spawned with
    /// `pre_exec(setsid)`, so its pid *is* its process-group id). A pid of 0
    /// — `Child::id` after the child was already reaped — is inert.
    pub fn arm(pid: i32) -> Self {
        Self { pid, armed: true }
    }

    /// Stop the guard from killing on drop. Call it once the group is known
    /// to be gone: the child exited normally, or the caller already killed
    /// the group itself on its timeout path.
    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// SIGKILL the group now, and disarm. The timeout path: the child is
    /// still running and must die *before* the caller returns an error,
    /// rather than at some later scope exit.
    pub fn kill_now(&mut self) {
        self.armed = false;
        // Same real-pid guard as `Drop`.
        if self.pid > 0 {
            unsafe {
                libc::kill(-self.pid, libc::SIGKILL);
            }
        }
    }
}

#[cfg(unix)]
impl Drop for GroupKillGuard {
    fn drop(&mut self) {
        // Guard on a real pid: kill(-0, …) would SIGKILL Stella's OWN
        // process group.
        if self.armed && self.pid > 0 {
            unsafe {
                libc::kill(-self.pid, libc::SIGKILL);
            }
        }
    }
}

/// Shared spawn/wait/kill body of `run` and `run_argv` — `command` is
/// the human-readable command line for error messages. Output is returned
/// UNTRUNCATED; the wrappers apply [`truncate_middle`] except
/// [`run_argv_untruncated`], whose callers parse the full stream.
///
/// Merges stderr into stdout, which is right for everything a human or a
/// model reads (a failure's explanation lives on stderr and must not be
/// dropped) and wrong for anything PARSED as a value — see [`drive_split`].
async fn drive(
    cmd: Command,
    command: &str,
    dir: &std::path::Path,
    timeout_secs: u64,
    preserved_sensitive_env: &[&str],
) -> Result<(i32, String), String> {
    let (code, stdout, stderr) =
        drive_split(cmd, command, dir, timeout_secs, preserved_sensitive_env).await?;
    let mut combined = stdout;
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&stderr);
    }
    Ok((code, combined))
}

/// [`drive`] with the two streams kept APART.
///
/// Every guarantee of the merged path is preserved — credential scrubbing,
/// null stdin, its own process group, the deadline, and the group kill on
/// timeout or drop. The only difference is that the caller decides what to do
/// with stderr.
///
/// This exists because a merged stream cannot be parsed as a value. git writes
/// advice hints, warnings, and (when `GIT_TRACE` is set anywhere in the
/// environment) trace lines to stderr; folded into stdout they become part of
/// whatever the caller thinks it just read. A branch name is the sharp case:
/// `rev-parse --abbrev-ref HEAD` plus one hint yields a "branch" that git then
/// rejects as `fatal: invalid refspec`. `verify` already hit this and moved to
/// a stdout-only read; `repo` had not.
async fn drive_split(
    mut cmd: Command,
    command: &str,
    dir: &std::path::Path,
    timeout_secs: u64,
    preserved_sensitive_env: &[&str],
) -> Result<(i32, String, String), String> {
    cmd.current_dir(dir);
    crate::subprocess_env::scrub_spawn_env_except(&mut cmd, preserved_sensitive_env);
    // No stdin: everything driven through here is non-interactive, and an
    // inherited stdin is the TUI's terminal — a build or test that prompts
    // would consume the user's keystrokes and then hang until the timeout
    // instead of failing fast on EOF.
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    // Reap the direct child if this future is dropped mid-wait (Esc, a
    // session timeout). On unix this only backs up `GroupKillGuard` below —
    // that guard's group SIGKILL already reaches the direct child because a
    // setsid'd child's pid IS its process-group id — but on non-unix, where
    // no such guard exists, this is the ONLY thing that stops a cancelled
    // `run`/`run_argv`/`verify_done` call from leaking its subprocess. Same
    // reasoning as `crate::bash`, `crate::hook_runner`, and `crate::custom`.
    cmd.kill_on_drop(true);
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn `{command}`: {e}"))?;
    #[cfg(unix)]
    let pid = child.id().unwrap_or(0) as i32;
    #[cfg(unix)]
    let mut guard = GroupKillGuard::arm(pid);

    // Capped rather than `wait_with_output`: the child's command text comes
    // from the model on several of these paths, so the buffer it fills must
    // be bounded before the timeout, not after it (see [`MAX_CAPTURE_BYTES`]).
    let output = match tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        wait_with_capped_output(child, MAX_CAPTURE_BYTES),
    )
    .await
    {
        Ok(Ok(output)) => {
            #[cfg(unix)]
            guard.disarm();
            output
        }
        // Wait failure leaves the child's state unknown — the still-armed
        // guard kills the group on return rather than leak it.
        Ok(Err(e)) => return Err(format!("command failed: {e}")),
        Err(_) => {
            #[cfg(unix)]
            guard.kill_now();
            return Err(format!("`{command}` timed out after {timeout_secs}s"));
        }
    };

    Ok((
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    ))
}

/// `run_argv` keeping stdout and stderr apart, untruncated — the argv-based
/// entry point to [`drive_split`] for callers that PARSE stdout as a value.
pub(crate) async fn run_argv_split(
    program: &str,
    args: &[String],
    dir: &std::path::Path,
    timeout_secs: u64,
) -> Result<(i32, String, String), String> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    let display = std::iter::once(program)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    drive_split(cmd, &display, dir, timeout_secs, &[]).await
}

/// `run` with the PASSED/FAILED framing shared by `build_project`,
/// `run_tests`, and `run_script` — the model reads success or failure from
/// the first line without a follow-up question.
pub(crate) async fn run_and_report(
    command: &str,
    dir: &std::path::Path,
    timeout_secs: u64,
) -> stella_protocol::tool::ToolOutput {
    use stella_protocol::tool::ToolOutput;
    match run(command, dir, timeout_secs).await {
        Ok((0, output)) => ToolOutput::Ok {
            content: format!("`{command}` PASSED (exit 0)\n{output}"),
        },
        Ok((code, output)) => ToolOutput::Error {
            message: format!("`{command}` FAILED (exit {code})\n{output}"),
        },
        Err(e) => ToolOutput::Error { message: e },
    }
}

/// POSIX single-quote an argument for a `bash -c` command line, escaping any
/// embedded quote the only portable way (`'\''`).
///
/// The crate's ONE implementation of this security primitive: it lives here
/// because this module owns the `bash -c` runner every composed command line
/// eventually reaches (`run`, [`run_github`]). `scripts::shell_quote`,
/// `ci::shell_quote`, `screenshot::shell_quote`, `project::shell_quote` and
/// `issue_ops::quote` were five independent copies — the shape that lets one
/// of them drift. They are all thin aliases for this function now; a new one
/// must be too.
pub(crate) fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Cap a preview of REMOTE-supplied text at `max_bytes`, cutting on a char
/// boundary.
///
/// `String::truncate` panics on a non-boundary index, and every caller of
/// this helper previews an HTTP error body the endpoint chose: a UTF-8
/// message (a localized GitHub/Linear/search-provider error) straddling the
/// cut would abort the tool call instead of reporting the failure it was
/// quoting.
pub(crate) fn truncate_preview(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text[..cut].to_string()
}

/// Keep head and tail when output exceeds the shared runner's page budget —
/// build/test failures matter at both ends (first error, final summary).
pub(crate) fn truncate_middle(s: String) -> String {
    if s.len() <= MAX_OUTPUT_BYTES {
        return s;
    }
    truncate_middle_capped(&s, MAX_OUTPUT_BYTES)
}

/// Keep the head and tail of `s` when it exceeds `max_bytes`, eliding the
/// middle with a marker that names both the elided byte count and the cap.
///
/// The crate's ONE model-facing elision spelling: [`truncate_middle`],
/// [`crate::bash`], and [`crate::custom`] all cut through here, so their
/// markers and budgets cannot drift apart (#1889 — they had, three ways).
/// The split is tail-biased — 40% head, 60% tail — because a failing
/// command's signal concentrates at both ends and the tail end is the denser
/// one: the final test summary, the last error, the exit status (lesson
/// L-S3, first applied in `crate::custom`). Both cuts land on UTF-8 char
/// boundaries so multibyte output can never panic the slice.
pub(crate) fn truncate_middle_capped(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let head_budget = max_bytes * 2 / 5; // 40% head …
    let tail_budget = max_bytes - head_budget; // … 60% tail (≥ head, L-S3)
    let head_end = floor_char_boundary(s, head_budget);
    let tail_start = ceil_char_boundary(s, s.len().saturating_sub(tail_budget));
    let elided = tail_start - head_end;
    format!(
        "{}\n[… {elided} bytes truncated: output exceeded the {max_bytes}-byte cap; the head \
         and tail are kept …]\n{}",
        &s[..head_end],
        &s[tail_start..]
    )
}

/// Largest char boundary `<= i` (clamped to `s.len()`).
fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest char boundary `>= i`.
fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_captures_exit_code_and_output() {
        let (code, out) = run("echo hi; exit 3", std::path::Path::new("/tmp"), 30)
            .await
            .unwrap();
        assert_eq!(code, 3);
        assert!(out.contains("hi"));
    }

    #[tokio::test]
    async fn run_argv_execs_without_a_shell() {
        // `echo` is a real binary; the argument is NOT shell-interpreted, so
        // a metacharacter-laden string arrives verbatim.
        let (code, out) = run_argv(
            "echo",
            &["$(id); `id`; && ||".to_string()],
            std::path::Path::new("/tmp"),
            30,
        )
        .await
        .unwrap();
        assert_eq!(code, 0);
        assert!(out.contains("$(id); `id`; && ||"), "{out}");
    }

    #[tokio::test]
    async fn shell_and_direct_argv_scrub_inherited_credentials() {
        let _fixture = crate::subprocess_env::test_support::InheritedCredentialFixture::install();
        let probe = crate::subprocess_env::test_support::PROBE_COMMAND;

        let (shell_code, shell_out) = run(probe, std::path::Path::new("/tmp"), 30)
            .await
            .expect("shell runner");
        assert_eq!(shell_code, 0);
        crate::subprocess_env::test_support::assert_scrubbed(&shell_out);

        let (argv_code, argv_out) = run_argv(
            "sh",
            &["-c".to_string(), probe.to_string()],
            std::path::Path::new("/tmp"),
            30,
        )
        .await
        .expect("direct argv runner");
        assert_eq!(argv_code, 0);
        crate::subprocess_env::test_support::assert_scrubbed(&argv_out);
    }

    #[tokio::test]
    async fn github_runner_preserves_only_github_cli_auth() {
        let _fixture = crate::subprocess_env::test_support::InheritedCredentialFixture::install();
        let probe = crate::subprocess_env::test_support::PROBE_COMMAND;

        let (code, output) = run_github(probe, std::path::Path::new("/tmp"), 30)
            .await
            .expect("GitHub runner");
        assert_eq!(code, 0);
        assert_eq!(output, "|leaked||visible|present");
    }

    #[tokio::test]
    async fn model_controlled_commands_cannot_inherit_registered_host_credentials() {
        let secret_name = "STELLA_TEST_ENROLLMENT_VERIFY_SECRET";
        let token_name = "STELLA_TEST_ENROLLMENT_BEARER";
        register_sensitive_env_names([secret_name, token_name]);
        unsafe {
            std::env::set_var(secret_name, "verification-secret-value");
            std::env::set_var(token_name, "bearer-secret-value");
        }
        let result = run_argv("env", &[], std::path::Path::new("/tmp"), 30).await;
        unsafe {
            std::env::remove_var(secret_name);
            std::env::remove_var(token_name);
        }
        let (code, output) = result.unwrap();
        assert_eq!(code, 0);
        for forbidden in [
            secret_name,
            token_name,
            "verification-secret-value",
            "bearer-secret-value",
        ] {
            assert!(!output.contains(forbidden), "credential leaked: {output}");
        }
    }

    /// Dropping the future mid-wait (a cancelled turn) must kill the whole
    /// process group — the [`GroupKillGuard`] backstop. Without it, Esc
    /// during a long build left the build running and mutating the tree.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_run_kills_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        // Record the *grandchild*'s pid: when the group dies, the orphaned
        // sleep is reaped by init, so a surviving pid means a real leak.
        let cmd = format!("sleep 30 & echo $! > {} && wait", pidfile.display());
        let dir_path = dir.path().to_path_buf();
        let handle = tokio::spawn(async move { run(&cmd, &dir_path, 60).await });
        let mut pid = None;
        for _ in 0..250 {
            if let Some(p) = std::fs::read_to_string(&pidfile)
                .ok()
                .and_then(|s| s.trim().parse::<i32>().ok())
            {
                pid = Some(p);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let pid = pid.expect("the child never started");
        handle.abort();
        let _ = handle.await;
        let mut dead = false;
        for _ in 0..250 {
            if unsafe { libc::kill(pid, 0) } == -1 {
                dead = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(dead, "cancelled run left subprocess {pid} running");
    }

    /// An inherited force-color env must not reach subprocesses: `gh`
    /// would wrap its `--json` output in ANSI escapes and every serde
    /// parse of it dies at line 1 column 1.
    #[tokio::test]
    async fn run_scrubs_forced_color_env() {
        unsafe { std::env::set_var("CLICOLOR_FORCE", "1") };
        let result = run(
            "echo \"force=${CLICOLOR_FORCE-unset}\"",
            std::path::Path::new("/tmp"),
            30,
        )
        .await;
        unsafe { std::env::remove_var("CLICOLOR_FORCE") };
        let (code, out) = result.unwrap();
        assert_eq!(code, 0);
        assert!(out.contains("force=unset"), "{out}");
    }

    #[tokio::test]
    async fn run_times_out_and_kills() {
        let err = run("sleep 30", std::path::Path::new("/tmp"), 1)
            .await
            .unwrap_err();
        assert!(err.contains("timed out"), "{err}");
    }

    #[test]
    fn shell_quote_neutralizes_embedded_quotes() {
        assert_eq!(shell_quote("main"), "'main'");
        assert_eq!(shell_quote("a'b"), r"'a'\''b'");
        assert_eq!(shell_quote("; rm -rf /"), "'; rm -rf /'");
    }

    /// The ingest-time half of the output policy: head and tail survive, the
    /// middle is dropped, and the drop is loud. Without it, `truncate_middle`
    /// only ran once the whole stream was already in memory.
    #[test]
    fn capped_stream_keeps_head_and_tail_and_names_the_drop() {
        let mut stream = CappedStream::new(100);
        stream.push(&[b'a'; 50]);
        stream.push(&[b'm'; 500]);
        stream.push(&[b'z'; 50]);
        let out = stream.into_bytes();
        let text = String::from_utf8(out.clone()).expect("ascii");
        assert!(text.starts_with(&"a".repeat(50)), "head survives: {text}");
        assert!(text.ends_with(&"z".repeat(50)), "tail survives: {text}");
        assert!(text.contains("bytes dropped"), "the drop is loud: {text}");
        assert!(out.len() < 300, "bounded (got {} bytes)", out.len());
    }

    /// A stream under the cap must come back byte-identical — the ceiling is
    /// invisible for every real command.
    #[test]
    fn capped_stream_is_a_no_op_below_the_cap() {
        let mut stream = CappedStream::new(MAX_CAPTURE_BYTES);
        stream.push(b"hello ");
        stream.push(b"world");
        assert_eq!(stream.into_bytes(), b"hello world");
    }

    /// The witness for the unbounded-child hazard: a 4 MB stream must not
    /// cost 4 MB of Stella's memory. `wait_with_output` had no such bound —
    /// `yes` or `cat /dev/urandom | base64` reached the OOM killer before the
    /// timeout, and the truncation that was supposed to protect the model ran
    /// only after the whole payload had already been allocated.
    #[tokio::test]
    async fn capped_capture_bounds_a_runaway_child() {
        let mut cmd = Command::new("bash");
        cmd.arg("-c").arg("yes stella | head -c 4000000");
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        let child = cmd.spawn().expect("spawn");
        let output = wait_with_capped_output(child, 64 * 1024)
            .await
            .expect("capped wait");
        assert!(
            output.stdout.len() < 64 * 1024 + 256,
            "held {} bytes for a 4 MB stream",
            output.stdout.len()
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("bytes dropped"),
            "the capped stream must say so"
        );
        assert!(output.status.success());
    }

    /// The end-to-end shape: a runaway command through the shared runner
    /// still answers, with its exit code intact and a bounded payload.
    #[tokio::test]
    async fn run_survives_a_runaway_command() {
        let (code, out) = run(
            "yes stella | head -c 20000000; exit 7",
            std::path::Path::new("/tmp"),
            120,
        )
        .await
        .expect("runner");
        assert_eq!(code, 7);
        assert!(
            out.len() < MAX_OUTPUT_BYTES + 4096,
            "the model-facing cut still applies (got {} bytes)",
            out.len()
        );
    }

    #[test]
    fn truncate_middle_keeps_head_and_tail() {
        let s = "a".repeat(20_000) + &"z".repeat(20_000);
        let t = truncate_middle(s);
        assert!(t.len() < 40_000);
        assert!(t.starts_with('a'));
        assert!(t.ends_with('z'));
        assert!(t.contains("truncated"));
    }

    /// The marker must account for every byte: it names the true elided count
    /// and the cap it enforced, both derived here rather than hard-coded so a
    /// moved cap cannot leave the assertion silently green (#1842's lesson).
    #[test]
    fn truncate_middle_capped_names_the_elided_count_and_cap() {
        let cap = 1_000;
        let s = "a".repeat(400) + &"m".repeat(5_000) + &"z".repeat(400);
        let t = truncate_middle_capped(&s, cap);
        assert!(t.starts_with('a'), "head survives: {t}");
        assert!(t.ends_with('z'), "tail survives: {t}");
        let head_kept = cap * 2 / 5;
        let tail_kept = cap - head_kept;
        assert!(tail_kept >= head_kept, "tail budget ≥ head budget (L-S3)");
        let elided = s.len() - head_kept - tail_kept;
        assert!(
            t.contains(&format!("[… {elided} bytes truncated")),
            "the marker names the elided count: {t}"
        );
        assert!(
            t.contains(&format!("the {cap}-byte cap")),
            "the marker names the cap: {t}"
        );
    }

    /// Both cuts land inside a 3-byte char at a 40/60 split of this cap; a
    /// raw byte slice would panic, the boundary-safe path must not.
    #[test]
    fn truncate_middle_capped_respects_utf8_boundaries() {
        let s = "€".repeat(1_000);
        let t = truncate_middle_capped(&s, 1_000);
        assert!(t.starts_with('€'), "{t}");
        assert!(t.ends_with('€'), "{t}");
        assert!(t.contains("bytes truncated"), "{t}");
    }

    #[test]
    fn truncate_middle_capped_is_a_no_op_at_or_below_the_cap() {
        assert_eq!(truncate_middle_capped("hello", 5), "hello");
        assert_eq!(truncate_middle_capped("hi", 5), "hi");
    }
}
