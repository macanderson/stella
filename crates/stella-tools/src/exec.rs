// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Shared subprocess plumbing for the spawn paths this crate still owns —
//! custom script tools ([`crate::custom`]) and shell decision hooks
//! ([`crate::hook_runner`]): a capped two-stream capture with a bounded
//! background-drain grace, the process-group cancellation backstop, and the
//! crate's one model-facing middle-out elision.
//!
//! The `bash -c` / argv runners that used to live here served the retired
//! built-in command tools; what survives is exactly the machinery the two
//! remaining spawn paths share, plus the `subprocess_env` facades kept for
//! callers that historically reached the spawn-env policy through `exec::`.

use std::collections::VecDeque;
use std::time::Duration;

use tokio::process::Command;

/// Per-stream in-memory ceiling for a captured subprocess.
///
/// `Child::wait_with_output` buffers whatever the child writes, so any
/// payload cap applied afterwards only ever ran once the whole stream was
/// already resident. `yes`, `cat /dev/urandom | base64`, or a build stuck in
/// a warning loop therefore grew Stella's RSS without bound until the OOM
/// killer or the timeout arrived — whichever came first, and the OOM killer
/// usually won. 8 MiB per stream sits far above any real build or test log,
/// so nothing observable changes, while turning an unbounded allocation into
/// a bounded one.
///
/// It is `pub` for the same reason [`GroupKillGuard`] is: the plugin
/// transport (`stella_runtime::wrapper::subprocess`) reads a child's stdio
/// under the same hazard and a lower trust — a plugin is third-party code a
/// user installed, where a hook is at least operator-authored — so it shares
/// this ceiling rather than growing a second number. One subprocess plane,
/// one capture ceiling, exactly as that transport already shares the hook
/// plane's two timeout constants.
pub const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

/// Ceiling on any model-supplied `timeout_secs`. The timeout is the hang
/// backstop for commands the model itself launches — accepting an arbitrary
/// u64 lets one tool call disable the backstop entirely (`u64::MAX` ≈ never).
pub(crate) const MAX_TIMEOUT_SECS: u64 = 600;

/// The effective timeout for a model-supplied `timeout_secs`, clamped to
/// [`MAX_TIMEOUT_SECS`].
///
/// `0` and a missing field both mean "use the tool's default" rather than
/// "no timeout": a caller that wants longer says so with a number, and one
/// that says nothing gets the backstop. A non-integer is treated the same
/// way — this field only ever narrows a backstop, so a malformed value that
/// silently kept the default is the safe reading, unlike `trace` (#3144)
/// where the caller believes it armed evidence.
pub(crate) fn timeout_from(input: &serde_json::Value, default: u64) -> u64 {
    input
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .filter(|&t| t > 0)
        .unwrap_or(default)
        .min(MAX_TIMEOUT_SECS)
}

/// Clip `text` to at most `max_bytes`, never splitting a UTF-8 character.
///
/// For previews and echoes inside a larger message (a drifted line, a long
/// source line), where the point is to show the shape rather than the whole
/// content — unlike [`truncate_middle_capped`], which keeps both ends because
/// the tail of a build log is where the summary lives.
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

/// Head-plus-tail byte accumulator for one child stream: keeps the first and
/// last `cap / 2` bytes and counts what fell out of the middle.
///
/// Head AND tail because a failing command's information sits at both ends —
/// the first error and the final summary — which is the same reason
/// [`truncate_middle_capped`] exists. This is that policy moved to *ingest*
/// time, so the bytes in between are never held at all.
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

    /// Ingest `chunk`, answering whether the stream has now crossed the cap.
    ///
    /// The answer is what [`Overflow::Refuse`] acts on. Under
    /// [`Overflow::Elide`] it is ignored: the bytes above the cap are simply
    /// the ones this accumulator drops.
    fn push(&mut self, mut chunk: &[u8]) -> bool {
        if self.head.len() < self.half {
            let take = (self.half - self.head.len()).min(chunk.len());
            self.head.extend_from_slice(&chunk[..take]);
            chunk = &chunk[take..];
        }
        if chunk.is_empty() {
            return self.dropped > 0;
        }
        self.tail.extend(chunk.iter().copied());
        if self.tail.len() > self.half {
            let excess = self.tail.len() - self.half;
            self.tail.drain(..excess);
            self.dropped += excess as u64;
        }
        self.dropped > 0
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

/// One `read` on a maybe-already-EOF'd stream. Never called with `reader ==
/// None` in practice — every call site guards on `.is_some()` first — but
/// takes the `Option` directly rather than an already-unwrapped reference so
/// callers can hold the same slot across the two-phase wait below without a
/// second `Option` wrapper at each call site.
async fn read_into<R>(reader: &mut Option<R>, buf: &mut [u8]) -> std::io::Result<usize>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;
    match reader {
        Some(r) => r.read(buf).await,
        None => std::future::pending().await,
    }
}

