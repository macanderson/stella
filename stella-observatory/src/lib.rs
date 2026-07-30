// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! The Stella Observatory — a local, loopback-only dashboard over the
//! workspace's own telemetry (`.stella/private/store.db`, `.stella/private/fleet.db`).
//!
//! Design constraints, in order:
//!
//! 1. **No phone-home, structurally.** The listener binds `127.0.0.1` and
//!    nothing here constructs an outbound connection. The page is a single
//!    embedded HTML file with zero external references (no CDN, no fonts,
//!    no analytics) — it renders fully offline.
//! 2. **Observer never mutates.** Every database open is
//!    `SQLITE_OPEN_READ_ONLY` (see the `db` module); a live `stella` session
//!    writing telemetry is never blocked or altered by a dashboard tab —
//!    `stella-store` keeps these files in WAL mode, so a reader and the
//!    writing session never contend for the same lock.
//! 3. **No new dependencies.** The HTTP layer is a deliberately tiny
//!    GET-only HTTP/1.1 responder over `tokio`'s `TcpListener` — the
//!    workspace already ships everything required. A router the size of a
//!    web framework would be bloat for a handful of read-only JSON routes.
//!
//! The server speaks just enough HTTP for every browser: request line +
//! headers (discarded beyond the path), `Connection: close`, explicit
//! `Content-Length`.
//!
//! One route breaks that shape on purpose. `/api/v1/live` is a Server-Sent
//! Events subscription — `keep-alive`, no `Content-Length`, open for as long
//! as the tab is — because the alternative was a page that re-ran twelve
//! full-history aggregates every five seconds and was still five seconds
//! behind. Catching a degrading agent is the job; five seconds is the wrong
//! order of magnitude for it. Being the only long-lived response here has
//! consequences for the read timeout and the connection semaphore that are
//! documented on each, and the transport itself lives in the `live` module.
//!
//! Every `/api/*` route accepts `?project=<id>`: the id is resolved against
//! the cross-project rollup's `projects` table (the `global` module) and, when
//! known, that project's workspace root replaces the serving root for the request —
//! the dashboard's project switcher. Unknown ids fall back to the serving
//! workspace rather than erroring, so a stale dropdown never breaks the page.

mod accept;
mod codegraph;
mod db;
mod fsview;
mod global;
mod live;

use accept::{AcceptAction, AcceptBackoff};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub use db::{DbError, Observatory};

/// The dashboard page, embedded so the binary is self-contained.
const INDEX_HTML: &str = include_str!("assets/index.html");
/// The Stella mark, served for the favicon.
const MARK_SVG: &str = include_str!("assets/mark.svg");
/// The Stella wordmark, served for the header lockup.
const WORDMARK_SVG: &str = include_str!("assets/wordmark.svg");

/// How long a peer has to deliver a complete request head.
///
/// Without it, a connection that opens and then says nothing occupies a task
/// forever. Ten seconds is generous for a request head from a browser on
/// loopback.
///
/// It covers **reading the head only**, never the response. That distinction
/// used to be invisible — every route answered once and closed, so wrapping
/// the whole exchange was equivalent — and became load-bearing the moment
/// `/api/v1/live` existed: a ten-second cap on a healthy SSE stream would
/// sever it on a timer. The hazard this guards against (a peer that connects
/// and says nothing) ends when the head arrives, so that is where the timeout
/// ends too.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// **One-shot** connections served at once.
///
/// A semaphore is safe here in a way it is not in `stella-serve` because
/// every request it bounds answers once and closes: a permit is held for one
/// request rather than for a whole turn, so bounding connections cannot
/// deadlock a protocol against itself. 64 is far past what one dashboard page
/// opens while keeping the blocking-pool fan-out bounded.
///
/// That reasoning is exactly why the live SSE stream is **not** counted here.
/// An SSE connection is held open for as long as a tab is, so a handful of
/// them on this semaphore would hold permits indefinitely and starve every
/// one-shot route behind them — the page would hang while its own stream was
/// perfectly healthy. Streams carry their own separate, smaller budget in
/// the `live` module. Do not merge the two.
const MAX_LIVE_CONNECTIONS: usize = 64;

