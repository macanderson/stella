//! Streamable-HTTP transport: POST JSON-RPC to an endpoint URL and read the
//! response, which may arrive as either `application/json` (one message) or
//! `text/event-stream` (JSON-RPC messages carried as SSE `data:` lines). A
//! session id assigned by the server in the `Mcp-Session-Id` response header
//! (typically on `initialize`) is captured and replayed on every subsequent
//! request, and so is the protocol revision the server named in its
//! `initialize` result — the `MCP-Protocol-Version` header the 2025-06-18
//! spec requires after initialization.
//!
//! Each POST is self-correlating — one request, one response — so there is no
//! pending-map here; when the server streams SSE, this transport scans the
//! stream for the message whose id echoes the request (progress
//! notifications interleaved before it are skipped).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::error::McpError;
use crate::oauth::OAuthTokenSource;
use crate::protocol::{JsonRpcMessage, JsonRpcNotification, JsonRpcRequest};
use crate::sse::SseDecoder;
use crate::transport::Transport;

/// The MCP streamable-HTTP session header.
const SESSION_HEADER: &str = "Mcp-Session-Id";

/// The protocol-version header the 2025-06-18 streamable-HTTP spec requires on
/// every request *after* initialization, carrying the negotiated revision.
const PROTOCOL_VERSION_HEADER: &str = "MCP-Protocol-Version";

/// The largest response body this transport will hold in memory. An MCP
/// server is untrusted input, and `reqwest`'s own body readers are unbounded:
/// a server that answers a `tools/call` — or a 500 whose error body we quote
/// back — with a gigabyte of bytes would be an out-of-memory kill of *stella*
/// long before the per-call timeout noticed. The stdio transport bounds a
/// single frame at the same 8 MiB (`stdio::MAX_LINE_BYTES`) and `sse.rs`
/// bounds an unterminated event there too, so this keeps the three framings on
/// one budget: far past any plausible JSON-RPC message, still cheap to hold.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// A streamable-HTTP connection to one MCP server.
pub struct HttpTransport {
    client: Client,
    url: String,
    base_headers: HeaderMap,
    session_id: Mutex<Option<String>>,
    /// The protocol revision the server named in its `initialize` result,
    /// captured off the wire (like the session id) and replayed as the
    /// [`PROTOCOL_VERSION_HEADER`] on every later request, per the 2025-06-18
    /// streamable-HTTP spec. `None` until initialization — the `initialize`
    /// request itself carries the offered version in its body, not a header.
    protocol_version: Mutex<Option<String>>,
    next_id: AtomicU64,
    server_name: String,
    /// OAuth bearer supplier. Lazy: yields no header until a login is stored
    /// for this server, so static-header servers are unaffected.
    bearer: Option<Arc<OAuthTokenSource>>,
}

impl HttpTransport {
    /// Build a transport for `url`, replaying `headers` on every request.
    /// `timeout` bounds each POST (including the SSE body read).
    pub fn new(
        server_name: &str,
        url: &str,
        headers: &BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<Self, McpError> {
        let mut base_headers = HeaderMap::new();
        for (key, value) in headers {
            let name = HeaderName::from_bytes(key.as_bytes())
                .map_err(|e| McpError::Config(format!("invalid header name `{key}`: {e}")))?;
            let value = HeaderValue::from_str(value)
                .map_err(|e| McpError::Config(format!("invalid header value for `{key}`: {e}")))?;
            base_headers.insert(name, value);
        }
        let client = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| McpError::Transport(format!("failed to build HTTP client: {e}")))?;
        Ok(Self {
            client,
            url: url.to_string(),
            base_headers,
            session_id: Mutex::new(None),
            protocol_version: Mutex::new(None),
            next_id: AtomicU64::new(1),
            server_name: server_name.to_string(),
            bearer: None,
        })
    }

    /// Attach an OAuth bearer supplier consulted on every request. When it
    /// yields a token, it *overrides* any static `Authorization` header.
    #[must_use]
    pub fn with_bearer_source(mut self, source: Arc<OAuthTokenSource>) -> Self {
        self.bearer = Some(source);
        self
    }

