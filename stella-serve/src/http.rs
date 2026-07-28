//! A tiny hand-rolled HTTP/1.1 layer, following `stella-observatory`'s idiom
//! (no web-framework dependency) but extended for what an engine server needs
//! the read-only dashboard did not: request bodies (POST), bearer auth, and
//! long-lived Server-Sent-Events responses.
//!
//! Deliberately minimal: one request per connection, `Connection: close`, an
//! SSE writer that streams frames until the turn ends and then closes. Enough
//! for a governed sidecar behind the host, not a general-purpose server.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Cap applied to the request head and to the body, **each** — `read_request`
/// enforces it per part, so one request may buffer up to twice this in total.
/// A turn request carries an assembled conversation, so it is larger than the
/// dashboard's 8 KiB GET cap, but still bounded — a host that needs more is
/// misusing the endpoint.
const MAX_REQUEST_BYTES: usize = 1024 * 1024;

/// One parsed HTTP request.
pub(crate) struct Request {
    pub method: String,
    pub path: String,
    /// Header names lowercased for case-insensitive lookup; values trimmed.
    headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// Case-insensitive header lookup. Names are lowercased at parse time, so
    /// this matches without allocating a lowercased copy of `name`.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The bearer token from `Authorization: Bearer <token>`, if present.
    ///
    /// The scheme is matched case-insensitively because RFC 7235 §2.1 defines
    /// it that way: a client that sends `bearer <token>` (curl's `--oauth2-bearer`
    /// spells it `Bearer`, but hand-rolled and proxy-rewritten clients do not
    /// always) is presenting a well-formed credential, and rejecting it as a 401
    /// is an interop bug that reads as an auth failure.
    pub fn bearer(&self) -> Option<&str> {
        let value = self.header("authorization")?;
        let (scheme, token) = value.split_once(' ')?;
        scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
    }
}

/// What one read attempt produced.
///
/// The point of this enum is that the caller can tell these apart. They used to
/// collapse into a bare `None`, so a host whose POST was one byte over the cap
/// got the same silent connection close as a crashed peer — and if that POST was
/// a `tool-result`, the engine step it would have answered stayed parked with no
/// way to learn why.
pub(crate) enum ReadOutcome {
    /// A complete, parseable request.
    Request(Box<Request>),
    /// The peer closed before sending a complete head. No response is owed.
    Hangup,
    /// Head or body exceeded its cap — answer 413.
    TooLarge,
    /// Bytes arrived but do not form a request line — answer 400.
    Malformed,
    /// The peer did not finish within [`READ_TIMEOUT`] — answer 408.
    Timeout,
}

/// Read and parse one request (head + `Content-Length` body), bounded by
/// [`READ_TIMEOUT`], [`MAX_HEAD_BYTES`] and [`MAX_BODY_BYTES`].
pub(crate) async fn read_request<S: AsyncRead + Unpin>(
    stream: &mut S,
) -> std::io::Result<ReadOutcome> {
    match tokio::time::timeout(READ_TIMEOUT, read_request_inner(stream)).await {
        Ok(result) => result,
        Err(_elapsed) => Ok(ReadOutcome::Timeout),
    }
}

