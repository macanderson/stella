//! A tiny hand-rolled HTTP/1.1 layer, following `stella-observatory`'s idiom
//! (no web-framework dependency) but extended for what an engine server needs
//! the read-only dashboard did not: request bodies (POST), bearer auth, and
//! long-lived Server-Sent-Events responses.
//!
//! Deliberately minimal: one request per connection, `Connection: close`, an
//! SSE writer that streams frames until the turn ends and then closes. Enough
//! for a governed sidecar behind the host, not a general-purpose server.
//!
//! Reading a request is **two steps, not one** — [`read_head`], then
//! [`read_body`] or [`discard_body`]. `server.rs` authenticates in between, so
//! an unauthenticated peer can never make this process buffer [`MAX_BODY_BYTES`]
//! on its behalf; a refused request costs one [`DISCARD_CHUNK_BYTES`] scratch
//! buffer instead. Both steps share one deadline, so splitting the read does not
//! double how long a peer may hold a connection.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

/// Cap on the request head (request line + headers) we will buffer.
///
/// Split from [`MAX_BODY_BYTES`] deliberately: a head and a body are abused in
/// different ways and deserve different ceilings. No legitimate client sends
/// 64 KiB of headers, so anything past this is a peer trickling header bytes to
/// hold the connection — the read deadline bounds how long that can last, and
/// this bounds how much it can cost while it does.
const MAX_HEAD_BYTES: usize = 64 * 1024;

/// Cap on the request body we will buffer.
///
/// A turn request carries an assembled conversation, and a `tool-result` POST
/// carries one tool's whole output — which `stella-tools` caps at 100 KB per
/// call — so the old 1 MiB ceiling was roughly ten tool results, close enough
/// to a real conversation to be reachable by accident. 8 MiB is far past any
/// legitimate turn while still bounded.
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// How long a peer has to deliver a complete request head and body.
///
/// This bounds the read side only. It is deliberately **not** applied to the SSE
/// response, which is long-lived by design: a turn streams frames for as long as
/// the host takes to answer its reverse requests, and a deadline there would kill
/// healthy turns. Without this, a connection that opens and then says nothing
/// occupies a task forever, and `server.rs` spawns that task *before* auth.
///
/// The deadline covers head **and** body as one budget: [`read_head`] stamps
/// [`Request::deadline`] when it starts, and [`read_body`] / [`discard_body`]
/// finish against that same instant. Splitting the read in two (so auth can run
/// on the head alone) must not double how long a peer may hold a connection.
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Scratch buffer for a body that is being thrown away rather than parsed.
///
/// The whole point of [`discard_body`] is that refusing a request costs
/// constant memory, so the drain reuses one small buffer instead of growing a
/// `Vec` to `Content-Length`.
const DISCARD_CHUNK_BYTES: usize = 8192;

/// One parsed HTTP request head, plus whatever is needed to finish its body.
///
/// The head and the body are read in two steps on purpose: `server.rs`
/// authenticates on the head, so an unauthenticated peer never gets the server
/// to buffer [`MAX_BODY_BYTES`] on its behalf. Until [`read_body`] runs, `body`
/// is empty and `content_length` is what the peer *declared*.
pub(crate) struct Request {
    pub method: String,
    pub path: String,
    /// Header names lowercased for case-insensitive lookup; values trimmed.
    headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The validated `Content-Length`. Already checked against
    /// [`MAX_BODY_BYTES`] by [`read_head`].
    content_length: usize,
    /// Body bytes that arrived in the same read as the tail of the head.
    prefetched: Vec<u8>,
    /// When the whole read (head + body) must be finished by.
    deadline: tokio::time::Instant,
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
    /// Bytes arrived but do not form a request line, or its framing headers
    /// contradict each other — answer 400.
    Malformed,
    /// The peer framed its body with `Transfer-Encoding` — answer 501.
    UnsupportedTransferEncoding,
    /// The peer did not finish within [`READ_TIMEOUT`] — answer 408.
    Timeout,
}