    /// POST `body`, capture any assigned session id, and reject non-2xx.
    ///
    /// OAuth: when a bearer source is attached and holds a login, its access
    /// token rides as `Authorization` (refreshed ahead of expiry inside the
    /// source). A 401 answer triggers exactly one forced refresh + resend; a
    /// second 401 surfaces as a re-auth error.
    async fn send(&self, mut body: String) -> Result<reqwest::Response, McpError> {
        for attempt in 0..2u8 {
            let mut headers = self.base_headers.clone();
            let mut has_oauth = false;
            if let Some(source) = &self.bearer {
                let token = if attempt == 0 {
                    source.bearer().await?
                } else {
                    Some(source.refreshed_bearer().await?)
                };
                if let Some(token) = token {
                    let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
                        .map_err(|e| McpError::Auth(format!("token is not header-safe: {e}")))?;
                    value.set_sensitive(true);
                    headers.insert(AUTHORIZATION, value);
                    has_oauth = true;
                }
            }

            // Only the OAuth path can ever reach attempt 1 (a 401 forces one
            // refresh + resend), so a server with no bearer source hands the
            // body straight to `reqwest` instead of copying a `tools/call`
            // payload that may be megabytes of file content on every request.
            let payload = if self.bearer.is_some() {
                body.clone()
            } else {
                std::mem::take(&mut body)
            };
            let mut builder = self
                .client
                .post(&self.url)
                .headers(headers)
                .header(CONTENT_TYPE, "application/json")
                .header(ACCEPT, "application/json, text/event-stream")
                .body(payload);
            if let Some(session) = self.session_id.lock().await.clone() {
                builder = builder.header(SESSION_HEADER, session);
            }
            // After initialization the spec requires the negotiated revision
            // on every request; a header value the server chose that is not
            // header-safe simply is not replayed (it could never have been a
            // real revision string).
            if let Some(version) = self.protocol_version.lock().await.clone()
                && let Ok(value) = HeaderValue::from_str(&version)
            {
                builder = builder.header(PROTOCOL_VERSION_HEADER, value);
            }

            let response = builder.send().await.map_err(|e| {
                McpError::Transport(format!(
                    "HTTP request to `{}` failed: {e}",
                    self.server_name
                ))
            })?;

            // Capture the session id the first time the server assigns one.
            if let Some(session) = response
                .headers()
                .get(SESSION_HEADER)
                .and_then(|v| v.to_str().ok())
            {
                let mut guard = self.session_id.lock().await;
                if guard.is_none() {
                    *guard = Some(session.to_string());
                }
            }

            let status = response.status();
            if status == StatusCode::UNAUTHORIZED && attempt == 0 && has_oauth {
                // Stale/revoked access token: force one refresh and resend.
                continue;
            }
            if !status.is_success() {
                // An error body is server-chosen text we are about to quote
                // back, so it is read under the same cap as a success body —
                // otherwise the cheapest way to OOM the agent is a 500 with a
                // gigabyte attached. A body that blows the cap (or is not
                // UTF-8) simply contributes no detail to the diagnostic.
                let text = self.read_body(response).await.unwrap_or_default();
                let hint = if status == StatusCode::UNAUTHORIZED {
                    format!(
                        " — authorize with `stella mcp login {}` (or `o` on it in the deck's MCP tab)",
                        self.server_name
                    )
                } else {
                    String::new()
                };
                return Err(McpError::Transport(format!(
                    "server `{}` returned HTTP {status}: {}{hint}",
                    self.server_name,
                    truncate(&text, 500)
                )));
            }
            return Ok(response);
        }
        // Unreachable today: only a 401 on attempt 0 continues, so attempt 1
        // always returns. Kept as a typed error rather than `unreachable!` so
        // a future edit to the retry condition degrades into a bad request
        // instead of panicking the agent mid-turn.
        Err(McpError::Transport(format!(
            "server `{}` did not answer within the allowed re-auth attempts",
            self.server_name
        )))
    }

