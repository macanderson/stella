//! Where one message ends and the next begins on a child's stdout — and the one
//! task that owns its stdin.
//!
//! Shared by both child transports on this plane:
//! [`SubprocessWrapper`](super::SubprocessWrapper), which speaks the wrapper
//! socket's points, and [`SubprocessDriver`](super::SubprocessDriver), which
//! opens a driver session. Those two conversations differ in every way that
//! matters — their message vocabularies, what ends them, and what each may ask
//! the host for — and in no way at all here: where a JSON value ends is a fact
//! about JSON, and the newline a plugin author's `read` needs is a fact about
//! shells.
//!
//! Extracted rather than copied, because the framer is the one piece of this
//! plane whose second implementation would be wrong in a way tests do not
//! catch. The naive version is four lines and quadratic — [`Framer`]'s own doc
//! carries the measurement — so a copy is one well-meaning simplification away
//! from re-earning a ten-second timeout somewhere else, with nothing tying the
//! two together.

use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;
use tokio::sync::mpsc;

/// How much of a child's stderr or unreadable stdout reaches an error message.
pub(super) const OUTPUT_EXCERPT_CHARS: usize = 2_000;

/// How much of either stream is taken in one read.
///
/// Both conversations read incrementally rather than through
/// [`stella_tools::exec::capture`], which waits for the child to exit — a child
/// that is mid-conversation has not exited and must not be waited for.
pub(super) const READ_CHUNK: usize = 8 * 1024;

/// Where one message ends in a stream of them.
///
/// **Not a line reader, and not a re-parse either**, and both alternatives were
/// tried on the way here:
///
/// - *One message per line* is what the writer produces and what both shipped
///   plugins write, but it forbids a plugin from pretty-printing its answer —
///   which is what `json.dumps(x, indent=2)` and `JSON.stringify(x, null, 2)`
///   do, and refusing them would make the wire contract a Rust programmer's
///   idea of JSON.
/// - *Try to parse the whole buffer on every read* accepts both shapes and is
///   four lines, and it is quadratic: `crates/stella-runtime/tests/wrapper_transport_limits.rs`
///   drives 9 MiB through this path in 8 KiB reads, which re-scanned about
///   6 GiB and turned a prompt refusal into a ten-second timeout. The transport
///   is the one place in this crate that reads attacker-shaped volume, so
///   "correct but quadratic" is a defect here rather than a trade.
///
/// So the framer scans each byte exactly once, tracking nesting depth outside
/// of strings, and hands over a value the moment its depth returns to zero. The
/// scan is also what refuses output that is not a message at all: a plugin whose
/// stdout starts with a log line is told so on the first read rather than at the
/// ceiling, which is 8 MiB of reading later.
#[derive(Debug, Default)]
pub(super) struct Framer {
    buf: Vec<u8>,
    /// How far into `buf` the scan has already reached, so no byte is examined
    /// twice.
    scanned: usize,
    /// Nesting depth of `{`/`[` outside strings. `0` with `started` set means a
    /// complete value ends at `scanned`.
    depth: u32,
    started: bool,
    complete: Option<usize>,
    in_string: bool,
    escaped: bool,
}

impl Framer {
    /// Add what one read produced.
    pub(super) fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// What is buffered but not yet a message — for an error's excerpt.
    pub(super) fn buffered(&self) -> &[u8] {
        &self.buf
    }