async fn read_request_inner<S: AsyncRead + Unpin>(stream: &mut S) -> std::io::Result<ReadOutcome> {
    let mut buf = Vec::with_capacity(8192);
    let mut chunk = [0_u8; 8192];
    // Rescanning the whole buffer after every read would be quadratic in the
    // head size on a path an unauthenticated peer controls; a terminator can
    // only straddle a read boundary by three bytes, so resume the search there.
    let mut scanned = 0_usize;
    let head_end = loop {
        if let Some(pos) = find_head_end(&buf[scanned..]) {
            break scanned + pos;
        }
        if buf.len() > MAX_HEAD_BYTES {
            return Ok(ReadOutcome::TooLarge);
        }
        scanned = buf.len().saturating_sub(3);
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            // Nothing at all means a probe or a dropped connection: no response
            // is owed. Bytes that stop mid-head are a truncated request, which
            // the peer should be told about.
            return Ok(if buf.is_empty() {
                ReadOutcome::Hangup
            } else {
                ReadOutcome::Malformed
            });
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut req_parts = request_line.split_whitespace();
    let (Some(method), Some(path)) = (req_parts.next(), req_parts.next()) else {
        return Ok(ReadOutcome::Malformed);
    };

    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }

    // `Content-Length` only: chunked bodies are not decoded, so a chunked POST
    // parses as an empty body and fails validation with a 400. That is safe
    // rather than a smuggling hole precisely because this layer serves one
    // request per connection and then closes — leftover bytes are never
    // reinterpreted as a second request.
    let content_length = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        return Ok(ReadOutcome::TooLarge);
    }

    let body_start = head_end + 4;
    let mut body = buf[body_start..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_length);

    Ok(ReadOutcome::Request(Box::new(Request {
        method: method.to_string(),
        path: path.to_string(),
        headers,
        body,
    })))
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Write a one-shot response (status, JSON content type, body) and close.
pub(crate) async fn write_json(
    stream: &mut TcpStream,
    status: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write_json_with_headers(stream, status, &[], body).await
}

/// [`write_json`] plus caller-supplied headers, for the responses that carry one
/// (`Retry-After` on a 429). Each pair is emitted verbatim as `name: value`.
pub(crate) async fn write_json_with_headers(
    stream: &mut TcpStream,
    status: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        body.len(),
    );
    for (name, value) in extra {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await
}

/// Write the SSE response head, leaving the connection open to stream frames.
///
/// `X-Accel-Buffering: no` is not decoration. The deployment this crate
/// documents puts a reverse proxy in front (see `serve`'s operational limits),
/// and nginx-family proxies buffer a response body by default — which for this
/// stream is a deadlock, not a latency wart: the host cannot answer a
/// `provider_request` it has not received, so the engine parks forever and the
/// buffered stream never reaches the size that would flush it. The header is the
/// standard opt-out and is ignored by proxies that do not honour it.
pub(crate) async fn write_sse_head(stream: &mut TcpStream) -> std::io::Result<()> {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nX-Accel-Buffering: no\r\nConnection: close\r\n\r\n";
    stream.write_all(head.as_bytes()).await
}