    /// Read a whole response body into a `String`, refusing anything past
    /// [`MAX_BODY_BYTES`].
    ///
    /// `reqwest`'s own `text()`/`bytes()` buffer whatever the server chooses to
    /// send, which would let an untrusted MCP server decide how much of
    /// stella's memory it occupies — so the body is streamed and abandoned the
    /// moment the running total would cross the cap. A declared
    /// `Content-Length` past the cap is refused before a single byte is read.
    ///
    /// A non-UTF-8 body is a protocol error rather than a lossy replacement:
    /// JSON-RPC is UTF-8 by definition, and silently substituting U+FFFD would
    /// only move the failure into the decoder with a worse message.
    async fn read_body(&self, response: reqwest::Response) -> Result<String, McpError> {
        if let Some(len) = response.content_length()
            && len > MAX_BODY_BYTES as u64
        {
            return Err(McpError::Transport(format!(
                "server `{}` declared a {len}-byte response body, past the \
                 {MAX_BODY_BYTES}-byte cap",
                self.server_name
            )));
        }
        let mut buf: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                McpError::Transport(format!(
                    "reading response body from `{}` failed: {e}",
                    self.server_name
                ))
            })?;
            if buf.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
                return Err(McpError::Transport(format!(
                    "server `{}` streamed a response body past the {MAX_BODY_BYTES}-byte cap",
                    self.server_name
                )));
            }
            buf.extend_from_slice(&chunk);
        }
        String::from_utf8(buf).map_err(|_| {
            McpError::Protocol(format!(
                "response body from `{}` is not valid UTF-8",
                self.server_name
            ))
        })
    }

    /// Read a `text/event-stream` body and return the JSON-RPC message that
    /// echoes `expected_id` (or, failing an id match, the first message that
    /// carries a `result`/`error`).
    async fn read_sse_message(
        &self,
        response: reqwest::Response,
        expected_id: u64,
    ) -> Result<JsonRpcMessage, McpError> {
        let mut decoder = SseDecoder::new();
        let mut stream = response.bytes_stream();
        let mut fallback: Option<JsonRpcMessage> = None;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| {
                McpError::Transport(format!("SSE stream from `{}` broke: {e}", self.server_name))
            })?;
            decoder.push_bytes(&chunk).map_err(|e| {
                McpError::Transport(format!("SSE stream from `{}`: {e}", self.server_name))
            })?;
            for event in decoder.poll() {
                if event.data.trim().is_empty() {
                    continue;
                }
                // Tolerate non-JSON `data:` lines (comments/keep-alives).
                let message: JsonRpcMessage = match serde_json::from_str(event.data.trim()) {
                    Ok(message) => message,
                    Err(_) => continue,
                };
                if message.correlated_id() == Some(expected_id) {
                    return Ok(message);
                }
                if fallback.is_none() && (message.result.is_some() || message.error.is_some()) {
                    fallback = Some(message);
                }
                // Otherwise a server notification (e.g. progress) — keep reading.
            }
        }

        fallback.ok_or_else(|| {
            McpError::Protocol(format!(
                "SSE stream from `{}` ended without a response for request {expected_id}",
                self.server_name
            ))
        })
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn request(&self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = JsonRpcRequest::new(id, method, params);
        let body =
            serde_json::to_string(&request).map_err(|e| McpError::Protocol(e.to_string()))?;
        let response = self.send(body).await?;

        let is_sse = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|ct| ct.to_ascii_lowercase().contains("text/event-stream"))
            .unwrap_or(false);

        let message = if is_sse {
            self.read_sse_message(response, id).await?
        } else {
            // Under the same cap as the error path: a success body is exactly
            // as server-chosen, and this is the seam that bounds it *before*
            // the JSON decoder has to hold a second copy of it.
            let text = self.read_body(response).await?;
            serde_json::from_str::<JsonRpcMessage>(text.trim()).map_err(|e| {
                McpError::Protocol(format!("could not decode JSON-RPC response: {e}"))
            })?
        };
        // Capture the negotiated protocol revision off the `initialize`
        // result (the same off-the-wire capture as the session id above) so
        // every later request can replay it as [`PROTOCOL_VERSION_HEADER`].
        // Whether the named revision is one this client can speak is the
        // client layer's decision — an unsupported one aborts the handshake
        // there, so nothing is ever sent under a version we then rejected.
        if method == "initialize"
            && let Some(version) = message
                .result
                .as_ref()
                .and_then(|r| r.get("protocolVersion"))
                .and_then(Value::as_str)
        {
            *self.protocol_version.lock().await = Some(version.to_string());
        }
        message.into_result()
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        let note = JsonRpcNotification::new(method, params);
        let body = serde_json::to_string(&note).map_err(|e| McpError::Protocol(e.to_string()))?;
        // A notification's response body (typically `202 Accepted`) is
        // irrelevant; we only need the POST to have succeeded.
        let _ = self.send(body).await?;
        Ok(())
    }

    async fn close(&self) -> Result<(), McpError> {
        // No persistent connection to tear down. (A future revision could
        // `DELETE` the session; v1 lets the server expire it.)
        Ok(())
    }
}

