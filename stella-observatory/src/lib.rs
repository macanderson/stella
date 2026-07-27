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
/// How long a peer has to deliver a complete request head.
///
/// Without it, a connection that opens and then says nothing occupies a task
/// forever. Ten seconds is generous for a request head from a browser on
/// loopback, and the dashboard has no long-lived response for this to endanger
/// — every route answers once and closes.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Connections served at once.
///
/// A semaphore is safe here in a way it is not in `stella-serve`: nothing the
/// observatory serves is a long-lived stream, so a permit is held for one
/// request rather than for a whole turn, and bounding connections cannot
/// deadlock a protocol against itself. 64 is far past what one dashboard page
/// opens while keeping the blocking-pool fan-out bounded.
const MAX_LIVE_CONNECTIONS: usize = 64;

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

/// Route a request path to a response. Pure function of (workspace, path) —
/// the unit tests drive this directly, no sockets involved.
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
async fn handle(stream: TcpStream, workspace_root: &Path) -> std::io::Result<()> {
    match tokio::time::timeout(READ_TIMEOUT, handle_inner(stream, workspace_root)).await {
        Ok(result) => result,
        // The peer never finished its head. Dropping the connection is the whole
        // remedy: there is nobody to tell, because a client that cannot send a
        // request head in ten seconds is not waiting for a status code.
        Err(_elapsed) => Ok(()),
    }
}

