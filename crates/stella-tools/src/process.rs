//! Long-lived process group: `start_process`, `read_output`, `clear_output`,
//! `send_stdin`, `stop_process`.
//!
//! For servers, REPLs, and watchers — anything that outlives one tool call.
//! One-shot work belongs in `run_tests` / `build_project` / `run_script`,
//! and every tool description here says so, steering the model away from
//! parking a build in a process slot.
//!
//! `read_output` and `clear_output` used to be one tool with a `clear:
//! bool` mode flag — a parameter that picked *which operation ran* rather
//! than scoping the same one, which blurs the description the model
//! decides by and defeats per-tool policy (`tools.<name>` cannot deny the
//! destructive arm without also denying the harmless read). #2699 split
//! them. `read_output`'s `clear` field survives for one release as a
//! deprecation: any truthy value is refused with a named error pointing at
//! `clear_output`, rather than silently ignored or silently accepted.
//!
//! Contracts:
//! - `start_process` spawns an **argv vector directly** (no shell), cwd
//!   pinned to the workspace root, in its own process group, with
//!   `kill_on_drop`. It returns a handle id (`proc-N`). "No shell" is a
//!   quoting guarantee, not a power bound — `argv[0]` may itself be a shell —
//!   so the registry gates the joined argv through the same
//!   `command.started` policy chain as `bash` before the spawn.
//!
//!   **The session opened by that spawn is gated too.** `send_stdin`'s
//!   `text` field is itself in the registry's `command_line_for` (#804), so
//!   every write into a live interpreter's stdin rides its own
//!   `command.started` evaluation before the bytes reach the pipe — a policy
//!   that denies `rm -rf` sees it whether it arrives via `bash` or via text
//!   typed into a `["sh"]` session opened by `start_process`. Only the
//!   *spawn's own argv* — `["python3"]`, `["node"]` — is judged once, at
//!   start time; a policy that means to bound what an interpreter is later
//!   asked to run should still consult both events, since the spawn's argv
//!   alone does not say what will be typed into it.
//! - At most `MAX_LIVE_PROCESSES` processes may be LIVE at once; past
//!   that `start_process` refuses with a named error rather than letting a
//!   runaway loop spawn children without bound.
//! - Output (stdout + stderr, interleaved by arrival) accumulates in a
//!   capped ring buffer per process; when the cap overflows the oldest
//!   bytes are dropped and the drop is FLAGGED on the next read.
//! - `read_output` returns the output buffered since the last read —
//!   middle-truncated at the shared 30 KB page cap, with the cut named —
//!   and reports `running` / `exited (code N)`. Reading is itself
//!   destructive: it drains the buffer it returns, exactly like
//!   `clear_output` does, just without discarding the bytes unread. That
//!   is why `read_output` stays `read_only: false` even after the split —
//!   the engine's read-only concurrency contract
//!   ([`stella_protocol::tool::ToolSchema::read_only`]) requires a call
//!   to mutate no process/filesystem/environment state, and this one
//!   always mutates the process table's buffer (and can reap the entry
//!   into a tombstone) on every call, `clear` or not.
//! - `clear_output` discards a process's buffered output without
//!   returning it, for a caller that wants a clean slate before a noisy
//!   step. It does not stop the process. Like `read_output`, it mutates
//!   the table (and may reap a fully-drained, exited entry), so it is
//!   also `read_only: false`.
//! - Once a process has exited, both its pipes are at EOF, and its buffer
//!   has been drained (by either `read_output` or `clear_output`), the
//!   entry is REAPED — its `Child` and its buffer are released and a
//!   small tombstone takes their place, so the handle still reports its
//!   exit instead of degrading to "unknown handle".
//! - `stop_process` closes stdin, sends SIGTERM to the process group,
//!   waits `STOP_GRACE_MS`, then SIGKILLs the group — and any process
//!   still alive when the registry (and with it this table) drops is
//!   killed the same way, so nothing outlives the session.
//!
//! All errors are typed and named: unknown handle, exited process, closed
//! stdin, empty argv.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin, Command};

use crate::registry::Tool;

