//! The loopback half of the browser flow: a listener on `127.0.0.1` that the
//! authorization server redirects the user's browser back to, and the reader
//! that gets one whole HTTP request head off that socket before anything is
//! parsed out of it.

use reqwest::Url;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::error::McpError;

/// Hard cap on the bytes accepted before the end of a request head.
///
/// Unchanged from the single-read version this replaced, on purpose: 8 KiB is
/// far beyond any browser's `GET /callback?…` head, and raising it would only
/// widen how much a local process can make us buffer. What changed is that the
/// cap is now a *cap* rather than a *truncation point* — a head that exceeds it
/// is rejected instead of being parsed from its first 8 KiB.
const MAX_CALLBACK_HEAD_BYTES: usize = 8192;

/// Why a callback connection yielded no request head. Both variants mean
/// "ignore this connection and keep waiting" — the browser retries, and the
/// caller's overall `options.timeout` still bounds the wait.
#[derive(Debug, PartialEq, Eq)]
enum HeadError {
    /// The peer closed (or errored) before `\r\n\r\n` arrived.
    Incomplete,
    /// More than [`MAX_CALLBACK_HEAD_BYTES`] arrived with no end of head.
    TooLarge,
}

/// Read one HTTP request head — everything up to and including the blank line
/// that terminates it — from `stream`.
///
/// A single `read()` was the bug this replaces. TCP is a byte stream with no
/// message boundaries: nothing obliges a peer (or the kernel, or a loopback
/// MTU) to deliver the request line in one segment. Because the `state`
/// parameter that gates code acceptance was parsed out of that one buffer, a
/// callback split mid-`state` produced a *truncated* target whose `state` was
/// absent or clipped. That is not merely a missed login — it is the CSRF check
/// being decided on a partial view of the attacker-influenced input.
///
/// Reading to the end of the head fixes the split; the byte cap keeps the
/// unbounded-growth failure from replacing it. The head is all this endpoint
/// ever needs: the callback is a `GET` with no body.
async fn read_request_head<R>(stream: &mut R) -> Result<Vec<u8>, HeadError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut head = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        // Search from just before the newly-appended bytes so a terminator
        // straddling two reads is still found, without rescanning the head.
        let scan_from = head.len().saturating_sub(3);
        let n = match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return Err(HeadError::Incomplete),
            Ok(n) => n,
        };
        head.extend_from_slice(&chunk[..n]);
        if head[scan_from..].windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(head);
        }
        if head.len() > MAX_CALLBACK_HEAD_BYTES {
            return Err(HeadError::TooLarge);
        }
    }
}

/// Serve the loopback redirect: accept connections until one carries
/// `GET /callback?…` with our `state`, answer it with a tiny page, and
/// return the authorization code.
///
/// The whole loop — every connection, and every read within one — runs inside
/// the caller's `tokio::time::timeout(options.timeout, …)`, so a peer that
/// dribbles bytes forever is bounded by the login deadline exactly as before.
pub(super) async fn wait_for_code(
    listener: TcpListener,
    expected_state: &str,
) -> Result<String, McpError> {
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|e| McpError::Auth(format!("loopback accept failed: {e}")))?;

        let head = match read_request_head(&mut stream).await {
            Ok(head) => head,
            // A hangup before the head completed: nothing to answer, and the
            // browser will retry.
            Err(HeadError::Incomplete) => continue,
            // An oversized head is answered rather than dropped, so a real
            // client learns why instead of hanging until the login deadline.
            Err(HeadError::TooLarge) => {
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 431 Request Header Fields Too Large\r\ncontent-length: 0\r\n\
                          connection: close\r\n\r\n",
                    )
                    .await;
                let _ = stream.shutdown().await;
                continue;
            }
        };
        let request = String::from_utf8_lossy(&head);
        let Some(target) = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
        else {
            continue;
        };

        // Anything but the callback (favicon probes etc.) gets a 404 and the
        // wait continues.
        if !target.starts_with("/callback") {
            let _ = stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await;
            continue;
        }

        // `Url` does the query parsing + percent-decoding.
        let parsed = Url::parse(&format!("http://127.0.0.1{target}"))
            .map_err(|e| McpError::Auth(format!("malformed redirect: {e}")))?;
        let mut code = None;
        let mut state = None;
        let mut error = None;
        let mut error_description = None;
        for (key, value) in parsed.query_pairs() {
            match key.as_ref() {
                "code" => code = Some(value.into_owned()),
                "state" => state = Some(value.into_owned()),
                "error" => error = Some(value.into_owned()),
                "error_description" => error_description = Some(value.into_owned()),
                _ => {}
            }
        }

        let outcome: Result<String, McpError> = if let Some(error) = error {
            Err(McpError::Auth(match error_description {
                Some(desc) => format!("authorization was refused: {error} — {desc}"),
                None => format!("authorization was refused: {error}"),
            }))
        } else if state.as_deref() != Some(expected_state) {
            Err(McpError::Auth(
                "redirect state mismatch — possible CSRF; aborting login".to_string(),
            ))
        } else if let Some(code) = code {
            Ok(code)
        } else {
            Err(McpError::Auth(
                "redirect carried neither a code nor an error".to_string(),
            ))
        };

        let page = match &outcome {
            Ok(_) => {
                "<!doctype html><meta charset=\"utf-8\"><title>stella</title>\
                 <body style=\"font-family:system-ui;padding:3rem\">\
                 <h2>✓ Signed in</h2><p>You can close this tab and return to stella.</p>"
            }
            Err(_) => {
                "<!doctype html><meta charset=\"utf-8\"><title>stella</title>\
                 <body style=\"font-family:system-ui;padding:3rem\">\
                 <h2>Sign-in failed</h2><p>Return to stella for details.</p>"
            }
        };
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{page}",
            page.len()
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;

        return outcome;
    }
}

