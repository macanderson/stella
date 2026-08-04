//! stdio transport: spawn an MCP server as a child process and speak
//! newline-delimited JSON-RPC over its stdin/stdout.
//!
//! Two properties are load-bearing:
//!
//! 1. **Explicit environment.** The child is spawned
//!    with [`Command::env_clear`] and receives only keys explicitly listed in
//!    the server's config `env`, plus `PATH`. No parent-shell credential
//!    (`ANTHROPIC_API_KEY`, `AWS_*`, …) is inherited. Explicit config values
//!    remain available because they are MCP servers' documented auth channel;
//!    `PATH` is the sole inherited exception — it is not a secret and
//!    a bare runner command (`npx`, `uvx`, `docker`, …) cannot resolve
//!    without it — and a config `env` may still override it.
//! 2. **Concurrent in-flight requests.** Each request gets a monotonically
//!    increasing id and a `oneshot` slot in a pending-map; a single reader
//!    task demultiplexes responses back to the right waiter by id. Many
//!    requests can be outstanding at once — though today
//!    [`crate::client::McpClient`] holds its connection mutex for the whole
//!    call, so calls to one server are serialized a layer above this one.
//!    Dropping a `request` future (a caller-side timeout, Ctrl-C, a `select!`
//!    losing the race) reclaims its slot immediately: the slot is owned by a
//!    `PendingSlot` RAII guard on the request future's own stack, which is
//!    why the map is behind a `std::sync::Mutex` and not tokio's — `Drop`
//!    cannot await. Before #613 the entry and its `oneshot` sender leaked
//!    until the matching response arrived or the connection was drained by
//!    EOF/`close`, so a server that timed out N times held N dead slots for
//!    the life of the session.
//!
//! Server stderr is kept on its **own pipe**, never merged into stdout, so a
//! server that logs to stderr cannot corrupt the JSON-RPC framing — but it is
//! no longer thrown away (#638). A drain task copies it into a small bounded
//! ring (`StderrTail`: the newest `MAX_STDERR_LINES` lines, each clamped),
//! and the *tail* of that ring is attached to the [`McpError::Closed`] /
//! [`McpError::Transport`] a dead child produces. A server that starts and then
//! dies used to give the operator a bare "closed the connection before
//! responding"; now it hands back the server's own last words. The ring is
//! read-only diagnostics: nothing from stderr is ever parsed as JSON-RPC or
//! written to the child's stdin.
//!
//! Non-JSON lines that *do*
//! appear on stdout (a misbehaving server logging to the wrong stream) are
//! tolerated — skipped, never fatal. Every line is also read under a hard byte
//! cap (`MAX_LINE_BYTES`, 8 MiB): a server that never sends a newline is a memory
//! exhaustion, not a protocol error, so an over-cap line is truncated, its
//! remainder drained to the next newline, and the whole thing skipped like any
//! other unparseable line.
//!
//! **The transport itself never reconnects.** A child that exits leaves this
//! transport permanently dead: the reader drains every outstanding and future
//! request with [`McpError::Closed`]. Recovery is a *caller* decision, not a
//! silent transport behavior — [`crate::client::McpClient`] is the layer that
//! respawns a fresh `StdioTransport` (same scrubbed environment) under bounded
//! backoff, so the session self-heals without this file hiding a process
//! restart behind a `request` call.

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

use crate::error::McpError;
use crate::http::truncate;
use crate::protocol::{JsonRpcMessage, JsonRpcNotification, JsonRpcRequest};
use crate::transport::Transport;

/// How long `close()` waits for a clean exit after closing stdin before it
/// resorts to SIGKILL.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

/// How many stderr lines the diagnostic ring keeps. The *newest* ones: a
/// server that dies explains itself on its way out, so the tail is the part
/// worth having.
const MAX_STDERR_LINES: usize = 16;

/// Per-line character budget in the ring. A chatty server must not grow
/// stella's memory, so the ring is bounded on both axes — at most
/// `MAX_STDERR_LINES` × `MAX_STDERR_LINE_CHARS` characters are retained
/// (≤ 32 KiB even if every character is a 4-byte codepoint), no matter how
/// much the child writes.
const MAX_STDERR_LINE_CHARS: usize = 512;