/// Byte cap on each process's buffered (unread) output.
const MAX_BUFFER_BYTES: usize = 200_000;
/// How much headroom an overflowing buffer reclaims in one drain — 25% of
/// the cap, so the memmove amortizes instead of running per chunk.
const BUFFER_DRAIN_SLACK: usize = MAX_BUFFER_BYTES / 4;
/// How long `stop_process` waits after SIGTERM before SIGKILL.
const STOP_GRACE_MS: u64 = 2_000;
/// How many LIVE processes one session may hold at once. Exited entries do
/// not count (a drained-but-unreaped process must never block a new start).
/// A picked number: high enough for a dev server, a watcher, a REPL and
/// room to spare; low enough that a runaway loop is stopped early.
const MAX_LIVE_PROCESSES: usize = 16;
/// How many reaped handles keep a tombstone. Bounded so the table cannot
/// grow without limit across a long session; the oldest handle is forgotten
/// first, and a forgotten handle degrades to the unknown-handle error.
const MAX_TOMBSTONES: usize = 64;
/// How many exited-but-unread entries the table retains. Exited entries are
/// exempt from [`MAX_LIVE_PROCESSES`] on purpose (a finished process must
/// never block a new start), but reaping only happens on `read_output` /
/// `clear_output` — so without a bound of their own, a start/exit loop
/// that never reads pins a `Child` plus up to [`MAX_BUFFER_BYTES`] per
/// iteration for the rest of the session. 32 keeps twice the live cap's
/// worth of recently finished output (~6.4 MB worst case) while bounding
/// the table; the excess is evicted into a tombstone, so the handle still
/// answers with its exit code.
const MAX_EXITED_ENTRIES: usize = 32;

/// The shared process table — held by the registry's five process tool
/// instances, so it lives exactly as long as the registry and its `Drop`
/// reaps whatever is still running when the session ends.
pub type ProcessTableHandle = Arc<Mutex<ProcessTable>>;

/// Capped interleaved stdout+stderr buffer. Overflow drops the OLDEST
/// bytes (a server's latest lines are the useful ones) and counts them so
/// the next read can flag the gap.
#[derive(Default)]
struct OutputBuffer {
    data: Vec<u8>,
    dropped: u64,
}

impl OutputBuffer {
    fn push(&mut self, chunk: &[u8]) {
        self.data.extend_from_slice(chunk);
        if self.data.len() > MAX_BUFFER_BYTES {
            // Drain down to a slack line rather than to exactly the cap: at
            // the cap, dropping only the excess memmoves the WHOLE 200 KB
            // buffer for every arriving chunk (a chatty server at 8 KB
            // chunks pays a 200 KB move 25 times per 200 KB of output).
            // Draining a quantum amortizes that to one move per quantum, at
            // the cost of reporting a few more dropped bytes — and dropped
            // bytes are counted, never silent.
            let excess = self.data.len() - (MAX_BUFFER_BYTES - BUFFER_DRAIN_SLACK);
            self.data.drain(..excess);
            self.dropped += excess as u64;
        }
    }

    /// Take everything buffered since the last take, with the dropped-byte
    /// count for the same window.
    fn take(&mut self) -> (Vec<u8>, u64) {
        (
            std::mem::take(&mut self.data),
            std::mem::take(&mut self.dropped),
        )
    }
}

/// One live (or recently exited) process.
struct ProcessEntry {
    /// Optional human label from `start_process`.
    name: Option<String>,
    /// The spawned argv, joined for display.
    display: String,
    child: Child,
    /// Taken out of `child` at spawn so `send_stdin` can write without
    /// holding the table lock across an await; `None` once closed.
    stdin: Option<ChildStdin>,
    /// Latched (under the table lock) when `stop_process` closes stdin. A
    /// `send_stdin` that had already taken the handle out for a write holds
    /// the pipe open past that close; without the latch its put-back
    /// resurrects the stdin the stop deliberately closed — revoking the EOF
    /// that lets a SIGTERM-ignoring REPL exit during the grace window.
    stdin_closed: bool,
    output: Arc<Mutex<OutputBuffer>>,
    /// Pumps still attached to this process's pipes. Zero means both pipes
    /// hit EOF, so no further byte can ever arrive — the precondition for
    /// reaping the entry without losing tail output.
    pumps: Arc<std::sync::atomic::AtomicUsize>,
    /// Group-kill target (unix); 0 when the pid was unavailable.
    pid: i32,
    /// Cached exit code once observed (signal exits report -1).
    exit_code: Option<i32>,
}