    /// The next complete message, or `None` while one is still arriving.
    ///
    /// # Errors
    ///
    /// The bytes are not a JSON object at all, described in the words the
    /// caller wraps into a decode failure. A message is an object in both
    /// directions of this wire — a bare scalar is not a shape either side has —
    /// so refusing on the first byte is strictly more useful than waiting to
    /// fail on the whole document.
    pub(super) fn take(&mut self) -> Result<Option<Vec<u8>>, String> {
        while self.scanned < self.buf.len() {
            let byte = self.buf[self.scanned];
            self.scanned += 1;
            if self.in_string {
                if self.escaped {
                    self.escaped = false;
                } else if byte == b'\\' {
                    self.escaped = true;
                } else if byte == b'"' {
                    self.in_string = false;
                }
                continue;
            }
            match byte {
                // Whitespace between messages is ordinary — the writer's own
                // newline is one.
                b' ' | b'\t' | b'\r' | b'\n' if !self.started => {}
                b'{' | b'[' => {
                    self.depth += 1;
                    self.started = true;
                }
                // Anything else before a value has opened is a plugin writing
                // something that is not a message to the stream its answer
                // arrives on: a log line, a stray `}`, a bare string. Refused
                // on the byte rather than accumulated to the ceiling, which is
                // 8 MiB of reading later.
                other if !self.started => {
                    return Err(format!(
                        "expected a JSON object on stdout, found `{}`",
                        char::from(other).escape_debug()
                    ));
                }
                b'"' => self.in_string = true,
                b'}' | b']' => {
                    self.depth = self.depth.saturating_sub(1);
                    if self.depth == 0 {
                        self.complete = Some(self.scanned);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = self.complete.take() else {
            return Ok(None);
        };
        let message: Vec<u8> = self.buf.drain(..end).collect();
        self.scanned = 0;
        self.started = false;
        Ok(Some(message))
    }
}

/// Write every frame the conversation produces, and nothing else.
///
/// One task owns stdin for the life of the exchange. The newline after each
/// frame is not what delimits a message — the parser decides that — but a
/// line-oriented reader (`read` in a shell, `sys.stdin.readline()` in Python) is
/// the first thing a plugin author reaches for, and denying them it buys
/// nothing.
///
/// The shutdown at the end is the plugin's EOF, and it is why the sender is
/// dropped the moment the point response lands: a plugin blocked on `cat` or
/// `sys.stdin.read()` cannot exit until this happens.
pub(super) async fn write_frames(
    mut stdin: ChildStdin,
    mut outbox: mpsc::Receiver<Vec<u8>>,
) -> std::io::Result<()> {
    while let Some(frame) = outbox.recv().await {
        stdin.write_all(&frame).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
    }
    stdin.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames_of(chunks: &[&str]) -> Result<Vec<String>, String> {
        let mut framer = Framer::default();
        let mut out = Vec::new();
        for chunk in chunks {
            framer.push(chunk.as_bytes());
            while let Some(message) = framer.take()? {
                out.push(String::from_utf8_lossy(&message).into_owned());
            }
        }
        Ok(out)
    }

    /// The shape both shipped plugins write: one message, one line.
    #[test]
    fn a_message_per_line_is_one_message_per_line() {
        assert_eq!(
            frames_of(&["{\"a\":1}\n{\"b\":2}\n"]).expect("both messages frame"),
            vec!["{\"a\":1}".to_string(), "\n{\"b\":2}".to_string()],
        );
    }

    /// A plugin may pretty-print, because `json.dumps(x, indent=2)` is what its
    /// language does by default and a wire contract that refuses it is a Rust
    /// programmer's idea of JSON. A line reader would have cut this into five
    /// unparseable pieces.
    #[test]
    fn a_message_may_span_lines() {
        let pretty = "{\n  \"point\": \"before_turn\",\n  \"body\": {\n    \"protocol_version\": 1\n  }\n}\n";
        let framed = frames_of(&[pretty]).expect("a pretty-printed message frames");
        assert_eq!(framed.len(), 1);
        assert!(framed[0].contains("before_turn"));
    }

    /// The framing does not depend on where a read happened to land — which is
    /// the whole reason it is a scanner and not a `split('\n')`.
    #[test]
    fn a_message_split_across_reads_is_reassembled() {
        assert_eq!(
            frames_of(&["{\"goa", "l\":\"x\"", "}"]).expect("frames"),
            vec!["{\"goal\":\"x\"}".to_string()],
        );
    }

    /// Braces inside a string are text, not structure — a goal containing `}`
    /// is ordinary, and a framer that ended the message there would truncate
    /// every request carrying one.
    #[test]
    fn a_brace_inside_a_string_does_not_end_the_message() {
        assert_eq!(
            frames_of(&[r#"{"goal":"fix fn f() {}","n":1}"#]).expect("frames"),
            vec![r#"{"goal":"fix fn f() {}","n":1}"#.to_string()],
        );
        assert_eq!(
            frames_of(&[r#"{"goal":"a \" then }"}"#]).expect("frames"),
            vec![r#"{"goal":"a \" then }"}"#.to_string()],
            "an escaped quote does not leave the string"
        );
    }

    /// Output that is not a message at all is refused on the byte, not at the
    /// 8 MiB ceiling: a plugin logging to stdout is told what it did wrong.
    #[test]
    fn output_that_is_not_a_message_is_refused_immediately() {
        let error = frames_of(&["starting up...\n{\"point\":\"before_turn\"}"])
            .expect_err("a log line on stdout is refused");
        assert!(error.contains("expected a JSON object"), "{error}");

        let stray = frames_of(&["}{\"a\":1}"]).expect_err("a stray brace is refused");
        assert!(stray.contains("expected a JSON object"), "{stray}");
    }
}