async fn handle_inner(mut stream: TcpStream, workspace_root: &Path) -> std::io::Result<()> {
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
    let head = String::from_utf8_lossy(&buf[..read]);
    let mut parts = head.split_whitespace();
    let (method, path) = match (parts.next(), parts.next()) {
        (Some(m), Some(p)) => (m, p),
        _ => return Ok(()),
    };
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
mod tests {
    use rusqlite::Connection;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn host_header_gates_dns_rebinding() {
        let local = [
            "GET /api/executions HTTP/1.1\r\nHost: 127.0.0.1:7787\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: localhost:7787\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
            "GET / HTTP/1.1\r\nhost: [::1]:7787\r\n\r\n",
            "GET / HTTP/1.1\r\n\r\n", // no Host (raw socket) — allowed
        ];
        for h in local {
            assert!(host_is_local(h), "should allow: {h:?}");
        }
        let remote = [
            "GET / HTTP/1.1\r\nHost: attacker.example:7787\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: evil.com\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: 127evil.com\r\n\r\n",
            // A registrable name that a prefix test on "127." waves through.
            "GET / HTTP/1.1\r\nHost: 127.0.0.1.attacker.example\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: 127.0.0.1.attacker.example:7787\r\n\r\n",
        ];
        for h in remote {
            assert!(!host_is_local(h), "should refuse: {h:?}");
        }
    }

    /// Build a workspace with a seeded `.stella/private/store.db` shaped like the
    /// real schema (the subset the observatory reads).
    ///
    /// A hand-written *subset*, not a copy, and already a divergent one:
    /// `stella-store`'s shipped DDL also carries `executions.session_id`,
    /// `usage_complete` and `usage_status`, and `telemetry.call_role` and
    /// `usage_complete` — columns no query in this crate selects. Nothing
    /// checks the two schemas against each other, so a column this crate *does*
    /// read, renamed in `stella-store`, keeps this suite green and 500s the
    /// dashboard at runtime. Every INSERT below therefore names its columns:
    /// the fixture must survive the store growing another one, not silently
    /// depend on the column order of the day.
    fn seeded_workspace() -> TempDir {
        let dir = TempDir::new().unwrap();
        let dot = dir.path().join(".stella");
        std::fs::create_dir_all(dot.join("private")).unwrap();
        let conn = Connection::open(dot.join("private/store.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE executions (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               kind TEXT NOT NULL, prompt TEXT NOT NULL,
               provider TEXT NOT NULL, model TEXT NOT NULL,
               started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               finished_at TEXT, outcome TEXT,
               cost_usd REAL NOT NULL DEFAULT 0);
             CREATE TABLE telemetry (
               execution_id INTEGER NOT NULL, step INTEGER NOT NULL,
               ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               provider TEXT NOT NULL, model TEXT NOT NULL,
               input_tokens INTEGER NOT NULL,
               estimated_input_tokens INTEGER NOT NULL DEFAULT 0,
               output_tokens INTEGER NOT NULL,
               cache_read_tokens INTEGER NOT NULL,
               cache_miss_tokens INTEGER NOT NULL,
               cache_write_tokens INTEGER NOT NULL DEFAULT 0,
               cost_usd REAL NOT NULL, duration_ms INTEGER NOT NULL,
               retries INTEGER NOT NULL, tool_calls INTEGER NOT NULL);
             CREATE TABLE tool_calls (
               execution_id INTEGER NOT NULL, seq INTEGER NOT NULL,
               call_id TEXT NOT NULL DEFAULT '', name TEXT NOT NULL,
               surface TEXT NOT NULL DEFAULT 'native',
               args_json TEXT NOT NULL DEFAULT '{}',
               args_digest TEXT NOT NULL DEFAULT '',
               reason TEXT NOT NULL DEFAULT '',
               ok INTEGER NOT NULL DEFAULT 1,
               error TEXT NOT NULL DEFAULT '',
               bytes_out INTEGER NOT NULL DEFAULT 0,
               duration_ms INTEGER NOT NULL DEFAULT 0,
               ts TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             CREATE TABLE files_touched (
               execution_id INTEGER NOT NULL, path TEXT NOT NULL,
               ops TEXT NOT NULL, lines_added INTEGER NOT NULL DEFAULT 0,
               lines_removed INTEGER NOT NULL DEFAULT 0,
               events TEXT NOT NULL DEFAULT '[]');
             INSERT INTO executions
               (kind, prompt, provider, model, outcome, cost_usd)
             VALUES
               ('run', 'add a function', 'zai', 'glm-5.2', 'completed', 0.03),
               ('goal', 'make tests pass', 'local', 'llama', 'goal_unmet', 0.0);
             INSERT INTO telemetry
               (execution_id, step, ts, provider, model, input_tokens,
                estimated_input_tokens, output_tokens, cache_read_tokens,
                cache_miss_tokens, cache_write_tokens, cost_usd, duration_ms,
                retries, tool_calls)
             VALUES
               (1, 1, CURRENT_TIMESTAMP, 'zai', 'glm-5.2',
                1000, 0, 200, 400, 600, 50, 0.03, 1500, 0, 2),
               (2, 1, CURRENT_TIMESTAMP, 'local', 'llama',
                2000, 0, 100, 0, 2000, 0, 0.0, 900, 1, 1);
             INSERT INTO tool_calls
               (execution_id, seq, name, ok, error, bytes_out, duration_ms)
             VALUES
               (1, 1, 'read_file', 1, '', 2048, 12),
               (1, 2, 'edit_file', 1, '', 64, 3),
               (2, 1, 'bash', 0, 'exit 1', 0, 40);
             INSERT INTO files_touched VALUES
               (1, 'src/lib.rs', 'RU', 4, 1, '[]');
             CREATE TABLE execution_reflection (
               execution_id INTEGER PRIMARY KEY,
               prompt TEXT NOT NULL DEFAULT '',
               delivered INTEGER, self_rating INTEGER,
               what_went_well TEXT NOT NULL DEFAULT '',
               what_to_improve TEXT NOT NULL DEFAULT '',
               critique TEXT NOT NULL DEFAULT '',
               produced_output INTEGER NOT NULL DEFAULT 0,
               wrote_files INTEGER NOT NULL DEFAULT 0,
               truncated INTEGER NOT NULL DEFAULT 0,
               recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             INSERT INTO execution_reflection
               (execution_id, delivered, self_rating, what_to_improve)
             VALUES (1, 1, 8, 'read the failing test first');
             CREATE TABLE rules (
               rule_id TEXT PRIMARY KEY, contents TEXT NOT NULL,
               source TEXT NOT NULL,
               created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
               updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
             INSERT INTO rules (rule_id, contents, source)
             VALUES ('no-vacuous-fixes', 'a fix must change production code', 'reflection');",
        )
        .unwrap();
        dir
    }

    /// Layer the filesystem-backed surfaces (skills, memories, rules,
    /// lessons, mcp.toml, settings.json, codegraph.db) onto a seeded
    /// workspace.
    fn seed_fs_surfaces(dir: &TempDir) {
        let dot = dir.path().join(".stella");
        std::fs::create_dir_all(dot.join("skills/my-skill")).unwrap();
        std::fs::write(
            dot.join("skills/my-skill/SKILL.md"),
            "---\nname: my-skill\ndescription: does the thing\n---\n# body",
        )
        .unwrap();
        std::fs::create_dir_all(dot.join("skills/learned")).unwrap();
        std::fs::write(
            dot.join("skills/learned/hard-won.md"),
            "---\nname: hard-won\ndescription: extracted from a failure\n---\n# lesson",
        )
        .unwrap();
        std::fs::create_dir_all(dot.join("memories")).unwrap();
        std::fs::write(
            dot.join("memories/build-quirk.md"),
            "The build needs -p flags.",
        )
        .unwrap();
        std::fs::create_dir_all(dot.join("rules")).unwrap();
        std::fs::write(dot.join("rules/style.md"), "Prefer witness tests.").unwrap();
        std::fs::write(
            dot.join("private/reflections.jsonl"),
            r#"{"lesson":"check the test's invariant first","domains":["testing"],"occurred_at":1700000000}
{"lesson":"verify dependency chains before citing them","domains":["docs"],"occurred_at":1700000100}
"#,
        )
        .unwrap();
        std::fs::write(
            dot.join("mcp.toml"),
            "[servers.github]\ntransport = \"stdio\"\ncmd = \"gh-mcp\"\nargs = [\"--stdio\"]\n\
             [servers.github.env]\nGITHUB_TOKEN = \"ghp_supersecret\"\n",
        )
        .unwrap();
        std::fs::write(
            dot.join("settings.json"),
            r#"{"providers":{"zai":{"api_key":"sk-live-topsecret","api_key_env":"ZAI_KEY"}},"agent_engine_config":{"agents":{"judge":{"model":"glm-5.2"}}}}"#,
        )
        .unwrap();
        let graph = Connection::open(dot.join("private/codegraph.db")).unwrap();
        graph
            .execute_batch(
                "CREATE TABLE code_graph_files (
                   id INTEGER PRIMARY KEY, path TEXT NOT NULL UNIQUE,
                   language TEXT NOT NULL, content_sha256 TEXT NOT NULL,
                   mtime_ns INTEGER NOT NULL, indexed_at INTEGER NOT NULL);
                 CREATE TABLE code_graph_symbols (
                   id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL,
                   name TEXT NOT NULL, kind TEXT NOT NULL,
                   start_line INTEGER NOT NULL, end_line INTEGER NOT NULL);
                 CREATE TABLE code_graph_imports (
                   id INTEGER PRIMARY KEY, from_file_id INTEGER NOT NULL,
                   specifier TEXT NOT NULL, to_path TEXT, kind TEXT NOT NULL);
                 INSERT INTO code_graph_files VALUES
                   (1, 'crate-a/src/lib.rs',  'rust', 'x', 0, 0),
                   (2, 'crate-a/src/util.rs', 'rust', 'x', 0, 0),
                   (3, 'crate-b/src/lib.rs',  'rust', 'x', 0, 0),
                   (4, 'tools/x.py', 'python', 'x', 0, 0),
                   (5, 'tools/y.py', 'python', 'x', 0, 0);
                 INSERT INTO code_graph_symbols VALUES
                   (1, 1, 'run', 'function', 1, 9),
                   (2, 2, 'Util', 'struct', 1, 5);
                 INSERT INTO code_graph_imports VALUES
                   (1, 3, 'crate_a::util::Util', NULL, 'absolute'),
                   (2, 2, 'crate::run',          NULL, 'absolute'),
                   (3, 2, 'std::fs',             NULL, 'absolute'),
                   (4, 4, './y', 'tools/y.py',   'relative');",
            )
            .unwrap();
    }

    #[test]
    fn overview_aggregates_runs_cost_and_tokens() {
        let ws = seeded_workspace();
        let response = respond(ws.path(), "/api/overview");
        let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(v["runs"], 2);
        assert_eq!(v["resolved"], 1);
        assert_eq!(v["input_tokens"], 3000);
        assert_eq!(v["output_tokens"], 300);
        assert_eq!(v["timeline"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn models_mirror_stats_semantics() {
        let ws = seeded_workspace();
        let response = respond(ws.path(), "/api/models");
        let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        // Ordered by cost desc: zai first, then local (off-grid).
        assert_eq!(rows[0]["provider"], "zai");
        assert_eq!(rows[0]["resolve_rate"], 1.0);
        assert_eq!(rows[1]["division"], "off-grid");
        assert_eq!(rows[1]["cost_per_resolved_usd"], serde_json::Value::Null);
    }

    #[test]
    fn execution_detail_includes_steps_tools_files() {
        let ws = seeded_workspace();
        let response = respond(ws.path(), "/api/execution?id=1");
        let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(v["id"], 1);
        assert_eq!(v["steps"].as_array().unwrap().len(), 1);
        assert_eq!(v["tools"].as_array().unwrap().len(), 2);
        assert_eq!(v["files"][0]["path"], "src/lib.rs");
        assert_eq!(v["reflection"]["self_rating"], 8);
        let none = respond(ws.path(), "/api/execution?id=2");
        let v: serde_json::Value = serde_json::from_slice(&none.body).unwrap();
        assert_eq!(
            v["reflection"],
            serde_json::Value::Null,
            "unreflected runs stay null"
        );
    }

    #[test]
    fn tool_leaderboard_counts_errors() {
        let ws = seeded_workspace();
        let response = respond(ws.path(), "/api/tools");
        let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        let bash = v
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "bash")
            .unwrap();
        assert_eq!(bash["calls"], 1);
        assert_eq!(bash["errors"], 1);
    }

    #[test]
    fn empty_workspace_degrades_to_empty_payloads_not_errors() {
        let ws = TempDir::new().unwrap();
        for route in [
            "/api/meta",
            "/api/overview",
            "/api/executions",
            "/api/models",
            "/api/tools",
            "/api/files",
            "/api/memory",
            "/api/mcp",
            "/api/fleet",
            "/api/activity",
            "/api/projects",
            "/api/hub-telemetry",
            "/api/codegraph",
            "/api/skills",
            "/api/mcp-servers",
            "/api/config",
            "/api/memories",
            "/api/explorations",
            "/api/rules",
            "/api/reflections",
        ] {
            let response = respond(ws.path(), route);
            assert_eq!(response.status, "200 OK", "route {route}");
        }
    }

    /// The route table and the embedded page must not drift apart. A route
    /// nothing fetches is dead weight that still drags in its dependencies —
    /// `/api/explorations` sat unconsumed for exactly that long (#640).
    #[test]
    fn every_served_route_is_fetched_by_the_embedded_page() {
        // Route-table arms are `"/api/<name>" => …`; the same literal
        // appearing as a call argument (tests, 404 fixtures) has no `=>`.
        let routes: Vec<&str> = include_str!("lib.rs")
            .lines()
            .filter_map(|line| {
                let rest = line.trim().strip_prefix("\"/api/")?;
                let (route, tail) = rest.split_once('"')?;
                tail.trim_start().starts_with("=>").then_some(route)
            })
            .collect();
        assert!(
            routes.len() >= 19,
            "route table not recognised, only found {routes:?}"
        );
        for route in routes {
            // A bare `contains` lets a shorter route ride on a longer one that
            // starts with it: `/api/mcp` would pass on the strength of
            // `/api/mcp-servers` alone, and `/api/memory` on `/api/memories`,
            // `/api/execution` on `/api/executions`. Require an occurrence
            // whose next character actually ends the path — the page writes
            // `"/api/x"`, `` `/api/x` `` or `/api/x?…`.
            let needle = format!("/api/{route}");
            let fetched = INDEX_HTML.match_indices(needle.as_str()).any(|(at, _)| {
                INDEX_HTML[at + needle.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '-')
            });
            assert!(
                fetched,
                "/api/{route} is served but nothing in the dashboard fetches it: \
                 give it a consumer or delete the route"
            );
        }
    }

    #[test]
    fn activity_rolls_up_runs_tokens_and_tool_calls_by_day() {
        let ws = seeded_workspace();
        let response = respond(ws.path(), "/api/activity");
        let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        let days = v.as_array().unwrap();
        assert_eq!(days.len(), 1, "everything seeded lands on today");
        let d = &days[0];
        assert_eq!(d["runs"], 2);
        assert_eq!(d["resolved"], 1);
        assert_eq!(d["input_tokens"], 3000);
        assert_eq!(d["tool_calls"], 3);
        assert_eq!(d["tool_errors"], 1);
    }

    /// A timestamp `date()` can't parse used to NULL out the group key and
    /// fail the whole rollup — one bad row blanked the dashboard. The work
    /// is real, so it lands in a named bucket that sorts last.
    #[test]
    fn activity_buckets_unparseable_timestamps_instead_of_failing() {
        let ws = seeded_workspace();
        let conn = Connection::open(ws.path().join(".stella/private/store.db")).unwrap();
        conn.execute_batch(
            "INSERT INTO executions
               (kind, prompt, provider, model, started_at, outcome, cost_usd)
             VALUES ('run', 'clock skew', 'zai', 'glm-5.2', 'sometime', 'completed', 0.5);
             INSERT INTO telemetry
               (execution_id, step, ts, provider, model, input_tokens,
                estimated_input_tokens, output_tokens, cache_read_tokens,
                cache_miss_tokens, cache_write_tokens, cost_usd, duration_ms,
                retries, tool_calls)
             VALUES (3, 1, 'sometime', 'zai', 'glm-5.2', 7, 0, 9, 0, 0, 0, 0.5, 11, 0, 1);
             INSERT INTO tool_calls
               (execution_id, seq, name, ok, error, bytes_out, duration_ms, ts)
             VALUES (3, 1, 'bash', 0, 'boom', 0, 4, 'sometime');",
        )
        .unwrap();
        let response = respond(ws.path(), "/api/activity");
        assert_eq!(response.status, "200 OK");
        let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        let days = v.as_array().unwrap();
        assert_eq!(days.len(), 2, "today plus the undated bucket");
        let undated = days.last().unwrap();
        assert_eq!(undated["day"], crate::db::UNDATED);
        assert_eq!(undated["runs"], 1);
        assert_eq!(undated["resolved"], 1);
        assert_eq!(undated["input_tokens"], 7);
        assert_eq!(undated["tool_calls"], 1);
        assert_eq!(undated["tool_errors"], 1);
    }

    #[test]
    fn codegraph_resolves_rust_specifiers_and_relative_imports() {
        let ws = seeded_workspace();
        seed_fs_surfaces(&ws);
        let response = respond(ws.path(), "/api/codegraph");
        let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        let nodes = v["nodes"].as_array().unwrap();
        assert_eq!(nodes.len(), 5);
        assert_eq!(v["total_symbols"], 2);
        let path_of = |i: usize| nodes[i]["path"].as_str().unwrap();
        let edges: Vec<(usize, usize)> = v["edges"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e[0].as_u64().unwrap() as usize,
                    e[1].as_u64().unwrap() as usize,
                )
            })
            .collect();
        let has = |from: &str, to: &str| {
            edges
                .iter()
                .any(|&(a, b)| path_of(a) == from && path_of(b) == to)
        };
        // Cross-crate `crate_a::util::Util`, crate-local `crate::run`, and
        // the indexer-resolved Python relative import.
        assert!(has("crate-b/src/lib.rs", "crate-a/src/util.rs"));
        assert!(has("crate-a/src/util.rs", "crate-a/src/lib.rs"));
        assert!(has("tools/x.py", "tools/y.py"));
        // `std::fs` resolves to nothing.
        assert_eq!(edges.len(), 3);
    }

    #[test]
    fn skills_list_project_scope_and_learned_flags() {
        let ws = seeded_workspace();
        seed_fs_surfaces(&ws);
        let response = respond(ws.path(), "/api/skills");
        let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        let rows = v.as_array().unwrap();
        let find = |name: &str| rows.iter().find(|r| r["name"] == name);
        let skill = find("my-skill").expect("project skill listed");
        assert_eq!(skill["scope"], "project");
        assert_eq!(skill["learned"], false);
        assert_eq!(skill["description"], "does the thing");
        let learned = find("hard-won").expect("learned skill listed");
        assert_eq!(learned["learned"], true);
    }

    #[test]
    fn explorations_route_reports_per_map_freshness() {
        let ws = seeded_workspace();
        let dir = ws.path().join(".stella/explorations");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(ws.path().join("covered.rs"), "fn mapped() {}").unwrap();
        // sha256("fn mapped() {}") — computed with the same encoding the
        // producer uses; freshness must verify against the live file.
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(b"fn mapped() {}");
        let fresh_hash: String = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        std::fs::write(
            dir.join("zone.json"),
            serde_json::json!({
                "slice": "zone", "title": "Zone", "summary": "s", "content": "body",
                "files": ["covered.rs"], "created_at_ms": 2u64,
                "manifest": { "covered.rs": fresh_hash, "gone.rs": "0000" },
                "status": "complete", "pid": 42u32
            })
            .to_string(),
        )
        .unwrap();
        // A pre-manifest (v1) record must report "unknown", not crash.
        std::fs::write(
            dir.join("legacy.json"),
            serde_json::json!({
                "slice": "legacy", "title": "Old", "summary": "s", "content": "b",
                "files": [], "created_at_ms": 1u64
            })
            .to_string(),
        )
        .unwrap();

        let response = respond(ws.path(), "/api/explorations");
        let v: serde_json::Value = serde_json::from_slice(&response.body).unwrap();
        let rows = v.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        // Newest first.
        assert_eq!(rows[0]["slice"], "zone");
        assert_eq!(rows[0]["freshness"], "drifted");
        assert_eq!(rows[0]["missing"][0], "gone.rs");
        assert!(rows[0]["changed"].as_array().unwrap().is_empty());
        assert_eq!(rows[0]["manifest_files"], 2);
        assert_eq!(rows[1]["slice"], "legacy");
        assert_eq!(rows[1]["freshness"], "unknown");
        // Bodies are sized, never served in the listing.
        assert!(rows[0]["content"].is_null());
        assert_eq!(rows[0]["content_chars"], 4);
    }

    #[test]
    fn mcp_servers_expose_names_but_never_credential_values() {
        let ws = seeded_workspace();
        seed_fs_surfaces(&ws);
        let response = respond(ws.path(), "/api/mcp-servers");
        let body = String::from_utf8(response.body).unwrap();
        assert!(body.contains("github"));
        assert!(body.contains("GITHUB_TOKEN"), "env var names are shown");
        assert!(
            !body.contains("ghp_supersecret"),
            "env var values must never be served"
        );
    }

    #[test]
    fn config_serves_scope_chain_with_secrets_redacted() {
        let ws = seeded_workspace();
        seed_fs_surfaces(&ws);
        let response = respond(ws.path(), "/api/config");
        let body = String::from_utf8(response.body).unwrap();
        assert!(!body.contains("sk-live-topsecret"), "api keys are redacted");
        assert!(body.contains("ZAI_KEY"), "env var *names* survive");
        assert!(body.contains("glm-5.2"), "engine config is visible");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        let scopes = v["scopes"].as_array().unwrap();
        assert_eq!(scopes.len(), 3);
        let project = scopes.iter().find(|s| s["scope"] == "project").unwrap();
        assert_eq!(project["exists"], true);
    }

    #[test]
    fn memories_rules_and_reflections_come_from_disk_and_db() {
        let ws = seeded_workspace();
        seed_fs_surfaces(&ws);
        let memories: serde_json::Value =
            serde_json::from_slice(&respond(ws.path(), "/api/memories").body).unwrap();
        assert_eq!(memories[0]["name"], "build-quirk");
        let rules: serde_json::Value =
            serde_json::from_slice(&respond(ws.path(), "/api/rules").body).unwrap();
        assert_eq!(rules["db"][0]["rule_id"], "no-vacuous-fixes");
        assert_eq!(rules["files"][0]["name"], "style");
        let refl: serde_json::Value =
            serde_json::from_slice(&respond(ws.path(), "/api/reflections").body).unwrap();
        assert_eq!(refl["lessons"].as_array().unwrap().len(), 2);
        let ratings = refl["ratings"].as_array().unwrap();
        assert_eq!(ratings.len(), 1);
        assert_eq!(ratings[0]["self_rating"], 8);
        assert_eq!(ratings[0]["what_to_improve"], "read the failing test first");
    }

    /// Serializes the tests that mutate `STELLA_DATA_DIR` — it is process-wide,
    /// so one test's `set_var` racing another's `respond` (which reads it via
    /// `usage_db_path`) would flake. Poison-tolerant: a panicking holder must
    /// not cascade into unrelated failures.
    static DATA_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn project_param_drills_into_another_registered_workspace() {
        let _env = DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Two workspaces: `home` is empty, `other` is seeded. A usage.db in a
        // private STELLA_DATA_DIR registers `other`; ?project= must re-point
        // the request at it, and unknown ids must fall back to `home`.
        let home = TempDir::new().unwrap();
        let other = seeded_workspace();
        let data = TempDir::new().unwrap();
        let usage = Connection::open(data.path().join("usage.db")).unwrap();
        usage
            .execute_batch(&format!(
                "CREATE TABLE projects (
                   project_id TEXT PRIMARY KEY, name TEXT NOT NULL,
                   root_path TEXT NOT NULL, first_seen_at TEXT NOT NULL,
                   last_seen_at TEXT NOT NULL);
                 CREATE TABLE execution_rollup (
                   project_id TEXT NOT NULL, execution_id INTEGER NOT NULL,
                   kind TEXT NOT NULL, prompt_digest TEXT NOT NULL,
                   prompt_preview TEXT NOT NULL DEFAULT '',
                   model TEXT NOT NULL, provider TEXT NOT NULL,
                   outcome TEXT NOT NULL, cost_usd REAL NOT NULL,
                   input_tokens INTEGER NOT NULL, output_tokens INTEGER NOT NULL,
                   duration_ms INTEGER NOT NULL, tool_calls INTEGER NOT NULL,
                   files_written INTEGER NOT NULL, produced_output INTEGER NOT NULL,
                   self_rating INTEGER, started_at TEXT NOT NULL,
                   PRIMARY KEY (project_id, execution_id));
                 INSERT INTO projects VALUES
                   ('feedbeef00000001', 'other', '{}', '2026-01-01', '2026-01-02');",
                other.path().display()
            ))
            .unwrap();
        // SAFETY: env mutation in tests — this is the only test that sets
        // STELLA_DATA_DIR, and every assertion runs before it's removed.
        unsafe { std::env::set_var("STELLA_DATA_DIR", data.path()) };
        let drilled = respond(home.path(), "/api/overview?project=feedbeef00000001");
        let unknown = respond(home.path(), "/api/overview?project=doesnotexist");
        let listed = respond(home.path(), "/api/projects");
        unsafe { std::env::remove_var("STELLA_DATA_DIR") };
        let v: serde_json::Value = serde_json::from_slice(&drilled.body).unwrap();
        assert_eq!(v["runs"], 2, "?project= reads the other workspace");
        let v: serde_json::Value = serde_json::from_slice(&unknown.body).unwrap();
        assert_eq!(v["runs"], 0, "unknown ids fall back to the serving root");
        let v: serde_json::Value = serde_json::from_slice(&listed.body).unwrap();
        assert_eq!(v["available"], true);
        assert_eq!(v["projects"][0]["name"], "other");
        assert_eq!(v["projects"][0]["has_store"], true);
    }

    #[test]
    fn hub_telemetry_aggregates_scope_and_drills_to_project() {
        let _env = DATA_DIR_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = TempDir::new().unwrap();

        // A hub dir with no usage.db is an empty state, never an error.
        let empty = TempDir::new().unwrap();
        unsafe { std::env::set_var("STELLA_DATA_DIR", empty.path()) };
        let none = respond(home.path(), "/api/hub-telemetry");

        // A populated hub spanning an org AND the common never-cloud-registered
        // case (NULL org/workspace, '' repo), plus a row with no recorded_at.
        let data = TempDir::new().unwrap();
        let usage = Connection::open(data.path().join("usage.db")).unwrap();
        usage
            .execute_batch(
                "CREATE TABLE projects (
                   project_id TEXT PRIMARY KEY, name TEXT NOT NULL,
                   root_path TEXT NOT NULL, first_seen_at TEXT NOT NULL,
                   last_seen_at TEXT NOT NULL);
                 CREATE TABLE telemetry (
                   project_id TEXT NOT NULL, source_rowid INTEGER NOT NULL,
                   org_id TEXT, workspace_id TEXT, repo_id TEXT NOT NULL DEFAULT '',
                   execution_id INTEGER NOT NULL, step INTEGER NOT NULL,
                   recorded_at TEXT NOT NULL DEFAULT '',
                   provider TEXT NOT NULL, call_role TEXT NOT NULL, model TEXT NOT NULL,
                   input_tokens INTEGER NOT NULL, estimated_input_tokens INTEGER NOT NULL,
                   output_tokens INTEGER NOT NULL, cache_read_tokens INTEGER NOT NULL,
                   cache_miss_tokens INTEGER NOT NULL, cache_write_tokens INTEGER NOT NULL,
                   cost_usd REAL NOT NULL, duration_ms INTEGER NOT NULL,
                   retries INTEGER NOT NULL, tool_calls INTEGER NOT NULL,
                   usage_complete INTEGER NOT NULL,
                   PRIMARY KEY (project_id, source_rowid));
                 INSERT INTO projects VALUES
                   ('p1','proj-one','/tmp/p1','2026-07-20','2026-07-21'),
                   ('p2','proj-two','/tmp/p2','2026-07-20','2026-07-20'),
                   ('p3','proj-three','/tmp/p3','2026-07-22','2026-07-22');
                 INSERT INTO telemetry
                   (project_id,source_rowid,org_id,workspace_id,repo_id,execution_id,step,
                    recorded_at,provider,call_role,model,input_tokens,estimated_input_tokens,
                    output_tokens,cache_read_tokens,cache_miss_tokens,cache_write_tokens,
                    cost_usd,duration_ms,retries,tool_calls,usage_complete)
                 VALUES
                   ('p1',1,'acme','ws1','r1',100,0,'2026-07-20 10:00:00','anthropic','worker','claude-x',1000,1000,200,800,200,0,0.50,1200,0,3,1),
                   ('p1',2,'acme','ws1','r1',101,0,'2026-07-21 10:00:00','anthropic','worker','claude-x',500,500,100,400,100,0,0.25,900,0,1,1),
                   ('p2',1,'acme','ws1','r2',200,0,'2026-07-20 12:00:00','openai','worker','gpt-y',800,800,300,0,800,0,0.40,1500,0,2,1),
                   ('p3',1,NULL,NULL,'',300,0,'2026-07-22 09:00:00','anthropic','worker','claude-x',300,300,50,100,200,0,0.10,600,0,0,1),
                   ('p3',2,NULL,NULL,'',301,0,'','anthropic','worker','claude-x',100,100,20,0,100,0,0.05,400,0,0,1);",
            )
            .unwrap();
        drop(usage);
        unsafe { std::env::set_var("STELLA_DATA_DIR", data.path()) };

        let all = respond(home.path(), "/api/hub-telemetry");
        let acme = respond(home.path(), "/api/hub-telemetry?org=acme");
        let local = respond(home.path(), "/api/hub-telemetry?org=(local)");
        let repos = respond(home.path(), "/api/hub-telemetry?org=acme&workspace=ws1");
        let proj = respond(
            home.path(),
            "/api/hub-telemetry?org=acme&workspace=ws1&repo=r1",
        );
        unsafe { std::env::remove_var("STELLA_DATA_DIR") };

        // Empty hub: available:false, zeroed, 200 OK.
        assert_eq!(none.status, "200 OK");
        let v: serde_json::Value = serde_json::from_slice(&none.body).unwrap();
        assert_eq!(v["available"], false);
        assert_eq!(v["totals"]["calls"], 0);
        assert!(v["drill"]["rows"].as_array().unwrap().is_empty());

        // Unscoped totals + org drill.
        let v: serde_json::Value = serde_json::from_slice(&all.body).unwrap();
        assert_eq!(v["available"], true);
        assert_eq!(v["totals"]["calls"], 5);
        assert_eq!(v["totals"]["projects"], 3);
        assert_eq!(v["totals"]["orgs"], 1, "NULL org is not a distinct org");
        assert!((v["totals"]["cost_usd"].as_f64().unwrap() - 1.30).abs() < 1e-9);
        assert_eq!(v["drill"]["level"], "org");
        let orgs = v["drill"]["rows"].as_array().unwrap();
        let acme_row = orgs.iter().find(|r| r["key"] == "acme").unwrap();
        assert!((acme_row["cost_usd"].as_f64().unwrap() - 1.15).abs() < 1e-9);
        assert_eq!(acme_row["projects"], 2);
        let local_row = orgs.iter().find(|r| r["key"].is_null()).unwrap();
        assert!((local_row["cost_usd"].as_f64().unwrap() - 0.15).abs() < 1e-9);
        // provider/model mirrors `usage report`, ordered by cost, with a rate.
        let pm = v["by_provider_model"].as_array().unwrap();
        assert_eq!(pm[0]["provider"], "anthropic");
        assert_eq!(pm[0]["model"], "claude-x");
        assert_eq!(pm[0]["calls"], 4);
        assert!((pm[0]["cache_read_rate"].as_f64().unwrap() - 1300.0 / 1900.0).abs() < 1e-6);
        // Daily series excludes the row with no recorded_at.
        let days: Vec<&str> = v["by_day"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["day"].as_str().unwrap())
            .collect();
        assert_eq!(days, ["2026-07-20", "2026-07-21", "2026-07-22"]);

        // Org filter narrows totals and steps the drill to workspace.
        let v: serde_json::Value = serde_json::from_slice(&acme.body).unwrap();
        assert_eq!(v["totals"]["calls"], 3);
        assert_eq!(v["drill"]["level"], "workspace");
        assert_eq!(v["drill"]["rows"][0]["key"], "ws1");

        // The `(local)` sentinel selects the NULL-org bucket.
        let v: serde_json::Value = serde_json::from_slice(&local.body).unwrap();
        assert_eq!(v["totals"]["calls"], 2);
        assert!((v["totals"]["cost_usd"].as_f64().unwrap() - 0.15).abs() < 1e-9);
        assert_eq!(v["drill"]["level"], "workspace");
        assert!(v["drill"]["rows"][0]["key"].is_null());

        // workspace set → drill by repo.
        let v: serde_json::Value = serde_json::from_slice(&repos.body).unwrap();
        assert_eq!(v["drill"]["level"], "repo");
        let repo_keys: Vec<&str> = v["drill"]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["key"].as_str().unwrap())
            .collect();
        assert!(repo_keys.contains(&"r1") && repo_keys.contains(&"r2"));

        // repo set → drill to the project leaf, carrying its name.
        let v: serde_json::Value = serde_json::from_slice(&proj.body).unwrap();
        assert_eq!(v["drill"]["level"], "project");
        let leaf = &v["drill"]["rows"][0];
        assert_eq!(leaf["key"], "p1");
        assert_eq!(leaf["name"], "proj-one");
        assert_eq!(leaf["calls"], 2);
        assert!((leaf["cost_usd"].as_f64().unwrap() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn unknown_route_is_404_and_missing_id_is_400() {
        let ws = TempDir::new().unwrap();
        assert_eq!(respond(ws.path(), "/api/nope").status, "404 Not Found");
        assert_eq!(
            respond(ws.path(), "/api/execution").status,
            "400 Bad Request"
        );
        assert_eq!(
            respond(ws.path(), "/api/execution?id=abc").status,
            "400 Bad Request"
        );
    }

    #[test]
    fn index_and_mark_are_embedded() {
        let ws = TempDir::new().unwrap();
        let index = respond(ws.path(), "/");
        assert_eq!(index.content_type, "text/html; charset=utf-8");
        assert!(
            String::from_utf8(index.body)
                .unwrap()
                .contains("Observatory")
        );
        let mark = respond(ws.path(), "/assets/mark.svg");
        assert_eq!(mark.content_type, "image/svg+xml");
        // The header lockup needs both cuts; a missing wordmark route would
        // render as a broken image rather than failing loudly.
        let wordmark = respond(ws.path(), "/assets/wordmark.svg");
        assert_eq!(wordmark.content_type, "image/svg+xml");
        assert!(String::from_utf8(wordmark.body).unwrap().contains("<svg"));
    }

    /// The dashboard sends `encodeURIComponent` output, so a scope value with
    /// a space, `&` or `/` arrives escaped. Comparing the still-encoded bytes
    /// against the stored value silently drilled into an empty scope.
    #[test]
    fn query_param_percent_decodes_the_value() {
        let q = Some("org=acme%20corp&repo=owner%2Fname&workspace=a+b&empty=");
        assert_eq!(query_param(q, "org").as_deref(), Some("acme corp"));
        assert_eq!(query_param(q, "repo").as_deref(), Some("owner/name"));
        assert_eq!(query_param(q, "workspace").as_deref(), Some("a b"));
        assert_eq!(query_param(q, "empty"), None);
        assert_eq!(query_param(q, "absent"), None);
        assert_eq!(query_param(None, "org"), None);
        // `%20`/`+` decode to a real space: whitespace is a value, not an
        // absent filter.
        assert_eq!(query_param(Some("org=%20"), "org").as_deref(), Some(" "));
        assert_eq!(query_param(Some("org=+"), "org").as_deref(), Some(" "));
    }

    /// This parses attacker-reachable bytes, so a malformed escape must be
    /// inert — never a panic, never a dropped byte. Multi-byte UTF-8 split
    /// across escapes must reassemble; invalid bytes become U+FFFD.
    #[test]
    fn percent_decode_tolerates_malformed_escapes() {
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%A"), "%A");
        assert_eq!(percent_decode("%ZZ"), "%ZZ");
        assert_eq!(percent_decode("a%%20b"), "a% b");
        // é is two bytes, escaped separately by encodeURIComponent.
        assert_eq!(percent_decode("caf%C3%A9"), "café");
        assert_eq!(percent_decode("%ff"), "\u{fffd}");
        assert_eq!(percent_decode("plain"), "plain");
    }

    /// The page must be fully self-contained: any http(s) URL in the HTML
    /// would be an outbound fetch from the user's browser — a phone-home.
    #[test]
    fn dashboard_html_has_no_external_references() {
        for needle in ["http://", "https://", "//cdn", "@import", "integrity="] {
            assert!(
                !INDEX_HTML.contains(needle),
                "embedded dashboard must not reference {needle}"
            );
        }
    }

    #[tokio::test]
    async fn serves_over_a_real_socket() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let ws = seeded_workspace();
        let root = ws.path().to_path_buf();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let _ = serve(root, 0, move |addr| {
                let _ = tx.send(addr);
            })
            .await;
        });
        let addr = rx.await.unwrap();
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            // A loopback Host: the DNS-rebinding gate (host_is_local, its
            // own unit test above) would 403 a non-local name.
            .write_all(b"GET /api/overview HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).await.unwrap();
        assert!(body.starts_with("HTTP/1.1 200 OK"));
        assert!(body.contains("\"runs\":2"));
        // The no-phone-home guarantee, enforced by the browser rather than
        // only by `dashboard_html_has_no_external_references`.
        assert!(
            body.contains("\r\nX-Content-Type-Options: nosniff\r\n"),
            "head was: {body}"
        );
        assert!(
            body.contains(&format!("\r\nContent-Security-Policy: {CSP}\r\n")),
            "head was: {body}"
        );
        server.abort();
    }

    /// The CSP must keep the embedded page working: it is one document with
    /// inline script, an inline `<style>`, and inline `style="…"` attributes,
    /// so dropping `'unsafe-inline'` from either directive would silently
    /// break it. Same-origin `/assets/*.svg` needs `img-src 'self'`.
    #[test]
    fn csp_admits_everything_the_embedded_dashboard_actually_uses() {
        for directive in [
            "script-src 'self' 'unsafe-inline'",
            "style-src 'self' 'unsafe-inline'",
            "img-src 'self'",
            "connect-src 'self'",
            "frame-ancestors 'none'",
        ] {
            assert!(CSP.contains(directive), "CSP is missing {directive}");
        }
        // A header value may not carry a newline — it would split the head.
        assert!(!CSP.contains('\r') && !CSP.contains('\n'));
        assert!(INDEX_HTML.contains("<style>") && INDEX_HTML.contains("<script>"));
        assert!(INDEX_HTML.contains("src=\"/assets/mark.svg\""));
    }

    /// Padding the request line to the 8 KiB head cap used to be served: the
    /// route still parsed out of the truncated path, while every header —
    /// `Host` included — sat past the cap, and an absent Host is allowed. That
    /// handed a rebound page the whole dashboard cross-origin. A head with no
    /// terminator inside the cap is now refused before routing.
    ///
    /// Sized to *exactly* the cap so the server consumes every byte the client
    /// wrote: closing a socket with unread bytes still in its receive queue
    /// sends an RST, which would race the client's read of the response.
    #[tokio::test]
    async fn unterminated_request_head_is_refused_not_routed() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let ws = seeded_workspace();
        let root = ws.path().to_path_buf();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let _ = serve(root, 0, move |addr| {
                let _ = tx.send(addr);
            })
            .await;
        });
        let addr = rx.await.unwrap();
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let prefix = "GET /api/overview?pad=";
        let suffix = " HTTP/1.1\r\n";
        let pad = "a".repeat(8192 - prefix.len() - suffix.len());
        let request = format!("{prefix}{pad}{suffix}");
        assert_eq!(request.len(), 8192, "fills the cap without terminating");
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).await.unwrap();
        assert!(
            body.starts_with("HTTP/1.1 431"),
            "an unterminated head must be refused, got: {}",
            body.lines().next().unwrap_or_default()
        );
        assert!(!body.contains("\"runs\""), "no telemetry may leak");
        server.abort();
    }
}
