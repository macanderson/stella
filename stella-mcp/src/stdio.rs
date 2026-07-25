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
//!    Dropping a `request` future (a caller-side timeout) leaves its slot in
//!    the map until the matching response arrives or the connection is drained
//!    by EOF/`close`; the sender is then discarded harmlessly.
//!
//! Server stderr is discarded (`Stdio::null`) so a server that logs to stderr
//! cannot corrupt the JSON-RPC framing on stdout. Non-JSON lines that *do*
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
use crate::protocol::{JsonRpcMessage, JsonRpcNotification, JsonRpcRequest};
use crate::transport::Transport;

/// How long `close()` waits for a clean exit after closing stdin before it
/// resorts to SIGKILL.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

/// The largest single stdout line the reader will buffer. An MCP server is
/// untrusted input: an unbounded line read grows one buffer until the
/// allocator gives up, so a server that streams bytes and never sends a
/// newline would be an out-of-memory kill of *stella*, not a failed call. The
/// JSON-RPC frames this transport sends and expects are small; 8 MiB is far
/// past any plausible one and matches the bound `sse.rs` puts on an
/// unterminated event.
const MAX_LINE_BYTES: u64 = 8 * 1024 * 1024;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, McpError>>>>>;

/// A live stdio connection to one MCP server.
pub struct StdioTransport {
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Option<Child>>,
    pending: Pending,
    next_id: AtomicU64,
    closed: Arc<AtomicBool>,
    reader: Mutex<Option<JoinHandle<()>>>,
    server_name: String,
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
            .stderr(Stdio::null()) // keep server logs off the JSON-RPC stream.
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

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let reader = tokio::spawn(read_loop(stdout, pending.clone(), closed.clone()));

        Ok(Self {
            stdin: Mutex::new(Some(stdin)),
            child: Mutex::new(Some(child)),
            pending,
            next_id: AtomicU64::new(1),
            closed,
            reader: Mutex::new(Some(reader)),
            server_name: server_name.to_string(),
        })
    }

    fn closed_error(&self) -> McpError {
        McpError::Closed(format!("server `{}` is not connected", self.server_name))
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
        self.pending.lock().await.insert(id, tx);

        let request = JsonRpcRequest::new(id, method, params);
        let line = encode_line(&request)?;

        // Write under the stdin lock; on any write failure, reclaim the
        // pending slot so it never leaks.
        if let Err(e) = self.write_line(&line).await {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }

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
        let mut pending = self.pending.lock().await;
        for (_id, tx) in pending.drain() {
            let _ = tx.send(Err(McpError::Closed(format!(
                "server `{}` connection was closed before the response arrived",
                self.server_name
            ))));
        }
        Ok(())
    }
}

impl StdioTransport {
    async fn write_line(&self, line: &str) -> Result<(), McpError> {
        let mut guard = self.stdin.lock().await;
        let stdin = guard.as_mut().ok_or_else(|| self.closed_error())?;
        stdin.write_all(line.as_bytes()).await.map_err(|e| {
            McpError::Transport(format!("write to `{}` failed: {e}", self.server_name))
        })?;
        stdin.flush().await.map_err(|e| {
            McpError::Transport(format!("flush to `{}` failed: {e}", self.server_name))
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
async fn read_loop<R: AsyncRead + Unpin>(stdout: R, pending: Pending, closed: Arc<AtomicBool>) {
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
            && let Some(tx) = pending.lock().await.remove(&id)
        {
            let _ = tx.send(message.into_result());
        }
    }

    closed.store(true, Ordering::SeqCst);
    let mut map = pending.lock().await;
    for (_id, tx) in map.drain() {
        let _ = tx.send(Err(McpError::Closed(
            "server closed the connection before responding".into(),
        )));
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

    /// Drive `read_loop` over an in-memory pipe with one pending waiter, feed
    /// it `lines`, and return what (if anything) the waiter for `id` received.
    async fn route_lines(id: u64, lines: Vec<Vec<u8>>) -> Option<Result<Value, McpError>> {
        let (reader_side, mut writer_side) = tokio::io::duplex(64 * 1024);
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let closed = Arc::new(AtomicBool::new(false));
        let (tx, rx) = oneshot::channel();
        pending.lock().await.insert(id, tx);

        let reader = tokio::spawn(read_loop(reader_side, pending.clone(), closed.clone()));
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
}