impl ProcessEntry {
    /// Poll (and cache) the exit status without blocking.
    fn poll_exit(&mut self) -> Option<i32> {
        if self.exit_code.is_none()
            && let Ok(Some(status)) = self.child.try_wait()
        {
            self.exit_code = Some(status.code().unwrap_or(-1));
        }
        self.exit_code
    }

    /// Can this entry be dropped? Only once it has exited, both pumps have
    /// seen EOF, and the buffer they filled has been handed to the model.
    fn is_reapable(&mut self) -> bool {
        self.poll_exit().is_some()
            && self.pumps.load(std::sync::atomic::Ordering::Acquire) == 0
            && self
                .output
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .data
                .is_empty()
    }
}

/// What survives a reaped process: enough to keep answering about a handle
/// the model still holds, without keeping its `Child` and its buffer alive.
struct ProcessTombstone {
    name: Option<String>,
    display: String,
    exit_code: i32,
    /// Buffered output the model never read, discarded at reap time. Zero on
    /// the ordinary drain-then-reap path; non-zero only when the exited-entry
    /// cap evicted the entry — counted so the tombstone can say so instead of
    /// pretending the buffer was drained.
    discarded_bytes: u64,
}

/// The session's process table. Handles are `proc-N`, N monotonic.
#[derive(Default)]
pub struct ProcessTable {
    next_id: u64,
    entries: HashMap<String, ProcessEntry>,
    /// Handles whose entry has been reaped. Bounded by [`MAX_TOMBSTONES`].
    tombstones: HashMap<String, ProcessTombstone>,
}

impl ProcessTable {
    fn known_handles(&self) -> String {
        if self.entries.is_empty() && self.tombstones.is_empty() {
            return "none".to_string();
        }
        let mut names: Vec<&str> = self
            .entries
            .keys()
            .chain(self.tombstones.keys())
            .map(String::as_str)
            .collect();
        names.sort_unstable();
        names.join(", ")
    }

    /// Processes still running. Exited-but-unreaped entries are excluded on
    /// purpose: they hold no OS resources the cap is protecting, and
    /// counting them would let a drained process block every new start.
    fn live_count(&mut self) -> usize {
        // A plain loop, not `filter().count()`: `poll_exit` caches the status
        // and so needs `&mut`, which `filter`'s `&&mut` cannot give.
        let mut live = 0;
        for entry in self.entries.values_mut() {
            if entry.poll_exit().is_none() {
                live += 1;
            }
        }
        live
    }

    /// Drop `handle`'s entry, leaving a tombstone so `read_output` /
    /// `clear_output` / `stop_process` can still explain it. Callers must have established
    /// [`ProcessEntry::is_reapable`] — except [`Self::enforce_exited_cap`],
    /// whose evictions knowingly discard an unread buffer and record the
    /// discard on the tombstone.
    fn reap(&mut self, handle: &str) {
        let Some(mut entry) = self.entries.remove(handle) else {
            return;
        };
        let exit_code = entry.poll_exit().unwrap_or(-1);
        let discarded_bytes = {
            let (bytes, dropped) = entry
                .output
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take();
            bytes.len() as u64 + dropped
        };
        self.tombstones.insert(
            handle.to_string(),
            ProcessTombstone {
                name: entry.name.take(),
                display: std::mem::take(&mut entry.display),
                exit_code,
                discarded_bytes,
            },
        );
        // Forget the oldest handle first — `proc-N` is monotonic, so the
        // smallest N is the least likely to still be held by the model.
        while self.tombstones.len() > MAX_TOMBSTONES {
            let Some(oldest) = self
                .tombstones
                .keys()
                .min_by_key(|h| {
                    h.strip_prefix("proc-")
                        .and_then(|n| n.parse::<u64>().ok())
                        .unwrap_or(u64::MAX)
                })
                .cloned()
            else {
                break;
            };
            self.tombstones.remove(&oldest);
        }
    }

    /// Evict exited entries beyond [`MAX_EXITED_ENTRIES`] into tombstones.
    /// Runs at the table's one growth point (`start_process`), which bounds
    /// total entries at the cap plus the live limit. Only entries whose exit
    /// has been observed are candidates — a running process is NEVER evicted.
    /// Oldest handle first (`proc-N` is monotonic), the same rule the
    /// tombstone bound uses: the smallest N is the least likely to still be
    /// polled.
    fn enforce_exited_cap(&mut self) {
        let mut exited: Vec<String> = Vec::new();
        for (handle, entry) in self.entries.iter_mut() {
            if entry.poll_exit().is_some() {
                exited.push(handle.clone());
            }
        }
        if exited.len() <= MAX_EXITED_ENTRIES {
            return;
        }
        exited.sort_unstable_by_key(|h| {
            h.strip_prefix("proc-")
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(u64::MAX)
        });
        let excess = exited.len() - MAX_EXITED_ENTRIES;
        for handle in exited.iter().take(excess) {
            self.reap(handle);
        }
    }
}