/// Sent on every response, making the dashboard's zero-external-reference
/// guarantee enforceable by the browser rather than only by a test: nothing
/// may be fetched from, or connected to, any origin but this one.
///
/// `'unsafe-inline'` is unavoidable on both script and style — the embedded
/// page is a single document with one inline `<script>`, one inline
/// `<style>`, and ~54 inline `style="…"` attributes. Dropping it from
/// `style-src` would silently strip the layout. `frame-ancestors 'none'`
/// complements the existing Host check: a rebound page cannot frame the
/// dashboard either.
const CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; \
     style-src 'self' 'unsafe-inline'; img-src 'self'; connect-src 'self'; \
     base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Errors starting or running the observatory server.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The loopback listener could not be created (port in use, etc.).
    #[error("cannot bind 127.0.0.1:{port}: {source}")]
    Bind {
        port: u16,
        #[source]
        source: std::io::Error,
    },
    /// Accepting a connection failed fatally.
    #[error("accept failed: {0}")]
    Accept(#[from] std::io::Error),
}

/// A minimal HTTP response: status line, content type, body bytes.
#[derive(Debug)]
pub struct Response {
    pub status: &'static str,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl Response {
    fn json(value: serde_json::Value) -> Self {
        Self {
            status: "200 OK",
            content_type: "application/json",
            body: value.to_string().into_bytes(),
        }
    }

    fn error(status: &'static str, message: &str) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: serde_json::json!({ "error": message })
                .to_string()
                .into_bytes(),
        }
    }
}

/// The live SSE subscription. Handled before the one-shot router because it
/// is the one route that does not answer once and close — see the `live`
/// module for why it also needs its own connection budget.
const LIVE_ROUTE: &str = "/api/v1/live";

/// The path with any query string removed.
fn route_of(path: &str) -> &str {
    path.split_once('?').map_or(path, |(route, _)| route)
}