/// Write one SSE `data:` frame carrying a JSON payload.
///
/// Single-line framing is sound because the payload is always `serde_json`
/// output, which escapes newlines inside strings — a raw `\n` would otherwise
/// split one frame into two and desynchronize the host's parser.
pub(crate) async fn write_sse_frame(stream: &mut TcpStream, json: &str) -> std::io::Result<()> {
    stream.write_all(b"data: ").await?;
    stream.write_all(json.as_bytes()).await?;
    stream.write_all(b"\n\n").await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_with_auth(value: &str) -> Request {
        Request {
            method: "GET".to_string(),
            path: "/".to_string(),
            headers: vec![("authorization".to_string(), value.to_string())],
            body: Vec::new(),
        }
    }

    #[test]
    fn bearer_scheme_is_matched_case_insensitively() {
        // RFC 7235 §2.1 makes the scheme case-insensitive. Getting this wrong
        // costs more than it looks: the failure surfaces as a 401, which an
        // operator reads as "wrong token", not as an interop bug.
        assert_eq!(request_with_auth("Bearer tok").bearer(), Some("tok"));
        assert_eq!(request_with_auth("bearer tok").bearer(), Some("tok"));
        assert_eq!(request_with_auth("BEARER tok").bearer(), Some("tok"));
    }

    #[test]
    fn a_non_bearer_credential_is_never_read_as_a_token() {
        assert_eq!(request_with_auth("Basic dXNlcjpwdw==").bearer(), None);
        assert_eq!(request_with_auth("Bearer").bearer(), None);
        assert_eq!(request_with_auth("").bearer(), None);
    }

    /// A reader that accepts the connection and then never produces a byte —
    /// the "slowloris" shape the read deadline exists to bound.
    struct NeverReady;

    impl AsyncRead for NeverReady {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Pending
        }
    }

    /// A peer that opens a connection and then says nothing used to hold the
    /// spawned task forever — and `server.rs` spawns that task *before* auth, so
    /// it cost nothing to do at scale. The paused clock makes the 30s deadline
    /// assertable in microseconds: [`NeverReady`] never becomes ready, so the
    /// runtime idles and auto-advances virtual time to the timer.
    #[tokio::test(start_paused = true)]
    async fn a_peer_that_never_finishes_its_head_is_timed_out() {
        let mut silent = NeverReady;
        let outcome = read_request(&mut silent).await.expect("read");
        assert!(
            matches!(outcome, ReadOutcome::Timeout),
            "a silent peer must hit the read deadline, not park forever",
        );
    }

    #[tokio::test]
    async fn a_clean_hangup_is_owed_no_response() {
        let mut nothing = &b""[..];
        assert!(matches!(
            read_request(&mut nothing).await.expect("read"),
            ReadOutcome::Hangup
        ));
    }

    /// Bytes that stop mid-head are a truncated request, not a probe: the peer
    /// is still there and can be told. Collapsing this into `Hangup` is what made
    /// a dropped `tool-result` indistinguishable from a crashed host.
    #[tokio::test]
    async fn a_truncated_head_is_malformed_rather_than_a_hangup() {
        let mut truncated = &b"POST /v1/turns HTTP/1.1\r\nHost: engine"[..];
        assert!(matches!(
            read_request(&mut truncated).await.expect("read"),
            ReadOutcome::Malformed
        ));
    }

    #[tokio::test]
    async fn a_request_line_without_a_path_is_malformed() {
        let mut garbage = &b"NOTHTTP\r\n\r\n"[..];
        assert!(matches!(
            read_request(&mut garbage).await.expect("read"),
            ReadOutcome::Malformed
        ));
    }

    /// The declared body size is rejected on the header, before a single body
    /// byte is buffered — otherwise the cap would be enforced by first paying it.
    #[tokio::test]
    async fn an_over_cap_content_length_is_too_large_and_is_never_buffered() {
        let declared = MAX_BODY_BYTES + 1;
        let head = format!(
            "POST /v1/turns HTTP/1.1\r\nHost: engine\r\nContent-Length: {declared}\r\n\r\n"
        );
        let mut stream = head.as_bytes();
        assert!(matches!(
            read_request(&mut stream).await.expect("read"),
            ReadOutcome::TooLarge
        ));
    }

    #[tokio::test]
    async fn an_over_cap_head_is_too_large() {
        let mut head = String::from("GET / HTTP/1.1\r\n");
        while head.len() <= MAX_HEAD_BYTES {
            head.push_str("X-Filler: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n");
        }
        let mut stream = head.as_bytes();
        assert!(matches!(
            read_request(&mut stream).await.expect("read"),
            ReadOutcome::TooLarge
        ));
    }

    #[tokio::test]
    async fn a_well_formed_request_still_parses_with_its_body() {
        let body = r#"{"provider_id":"mock"}"#;
        let raw = format!(
            "POST /v1/turns HTTP/1.1\r\nHost: engine\r\nAuthorization: Bearer tok\r\nContent-Length: {}\r\n\r\n{body}",
            body.len(),
        );
        let mut stream = raw.as_bytes();
        let ReadOutcome::Request(req) = read_request(&mut stream).await.expect("read") else {
            panic!("a well-formed request must parse");
        };
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/turns");
        assert_eq!(req.bearer(), Some("tok"));
        assert_eq!(req.body, body.as_bytes());
    }
}