impl Drop for ProcessTable {
    /// Reap every process still running when the table (and the registry
    /// holding it) drops: group-SIGKILL on unix so grandchildren die too;
    /// `kill_on_drop` on each `Child` backs this up for the direct child.
    fn drop(&mut self) {
        for entry in self.entries.values_mut() {
            if entry.poll_exit().is_none() {
                #[cfg(unix)]
                if entry.pid > 0 {
                    // SAFETY: plain libc::kill on a recorded child pgid;
                    // guarded > 0 so we never signal our own group.
                    unsafe {
                        libc::kill(-entry.pid, libc::SIGKILL);
                    }
                }
                let _ = entry.child.start_kill();
            }
        }
    }
}

fn unknown_handle_error(table: &ProcessTable, handle: &str) -> ToolOutput {
    ToolOutput::Error {
        message: format!(
            "unknown process handle `{handle}` — known handles: {}",
            table.known_handles()
        ),
    }
}

/// Pump one child stream into the shared buffer until EOF. The lock is
/// held only for the synchronous push, never across an await. `live` is
/// decremented at EOF so the table can tell when no more output can arrive.
fn spawn_pump<R>(
    mut reader: R,
    output: Arc<Mutex<OutputBuffer>>,
    live: Arc<std::sync::atomic::AtomicUsize>,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => output
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(&buf[..n]),
            }
        }
        live.fetch_sub(1, std::sync::atomic::Ordering::Release);
    });
}

/// `start_process` — see the module doc.
pub struct StartProcess {
    pub handle: ProcessTableHandle,
    pub scratch: Option<std::path::PathBuf>,
}

#[async_trait]
impl Tool for StartProcess {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "start_process".into(),
            description: "Start a LONG-RUNNING process (dev server, REPL, watcher) from an \
                          argv vector — no shell, cwd = workspace root. Returns a handle for \
                          read_output / send_stdin / stop_process. For one-shot commands \
                          prefer run_tests, build_project, or run_script instead."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "argv": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Program and arguments, spawned directly (argv[0] is the program)" },
                    "name": { "type": "string", "description": "Optional human label for the handle" }
                },
                "required": ["argv"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput {
        let argv: Vec<String> = input
            .get("argv")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        if argv.is_empty() {
            return ToolOutput::Error {
                message: "`argv` must be a non-empty array of strings (argv[0] is the program)"
                    .into(),
            };
        }
        let name = input
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        // Refuse BEFORE spawning: nothing caps how many children a runaway
        // loop can create, and each one holds a pipe pair and up to
        // MAX_BUFFER_BYTES until it is stopped.
        {
            let mut table = self.handle.lock().unwrap_or_else(|p| p.into_inner());
            // Bound the exited-but-unread backlog before growing the table —
            // see [`ProcessTable::enforce_exited_cap`].
            table.enforce_exited_cap();
            let live = table.live_count();
            if live >= MAX_LIVE_PROCESSES {
                return ToolOutput::Error {
                    message: format!(
                        "{live} processes are already running and the limit is \
                         {MAX_LIVE_PROCESSES} — stop one with stop_process (known handles: {}), \
                         or use run_tests/build_project/run_script for one-shot work",
                        table.known_handles()
                    ),
                };
            }
        }

        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        cmd.current_dir(root);
        // Full spawn policy: git-repo retargeting and forced-color overrides
        // are scrubbed along with credentials, exactly like `exec::drive` —
        // a server's output lands in the captured buffer, never a terminal.
        crate::subprocess_env::scrub_spawn_env(&mut cmd);
        crate::subprocess_env::inject_scratch_env(&mut cmd, self.scratch.as_deref());
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);
        // Own process group so stop/reap can take down grandchildren.
        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return ToolOutput::Error {
                    message: format!("failed to spawn `{}`: {e}", argv.join(" ")),
                };
            }
        };
        let pid = child.id().unwrap_or(0) as i32;
        let stdin = child.stdin.take();
        let output: Arc<Mutex<OutputBuffer>> = Arc::default();
        let pumps = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        if let Some(stdout) = child.stdout.take() {
            pumps.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            spawn_pump(stdout, output.clone(), pumps.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            pumps.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            spawn_pump(stderr, output.clone(), pumps.clone());
        }

        let display = argv.join(" ");
        let mut table = self.handle.lock().unwrap_or_else(|p| p.into_inner());
        table.next_id += 1;
        let handle = format!("proc-{}", table.next_id);
        table.entries.insert(
            handle.clone(),
            ProcessEntry {
                name: name.clone(),
                display: display.clone(),
                child,
                stdin,
                stdin_closed: false,
                output,
                pumps,
                pid,
                exit_code: None,
            },
        );
        ToolOutput::Ok {
            content: format!(
                "started `{display}`{} as {handle} (pid {pid}) — poll it with read_output, \
                 stop it with stop_process",
                name.map(|n| format!(" ({n})")).unwrap_or_default()
            ),
        }
    }
}