/// The largest single stderr line the drain task will buffer before clamping
/// and skipping the remainder — the same defense [`MAX_LINE_BYTES`] gives
/// stdout, sized far smaller because this stream is only ever diagnostics.
const MAX_STDERR_LINE_READ_BYTES: u64 = 64 * 1024;

/// How long the stdout reader waits for the stderr drain task to reach EOF
/// before it composes the "connection closed" error. A child that died has
/// closed *both* pipes, so this normally resolves immediately; the bound is
/// there so a child that closes only stdout cannot stall the drain of pending
/// requests.
const STDERR_SETTLE: Duration = Duration::from_millis(150);

/// The largest single stdout line the reader will buffer. An MCP server is
/// untrusted input: an unbounded line read grows one buffer until the
/// allocator gives up, so a server that streams bytes and never sends a
/// newline would be an out-of-memory kill of *stella*, not a failed call. The
/// JSON-RPC frames this transport sends and expects are small; 8 MiB is far
/// past any plausible one and matches the bound `sse.rs` puts on an
/// unterminated event.
const MAX_LINE_BYTES: u64 = 8 * 1024 * 1024;

/// The in-flight request table: response id → the waiter's `oneshot` sender.
///
/// The lock is a **`std::sync::Mutex`**, not tokio's, and that is load-bearing
/// (#613). A dropped `request` future — a caller-side timeout, Ctrl-C, a
/// `select!` losing the race — must reclaim its slot, and reclamation can only
/// happen in `Drop`, which cannot `await`. The two shapes that a tokio mutex
/// leaves are both wrong: leak the entry (what this file did until #613 — one
/// map entry plus its sender per timed-out call, held until EOF or `close()`),
/// or `tokio::spawn` the removal from `Drop`, which silently does nothing
/// during runtime shutdown, precisely the case being fixed.
///
/// The one hazard of a sync mutex in async code is holding the guard across an
/// `.await`, which would block the executor thread. It never happens here:
/// every critical section is a single `insert`/`remove`/`take` on a `HashMap`
/// and the guard is dropped before the statement ends — see [`lock_pending`],
/// [`PendingSlot`], and the `std::mem::take` drains in [`read_loop`] and
/// `close`. Keep it that way.
type Pending = Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Result<Value, McpError>>>>>;

/// Lock the pending map, recovering from a poisoned mutex — a panicking
/// waiter must not wedge every later request on this connection.
///
/// **Never hold the returned guard across an `.await`.**
fn lock_pending(
    pending: &Pending,
) -> std::sync::MutexGuard<'_, HashMap<u64, oneshot::Sender<Result<Value, McpError>>>> {
    pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Owns one row of the pending map for the lifetime of a `request` call, and
/// removes it on drop (#613).
///
/// Always armed: on the happy path the reader task has already taken the entry
/// by the time this drops, so the removal is a no-op — ids are monotonic and
/// never reused, so it can never reclaim someone else's slot. That makes the
/// cancelled path (the future dropped mid-`await`) and the success path the
/// same code, with no flag to forget to set.
struct PendingSlot {
    pending: Pending,
    id: u64,
}

impl Drop for PendingSlot {
    fn drop(&mut self) {
        // Synchronous by construction — see the note on [`Pending`].
        lock_pending(&self.pending).remove(&self.id);
    }
}

/// The last few lines a child wrote to stderr, kept so a dead server can say
/// *why* it died (#638).
///
/// A bounded ring on both axes ([`MAX_STDERR_LINES`] lines, each clamped to
/// [`MAX_STDERR_LINE_CHARS`]): the drain task must keep reading forever — an
/// unread stderr pipe eventually blocks the child's own writes — so the buffer
/// has to be able to *forget*. Oldest lines are evicted first.
///
/// Cloneable and cheap: the transport, the stderr drain task, and the stdout
/// reader task all hold the same ring. The lock is a `std::sync::Mutex` (never
/// held across an `await`) so the synchronous error-construction paths
/// ([`StdioTransport::closed_error`]) can read the tail too.
#[derive(Clone, Default)]
pub(crate) struct StderrTail {
    lines: Arc<std::sync::Mutex<VecDeque<String>>>,
}