/// Route a request path to a response. Pure function of (workspace, path) —
/// the unit tests drive this directly, no sockets involved.
///
/// Every route here answers exactly once. The live SSE subscription
/// (`/api/v1/live`) is deliberately absent: it is a stream, it is dispatched
/// before this function is reached, and giving it a `Response` here would
/// mean buffering an endless one.
#[must_use]
pub fn respond(workspace_root: &Path, path: &str) -> Response {
    let (route, query) = match path.split_once('?') {
        Some((r, q)) => (r, Some(q)),
        None => (path, None),
    };
    // `?project=<id>` re-points the whole request at another registered
    // workspace (resolved from the rollup's own table — never a raw path
    // from the client). Unknown or vanished projects fall back to the
    // serving workspace.
    let effective_root = query_param(query, "project")
        .and_then(|id| global::resolve_project_root(&id))
        .unwrap_or_else(|| workspace_root.to_path_buf());
    let root = effective_root.as_path();
    let obs = Observatory::new(root);
    let result = match route {
        "/" | "/index.html" => {
            return Response {
                status: "200 OK",
                content_type: "text/html; charset=utf-8",
                body: INDEX_HTML.as_bytes().to_vec(),
            };
        }
        "/assets/mark.svg" | "/favicon.svg" => {
            return Response {
                status: "200 OK",
                content_type: "image/svg+xml",
                body: MARK_SVG.as_bytes().to_vec(),
            };
        }
        "/assets/wordmark.svg" => {
            return Response {
                status: "200 OK",
                content_type: "image/svg+xml",
                body: WORDMARK_SVG.as_bytes().to_vec(),
            };
        }
        "/api/meta" => Ok(obs.meta()),
        // The live plane's two one-shot faces. `/api/v1/cursor` is the
        // change fingerprint and `/api/v1/snapshot` the in-flight slice —
        // the same data [`LIVE_ROUTE`] pushes, reachable by plain GET. They
        // exist so the dashboard degrades to polling when SSE is refused or
        // unavailable, and so anything scripting against this (a CI monitor,
        // a status bar) can ask without holding a socket open.
        "/api/v1/cursor" => obs.cursor(),
        "/api/v1/snapshot" => obs.live(),
        "/api/overview" => obs.overview(),
        "/api/executions" => obs.executions(),
        "/api/execution" => match query_param(query, "id").and_then(|v| v.parse::<i64>().ok()) {
            Some(id) => obs.execution(id),
            None => return Response::error("400 Bad Request", "missing ?id=<execution id>"),
        },
        "/api/models" => obs.models(),
        "/api/tools" => obs.tools(),
        "/api/files" => obs.files(),
        "/api/memory" => obs.memory(),
        "/api/mcp" => obs.mcp(),
        "/api/fleet" => obs.fleet(),
        "/api/activity" => obs.activity(),
        "/api/projects" => Ok(global::projects(workspace_root)),
        // Hub telemetry is inherently cross-project: like `/api/projects` it
        // runs against the original `workspace_root`, ignoring `?project=`.
        // `?org=/workspace=/repo=` narrow the scope and step the drill in.
        "/api/hub-telemetry" => {
            let org = query_param(query, "org");
            let workspace = query_param(query, "workspace");
            let repo = query_param(query, "repo");
            Ok(global::hub_telemetry(
                workspace_root,
                org.as_deref(),
                workspace.as_deref(),
                repo.as_deref(),
            ))
        }
        "/api/codegraph" => Ok(codegraph::snapshot(root)),
        "/api/skills" => Ok(fsview::skills(root)),
        "/api/mcp-servers" => Ok(fsview::mcp_servers(root)),
        "/api/config" => Ok(fsview::config(root)),
        "/api/memories" => Ok(fsview::memories(root)),
        "/api/explorations" => Ok(fsview::explorations(root)),
        "/api/rules" => obs.rules().map(|db| {
            serde_json::json!({
                "db": db,
                "files": fsview::rules_files(root),
            })
        }),
        "/api/reflections" => obs.reflection_ratings().map(|ratings| {
            serde_json::json!({
                "lessons": fsview::lessons(root),
                "ratings": ratings,
            })
        }),
        _ => return Response::error("404 Not Found", "no such route"),
    };
    match result {
        Ok(value) => Response::json(value),
        // Ruling (#615): the 500 body **keeps its detail**. The alternative —
        // a generic string, with the real error logged — assumes an audience
        // split between "the attacker who sees the response" and "the operator
        // who reads the log". Here there is no such split: the listener is
        // loopback-only, read-only, and Host-gated against DNS rebinding, so
        // the only party who can read this body is the person whose workspace
        // it describes. Redacting would make the dashboard undiagnosable for
        // its sole reader while denying an attacker who cannot reach it.
        Err(e) => Response::error("500 Internal Server Error", &e.to_string()),
    }
}

/// Pull one `key=value` pair out of a query string, percent-decoded.
///
/// The page builds these with `encodeURIComponent`, so a scope value holding
/// a space, `&`, or `/` arrives escaped and has to be decoded before it is
/// compared against the stored value — otherwise `?repo=my%20repo` drills
/// into an empty scope. A value that is empty before *or* after decoding is
/// absent, matching what an unset filter sends.
fn query_param(query: Option<&str>, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    query?
        .split('&')
        .find_map(|pair| pair.strip_prefix(prefix.as_str()))
        .filter(|v| !v.is_empty())
        .map(percent_decode)
        .filter(|v| !v.is_empty())
}