/// The named error `read_output` returns for its deprecated `clear: true`
/// arm — see the module doc and #2699.
fn clear_removed_from_read_output_error() -> ToolOutput {
    ToolOutput::Error {
        message: "`read_output`'s `clear` field was removed — call `clear_output(handle)` \
                  instead to discard buffered output without reading it."
            .into(),
    }
}

/// `read_output` — see the module doc.
pub struct ReadOutput(pub ProcessTableHandle);

#[async_trait]
impl Tool for ReadOutput {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "read_output".into(),
            description: "Read a started process's buffered stdout+stderr since the last \
                          read, plus its running/exited state. To discard buffered output \
                          instead of reading it, use clear_output."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "handle": { "type": "string", "description": "Handle returned by start_process" },
                    "clear": { "type": "boolean", "description": "Deprecated and removed — use clear_output(handle) instead. Any truthy value is refused with a named error." }
                },
                "required": ["handle"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, _root: &std::path::Path) -> ToolOutput {
        let handle = match crate::input::required_str(input, "handle") {
            Ok(v) => v,
            Err(err) => {
                return ToolOutput::Error {
                    message: err.to_string(),
                };
            }
        };
        // Deprecation, kept for one release (#2699): a caller still sending
        // the removed `clear` mode flag gets a named error pointing at its
        // replacement, rather than the field being silently ignored (which
        // would leave buffered output the caller thought it discarded) or
        // silently honored (which would resurrect the split we just made).
        if input
            .get("clear")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return clear_removed_from_read_output_error();
        }
        let mut table = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(tomb) = table.tombstones.get(handle) {
            // Discarded bytes are reported, never silent — same contract as
            // the ring buffer's drop flag.
            let note = if tomb.discarded_bytes > 0 {
                format!(
                    "[{} unread bytes were discarded when this exited process was reaped to \
                     bound the table]",
                    tomb.discarded_bytes
                )
            } else {
                "[no new output — the process was reaped after its output was drained]".into()
            };
            return ToolOutput::Ok {
                content: format!(
                    "{handle} `{}`{}: exited (code {})\n{note}",
                    tomb.display,
                    tomb.name
                        .as_deref()
                        .map(|n| format!(" ({n})"))
                        .unwrap_or_default(),
                    tomb.exit_code,
                ),
            };
        }
        let Some(entry) = table.entries.get_mut(handle) else {
            return unknown_handle_error(&table, handle);
        };
        let status = match entry.poll_exit() {
            Some(code) => format!("exited (code {code})"),
            None => "running".to_string(),
        };
        let (bytes, dropped) = entry
            .output
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        let label = entry
            .name
            .as_deref()
            .map(|n| format!(" ({n})"))
            .unwrap_or_default();
        let mut content = format!("{handle} `{}`{label}: {status}", entry.display);
        if dropped > 0 {
            content.push_str(&format!(
                "\n[output truncated: {dropped} oldest bytes dropped before this read]"
            ));
        }
        if bytes.is_empty() {
            content.push_str("\n[no new output]");
        } else {
            content.push('\n');
            content.push_str(&crate::exec::truncate_middle(
                String::from_utf8_lossy(&bytes).into_owned(),
            ));
        }
        // The entry has served its purpose: the process is gone, both pipes
        // are at EOF, and everything they produced has now reached the
        // model. Keeping it would hold a `Child` and up to MAX_BUFFER_BYTES
        // for the rest of the session.
        if entry.is_reapable() {
            table.reap(handle);
        }
        ToolOutput::Ok { content }
    }
}