#[cfg(test)]
mod callback_read_tests {
    use super::*;

    use std::pin::Pin;
    use std::task::{Context, Poll};

    /// An `AsyncRead` that hands back exactly one scripted chunk per poll, so
    /// a split request head is deterministic instead of a race against the
    /// loopback stack.
    struct ChunkedReader(std::collections::VecDeque<Vec<u8>>);

    impl ChunkedReader {
        fn new(chunks: &[&str]) -> Self {
            Self(chunks.iter().map(|c| c.as_bytes().to_vec()).collect())
        }
    }

    impl tokio::io::AsyncRead for ChunkedReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if let Some(chunk) = self.0.pop_front() {
                let n = chunk.len().min(buf.remaining());
                buf.put_slice(&chunk[..n]);
                // A chunk larger than the reader's buffer keeps its tail for
                // the next poll — dropping it would silently turn an
                // oversized-head test into a truncated-head one.
                if n < chunk.len() {
                    self.0.push_front(chunk[n..].to_vec());
                }
            }
            // An exhausted script reads as EOF, which is what a browser that
            // hung up mid-head looks like.
            Poll::Ready(Ok(()))
        }
    }

    /// The witness for the split-read defect. The request line is delivered in
    /// two chunks that cut the `state` parameter in half. The old single-read
    /// implementation saw only `…&sta` and decided the CSRF check on that
    /// truncated view (state absent → mismatch → login refused, and the same
    /// truncation is what an attacker would aim a crafted split at). Reading
    /// to the end of the head must reassemble the full target, `state` intact.
    #[tokio::test]
    async fn a_head_split_mid_state_is_reassembled_before_the_state_check() {
        let mut reader = ChunkedReader::new(&[
            "GET /callback?code=the-code&sta",
            "te=expected-state HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n",
        ]);
        let head = read_request_head(&mut reader).await.expect("complete head");
        let request = String::from_utf8_lossy(&head);
        let target = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .expect("request target");

        let parsed = Url::parse(&format!("http://127.0.0.1{target}")).unwrap();
        let state = parsed
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.into_owned());
        let code = parsed
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.into_owned());

        assert_eq!(
            state.as_deref(),
            Some("expected-state"),
            "the state check must see the whole value, not the first chunk's prefix"
        );
        assert_eq!(code.as_deref(), Some("the-code"));
    }

    /// The terminator itself may straddle two reads; the rescan window must
    /// catch it rather than sliding past it.
    #[tokio::test]
    async fn a_terminator_split_across_reads_is_still_found() {
        let mut reader = ChunkedReader::new(&[
            "GET /callback?code=c&state=s HTTP/1.1\r\nHost: h\r\n\r",
            "\n",
        ]);
        let head = read_request_head(&mut reader).await.expect("complete head");
        assert!(head.ends_with(b"\r\n\r\n"));
    }

    /// The cap is enforced, so replacing the single read did not trade a
    /// truncation bug for an unbounded-buffer one.
    #[tokio::test]
    async fn a_head_that_never_ends_is_rejected_at_the_byte_cap() {
        let flood = format!(
            "GET /callback?code=c HTTP/1.1\r\nX-Pad: {}\r\n",
            "A".repeat(MAX_CALLBACK_HEAD_BYTES * 2)
        );
        let mut reader = ChunkedReader::new(&[&flood]);
        assert_eq!(
            read_request_head(&mut reader).await,
            Err(HeadError::TooLarge)
        );
    }

    /// A peer that hangs up mid-head is ignored, not treated as a request.
    #[tokio::test]
    async fn a_truncated_head_is_incomplete_not_a_request() {
        let mut reader = ChunkedReader::new(&["GET /callback?code=c HTTP/1.1\r\n"]);
        assert_eq!(
            read_request_head(&mut reader).await,
            Err(HeadError::Incomplete)
        );
    }
}