/// Decode one query-component value: `%XX` escapes, and `+` as a space (the
/// form-encoding convention, valid only in the query component).
///
/// A malformed escape (`%`, `%A`, `%ZZ`) is left literal rather than dropped
/// or panicked on — this parses attacker-reachable bytes, so it must have no
/// failure mode. Decoding runs over bytes and is only then re-validated as
/// UTF-8, so a multi-byte character split across several escapes reassembles;
/// genuinely invalid bytes become U+FFFD instead of an error.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                match (hi, lo) {
                    (Some(hi), Some(lo)) => {
                        out.push((hi * 16 + lo) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Bind the observatory on `127.0.0.1:port` and serve until the process
/// exits. `port` 0 picks a free port. Calls `on_ready` once with the bound
/// address (the CLI prints the URL from it).
pub async fn serve(
    workspace_root: PathBuf,
    port: u16,
    on_ready: impl FnOnce(SocketAddr),
) -> Result<(), ServeError> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|source| ServeError::Bind { port, source })?;
    let addr = listener.local_addr().map_err(ServeError::Accept)?;
    on_ready(addr);
    let connections = Arc::new(tokio::sync::Semaphore::new(MAX_LIVE_CONNECTIONS));
    let mut backoff = AcceptBackoff::new();
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => {
                backoff.succeeded();
                stream
            }
            Err(err) => match accept::classify(&err) {
                // A dead pending connection or an interrupted syscall: the
                // listener is fine, and this cannot spin (see `accept`).
                AcceptAction::Retry => {
                    backoff.succeeded();
                    continue;
                }
                // Exhaustion, or a condition `io::ErrorKind` cannot name. Sleep
                // so it cannot busy-loop, and give up if it never clears.
                AcceptAction::Backoff => match backoff.next_delay() {
                    Some(delay) => {
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    None => return Err(ServeError::Accept(err)),
                },
                AcceptAction::Fatal => return Err(ServeError::Accept(err)),
            },
        };
        // Bounded concurrency. Unlike `stella-serve`, nothing here is a
        // long-lived stream — every response is one shot and closes — so a
        // permit is held for a single request and a semaphore cannot starve a
        // protocol against itself. A connection that cannot get a permit waits
        // rather than being refused: the dashboard is a local tool, and a brief
        // queue is a better answer than an error the page has to render.
        let permit = Arc::clone(&connections)
            .acquire_owned()
            .await
            .expect("connection semaphore is never closed");
        let root = workspace_root.clone();
        tokio::spawn(async move {
            // Per-connection errors (bad request line, client hangup) only
            // affect that connection; the accept loop keeps serving.
            let _ = handle(stream, &root).await;
            drop(permit);
        });
    }
}

/// True when the request's `Host` header names a loopback address. Any other
/// Host (e.g. an attacker domain rebound to 127.0.0.1) is refused — the standard
/// DNS-rebinding defense for a header-less localhost server. A missing Host is
/// allowed: a browser `fetch` always sends one, so its absence means the request
/// did not originate from the web attack this guards against (raw curl, tests).
fn host_is_local(head: &str) -> bool {
    let host = head.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.trim().eq_ignore_ascii_case("host") {
            Some(value.trim())
        } else {
            None
        }
    });
    let Some(h) = host else {
        return true;
    };
    // Strip an optional :port, keeping bracketed IPv6 literals intact.
    let hostname = if let Some(rest) = h.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        h.rsplit_once(':').map(|(hn, _)| hn).unwrap_or(h)
    };
    // Parse, never prefix-match: `127.0.0.1.attacker.example` is a registrable
    // name that satisfies a `starts_with("127.")` test. `localhost` stays an
    // explicit arm because it is a name, not an address.
    hostname
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
        || hostname == "localhost"
}

/// Read one request head, answer it, close. GET only, 8 KiB head cap,
/// [`READ_TIMEOUT`] to deliver it.
///
/// The timeout covers **reading the head only**, never the response. It used
/// to wrap the whole exchange, which was harmless while every route answered
/// once and closed — and became a bug the moment `/api/v1/live` existed,
/// because a ten-second cap on a healthy stream would sever it on a timer.
/// What the timeout is actually for is a peer that opens a socket and says
/// nothing; that hazard ends when the head arrives.
async fn handle(mut stream: TcpStream, workspace_root: &Path) -> std::io::Result<()> {
    let head = match tokio::time::timeout(READ_TIMEOUT, read_head(&mut stream)).await {
        Ok(result) => result?,
        // The peer never finished its head. Dropping the connection is the whole
        // remedy: there is nobody to tell, because a client that cannot send a
        // request head in ten seconds is not waiting for a status code.
        Err(_elapsed) => return Ok(()),
    };
    respond_to_head(stream, workspace_root, head).await
}

/// One request head, and whether its terminator arrived inside the cap.
struct RequestHead {
    text: String,
    complete: bool,
}

