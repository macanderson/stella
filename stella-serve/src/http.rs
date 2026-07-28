//! A tiny hand-rolled HTTP/1.1 layer, following `stella-observatory`'s idiom
//! (no web-framework dependency) but extended for what an engine server needs
//! the read-only dashboard did not: request bodies (POST), bearer auth, and
//! long-lived Server-Sent-Events responses.
//!
//! Deliberately minimal: one request per connection, `Connection: close`, an
//! SSE writer that streams frames until the turn ends and then closes. Enough
//! for a governed sidecar behind the host, not a general-purpose server.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
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

/// Read and parse one request (head + `Content-Length` body). Returns `None` on
/// a clean early hangup or a malformed/over-cap request — the caller closes the
/// connection without a response, exactly as the observatory does.
///
/// Note the cost of that silence on the over-cap path: a host whose POST exceeds
/// [`MAX_REQUEST_BYTES`] sees a bare connection close, not a 413, so it cannot
/// distinguish "too large" from a crashed peer — and if the POST was a
/// `tool-result`, the engine step it would have answered stays parked. Callers
/// sizing a turn body (an assembled conversation) or a tool output against this
/// cap should treat it as a hard limit, not a soft one.
pub(crate) async fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
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
        if buf.len() > MAX_REQUEST_BYTES {
            return Ok(None);
        }
        scanned = buf.len().saturating_sub(3);
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut req_parts = request_line.split_whitespace();
    let (Some(method), Some(path)) = (req_parts.next(), req_parts.next()) else {
        return Ok(None);
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
    if content_length > MAX_REQUEST_BYTES {
        return Ok(None);
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

    Ok(Some(Request {
        method: method.to_string(),
        path: path.to_string(),
        headers,
        body,
    }))
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
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len(),
    );
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
}