impl StderrTail {
    /// Record one line, evicting the oldest when the ring is full. Blank lines
    /// are dropped — they would spend the budget saying nothing.
    fn push(&self, line: &str) {
        let line = line.trim_end();
        if line.trim().is_empty() {
            return;
        }
        let mut lines = self.lines.lock().unwrap_or_else(|p| p.into_inner());
        if lines.len() == MAX_STDERR_LINES {
            lines.pop_front();
        }
        lines.push_back(truncate(line, MAX_STDERR_LINE_CHARS));
    }

    /// The kept lines, oldest first, or `None` when the child said nothing.
    fn lines(&self) -> Option<Vec<String>> {
        let lines = self.lines.lock().unwrap_or_else(|p| p.into_inner());
        (!lines.is_empty()).then(|| lines.iter().cloned().collect())
    }

    /// Append the captured tail to a diagnostic `message`. A child that wrote
    /// nothing leaves the message byte-identical — no "(no stderr)" noise on
    /// the common path.
    fn annotate(&self, message: String) -> String {
        match self.lines() {
            None => message,
            Some(lines) => format!(
                "{message} — last {} line(s) of the server's stderr:\n  {}",
                lines.len(),
                lines.join("\n  ")
            ),
        }
    }
}

/// A live stdio connection to one MCP server.
pub struct StdioTransport {
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
    pending: Pending,
    next_id: AtomicU64,
    closed: Arc<AtomicBool>,
    reader: Mutex<Option<JoinHandle<()>>>,
    server_name: String,
    /// The child's last stderr lines, attached to every connection-death error
    /// this transport produces.
    stderr_tail: StderrTail,
}