/// After the direct child has exited, how much longer [`capture`] keeps
/// reading its stdout/stderr pipes before giving up on true EOF. A process that properly detaches (`setsid` with
/// its streams redirected away) never touches this window: its own copy of
/// the pipe's write end is already gone by the time we get here.
///
/// A plain `cmd &` backgrounded *inside* a spawned script is different — it
/// still holds THIS call's copy of the pipe open, because nothing told it
/// to detach. Requiring true EOF from such a pipe used to mean the whole
/// call hung until the caller's own timeout (seconds to minutes), even
/// though the command the model actually asked for finished instantly
/// (#2666). A short bounded drain gets the common case — output flushed
/// before backgrounding — without paying that price for the uncommon one.
///
/// `pub` because [`capture`] is: this window is part of that function's
/// contract — how long a call can outlast the child it waited for — not an
/// implementation detail a caller can reason without.
pub const BACKGROUND_DRAIN_GRACE: Duration = Duration::from_millis(300);

/// What a capped capture does with the bytes above its ceiling.
///
/// The two spawn planes want opposite answers, and each is right for its own
/// payload — which is why this is a parameter rather than a policy baked into
/// the loop below.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overflow {
    /// Keep reading and elide the middle of the stream. The hook and custom-
    /// tool planes: the output *is* the product, a log with its middle cut is
    /// still readable, and the command must still be allowed to finish.
    Elide,
    /// Stop the read and hand the caller a refusal. The plugin transport: the
    /// output is one JSON document, which a truncated copy cannot represent,
    /// so reading further only spends memory on a call that is already lost.
    Refuse,
}

/// How a capped capture ended.
#[derive(Debug)]
pub enum Capture {
    /// The direct child exited; here is what it wrote, with the middle elided
    /// under [`Overflow::Elide`] if it crossed the ceiling.
    Exited(std::process::Output),
    /// A stream crossed the ceiling under [`Overflow::Refuse`] and the read
    /// stopped there. **The child is still running** — killing it, and the
    /// process group it leads, is the caller's job.
    Refused {
        /// Which stream crossed the ceiling: `"stdout"` or `"stderr"`.
        stream: &'static str,
    },
}

/// [`tokio::process::Child::wait_with_output`] with a per-stream memory
/// ceiling (see [`MAX_CAPTURE_BYTES`]), a caller-chosen [`Overflow`] policy
/// for what crossing it means, and a bounded tolerance for a grandchild that
/// outlives the direct child while still holding its inherited copy of the
/// pipe (see [`BACKGROUND_DRAIN_GRACE`] and #2666).
///
/// Phase 1 races the exit wait against both pipes exactly as
/// `wait_with_output` does, so a child that fills one pipe while the other
/// is idle cannot deadlock. Phase 2 runs only once the direct child has
/// exited and only for streams still open: it drains whatever is already
/// sitting in the pipe, capped at [`BACKGROUND_DRAIN_GRACE`] rather than
/// waiting for a holder that may never let go.
///
/// Takes `&mut Child` rather than owning it so a caller refusing at the
/// ceiling still has the child — and its pid — to kill.
pub async fn capture(
    child: &mut tokio::process::Child,
    cap: usize,
    overflow: Overflow,
) -> std::io::Result<Capture> {
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut out = CappedStream::new(cap);
    let mut err = CappedStream::new(cap);
    let mut out_buf = [0u8; 16 * 1024];
    let mut err_buf = [0u8; 16 * 1024];

    let status = loop {
        tokio::select! {
            biased;
            res = child.wait() => break res?,
            res = read_into(&mut stdout, &mut out_buf), if stdout.is_some() => {
                match res? {
                    0 => stdout = None,
                    n => if out.push(&out_buf[..n]) && overflow == Overflow::Refuse {
                        return Ok(Capture::Refused { stream: "stdout" });
                    },
                }
            }
            res = read_into(&mut stderr, &mut err_buf), if stderr.is_some() => {
                match res? {
                    0 => stderr = None,
                    n => if err.push(&err_buf[..n]) && overflow == Overflow::Refuse {
                        return Ok(Capture::Refused { stream: "stderr" });
                    },
                }
            }
        }
    };

    let drain_deadline = tokio::time::sleep(BACKGROUND_DRAIN_GRACE);
    tokio::pin!(drain_deadline);
    while stdout.is_some() || stderr.is_some() {
        tokio::select! {
            biased;
            () = &mut drain_deadline => break,
            res = read_into(&mut stdout, &mut out_buf), if stdout.is_some() => {
                match res {
                    Ok(0) | Err(_) => stdout = None,
                    Ok(n) => if out.push(&out_buf[..n]) && overflow == Overflow::Refuse {
                        return Ok(Capture::Refused { stream: "stdout" });
                    },
                }
            }
            res = read_into(&mut stderr, &mut err_buf), if stderr.is_some() => {
                match res {
                    Ok(0) | Err(_) => stderr = None,
                    Ok(n) => if err.push(&err_buf[..n]) && overflow == Overflow::Refuse {
                        return Ok(Capture::Refused { stream: "stderr" });
                    },
                }
            }
        }
    }

    Ok(Capture::Exited(std::process::Output {
        status,
        stdout: out.into_bytes(),
        stderr: err.into_bytes(),
    }))
}