/// Read and parse one request **head**, bounded by [`READ_TIMEOUT`] and
/// [`MAX_HEAD_BYTES`]; the declared body length is validated against
/// [`MAX_BODY_BYTES`] but not yet consumed.
///
/// Stopping at the head is what lets `server.rs` check the bearer token before
/// a single body byte is buffered. Finish with [`read_body`] once the request
/// is authorized, or [`discard_body`] once it is refused.
pub(crate) async fn read_head<S: AsyncRead + Unpin>(
    stream: &mut S,
) -> std::io::Result<ReadOutcome> {
    let deadline = tokio::time::Instant::now() + READ_TIMEOUT;
    match tokio::time::timeout_at(deadline, read_head_inner(stream, deadline)).await {
        Ok(result) => result,
        Err(_elapsed) => Ok(ReadOutcome::Timeout),
    }
}

/// Consume the declared body into [`Request::body`], bounded by the deadline
/// [`read_head`] stamped.
///
/// A short body (the peer hung up mid-send) is not an error here: the request
/// then simply fails its own validation, which is a clearer answer than a
/// dropped connection.
pub(crate) async fn read_body<S: AsyncRead + Unpin>(
    stream: &mut S,
    req: &mut Request,
) -> std::io::Result<BodyOutcome> {
    if req.content_length == 0 {
        req.prefetched = Vec::new();
        return Ok(BodyOutcome::Complete);
    }
    let read = async {
        let mut body = std::mem::take(&mut req.prefetched);
        body.reserve(req.content_length.saturating_sub(body.len()));
        let mut chunk = [0_u8; DISCARD_CHUNK_BYTES];
        while body.len() < req.content_length {
            let n = stream.read(&mut chunk).await?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
        }
        body.truncate(req.content_length);
        req.body = body;
        Ok::<_, std::io::Error>(())
    };
    match tokio::time::timeout_at(req.deadline, read).await {
        Ok(result) => result.map(|()| BodyOutcome::Complete),
        Err(_elapsed) => Ok(BodyOutcome::Timeout),
    }
}

/// What [`read_body`] produced. Separate from [`ReadOutcome`] because the head
/// is already parsed by then: the only remaining question is whether the body
/// arrived inside the deadline.
pub(crate) enum BodyOutcome {
    /// The declared body (or as much of it as the peer sent) is in
    /// [`Request::body`].
    Complete,
    /// The peer did not finish the body within [`READ_TIMEOUT`] — answer 408.
    Timeout,
}

/// Read the declared body off the socket and throw it away, in constant memory.
///
/// Used on the paths that answer without looking at the body — a 401, a 413, a
/// 405. Draining first is what makes the response actually arrive: closing a
/// connection with unread bytes still in flight makes the kernel send an RST,
/// and a peer that gets an RST mid-send never reads the status we wrote. The
/// drain is bounded by the same deadline as the read it replaces, and by
/// `Content-Length` (already capped at [`MAX_BODY_BYTES`]), so a refused
/// request costs one 8 KiB buffer instead of a body-sized allocation.
pub(crate) async fn discard_body<S: AsyncRead + Unpin>(stream: &mut S, req: &mut Request) {
    let remaining = req.content_length.saturating_sub(req.prefetched.len());
    req.prefetched = Vec::new();
    if remaining == 0 {
        return;
    }
    let drain = async {
        let mut left = remaining;
        let mut chunk = [0_u8; DISCARD_CHUNK_BYTES];
        while left > 0 {
            let want = left.min(chunk.len());
            match stream.read(&mut chunk[..want]).await {
                Ok(0) | Err(_) => return,
                Ok(n) => left -= n,
            }
        }
    };
    // A peer that stops mid-drain is not owed anything more than the response
    // we are about to write, so a lapsed deadline just ends the drain.
    let _ = tokio::time::timeout_at(req.deadline, drain).await;
}