async fn read_head(stream: &mut TcpStream) -> std::io::Result<RequestHead> {
    let mut buf = vec![0_u8; 8192];
    let mut read = 0;
    let mut head_complete = false;
    // Read until the end of the request head (or the cap — a GET with no
    // body never legitimately exceeds it).
    while read < buf.len() {
        let n = stream.read(&mut buf[read..]).await?;
        if n == 0 {
            break;
        }
        // Scan only what this read added, plus the three bytes before it that
        // a terminator could straddle. Re-scanning `buf[..read]` every time
        // made a byte-at-a-time client cost O(cap²) comparisons — 67M for one
        // connection that never terminates its head, which is free CPU for
        // anything local that can open a socket.
        let scan_from = read.saturating_sub(3);
        read += n;
        if buf[scan_from..read].windows(4).any(|w| w == b"\r\n\r\n") {
            head_complete = true;
            break;
        }
    }
    Ok(RequestHead {
        text: String::from_utf8_lossy(&buf[..read]).into_owned(),
        complete: head_complete,
    })
}

/// Route one already-read head and write its answer.
async fn respond_to_head(
    mut stream: TcpStream,
    workspace_root: &Path,
    head: RequestHead,
) -> std::io::Result<()> {
    let RequestHead {
        text: head,
        complete: head_complete,
    } = head;
    let mut parts = head.split_whitespace();
    let (method, path) = match (parts.next(), parts.next()) {
        (Some(m), Some(p)) => (m, p),
        _ => return Ok(()),
    };
    // The live stream is the one route that does not answer once and close,
    // so it is dispatched before the one-shot response is built. It is gated
    // by the same two checks as everything else first — a truncated head or a
    // rebound Host must not buy a long-lived subscription to the workspace's
    // telemetry any more than it buys a single read of it.
    if head_complete && host_is_local(&head) && method == "GET" && route_of(path) == LIVE_ROUTE {
        let root = query_param(path.split_once('?').map(|(_, query)| query), "project")
            .and_then(|id| global::resolve_project_root(&id))
            .unwrap_or_else(|| workspace_root.to_path_buf());
        return live::serve_stream(stream, Arc::new(Observatory::new(&root)), CSP).await;
    }
    let response = if !head_complete {
        // A head with no terminator inside the cap is refused *before* routing.
        // Otherwise it is a hole straight through the rebinding gate below: pad
        // the request line past 8 KiB (`/api/executions?pad=aaa…`) and `Host`
        // never lands in the buffer at all, so `host_is_local`'s "no Host means
        // no browser" allowance would wave the rebound request through while the
        // route still parses out of the truncated path.
        Response::error(
            "431 Request Header Fields Too Large",
            "request head exceeded the 8 KiB cap before its terminator",
        )
    } else if !host_is_local(&head) {
        // DNS-rebinding defense: a web page that resolves an attacker domain to
        // 127.0.0.1 can otherwise read this loopback dashboard cross-origin
        // (prompts, touched-file paths, memory, code graph). A rebound request
        // carries the attacker's hostname in Host; refuse anything non-loopback.
        Response::error("403 Forbidden", "forbidden Host header")
    } else if method == "GET" {
        // `respond` opens SQLite and walks the workspace tree — blocking work
        // that would otherwise hold a reactor worker for the whole query, and
        // the dashboard polls. The semaphore above bounds how many of these can
        // be on the blocking pool at once.
        let root = workspace_root.to_path_buf();
        let route = path.to_string();
        tokio::task::spawn_blocking(move || respond(&root, &route))
            .await
            .unwrap_or_else(|_| {
                Response::error("500 Internal Server Error", "dashboard query failed")
            })
    } else {
        Response::error("405 Method Not Allowed", "GET only")
    };
    // Named apart from the request `head` above — same word, opposite direction.
    let response_head = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: {CSP}\r\nConnection: close\r\n\r\n",
        response.status,
        response.content_type,
        response.body.len(),
    );
    stream.write_all(response_head.as_bytes()).await?;
    stream.write_all(&response.body).await?;
    stream.shutdown().await
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod lib_tests;