/// `clear_output` — see the module doc. Split out of `read_output`'s
/// `clear: true` arm by #2699: discarding output is a distinct operation
/// from reading it, not a mode of the same one, and folding them together
/// meant a policy denying the destructive arm (`tools.clear_output: "off"`)
/// had no way to spell that without also denying the harmless read.
pub struct ClearOutput(pub ProcessTableHandle);

#[async_trait]
impl Tool for ClearOutput {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "clear_output".into(),
            description: "Discard a started process's buffered stdout+stderr without \
                          reading it — use before a step you don't want in the next \
                          read_output. Does not stop the process."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "handle": { "type": "string", "description": "Handle returned by start_process" }
                },
                "required": ["handle"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, _root: &std::path::Path) -> ToolOutput {
        let handle = match crate::input::required_str(input, "handle") {
            Ok(v) => v,
            Err(err) => {
                return ToolOutput::Error {
                    message: err.to_string(),
                };
            }
        };
        let mut table = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(tomb) = table.tombstones.get(handle) {
            // Already reaped: its buffer was drained (and any unread bytes
            // counted) when it was reaped, so there is nothing left to
            // discard. Answering rather than erroring matches `read_output`
            // and `stop_process`'s posture on a tombstoned handle — the
            // model still gets a coherent answer about a handle it holds.
            return ToolOutput::Ok {
                content: format!(
                    "{handle} `{}`{}: exited (code {}) — already reaped, nothing to clear",
                    tomb.display,
                    tomb.name
                        .as_deref()
                        .map(|n| format!(" ({n})"))
                        .unwrap_or_default(),
                    tomb.exit_code,
                ),
            };
        }
        let Some(entry) = table.entries.get_mut(handle) else {
            return unknown_handle_error(&table, handle);
        };
        let status = match entry.poll_exit() {
            Some(code) => format!("exited (code {code})"),
            None => "running".to_string(),
        };
        let label = entry
            .name
            .as_deref()
            .map(|n| format!(" ({n})"))
            .unwrap_or_default();
        let (bytes, dropped) = entry
            .output
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        let mut content = format!(
            "{handle} `{}`{label}: {status}\n[cleared {} buffered bytes]",
            entry.display,
            bytes.len()
        );
        if dropped > 0 {
            content.push_str(&format!(
                " ({dropped} more were already dropped before this clear)"
            ));
        }
        // Same reap contract as `read_output`: once exited, drained, and
        // pump-quiet, nothing further can ever arrive on this handle.
        if entry.is_reapable() {
            table.reap(handle);
        }
        ToolOutput::Ok { content }
    }
}

/// `send_stdin` — see the module doc.
pub struct SendStdin(pub ProcessTableHandle);

#[async_trait]
impl Tool for SendStdin {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "send_stdin".into(),
            description: "Write text to a started process's stdin (e.g. a REPL command — \
                          include the trailing newline yourself). Errors if the process has \
                          exited or its stdin was closed."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "handle": { "type": "string", "description": "Handle returned by start_process" },
                    "text": { "type": "string", "description": "Bytes to write, verbatim (no newline is added)" }
                },
                "required": ["handle", "text"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, _root: &std::path::Path) -> ToolOutput {
        let handle = match crate::input::required_str(input, "handle") {
            Ok(v) => v,
            Err(err) => {
                return ToolOutput::Error {
                    message: err.to_string(),
                };
            }
        };
        let text = match crate::input::required_str(input, "text") {
            Ok(v) => v,
            Err(err) => {
                return ToolOutput::Error {
                    message: err.to_string(),
                };
            }
        };
        // Take stdin out under the lock, write outside it (a lock guard
        // must not cross an await), then put it back.
        let mut stdin = {
            let mut table = self.0.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(tomb) = table.tombstones.get(handle) {
                return ToolOutput::Error {
                    message: format!("{handle} has already exited (code {})", tomb.exit_code),
                };
            }
            let Some(entry) = table.entries.get_mut(handle) else {
                return unknown_handle_error(&table, handle);
            };
            if let Some(code) = entry.poll_exit() {
                return ToolOutput::Error {
                    message: format!("{handle} has already exited (code {code})"),
                };
            }
            match entry.stdin.take() {
                Some(stdin) => stdin,
                None => {
                    return ToolOutput::Error {
                        message: format!("{handle}'s stdin is closed"),
                    };
                }
            }
        };
        let write = async {
            stdin.write_all(text.as_bytes()).await?;
            stdin.flush().await
        }
        .await;
        let result = match write {
            Ok(()) => ToolOutput::Ok {
                content: format!("wrote {} bytes to {handle}", text.len()),
            },
            Err(e) => ToolOutput::Error {
                message: format!("write to {handle} stdin failed: {e}"),
            },
        };
        let mut table = self.0.lock().unwrap_or_else(|p| p.into_inner());
        // Put the handle back ONLY if no stop_process closed stdin while the
        // write was in flight — re-inserting after a stop would keep the pipe
        // open and revoke the EOF the stop delivered. Dropping it here closes
        // our end instead.
        if let Some(entry) = table.entries.get_mut(handle)
            && !entry.stdin_closed
        {
            entry.stdin = Some(stdin);
        }
        result
    }
}

