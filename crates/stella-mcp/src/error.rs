//! Typed errors for the MCP client (fail loud,
//! never `panic!` in the hot path). Every fallible operation in this crate
//! returns [`McpError`]; the boundary that faces `stella-core` — the engine's
//! [`crate::McpToolSet`] — maps each of these onto a model-visible
//! `ToolOutput::Error` so a broken server is *data*, never an engine crash.

use thiserror::Error;

use crate::client::ingest::MAX_TOOL_RESULT_BYTES;
use crate::http::truncate_middle_out;

/// Everything that can go wrong talking to an MCP server.
#[derive(Debug, Error)]
pub enum McpError {
    /// Transport-level failure: the child process could not be spawned, the
    /// HTTP request failed, stdin/stdout broke, or the stream ended
    /// unexpectedly.
    #[error("transport error: {0}")]
    Transport(String),

    /// The server sent a JSON-RPC error object in response to a request.
    /// `code`/`message` are the server's own; `data` is passed through
    /// verbatim for diagnostics.
    #[error("json-rpc error {code}: {message}")]
    JsonRpc {
        code: i64,
        message: String,
        data: Option<serde_json::Value>,
    },

    /// A response arrived but could not be decoded into the shape the
    /// protocol requires (bad JSON, a `result` that isn't the expected
    /// object, a JSON-RPC message missing both `result` and `error`).
    #[error("malformed protocol message: {0}")]
    Protocol(String),

    /// The server negotiated a protocol revision this client cannot speak.
    #[error("unsupported MCP protocol version {offered:?} (this client speaks {supported:?})")]
    UnsupportedProtocol {
        offered: String,
        supported: Vec<String>,
    },

    /// The transport is closed — the child exited, or `close()` was already
    /// called. A subsequent request cannot be served.
    #[error("connection closed: {0}")]
    Closed(String),

    /// Configuration could not be parsed (bad TOML, unknown transport shape).
    #[error("invalid MCP configuration: {0}")]
    Config(String),

    /// OAuth authorization failed: discovery, registration, the browser
    /// round-trip, a token exchange, or a refresh. The message never carries
    /// a token value — only endpoint URLs and the server's own error codes.
    #[error("authorization error: {0}")]
    Auth(String),
}

impl McpError {
    /// A credential-free, **length-bounded** summary safe to surface to the
    /// model as a `ToolOutput::Error` message. (No `McpError` variant ever
    /// carries a secret — credentials are never logged nor forwarded,
    /// §8 — so this is the `Display` form, named for intent.)
    ///
    /// # Bounded, because half of this string is the server's
    ///
    /// Several variants interpolate text the *server* chose and this client
    /// only relayed: [`McpError::JsonRpc`]'s `message` is the wire error
    /// object's own, and [`McpError::Transport`]/[`McpError::Protocol`] wrap
    /// decoder and HTTP-body text. That makes the rendered string untrusted
    /// input on the same footing as a `tools/call` result, and it reaches the
    /// model through exactly the same door — `McpToolSet::execute_mcp` turns
    /// this into a `ToolOutput::Error`.
    ///
    /// [`crate::client`]'s result cap (#551) sits in `decode_call_result`,
    /// which a JSON-RPC *error* response never reaches — the request fails
    /// before it, so the cap there bounds only the `isError: true` content
    /// route. This is the seam that covers the rest, at the same
    /// `MAX_TOOL_RESULT_BYTES` budget and with the same middle-out elision
    /// marker, so a server cannot evict the conversation by answering
    /// `tools/call` with a gigabyte-long error message instead of a
    /// gigabyte-long result. An under-budget message is returned verbatim.
    pub fn user_message(&self) -> String {
        let rendered = self.to_string();
        // Under budget stays byte-identical (and pays for no second pass) —
        // only a hostile or badly-behaved server's message is rebuilt.
        if rendered.len() > MAX_TOOL_RESULT_BYTES {
            truncate_middle_out(&rendered, MAX_TOOL_RESULT_BYTES)
        } else {
            rendered
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_under_budget_message_is_the_display_form_verbatim() {
        let err = McpError::JsonRpc {
            code: -32602,
            message: "unknown argument `pth`".into(),
            data: None,
        };
        assert_eq!(err.user_message(), err.to_string());
        assert_eq!(
            err.user_message(),
            "json-rpc error -32602: unknown argument `pth`"
        );
    }

    /// The bound is on the *rendered* string, so it holds for every variant
    /// that relays server-chosen text — not just [`McpError::JsonRpc`].
    #[test]
    fn every_variant_relaying_server_text_is_bounded() {
        let flood = "Z".repeat(MAX_TOOL_RESULT_BYTES * 4);
        let cases = [
            McpError::JsonRpc {
                code: -32000,
                message: flood.clone(),
                data: None,
            },
            McpError::Transport(flood.clone()),
            McpError::Protocol(flood.clone()),
            McpError::Closed(flood.clone()),
            McpError::Auth(flood),
        ];
        for err in cases {
            let msg = err.user_message();
            assert!(
                msg.len() < MAX_TOOL_RESULT_BYTES + 128,
                "{err:?} rendered {} bytes",
                msg.len()
            );
            assert!(
                msg.contains("... [truncated ") && msg.contains(" bytes] ..."),
                "the elision is explicit and counted"
            );
        }
    }
}
