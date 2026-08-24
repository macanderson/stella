// SPDX-License-Identifier: AGPL-3.0-only
//! Hand-rolled loopback servers for the two broken-stream shapes wiremock
//! cannot express, shared by every dialect's streaming→non-streaming
//! fallback suite (#2686, #2746).
//!
//! Wiremock can serve a body, and it can delay a whole response; it cannot
//! send a complete HTTP head and then hold the socket open sending nothing,
//! which is exactly what a proxy buffering an SSE body looks like from the
//! client side. Both servers below take a `is_streaming` predicate over the
//! **raw request text** (request line, headers and body) rather than
//! hard-coding one dialect's discriminator: the OpenAI-shaped dialects mark
//! a streaming request with `"stream":true` in the body, while the Google
//! ones use a different method on the URL (`:streamGenerateContent` vs
//! `:generateContent`).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};

/// Read one HTTP request (headers + `Content-Length` body) off a blocking
/// socket — just enough parser for the servers below.
fn read_request(socket: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = socket.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf);
        if let Some(header_end) = text.find("\r\n\r\n") {
            let content_length = text
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if buf.len() >= header_end + 4 + content_length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// A streaming request is answered with its HTTP headers and then **not one
/// body byte** — the socket is held open, never closed, so the client sees a
/// live-but-silent stream rather than the EOF that would make this the
/// *empty stream* shape instead. A non-streaming request for the same host
/// completes normally with `unary_body`. Returns the base URL.
pub(crate) fn hang_streams_answer_unary(
    is_streaming: fn(&str) -> bool,
    unary_body: &'static str,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for conn in listener.incoming() {
            let Ok(mut socket) = conn else { break };
            let request = read_request(&mut socket);
            if is_streaming(&request) {
                let _ =
                    socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n");
                let _ = socket.flush();
                held.push(socket);
            } else {
                let _ = write!(
                    socket,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{}",
                    unary_body.len(),
                    unary_body
                );
            }
        }
    });
    format!("http://{addr}")
}

/// The mirror image of [`hang_streams_answer_unary`]: a streaming request
/// gets an empty 200 (the other latch-arming shape), and the non-streaming
/// request that follows gets a complete HTTP head — including a
/// `content-length` promising far more than is sent — then a few body bytes
/// and silence, the socket held open.
///
/// The head arriving is the whole point: `send()` returns, so the stall lands
/// inside the body read instead. `connection: close` on the stream response
/// keeps the client from pooling that socket and sending the unary request
/// down a connection this server has stopped reading, which would move the
/// stall back into `send()` and prove nothing.
pub(crate) fn empty_streams_stall_the_unary_body(is_streaming: fn(&str) -> bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for conn in listener.incoming() {
            let Ok(mut socket) = conn else { break };
            let request = read_request(&mut socket);
            if is_streaming(&request) {
                let _ = socket.write_all(
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                      content-length: 0\r\nconnection: close\r\n\r\n",
                );
                let _ = socket.flush();
            } else {
                let _ = write!(
                    socket,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: 4096\r\n\r\n{{\"choices\":"
                );
                let _ = socket.flush();
                held.push(socket);
            }
        }
    });
    format!("http://{addr}")
}

/// Every request gets a complete HTTP head — including a `content-length`
/// promising far more than is sent — then a few body bytes and silence, the
/// socket held open.
///
/// The unary half of [`empty_streams_stall_the_unary_body`] with no streaming
/// arm at all, for an adapter that has no stream to fall back from: `bedrock`
/// calls Converse rather than ConverseStream, so it never sends a streaming
/// request for a predicate to recognise, and its unary read bound is the only
/// clock over the response.
pub(crate) fn stall_the_unary_body() -> String {
    empty_streams_stall_the_unary_body(|_| false)
}

/// The OpenAI-shaped dialects' streaming discriminator, which rides in the
/// request body (`zai`'s chat-completions, Anthropic's Messages, OpenAI's
/// Responses all spell it the same way).
pub(crate) fn stream_flag_in_body(request: &str) -> bool {
    request.contains("\"stream\":true")
}

/// The Google surfaces' streaming discriminator, which rides on the URL: the
/// two delivery paths name different methods (`:streamGenerateContent` vs
/// `:generateContent`) and send byte-identical bodies.
pub(crate) fn stream_method_in_url(request: &str) -> bool {
    request.contains(":streamGenerateContent")
}