/// Char-boundary-safe truncation for diagnostic bodies. Shared by every
/// module that quotes an untrusted server's error body back to the user
/// ([`crate::registry`], [`crate::oauth`]) — one implementation, one set of
/// multi-byte tests, instead of three copies that could drift apart.
pub(crate) fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars).collect();
    format!("{head}…")
}

/// Middle-out, char-boundary-safe truncation for a *model-visible* payload,
/// lives next to its head-only sibling [`truncate`] so both share one set of
/// multi-byte tests. Keeps a head and a (larger) tail with an explicit elision
/// marker naming the dropped byte count.
///
/// Per lesson L-S3 the tail budget is ≥ the head budget: a tool result's
/// signal is usually in its tail (the error, the summary, the last row), so a
/// head-only cut is the one that throws away the answer. The marker text
/// matches the native tools' (`stella-tools`' `bash`/custom-tool runners) so a
/// model that has learned to read one reads both.
///
/// `max_bytes` is a budget for the *kept* text; the marker is added on top, so
/// the result can exceed it by the marker's own (small, fixed-shape) length.
pub(crate) fn truncate_middle_out(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let head_budget = max_bytes * 2 / 5; // 40% head …
    let tail_budget = max_bytes - head_budget; // … 60% tail (≥ head, L-S3)
    let head_end = floor_boundary(s, head_budget);
    let tail_start = ceil_boundary(s, s.len().saturating_sub(tail_budget));
    let elided = tail_start.saturating_sub(head_end);
    format!(
        "{}\n... [truncated {elided} bytes] ...\n{}",
        &s[..head_end],
        &s[tail_start..]
    )
}

/// Largest char boundary `<= i`.
fn floor_boundary(s: &str, mut i: usize) -> usize {
    if i >= s.len() {
        return s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Smallest char boundary `>= i`.
fn ceil_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_header_name_is_a_config_error() {
        let mut headers = BTreeMap::new();
        headers.insert("bad header".to_string(), "x".to_string());
        let result = HttpTransport::new("s", "https://h/mcp", &headers, Duration::from_secs(1));
        assert!(matches!(result, Err(McpError::Config(_))));
    }

    #[test]
    fn truncate_is_char_boundary_safe() {
        // Multi-byte chars must never be sliced mid-codepoint.
        let s = "áéíóú".repeat(200);
        let out = truncate(&s, 3);
        assert_eq!(out, "áéí…");
    }

    #[test]
    fn short_strings_are_untouched() {
        assert_eq!(truncate("hello", 500), "hello");
    }

    #[test]
    fn middle_out_keeps_head_and_tail_and_names_the_elision() {
        let s = format!("HEAD{}TAIL", "x".repeat(5_000));
        let out = truncate_middle_out(&s, 400);
        assert!(out.starts_with("HEAD"), "head survives: {}", &out[..20]);
        assert!(out.ends_with("TAIL"), "tail survives (L-S3)");
        assert!(
            out.contains("... [truncated ") && out.contains(" bytes] ..."),
            "the elision is explicit and counted: {out}"
        );
        // The kept text stays inside the budget; only the marker rides on top.
        assert!(out.len() < 400 + 64, "kept text respects the budget");
    }

    #[test]
    fn middle_out_is_char_boundary_safe() {
        // Every cut point lands mid-codepoint for a 3-byte char unless the
        // boundary helpers do their job; a naive slice would panic here.
        for budget in 1..200usize {
            let s = "漢".repeat(500);
            let out = truncate_middle_out(&s, budget);
            assert!(
                out.chars().all(|c| c == '漢' || c.is_ascii()),
                "no codepoint was sliced apart at budget {budget}"
            );
        }
        // …and the same for a 2-byte char with an odd-length budget.
        let s = "áéíóú".repeat(400);
        let out = truncate_middle_out(&s, 101);
        assert!(out.contains("... [truncated "));
    }

    #[test]
    fn middle_out_leaves_an_under_budget_string_byte_identical() {
        assert_eq!(truncate_middle_out("hello", 500), "hello");
        let exact = "x".repeat(500);
        assert_eq!(truncate_middle_out(&exact, 500), exact);
    }
}