async fn read_head_inner<S: AsyncRead + Unpin>(
    stream: &mut S,
    deadline: tokio::time::Instant,
) -> std::io::Result<ReadOutcome> {
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

    // `Content-Length` only: chunked bodies are not decoded. That used to make
    // a chunked POST parse as an *empty* body, so the host got a 400 blaming
    // its JSON for a framing feature the server does not implement; it is now
    // refused by name (501) instead. Either way it is not a smuggling hole,
    // because this layer serves one request per connection and then closes —
    // leftover bytes are never reinterpreted as a second request.
    if headers.iter().any(|(k, _)| k == "transfer-encoding") {
        return Ok(ReadOutcome::UnsupportedTransferEncoding);
    }
    // Every `Content-Length` must parse, and repeats must agree (RFC 9112
    // §6.3). Taking the first and ignoring the rest — or silently reading an
    // unparseable one as 0 — is how a body gets framed differently here than at
    // any proxy in front, and a request whose length is ambiguous is one this
    // server must refuse rather than guess at.
    let mut content_length = 0_usize;
    let mut declared = false;
    for (_, value) in headers.iter().filter(|(k, _)| k == "content-length") {
        let Ok(parsed) = value.trim().parse::<usize>() else {
            return Ok(ReadOutcome::Malformed);
        };
        if declared && parsed != content_length {
            return Ok(ReadOutcome::Malformed);
        }
        content_length = parsed;
        declared = true;
    }
    if content_length > MAX_BODY_BYTES {
        return Ok(ReadOutcome::TooLarge);
    }

    let body_start = head_end + 4;
    let mut prefetched = buf[body_start..].to_vec();
    prefetched.truncate(content_length);

    Ok(ReadOutcome::Request(Box::new(Request {
        method: method.to_string(),
        path: path.to_string(),
        headers,
        body: Vec::new(),
        content_length,
        prefetched,
        deadline,
    })))
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Write a response head plus body and close, reporting how many bytes went out.
///
/// The byte count is returned rather than merely written because the request
/// fold records `bytes_out` for every response, and a count each handler had to
/// remember to report would be wrong within one refactor. Callers go through
/// `observe::record::Responder`, which is what actually captures it.
pub(crate) async fn write_json_with_headers(
    stream: &mut TcpStream,
    status: &str,
    extra: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<u64> {
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
    let total = (head.len() + body.len()) as u64;
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await?;
    Ok(total)
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
/// `request_id` is echoed as `X-Request-Id` like every other response, so a host
/// can correlate a stream with the POST that created it.
pub(crate) async fn write_sse_head<W: AsyncWrite + Unpin>(
    stream: &mut W,
    request_id: &str,
) -> std::io::Result<u64> {
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nX-Accel-Buffering: no\r\nX-Request-Id: {request_id}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).await?;
    Ok(head.len() as u64)
}

/// Write one SSE `data:` frame carrying a JSON payload.
///
/// Single-line framing is sound because the payload is always `serde_json`
/// output, which escapes newlines inside strings — a raw `\n` would otherwise
/// split one frame into two and desynchronize the host's parser.
pub(crate) async fn write_sse_frame<W: AsyncWrite + Unpin>(
    stream: &mut W,
    json: &str,
) -> std::io::Result<u64> {
    stream.write_all(b"data: ").await?;
    stream.write_all(json.as_bytes()).await?;
    stream.write_all(b"\n\n").await?;
    Ok((b"data: ".len() + json.len() + 2) as u64)
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
            content_length: 0,
            prefetched: Vec::new(),
            deadline: tokio::time::Instant::now() + READ_TIMEOUT,
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
        let outcome = read_head(&mut silent).await.expect("read");
        assert!(
            matches!(outcome, ReadOutcome::Timeout),
            "a silent peer must hit the read deadline, not park forever",
        );
    }

    #[tokio::test]
    async fn a_clean_hangup_is_owed_no_response() {
        let mut nothing = &b""[..];
        assert!(matches!(
            read_head(&mut nothing).await.expect("read"),
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
            read_head(&mut truncated).await.expect("read"),
            ReadOutcome::Malformed
        ));
    }

    #[tokio::test]
    async fn a_request_line_without_a_path_is_malformed() {
        let mut garbage = &b"NOTHTTP\r\n\r\n"[..];
        assert!(matches!(
            read_head(&mut garbage).await.expect("read"),
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
            read_head(&mut stream).await.expect("read"),
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
            read_head(&mut stream).await.expect("read"),
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
        let ReadOutcome::Request(mut req) = read_head(&mut stream).await.expect("read") else {
            panic!("a well-formed request must parse");
        };
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/turns");
        assert_eq!(req.bearer(), Some("tok"));
        assert!(
            req.body.is_empty(),
            "the head read must not buffer the body — auth runs first"
        );
        assert!(matches!(
            read_body(&mut stream, &mut req).await.expect("body"),
            BodyOutcome::Complete
        ));
        assert_eq!(req.body, body.as_bytes());
    }

    /// A body that arrives in a *later* packet than the head still has to be
    /// assembled by the second phase — the prefetched tail is only ever part
    /// of it.
    #[tokio::test]
    async fn a_body_split_across_reads_is_reassembled_after_auth() {
        let body = r#"{"provider_id":"mock","messages":[]}"#;
        let raw = format!(
            "POST /v1/turns HTTP/1.1\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            &body[..4],
        );
        // `chain` makes the split explicit: the head read can only see the
        // first four body bytes, the rest arrives afterwards.
        let mut stream = std::io::Cursor::new(raw.into_bytes()).chain(&body.as_bytes()[4..]);
        let ReadOutcome::Request(mut req) = read_head(&mut stream).await.expect("read") else {
            panic!("a well-formed request must parse");
        };
        read_body(&mut stream, &mut req).await.expect("body");
        assert_eq!(req.body, body.as_bytes());
    }

    /// The refusal paths (401, 413, 405) answer without parsing the body, but
    /// they must still take it off the socket: closing with unread bytes in
    /// flight makes the kernel RST, and a peer that gets an RST mid-send never
    /// reads the status we wrote. Constant memory — the drain never grows a
    /// buffer to `Content-Length`.
    #[tokio::test]
    async fn a_discarded_body_is_consumed_off_the_socket_without_being_buffered() {
        let body = "x".repeat(64 * 1024);
        let raw = format!(
            "POST /v1/turns HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}TRAILING",
            body.len(),
        );
        let mut stream = raw.as_bytes();
        let ReadOutcome::Request(mut req) = read_head(&mut stream).await.expect("read") else {
            panic!("a well-formed request must parse");
        };
        discard_body(&mut stream, &mut req).await;
        assert!(
            req.body.is_empty(),
            "a discarded body is never materialized"
        );
        // Exactly `Content-Length` bytes were consumed: what follows is
        // untouched, which is what proves the drain is bounded by the declared
        // length rather than reading to EOF.
        assert_eq!(stream, b"TRAILING".as_slice());
    }

    /// An unparseable or self-contradicting `Content-Length` frames the body
    /// differently here than at any proxy in front. Refuse rather than guess.
    #[tokio::test]
    async fn ambiguous_content_length_is_refused() {
        for framing in [
            "Content-Length: 12abc\r\n",
            "Content-Length: -1\r\n",
            "Content-Length: 5\r\nContent-Length: 9\r\n",
        ] {
            let raw = format!("POST /v1/turns HTTP/1.1\r\n{framing}\r\nhello");
            let mut stream = raw.as_bytes();
            assert!(
                matches!(
                    read_head(&mut stream).await.expect("read"),
                    ReadOutcome::Malformed
                ),
                "ambiguous framing must be refused: {framing:?}"
            );
        }
        // Repeated but *agreeing* values are unambiguous, so they still parse.
        let raw = "POST /v1/turns HTTP/1.1\r\nContent-Length: 5\r\nContent-Length: 5\r\n\r\nhello";
        let mut stream = raw.as_bytes();
        let ReadOutcome::Request(mut req) = read_head(&mut stream).await.expect("read") else {
            panic!("agreeing lengths are unambiguous");
        };
        read_body(&mut stream, &mut req).await.expect("body");
        assert_eq!(req.body, b"hello");
    }

    /// A chunked POST used to parse as an empty body and come back as a 400
    /// blaming the host's JSON. The server does not decode chunked framing, so
    /// it says so.
    #[tokio::test]
    async fn a_chunked_body_is_named_rather_than_mis_reported_as_bad_json() {
        let raw = "POST /v1/turns HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n\r\n";
        let mut stream = raw.as_bytes();
        assert!(matches!(
            read_head(&mut stream).await.expect("read"),
            ReadOutcome::UnsupportedTransferEncoding
        ));
    }

    /// The deadline covers head *and* body as one budget: splitting the read
    /// in two so auth can run early must not double how long a peer may hold a
    /// connection.
    #[tokio::test(start_paused = true)]
    async fn a_peer_that_stalls_mid_body_hits_the_same_deadline() {
        let head = "POST /v1/turns HTTP/1.1\r\nContent-Length: 16\r\n\r\n";
        let mut stream = std::io::Cursor::new(head.as_bytes().to_vec()).chain(NeverReady);
        let ReadOutcome::Request(mut req) = read_head(&mut stream).await.expect("read") else {
            panic!("the head is complete");
        };
        assert!(matches!(
            read_body(&mut stream, &mut req).await.expect("body"),
            BodyOutcome::Timeout
        ));
    }
}