impl StdioTransport {
    /// Spawn `cmd args…` with a scrubbed environment plus exactly the keys in
    /// `env`, and start the reader task. `server_name` is used only to make
    /// error messages self-identifying.
    pub async fn spawn(
        server_name: &str,
        cmd: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> Result<Self, McpError> {
        let mut command = Command::new(cmd);
        command
            .args(args)
            .env_clear() // SCRUB — no ambient inheritance.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // A pipe of its own: server logs stay off the JSON-RPC stream (only
            // stdout is ever parsed) while still being readable as diagnostics.
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // The environment is scrubbed by design so no ambient *credential*
        // (`ANTHROPIC_API_KEY`, `AWS_*`, …) ever leaks into an MCP subprocess.
        // `PATH` is the one exception: it is not a secret, and without it a
        // server invoked by a bare runner name — `npx`, `uvx`, `docker`,
        // `dnx`, `node` — cannot be found at all, which is exactly how the
        // registry installs npm/pypi/oci servers. So PATH is inherited from
        // the parent unless the config pins its own; everything else stays
        // scrubbed. (An absolute `cmd` path needs no PATH and is unaffected.)
        if !env.contains_key("PATH")
            && let Some(path) = std::env::var_os("PATH")
        {
            command.env("PATH", path);
        }
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .map_err(|e| McpError::Transport(format!("failed to spawn `{cmd}`: {e}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("child process has no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("child process has no stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| McpError::Transport("child process has no stderr".into()))?;

        let pending: Pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        // Drain stderr continuously into the ring: an unread pipe would
        // eventually block a chatty child mid-write, so this task keeps
        // reading (and forgetting) for the whole life of the process.
        let stderr_tail = StderrTail::default();
        let stderr_drain = tokio::spawn(drain_stderr(stderr, stderr_tail.clone()));
        let reader = tokio::spawn(read_loop(
            stdout,
            pending.clone(),
            closed.clone(),
            server_name.to_string(),
            stderr_tail.clone(),
            Some(stderr_drain),
        ));

        Ok(Self {
            stdin: Mutex::new(Some(stdin)),
            child: Mutex::new(Some(child)),
            pending,
            next_id: AtomicU64::new(1),
            closed,
            reader: Mutex::new(Some(reader)),
            server_name: server_name.to_string(),
            stderr_tail,
        })
    }

    /// How many requests are currently outstanding. The observable that makes
    /// the #613 slot leak testable at all.
    #[cfg(test)]
    fn pending_len(&self) -> usize {
        lock_pending(&self.pending).len()
    }

    /// The "not connected" error, carrying whatever the child last wrote to
    /// stderr — for a server that died on startup that tail is the only
    /// explanation the operator will ever get.
    fn closed_error(&self) -> McpError {
        McpError::Closed(
            self.stderr_tail
                .annotate(format!("server `{}` is not connected", self.server_name)),
        )
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(self.closed_error());
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        lock_pending(&self.pending).insert(id, tx);
        // From here the slot belongs to this future's stack: every exit —
        // the write failing, the response arriving, or the CALLER DROPPING
        // this future mid-`await` — reclaims it. No `.await` is held under
        // the lock above; the guard dies with the statement.
        let _slot = PendingSlot {
            pending: self.pending.clone(),
            id,
        };

        let request = JsonRpcRequest::new(id, method, params);
        let line = encode_line(&request)?;

        self.write_line(&line).await?;

        // The reader task fulfills this via the oneshot. A dropped sender
        // (reader exited on EOF) surfaces as a closed connection.
        match rx.await {
            Ok(result) => result,
            Err(_) => Err(self.closed_error()),
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(self.closed_error());
        }
        let note = JsonRpcNotification::new(method, params);
        let line = encode_line(&note)?;
        self.write_line(&line).await
    }

    async fn close(&self) -> Result<(), McpError> {
        self.closed.store(true, Ordering::SeqCst);

        // Close stdin to signal EOF to a well-behaved server.
        {
            let mut guard = self.stdin.lock().await;
            guard.take(); // dropping ChildStdin closes it.
        }

        // Wait briefly for a clean exit, then kill.
        {
            let mut guard = self.child.lock().await;
            if let Some(mut child) = guard.take() {
                match tokio::time::timeout(SHUTDOWN_GRACE, child.wait()).await {
                    Ok(_) => {}
                    Err(_) => {
                        let _ = child.kill().await;
                    }
                }
            }
        }

        // Stop the reader task if it hasn't already exited on EOF.
        if let Some(handle) = self.reader.lock().await.take() {
            handle.abort();
        }

        // Fail whatever was still in flight. Aborting the reader means nothing
        // will ever fulfill those oneshots, and the senders live in this map —
        // so without this drain a concurrent `request` awaiting its response
        // would hang until its own caller-side timeout (forever, for a caller
        // that has none). Same shape as the reader's EOF drain.
        //
        // The whole map is moved out under the lock and the sends happen
        // outside it: `oneshot::Sender::send` never awaits, but taking first
        // keeps the critical section a single statement, which is the
        // invariant [`Pending`] depends on.
        let orphaned = std::mem::take(&mut *lock_pending(&self.pending));
        for (_id, tx) in orphaned {
            let _ = tx.send(Err(McpError::Closed(self.stderr_tail.annotate(format!(
                "server `{}` connection was closed before the response arrived",
                self.server_name
            )))));
        }
        Ok(())
    }
}

impl StdioTransport {
    /// Write one framed line to the child's stdin. A broken pipe here means the
    /// child is gone, so the failure carries its stderr tail — the write error
    /// itself ("broken pipe") never says why the process left.
    async fn write_line(&self, line: &str) -> Result<(), McpError> {
        let mut guard = self.stdin.lock().await;
        let stdin = guard.as_mut().ok_or_else(|| self.closed_error())?;
        stdin.write_all(line.as_bytes()).await.map_err(|e| {
            McpError::Transport(
                self.stderr_tail
                    .annotate(format!("write to `{}` failed: {e}", self.server_name)),
            )
        })?;
        stdin.flush().await.map_err(|e| {
            McpError::Transport(
                self.stderr_tail
                    .annotate(format!("flush to `{}` failed: {e}", self.server_name)),
            )
        })?;
        Ok(())
    }
}

/// Serialize a JSON-RPC value to a single newline-terminated line.
fn encode_line<T: serde::Serialize>(value: &T) -> Result<String, McpError> {
    let mut line = serde_json::to_string(value).map_err(|e| McpError::Protocol(e.to_string()))?;
    line.push('\n');
    Ok(line)
}

/// Reader task: demultiplex JSON-RPC messages from the child's stdout back to
/// their waiting requests. Exits on EOF, then fails every outstanding request
/// so no caller hangs on a dead server.
///
/// Generic over the byte source rather than taking [`tokio::process::ChildStdout`]
/// so the framing rules — the [`MAX_LINE_BYTES`] cap in particular — can be
/// driven directly from a test.
///
/// `stderr` / `stderr_drain` exist only for the death message: on EOF the loop
/// waits (briefly, bounded by [`STDERR_SETTLE`]) for the stderr drain task to
/// finish so the child's *final* lines are in the ring, then attaches them to
/// the [`McpError::Closed`] every orphaned waiter receives.
async fn read_loop<R: AsyncRead + Unpin>(
    stdout: R,
    pending: Pending,
    closed: Arc<AtomicBool>,
    server_name: String,
    stderr: StderrTail,
    stderr_drain: Option<JoinHandle<()>>,
) {
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        // `take` is what bounds the allocation: at most MAX_LINE_BYTES land in
        // `buf` no matter what the server sends. The loop ends on EOF (`Ok(0)`)
        // or a read error — either way the connection is gone and we fall
        // through to drain every pending request.
        let read = match (&mut reader)
            .take(MAX_LINE_BYTES)
            .read_until(b'\n', &mut buf)
            .await
        {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };

        // No terminator after a full cap's worth of bytes means the line is
        // over-cap. Its tail must be thrown away too, or the bytes after the
        // truncation point would be reparsed as if they were a fresh frame —
        // a server could then smuggle a response past the cap. Disposition is
        // otherwise identical to any other unparseable line: skip, keep going.
        if buf.last() != Some(&b'\n') && read as u64 == MAX_LINE_BYTES {
            if !discard_to_newline(&mut reader).await {
                break;
            }
            continue;
        }

        // Non-UTF-8 or non-JSON noise on stdout is tolerated: skip it, keep
        // reading.
        let Ok(line) = std::str::from_utf8(&buf) else {
            continue;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<JsonRpcMessage>(trimmed) else {
            continue;
        };
        // Only a genuine RESPONSE fulfills its waiter. A server-initiated
        // request (e.g. `ping`, which carries a `method` AND an id) or a
        // notification must never be routed to a pending client waiter — an id
        // collision would otherwise hand a caller the server's request as its
        // answer, failing the real call and orphaning the real response. An
        // unknown/timed-out id is dropped — v1 drives none of the
        // server->client surface.
        if message.is_response()
            && let Some(id) = message.correlated_id()
        {
            // Take the waiter out under the lock, then release it before the
            // send — no `.await` under the sync mutex ([`Pending`]).
            let waiter = lock_pending(&pending).remove(&id);
            if let Some(tx) = waiter {
                let _ = tx.send(message.into_result());
            }
        }
    }

    closed.store(true, Ordering::SeqCst);
    // The child's parting words usually land on stderr microseconds before its
    // stdout hits EOF. Let the drain task finish (it ends the moment the stderr
    // pipe closes, which a dead child has already done) so the tail we attach
    // includes the cause instead of racing it.
    if let Some(drain) = stderr_drain {
        let _ = tokio::time::timeout(STDERR_SETTLE, drain).await;
    }
    let orphaned = std::mem::take(&mut *lock_pending(&pending));
    for (_id, tx) in orphaned {
        let _ = tx.send(Err(McpError::Closed(stderr.annotate(format!(
            "server `{server_name}` closed the connection before responding"
        )))));
    }
}

/// Drain task: copy the child's stderr into `tail` forever, one line at a time,
/// and never do anything else with it — these bytes are diagnostics, never
/// JSON-RPC (only [`read_loop`], reading *stdout*, feeds the protocol).
///
/// It has to keep reading even when the ring is full: stderr is a pipe with a
/// finite kernel buffer, so a task that stopped consuming would eventually
/// block a chatty child mid-write and wedge the server. Each line is read under
/// [`MAX_STDERR_LINE_READ_BYTES`] and clamped again on the way into the ring,
/// so an unterminated flood costs bounded memory here as well.
async fn drain_stderr<R: AsyncRead + Unpin>(stderr: R, tail: StderrTail) {
    let mut reader = BufReader::new(stderr);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let read = match (&mut reader)
            .take(MAX_STDERR_LINE_READ_BYTES)
            .read_until(b'\n', &mut buf)
            .await
        {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        // Lossy on purpose: stderr is free-form output, and a server that emits
        // a stray non-UTF-8 byte should still have its message read back.
        tail.push(&String::from_utf8_lossy(&buf));
        // No terminator after a full cap's worth of bytes: throw the rest of
        // that line away rather than buffering it.
        if buf.last() != Some(&b'\n')
            && read as u64 == MAX_STDERR_LINE_READ_BYTES
            && !discard_to_newline(&mut reader).await
        {
            break;
        }
    }
}

/// Throw away bytes up to and including the next newline, without buffering
/// them. Returns `false` when the stream ended (or errored) first, which the
/// caller treats exactly like EOF.
async fn discard_to_newline<R: AsyncBufRead + Unpin>(reader: &mut R) -> bool {
    loop {
        let (consumed, found) = match reader.fill_buf().await {
            Ok([]) | Err(_) => return false,
            Ok(chunk) => match chunk.iter().position(|b| *b == b'\n') {
                Some(idx) => (idx + 1, true),
                None => (chunk.len(), false),
            },
        };
        reader.consume(consumed);
        if found {
            return true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawning_a_missing_binary_is_a_transport_error() {
        let env = BTreeMap::new();
        let result =
            StdioTransport::spawn("ghost", "definitely-not-a-real-binary-xyzzy", &[], &env).await;
        assert!(matches!(result, Err(McpError::Transport(_))));
    }

    /// A bare runner command (the shape the registry installs for
    /// npm/pypi/oci servers — `npx`, `uvx`, `docker`) must resolve via an
    /// inherited PATH even though the environment is otherwise scrubbed.
    /// Without the PATH pass-through this failed to spawn — every
    /// registry-installed stdio server was dead on arrival.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_bare_runner_command_resolves_via_inherited_path() {
        // `cat` is a bare name that only resolves through PATH; with the
        // scrub-everything-but-PATH policy it must still be found and spawned.
        let env = BTreeMap::new();
        let transport = StdioTransport::spawn("cat-server", "cat", &[], &env)
            .await
            .expect("a bare runner must resolve via the inherited PATH");
        let _ = transport.close().await;
    }

    /// #613: a caller-side timeout drops the `request` future, and the slot it
    /// registered in the pending map must go with it. `cat` is a server that
    /// never answers — it echoes the request line back, and an echoed
    /// *request* (it carries a `method`) is never routed to a waiter — so the
    /// call can only end by being dropped.
    ///
    /// Before the [`PendingSlot`] guard every such call left one map entry
    /// plus its `oneshot` sender behind until EOF or `close()`; a session that
    /// timed out repeatedly against one server grew the map monotonically.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_dropped_request_reclaims_its_pending_slot() {
        let env = BTreeMap::new();
        let transport = StdioTransport::spawn("cat-server", "cat", &[], &env)
            .await
            .expect("cat must spawn");

        for expected_id in 1..=3u64 {
            let call = transport.request("tools/list", serde_json::json!({}));
            let timed_out = tokio::time::timeout(Duration::from_millis(150), call).await;
            assert!(timed_out.is_err(), "cat must never answer a request");
            assert_eq!(
                transport.pending_len(),
                0,
                "request {expected_id} leaked its pending slot"
            );
        }

        let _ = transport.close().await;
    }

    /// The guard in isolation: the row is gone the moment it drops, with no
    /// runtime, no `await`, and no spawn — the property that makes it correct
    /// during runtime shutdown.
    #[test]
    fn the_pending_slot_guard_reclaims_synchronously_on_drop() {
        let pending: Pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let (tx, _rx) = oneshot::channel();
        lock_pending(&pending).insert(7, tx);

        let slot = PendingSlot {
            pending: pending.clone(),
            id: 7,
        };
        assert_eq!(lock_pending(&pending).len(), 1);
        drop(slot);
        assert!(lock_pending(&pending).is_empty());

        // Reclaiming a row the reader already took is a harmless no-op — ids
        // are monotonic, so it can never evict a later request's slot.
        let slot = PendingSlot {
            pending: pending.clone(),
            id: 7,
        };
        let (other, _other_rx) = oneshot::channel();
        lock_pending(&pending).insert(8, other);
        drop(slot);
        assert_eq!(lock_pending(&pending).len(), 1);
    }

    /// Drive `read_loop` over an in-memory pipe with one pending waiter, feed
    /// it `lines`, and return what (if anything) the waiter for `id` received.
    async fn route_lines(id: u64, lines: Vec<Vec<u8>>) -> Option<Result<Value, McpError>> {
        let (reader_side, mut writer_side) = tokio::io::duplex(64 * 1024);
        let pending: Pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let (tx, rx) = oneshot::channel();
        lock_pending(&pending).insert(id, tx);

        let reader = tokio::spawn(read_loop(
            reader_side,
            pending.clone(),
            closed.clone(),
            "fixture".to_string(),
            StderrTail::default(),
            None,
        ));
        for line in lines {
            writer_side.write_all(&line).await.expect("write line");
            writer_side
                .write_all(b"\n")
                .await
                .expect("write terminator");
        }

        let received = tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .expect("the reader must answer or drop the waiter well inside the timeout")
            .ok();
        reader.abort();
        received
    }

    /// A JSON-RPC response line whose padding pushes it past `MAX_LINE_BYTES`.
    fn oversized_response(id: u64) -> Vec<u8> {
        let pad = "x".repeat(MAX_LINE_BYTES as usize);
        format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"marker":"over-cap","pad":"{pad}"}}}}"#)
            .into_bytes()
    }

    fn response(id: u64, marker: &str) -> Vec<u8> {
        format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{{"marker":"{marker}"}}}}"#).into_bytes()
    }

    /// Witness for the [`MAX_LINE_BYTES`] cap. Before it, `.lines()` buffered a
    /// whole unbounded line into one `String` and then parsed it, so an
    /// over-cap frame was routed to its waiter (and a server that never sent a
    /// newline grew that buffer until the allocator failed). Now the read is
    /// truncated and the line is discarded like any other unparseable one — so
    /// the waiter must see the *later*, small response instead.
    #[tokio::test]
    async fn an_over_cap_line_is_skipped_and_a_later_response_still_routes() {
        let received = route_lines(7, vec![oversized_response(7), response(7, "in-band")])
            .await
            .expect("the waiter must still be fulfilled after an over-cap line")
            .expect("the small response is a success result");
        // Compare the marker rather than the whole value: an over-cap result
        // carries megabytes of padding and would drown the failure message.
        assert_eq!(
            received.get("marker").and_then(Value::as_str),
            Some("in-band"),
            "an over-cap line must never be routed to a waiter"
        );
    }

    /// The remainder of an over-cap line has to be drained to the next
    /// newline. Reading only the capped prefix and resuming would hand the
    /// tail to the parser as if it were a fresh frame — which is a way to
    /// smuggle a response past the cap, and re-orders the two answers this
    /// waiter could get.
    #[tokio::test]
    async fn the_tail_of_an_over_cap_line_is_not_reparsed_as_a_frame() {
        let mut smuggled = "x".repeat(MAX_LINE_BYTES as usize).into_bytes();
        smuggled.extend_from_slice(&response(9, "smuggled-tail"));

        let received = route_lines(9, vec![smuggled, response(9, "in-band")])
            .await
            .expect("the waiter must still be fulfilled after an over-cap line")
            .expect("the small response is a success result");
        assert_eq!(
            received.get("marker").and_then(Value::as_str),
            Some("in-band"),
            "the tail beyond the cap must be discarded, not parsed as a frame"
        );
    }

    // Server stderr as a diagnostic (#638). A child that starts and then dies
    // used to leave the operator with a bare "closed the connection before
    // responding"; its own explanation was written to a `Stdio::null()`.

    #[tokio::test]
    async fn the_stderr_ring_forgets_the_oldest_lines_and_clamps_each_one() {
        let tail = StderrTail::default();
        for i in 0..MAX_STDERR_LINES * 3 {
            tail.push(&format!("line {i}\n"));
        }
        tail.push("   \n"); // blank lines never spend the budget
        tail.push(&"w".repeat(MAX_STDERR_LINE_CHARS * 4));

        let lines = tail.lines().expect("the ring kept something");
        assert_eq!(
            lines.len(),
            MAX_STDERR_LINES,
            "a chatty server must not grow memory without limit"
        );
        assert!(
            lines[0].starts_with(&format!(
                "line {}",
                MAX_STDERR_LINES * 3 - MAX_STDERR_LINES + 1
            )),
            "the OLDEST lines are the ones evicted: {:?}",
            lines[0]
        );
        let clamped = lines.last().expect("non-empty");
        assert!(
            clamped.chars().count() <= MAX_STDERR_LINE_CHARS + 1,
            "one enormous line is clamped too: {} chars",
            clamped.chars().count()
        );
    }

    /// A child whose stderr said nothing leaves a diagnostic byte-identical —
    /// the annotation is additive, never noise on the healthy path.
    #[test]
    fn a_silent_server_leaves_the_message_untouched() {
        let tail = StderrTail::default();
        assert_eq!(tail.annotate("plain".to_string()), "plain");
    }

    /// The witness for #638's second finding: the error a dead child's orphaned
    /// waiters receive now names the server AND carries what it printed on the
    /// way out.
    #[tokio::test]
    async fn a_dead_child_reports_its_stderr_tail_to_every_orphaned_waiter() {
        let (stdout_reader, stdout_writer) = tokio::io::duplex(1024);
        let (stderr_reader, mut stderr_writer) = tokio::io::duplex(1024);

        let tail = StderrTail::default();
        let drain = tokio::spawn(drain_stderr(stderr_reader, tail.clone()));
        stderr_writer
            .write_all(b"Traceback (most recent call last):\nfatal: DATABASE_URL is unset\n")
            .await
            .expect("write stderr");
        drop(stderr_writer); // the child's stderr pipe closes with it

        let pending: Pending = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let (tx, rx) = oneshot::channel();
        lock_pending(&pending).insert(1, tx);

        drop(stdout_writer); // …and so does its stdout: EOF, connection dead
        let reader = tokio::spawn(read_loop(
            stdout_reader,
            pending.clone(),
            closed.clone(),
            "diesrv".to_string(),
            tail,
            Some(drain),
        ));

        let err = tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .expect("the reader must fail the waiter well inside the timeout")
            .expect("the waiter is answered, not dropped")
            .expect_err("a dead connection is an error");
        reader.abort();

        let msg = err.user_message();
        assert!(
            matches!(err, McpError::Closed(_)),
            "a dead child is a closed connection: {msg}"
        );
        assert!(msg.contains("diesrv"), "names the server: {msg}");
        assert!(
            msg.contains("closed the connection before responding"),
            "keeps the shape operators already know: {msg}"
        );
        assert!(
            msg.contains("fatal: DATABASE_URL is unset"),
            "and now says WHY it died: {msg}"
        );
        assert!(
            msg.contains("Traceback (most recent call last):"),
            "the whole retained tail rides along, not just the last line: {msg}"
        );
        assert!(
            closed.load(Ordering::SeqCst),
            "the transport is marked dead"
        );
    }
}
