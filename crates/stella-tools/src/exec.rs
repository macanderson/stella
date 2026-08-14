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
pub(crate) const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

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

/// After the direct child has exited, how much longer
/// [`wait_with_capped_output`] keeps reading its stdout/stderr pipes before
/// giving up on true EOF. A process that properly detaches (`setsid` with
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
const BACKGROUND_DRAIN_GRACE: Duration = Duration::from_millis(300);

/// [`tokio::process::Child::wait_with_output`] with a per-stream memory
/// ceiling (see [`MAX_CAPTURE_BYTES`]) and a bounded tolerance for a
/// grandchild that outlives the direct child while still holding its
/// inherited copy of the pipe (see [`BACKGROUND_DRAIN_GRACE`] and #2666).
///
/// Phase 1 races the exit wait against both pipes exactly as
/// `wait_with_output` does, so a child that fills one pipe while the other
/// is idle cannot deadlock. Phase 2 runs only once the direct child has
/// exited and only for streams still open: it drains whatever is already
/// sitting in the pipe, capped at [`BACKGROUND_DRAIN_GRACE`] rather than
/// waiting for a holder that may never let go.
pub(crate) async fn wait_with_capped_output(
    mut child: tokio::process::Child,
    cap: usize,
) -> std::io::Result<std::process::Output> {
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
                    n => out.push(&out_buf[..n]),
                }
            }
            res = read_into(&mut stderr, &mut err_buf), if stderr.is_some() => {
                match res? {
                    0 => stderr = None,
                    n => err.push(&err_buf[..n]),
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
                    Ok(n) => out.push(&out_buf[..n]),
                }
            }
            res = read_into(&mut stderr, &mut err_buf), if stderr.is_some() => {
                match res {
                    Ok(0) | Err(_) => stderr = None,
                    Ok(n) => err.push(&err_buf[..n]),
                }
            }
        }
    }

    Ok(std::process::Output {
        status,
        stdout: out.into_bytes(),
        stderr: err.into_bytes(),
    })
}

/// SIGKILLs `pid`'s process group on drop unless disarmed — the
/// cancellation backstop for the tools that spawn `pre_exec(setsid)`
/// children ([`crate::custom`], [`crate::hook_runner`]): when the future
/// driving a tool call is dropped mid-wait (Esc cancels the turn), the
/// detached process group must not keep running — and mutating the tree —
/// after the user believes the turn stopped. Normal exit and the timeout
/// path disarm it.
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