/// `stop_process` — see the module doc.
pub struct StopProcess(pub ProcessTableHandle);

#[async_trait]
impl Tool for StopProcess {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "stop_process".into(),
            description: "Stop a started process: closes stdin, sends SIGTERM to its process \
                          group, waits briefly, then SIGKILLs. Remaining output stays \
                          readable via read_output."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "handle": { "type": "string", "description": "Handle returned by start_process" }
                },
                "required": ["handle"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, _root: &std::path::Path) -> ToolOutput {
        let handle = match crate::input::required_str(input, "handle") {
            Ok(v) => v,
            Err(err) => {
                return ToolOutput::Error {
                    message: err.to_string(),
                };
            }
        };
        // Phase 1, under the lock: close stdin (EOF lets well-behaved
        // REPLs/servers exit on their own) and send the group SIGTERM.
        let pid = {
            let mut table = self.0.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(tomb) = table.tombstones.get(handle) {
                return ToolOutput::Ok {
                    content: format!("{handle} had already exited (code {})", tomb.exit_code),
                };
            }
            let Some(entry) = table.entries.get_mut(handle) else {
                return unknown_handle_error(&table, handle);
            };
            entry.stdin = None;
            // Latch the close so a send_stdin holding the handle out for an
            // in-flight write cannot put it back afterwards.
            entry.stdin_closed = true;
            if let Some(code) = entry.poll_exit() {
                return ToolOutput::Ok {
                    content: format!("{handle} had already exited (code {code})"),
                };
            }
            #[cfg(unix)]
            if entry.pid > 0 {
                // SAFETY: signalling the child's own process group; the
                // > 0 guard means we never signal our own group.
                unsafe {
                    libc::kill(-entry.pid, libc::SIGTERM);
                }
            }
            entry.pid
        };
        // Phase 2: poll for exit without holding the lock across sleeps.
        let mut waited = 0u64;
        let exit_after_term = loop {
            {
                let mut table = self.0.lock().unwrap_or_else(|p| p.into_inner());
                let Some(entry) = table.entries.get_mut(handle) else {
                    return ToolOutput::Error {
                        message: format!("{handle} disappeared while stopping"),
                    };
                };
                if let Some(code) = entry.poll_exit() {
                    break Some(code);
                }
            }
            if waited >= STOP_GRACE_MS {
                break None;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            waited += 100;
        };
        if let Some(code) = exit_after_term {
            return ToolOutput::Ok {
                content: format!("{handle} terminated (code {code})"),
            };
        }
        // Phase 3: it ignored SIGTERM — SIGKILL the group.
        #[cfg(unix)]
        if pid > 0 {
            // SAFETY: same guarded group signal as above.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        #[cfg(not(unix))]
        let _ = pid;
        let mut table = self.0.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(entry) = table.entries.get_mut(handle) {
            let _ = entry.child.start_kill();
            let code = entry.poll_exit();
            return ToolOutput::Ok {
                content: match code {
                    Some(code) => format!("{handle} killed after SIGTERM grace (code {code})"),
                    None => format!("{handle} killed after SIGTERM grace"),
                },
            };
        }
        ToolOutput::Error {
            message: format!("{handle} disappeared while stopping"),
        }
    }
}

#[cfg(test)]
mod tests;