/// [`capture`] under [`Overflow::Elide`], owning the child as
/// `wait_with_output` does — the shape the hook and custom-tool planes want,
/// where the elided output is the answer and there is nothing to refuse.
pub(crate) async fn wait_with_capped_output(
    mut child: tokio::process::Child,
    cap: usize,
) -> std::io::Result<std::process::Output> {
    match capture(&mut child, cap, Overflow::Elide).await? {
        Capture::Exited(output) => Ok(output),
        // Dead by construction: `Elide` never stops a read. Reported as an
        // error rather than an `unreachable!` because a library must not
        // abort its host over its own refactor (invariant 5).
        Capture::Refused { stream } => Err(std::io::Error::other(format!(
            "the eliding capture refused {stream}, which that policy cannot do"
        ))),
    }
}

/// Start the child in a group of its own, so [`GroupKillGuard`] can reach
/// everything inside it rather than the direct child alone.
///
/// It exists so a spawn site outside this crate can take the policy without
/// taking a platform dependency and a copy of the block — the plugin
/// transport (`stella_runtime::wrapper::subprocess`) is the first such
/// caller. Every site that wants this policy calls it: [`crate::bash`],
/// [`crate::custom`] and [`crate::hook_runner`] here, the plugin transport and
/// the driver transport in `stella-runtime` (#3549). The guard half was
/// already shared, and [`GroupKillGuard`]'s own doc says every such site must
/// use *this* guard rather than grow a second one; the same argument applies
/// verbatim to the call that creates the group the guard kills.
///
/// # Unix
///
/// A new session (`setsid(2)`), so the child leads a process group whose id
/// is its pid — which is what the guard kills.
///
/// The `unsafe` half of that policy, written once. A `pre_exec` closure runs
/// in the forked child between `fork` and `exec`, where only
/// async-signal-safe calls are legal; `setsid(2)` is one, and this closure
/// allocates nothing and touches no lock, which is the whole requirement.
///
/// `stella-cli`'s daemon and the TUI's pty harness are deliberately not in
/// that set — their `pre_exec` closures do more than this one and check what
/// this one ignores. Those two and the call below are the whole of the
/// workspace's `setsid` surface; a fourth means a copy has grown back.
///
/// # Windows
///
/// A new console process group (`CREATE_NEW_PROCESS_GROUP`). The
/// tree-reaching half is the **Job Object** [`GroupKillGuard::arm`] creates,
/// because Windows offers no pre-spawn hook that could hand a job handle
/// back from here — so the job is created and the child assigned to it
/// immediately after the spawn instead (#3550).
///
/// The consequence is the same on both platforms, and so is what covers it: a
/// child in its own group no longer receives the console's Ctrl-C, exactly as
/// a `setsid` child no longer receives SIGINT, and the guard is the thing that
/// reaps the tree instead.
#[cfg(unix)]
pub fn detach_into_own_process_group(cmd: &mut Command) {
    // SAFETY: the closure calls one async-signal-safe libc function and
    // returns; it allocates nothing and acquires no lock, so it is safe to run
    // between `fork` and `exec`.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

#[cfg(windows)]
pub fn detach_into_own_process_group(cmd: &mut Command) {
    cmd.creation_flags(windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP);
}

/// Kills the whole group a [`detach_into_own_process_group`] child leads, on
/// drop unless disarmed — the cancellation backstop for the tools that spawn
/// one ([`crate::bash`], [`crate::custom`], [`crate::hook_runner`], and the
/// two transports in `stella-runtime`): when the future driving a tool call is
/// dropped mid-wait (Esc cancels the turn), the detached group must not keep
/// running — and mutating the tree — after the user believes the turn
/// stopped. Normal exit and the timeout path disarm it.
///
/// It is deliberately `pub`: every such spawn site in the workspace must use
/// *this* guard rather than grow a second one. A detached child no longer
/// receives the terminal's interrupt (see [`detach_into_own_process_group`]),
/// so this guard is the only thing that reaps the tree, and it fires because
/// the CLI drops the work future on a signal instead of calling `exit`
/// (`stella-cli/src/signals.rs`).
///
/// Never `tokio::spawn` teardown from a `Drop` instead: during runtime
/// shutdown the spawn silently does nothing, which is precisely the case
/// being handled. Both platforms' kills are synchronous, so this guard needs
/// no runtime.
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

/// A Windows Job Object handle, closed when it goes out of scope.
///
/// `HANDLE` is a raw pointer, so nothing about it is `Send` by inference —
/// but a job object is an opaque kernel object with no thread affinity, and
/// the guard is held across awaits inside `tokio::spawn`ed tasks (the plugin
/// transport, the hook runner), which requires `Send`. The wrapper says that
/// once instead of at each call site.
#[cfg(windows)]
struct JobHandle(windows_sys::Win32::Foundation::HANDLE);

// SAFETY: a job object handle is a kernel handle with no thread affinity; the
// kernel32 entry points used below are documented thread-safe, and this
// wrapper exposes no interior reference that a second thread could race.
#[cfg(windows)]
unsafe impl Send for JobHandle {}

#[cfg(windows)]
impl Drop for JobHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateJobObjectW`, is owned solely by
        // this wrapper, and is closed exactly once.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

/// The Windows counterpart: a Job Object holding the child, terminated on
/// drop unless disarmed.
///
/// `kill_on_drop(true)` reaches the direct child and nothing else, so before
/// this a hook, a custom tool, a `bash` call or a plugin wrapper that
/// backgrounded work left that work running after the turn was reported
/// finished (#3550). A job takes the whole tree, grandchildren the child
/// spawned included, which is exactly what `kill_on_drop` cannot do.
///
/// **`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` is deliberately not set**, and the
/// kill is an explicit `TerminateJobObject` on the two paths that want one.
/// With the limit set, [`Self::disarm`] would have to *clear* it before the
/// handle closed, and a `SetInformationJobObject` that failed there would
/// kill a tree the turn had already finished with — a regression on the
/// normal path, which is the one direction this must not fail in. Explicit
/// termination mirrors the unix `kill(-pid, SIGKILL)` exactly: it fires when
/// this guard says so and never otherwise. What that gives up is coverage of
/// Stella itself being killed without unwinding, where a `KILL_ON_JOB_CLOSE`
/// job would still reap the tree; unix has no equivalent today either.
#[cfg(windows)]
pub struct GroupKillGuard {
    /// `None` when the kernel refused any step of the arming below, which
    /// leaves `kill_on_drop` as the only bound — the state every call site
    /// was in before this existed, rather than a new failure.
    job: Option<JobHandle>,
    armed: bool,
}

#[cfg(windows)]
impl GroupKillGuard {
    /// Arm a guard over the tree led by `pid`. A pid of 0 — `Child::id` after
    /// the child was already reaped — is inert, as it is on unix.
    ///
    /// The job is created and assigned *after* the spawn because Windows has
    /// no pre-spawn hook to do it from `detach_into_own_process_group`. That
    /// leaves a window between spawn and assignment in which a grandchild
    /// started by the child would escape the job; it is microseconds wide, and
    /// closing it needs `CREATE_SUSPENDED` plus a `ResumeThread` on a thread
    /// handle `tokio::process` does not expose.
    pub fn arm(pid: i32) -> Self {
        Self {
            job: (pid > 0).then(|| job_holding(pid as u32)).flatten(),
            armed: true,
        }
    }

    /// Stop the guard from killing on drop. Call it once the tree is known to
    /// be gone: the child exited normally, or the caller already killed it on
    /// its timeout path.
    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// Terminate the tree now, and disarm. The timeout path: the child is
    /// still running and must die *before* the caller returns an error,
    /// rather than at some later scope exit.
    pub fn kill_now(&mut self) {
        self.armed = false;
        self.terminate();
    }

    fn terminate(&self) {
        if let Some(job) = &self.job {
            // SAFETY: `job` is a live job handle owned by this guard, and the
            // exit code is an arbitrary non-zero value the kernel only
            // reports back on the terminated processes.
            unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(job.0, 1) };
        }
    }
}

#[cfg(windows)]
impl Drop for GroupKillGuard {
    fn drop(&mut self) {
        if self.armed {
            self.terminate();
        }
    }
}

/// A job object with the process `pid` leads assigned to it, or `None` when
/// the kernel refused any step.
///
/// Failure is silent by design: the caller's only alternative is to abort a
/// tool call over a cancellation backstop it could not install, and the
/// backstop's absence is the state every call site was already in. The
/// process handle is closed as soon as the assignment is made — the job holds
/// its own reference, and leaking one handle per spawned tool would be a slow
/// leak in a long session.
#[cfg(windows)]
fn job_holding(pid: u32) -> Option<JobHandle> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, CreateJobObjectW};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    // SAFETY: every call is a kernel32 entry point taking only the handles and
    // integers passed to it; each failure is reported by a null or zero
    // return and is checked before the next call runs. An unnamed job with
    // default security is created with two null pointers, which is what the
    // documented "no attributes, no name" form is.
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return None;
        }
        // Owned from here, so every early return below closes it.
        let job = JobHandle(job);
        // `PROCESS_TERMINATE` as well as `PROCESS_SET_QUOTA`: assignment
        // needs both, because a job that cannot terminate its members is not
        // the guarantee this guard advertises.
        let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
        if process.is_null() {
            return None;
        }
        let assigned = AssignProcessToJobObject(job.0, process) != 0;
        CloseHandle(process);
        assigned.then_some(job)
    }
}

/// Keep the head and tail of `s` when it exceeds `max_bytes`, eliding the
/// middle with a marker that names both the elided byte count and the cap.
///
/// The crate's ONE model-facing elision spelling: every capped payload cuts
/// through here, so markers and budgets cannot drift apart (#1889 — they
/// had, three ways). The split is tail-biased — 40% head, 60% tail —
/// because a failing command's signal concentrates at both ends and the
/// tail end is the denser one: the final summary, the last error, the exit
/// status (lesson L-S3). Both cuts land on UTF-8 char boundaries so
/// multibyte output can never panic the slice.
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

    /// The ingest-time half of the output policy: head and tail survive, the
    /// middle is dropped, and the drop is loud. Without it, the model-facing
    /// cut only ran once the whole stream was already in memory.
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

    /// [`Overflow::Refuse`]'s half of the same hazard: the read *stops* at the
    /// ceiling instead of eliding past it, and the child is handed back still
    /// running so the caller can kill the group it leads. The plugin transport
    /// needs this shape because its payload is one JSON document — a copy with
    /// its middle cut cannot be decoded, so every byte read after the ceiling
    /// is spent on a call already lost.
    #[tokio::test]
    async fn a_refusing_capture_stops_the_read_and_leaves_the_child_to_kill() {
        let mut cmd = Command::new("bash");
        // Unbounded on purpose: only a refusal can end this read.
        cmd.arg("-c").arg("yes stella");
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);
        let mut child = cmd.spawn().expect("spawn");
        let outcome = capture(&mut child, 64 * 1024, Overflow::Refuse)
            .await
            .expect("refusing wait");
        let Capture::Refused { stream } = outcome else {
            panic!("an endless stream can only end in a refusal, got {outcome:?}");
        };
        assert_eq!(stream, "stdout");
        assert!(
            child.id().is_some(),
            "the refusal hands back a live child, because killing its group is the caller's call"
        );
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
