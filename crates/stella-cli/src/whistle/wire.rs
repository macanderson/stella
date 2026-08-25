//! The whistle socket's wire protocol: one length-prefixed JSON frame each
//! way, then the connection closes.
//!
//! `stella whistle` (the sender) and a live session's control socket (the
//! receiver) are always the same binary talking to itself across processes
//! on one machine — this is not a public, cross-version wire contract like
//! `stella-protocol`'s, so there is no compatibility promise beyond "the
//! same build on both ends".
//!
//! Generic over the stream type rather than named on `tokio::net::UnixStream`
//! directly, so this module stays buildable on every platform; only the
//! socket itself (`listener.rs`, and `cmd.rs`'s sender) is `#[cfg(unix)]`.

use std::io;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// The steering text to inject at the session's next step boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WhistleRequest {
    pub(crate) text: String,
}

/// The receiving session's answer: the message was queued.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct WhistleAck {
    pub(crate) delivered: bool,
}

/// Refuses a frame larger than this rather than allocating an attacker- (or
/// bug-) chosen amount from a 4-byte length prefix.
const MAX_FRAME_BYTES: u32 = 64 * 1024;

pub(crate) async fn write_frame<S, T>(stream: &mut S, value: &T) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes =
        serde_json::to_vec(value).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len =
        u32::try_from(bytes.len()).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await
}

pub(crate) async fn read_frame<S, T>(stream: &mut S) -> io::Result<T>
where
    S: AsyncRead + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf);
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "whistle frame exceeds the size limit",
        ));
    }
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf).await?;
    serde_json::from_slice(&buf).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A duplex pipe round-trips a request the way the two ends of a real
    /// Unix socket would — this test never opens one, so it runs on every
    /// platform even though the socket itself is Unix-only.
    #[tokio::test]
    async fn a_request_round_trips_over_a_duplex_pipe() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let sent = WhistleRequest {
            text: "stop the compile, let CI handle the gate".to_string(),
        };
        write_frame(&mut a, &sent).await.unwrap();
        let received: WhistleRequest = read_frame(&mut b).await.unwrap();
        assert_eq!(received.text, sent.text);
    }

    #[tokio::test]
    async fn an_oversized_length_prefix_is_refused_before_allocating() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        // Write a length prefix past MAX_FRAME_BYTES with no payload behind
        // it — a real attacker's next move, and the point of the check is
        // that `read_frame` must reject it on the prefix alone.
        a.write_all(&(MAX_FRAME_BYTES + 1).to_be_bytes())
            .await
            .unwrap();
        drop(a);
        let result: io::Result<WhistleRequest> = read_frame(&mut b).await;
        assert!(result.is_err());
    }
}
