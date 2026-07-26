//! [`McpClient`]: the MCP protocol state machine over a [`Transport`]. It
//! runs the handshake (`initialize` → version negotiation →
//! `notifications/initialized`), discovers tools (`tools/list` with cursor
//! pagination), and invokes them (`tools/call`), translating MCP content
//! arrays into the engine's [`ToolOutput`].
//!
//! # Version negotiation
//!
//! The client offers [`PREFERRED_PROTOCOL_VERSION`] in `initialize` and
//! accepts whatever revision the server names back *if* it is one this client
//! can speak ([`SUPPORTED_PROTOCOL_VERSIONS`]); the negotiated version is
//! recorded ([`McpClient::negotiated_version`]). A server that names a
//! revision outside that set is a hard [`McpError::UnsupportedProtocol`] — a
//! client that guessed at an unknown wire format would be worse than one that
//! failed loudly.
//!
//! # Content mapping
//!
//! A `tools/call` result is a `content` array. `text` blocks are concatenated
//! (newline-joined); every non-text block is summarized as a compact
//! placeholder — `[image]`, `[audio]`, `[resource: <uri>]` — so a tool that
//! returns an image never floods the model context with base64, but the model
//! still learns *what* came back. `isError: true` maps to
//! [`ToolOutput::Error`]; a JSON-RPC error (a rejected request, distinct from
//! a tool that ran and failed) propagates as [`McpError::JsonRpc`].
//!
//! # Server-initiated traffic
//!
//! v1 is a pure client: it drives `initialize`/`tools/*` and ignores any
//! server-initiated request or notification (sampling, roots, progress). The
//! transport drops those silently.

mod health;
pub(crate) mod ingest;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use stella_protocol::ToolOutput;
use tokio::sync::Mutex;

use crate::config::{McpServerConfig, McpTransport};
use crate::error::McpError;
use crate::http::HttpTransport;
use crate::oauth::OAuthManager;
use crate::protocol::{
    CallToolParams, Implementation, InitializeParams, InitializeResult, PREFERRED_PROTOCOL_VERSION,
    SUPPORTED_PROTOCOL_VERSIONS, is_supported_version,
};
use crate::stdio::StdioTransport;
use crate::toolset::DEFAULT_CALL_TIMEOUT;
use crate::transport::Transport;

use health::{Connection, Health, is_connection_death};
use ingest::{decode_call_result, fetch_all_tools};

// The two submodules above hold what `client.rs` outgrew (#629's 1500-line
// ratchet). Every PUBLIC path is re-exported here, so no consumer's imports
// move; the crate-internal ingest budgets are addressed as
// `crate::client::ingest::*` (only `error` and tests want them).
pub use health::{HealthState, ServerHealth};
pub use ingest::{MAX_TOOLS_PER_SERVER, McpToolInfo, render_content};

/// Rebuilds a fresh (pre-handshake) transport for a server whose connection
/// dropped. Boxed so a [`McpClient`] can carry it without naming the concrete
/// spawn future; `None` means "no auto-reconnect" (the shape tests build via
/// [`McpClient::new`], which keeps the historical dead-stays-dead behavior).
type Reconnector = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Result<Box<dyn Transport>, McpError>> + Send>>
        + Send
        + Sync,
>;

/// The `clientInfo.name` advertised in `initialize`.
const CLIENT_NAME: &str = "stella";
/// The `clientInfo.version` advertised in `initialize`.
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A connected MCP client for a single server.
///
/// The transport lives behind a [`Mutex`] so that `call_tool(&self)` can
/// *replace* a dropped connection with a freshly-spawned one (auto-reconnect)
/// without a `&mut` at the call site. Everything set once at handshake time
/// (`name`, `tools`, `negotiated_version`, `server_info`) stays a plain field:
/// reconnect restores connectivity to the *same* advertised tool set, it does
/// not re-negotiate the surface mid-session.
pub struct McpClient {
    name: String,
    /// The live transport + reconnect bookkeeping. `None` transport = down.
    conn: Mutex<Connection>,
    /// How to rebuild a dropped transport. `None` = no auto-reconnect.
    reconnect: Option<Reconnector>,
    /// Per-call timeout (also the reconnect/handshake bound). Set at build
    /// time by [`crate::McpToolSet`]; a timed-out call is treated as a drop.
    call_timeout: Duration,
    negotiated_version: String,
    server_info: Option<Implementation>,
    tools: Vec<McpToolInfo>,
    /// How many advertised tools were refused past [`MAX_TOOLS_PER_SERVER`]
    /// (#551). Non-fatal: the server is live and every kept tool routes.
    dropped_tools: usize,
}

/// The outcome of one bounded transport request, classified so the caller can
/// react: a fast *drop* self-heals in-call, a *timeout* defers reconnect to
/// the next call, a *protocol* error is passed straight through (reconnecting
/// would not help — the server answered, it just answered badly).
enum RequestOutcome {
    Ok(Value),
    Dropped(McpError),
    Timeout(McpError),
    Protocol(McpError),
}

impl McpClient {
    /// Wrap a transport. Does **not** perform the handshake — call
    /// [`McpClient::initialize`] before using the client. (Splitting
    /// construction from the handshake is what lets tests drive a client over
    /// an in-memory transport.)
    pub fn new(name: impl Into<String>, transport: Box<dyn Transport>) -> Self {
        Self {
            name: name.into(),
            conn: Mutex::new(Connection {
                transport: Some(transport),
                health: Health::default(),
            }),
            reconnect: None,
            call_timeout: DEFAULT_CALL_TIMEOUT,
            negotiated_version: String::new(),
            server_info: None,
            tools: Vec::new(),
            dropped_tools: 0,
        }
    }

    /// Override the per-call timeout (default [`DEFAULT_CALL_TIMEOUT`]). Set by
    /// [`crate::McpToolSet`] when the set is assembled so every server shares one
    /// bound; a call that exceeds it is treated as a drop and schedules a
    /// reconnect.
    pub fn set_call_timeout(&mut self, timeout: Duration) {
        self.call_timeout = timeout;
    }

    /// Build the transport for `config`, then run the full handshake +
    /// tool discovery. `timeout` bounds the whole handshake and becomes the
    /// per-call timeout for every later [`McpClient::call_tool`].
    pub async fn connect(config: &McpServerConfig, timeout: Duration) -> Result<Self, McpError> {
        Self::connect_with_auth(config, timeout, None).await
    }

    /// [`McpClient::connect`], with an optional [`OAuthManager`] whose lazy
    /// per-server token source rides every HTTP transport (including each
    /// reconnect's fresh transport) — see [`crate::oauth`].
    pub async fn connect_with_auth(
        config: &McpServerConfig,
        timeout: Duration,
        auth: Option<Arc<OAuthManager>>,
    ) -> Result<Self, McpError> {
        let transport = build_transport(config, timeout, auth.as_deref()).await?;
        let mut client = McpClient::new(&config.name, transport);
        client.call_timeout = timeout;
        client.initialize().await?;
        // Retain a reconnector so a mid-session drop can be respawned from the
        // same config (with the same scrubbed environment for stdio servers).
        let cfg = config.clone();
        client.reconnect = Some(Arc::new(move || {
            let cfg = cfg.clone();
            let auth = auth.clone();
            Box::pin(async move { build_transport(&cfg, timeout, auth.as_deref()).await })
        }));
        Ok(client)
    }

    /// Run `initialize` → negotiate the version → `notifications/initialized`
    /// → `tools/list` (all pages). On success the client is ready for
    /// [`McpClient::call_tool`].
    ///
    /// Bounded by the per-call timeout, exactly like the reconnect handshake:
    /// a freshly-spawned server that accepts the connection and then never
    /// answers `initialize` must not hang the caller forever. (Nothing below
    /// this layer imposes a deadline on the stdio path — the transport just
    /// waits on its `oneshot` — so this is the only bound that exists.)
    pub async fn initialize(&mut self) -> Result<(), McpError> {
        let name = self.name.clone();
        let call_timeout = self.call_timeout;
        let handshake = {
            let conn = self.conn.get_mut();
            let transport = conn.transport.as_deref().ok_or_else(|| {
                McpError::Closed(format!("client `{name}` has no transport to initialize"))
            })?;
            tokio::time::timeout(call_timeout, run_handshake(&name, transport))
                .await
                .map_err(|_elapsed| {
                    McpError::Closed(format!(
                        "server `{name}` did not complete the handshake within {:.1}s",
                        call_timeout.as_secs_f64()
                    ))
                })??
        };
        self.negotiated_version = handshake.negotiated_version;
        self.server_info = handshake.server_info;
        self.tools = handshake.tools;
        self.dropped_tools = handshake.dropped_tools;
        // The handshake is itself a completed request round-trip
        // (`initialize` + `tools/list`), so a freshly-connected server is
        // legitimately `Live`.
        self.conn.get_mut().mark_call_success();
        Ok(())
    }

    /// Call `tool` with `arguments`, returning the mapped [`ToolOutput`].
    ///
    /// Resilience: the call is bounded by [`McpClient::set_call_timeout`]. If
    /// the connection has *dropped*, the transport is reconnected for the
    /// next call, but the request is transparently re-sent only when the tool
    /// advertises `readOnlyHint`/`idempotentHint` — a drop is ambiguous (the
    /// server may have executed the call before the connection died), so a
    /// non-idempotent tool surfaces a server-named ambiguity error instead of
    /// risking double execution. If the call *hangs* (timeout) or the
    /// reconnect is still backing off, a clear, server-named error is
    /// returned as data — the agent's turn is never aborted.
    pub async fn call_tool(&self, tool: &str, arguments: Value) -> Result<ToolOutput, McpError> {
        let params = CallToolParams {
            name: tool.to_string(),
            arguments: if arguments.is_null() {
                Value::Object(serde_json::Map::new())
            } else {
                arguments
            },
        };
        let raw_params = to_value(&params)?;

        let mut conn = self.conn.lock().await;

        // Already down from an earlier failure: try to reconnect first (a
        // fast, model-visible error if the backoff window has not elapsed).
        if conn.transport.is_none() {
            self.reconnect_locked(&mut conn).await?;
        }

        // `raw_params` is moved into the attempt rather than cloned: the retry
        // path below re-serializes from `params` instead, so the common case
        // (no drop) never deep-clones a tool input that may be megabytes of
        // file content.
        match self.request_once(&conn, "tools/call", raw_params).await {
            RequestOutcome::Ok(raw) => {
                conn.mark_call_success();
                decode_call_result(tool, raw)
            }
            // A hung call already burned the whole timeout; retrying now would
            // double the wait. Tear down (arming reconnect for the next call)
            // and surface the timeout as model-visible data.
            RequestOutcome::Timeout(err) => {
                conn.note_call_failure(&err);
                Err(err)
            }
            // The connection died fast. Reconnect (healing the transport for
            // the next call) and — only when the tool is advertised
            // read-only/idempotent — transparently re-send so a single blip
            // self-heals inside this turn. The drop is *ambiguous*: the
            // server may have executed the call before the connection died,
            // so re-sending a mutating tool risks double execution
            // (duplicate ticket, double insert). Those surface the drop as
            // model-visible data instead.
            RequestOutcome::Dropped(err) => {
                conn.note_call_failure(&err);
                let reconnected = self.reconnect_locked(&mut conn).await.is_ok();
                if !self.tool_is_retry_safe(tool) {
                    return Err(McpError::Transport(format!(
                        "connection to `{}` dropped mid-call; `{tool}` may or may not have \
                         executed and is not marked read-only/idempotent, so it was not \
                         re-sent automatically ({err})",
                        self.name
                    )));
                }
                if !reconnected {
                    return Err(err);
                }
                match self
                    .request_once(&conn, "tools/call", to_value(&params)?)
                    .await
                {
                    RequestOutcome::Ok(raw) => {
                        conn.mark_call_success();
                        decode_call_result(tool, raw)
                    }
                    RequestOutcome::Timeout(e2) | RequestOutcome::Dropped(e2) => {
                        conn.note_call_failure(&e2);
                        Err(e2)
                    }
                    RequestOutcome::Protocol(e2) => Err(e2),
                }
            }
            // A JSON-RPC / decode error: the server answered, just badly.
            // Reconnecting would not help, so pass it straight through.
            RequestOutcome::Protocol(err) => Err(err),
        }
    }

    /// Whether `tool` is advertised by the server as read-only or idempotent
    /// — the only cases where re-sending after an ambiguous connection drop
    /// cannot double a side effect. Unknown tools answer `false`.
    fn tool_is_retry_safe(&self, tool: &str) -> bool {
        self.tools
            .iter()
            .find(|t| t.name == tool)
            .is_some_and(|t| t.safe_to_retry)
    }

    /// One bounded request on the *current* transport, classified into a
    /// [`RequestOutcome`]. A timeout gets its own variant so the caller can
    /// tell "hung" from "dropped" and react differently.
    async fn request_once(&self, conn: &Connection, method: &str, params: Value) -> RequestOutcome {
        let Some(transport) = conn.transport.as_deref() else {
            return RequestOutcome::Dropped(McpError::Closed(format!(
                "server `{}` transport is closed",
                self.name
            )));
        };
        match tokio::time::timeout(self.call_timeout, transport.request(method, params)).await {
            Ok(Ok(raw)) => RequestOutcome::Ok(raw),
            Ok(Err(err)) if is_connection_death(&err) => RequestOutcome::Dropped(err),
            Ok(Err(err)) => RequestOutcome::Protocol(err),
            Err(_) => RequestOutcome::Timeout(McpError::Transport(format!(
                "server `{}` timed out after {}ms calling `{method}`",
                self.name,
                self.call_timeout.as_millis()
            ))),
        }
    }

    /// Rebuild a dropped transport, honoring the backoff clock. Fails fast
    /// (without spawning) when the backoff window has not elapsed, when there
    /// is no reconnector (test clients built via [`McpClient::new`]), or when
    /// the fresh handshake fails — each path arms the clock for the next try.
    ///
    /// On success the connection is trusted again but the server is **not**
    /// declared [`HealthState::Live`] unless its calls were already healthy:
    /// see [`Connection::mark_connected`] and the #638 contract on
    /// [`HealthState`].
    async fn reconnect_locked(&self, conn: &mut Connection) -> Result<(), McpError> {
        if let Some(at) = conn.health.next_retry_at {
            let now = Instant::now();
            if now < at {
                return Err(McpError::Closed(format!(
                    "server `{}` is down; next reconnect attempt in {:.1}s ({})",
                    self.name,
                    (at - now).as_secs_f64(),
                    conn.health
                        .last_error
                        .as_deref()
                        .unwrap_or("connection lost"),
                )));
            }
        }
        let Some(reconnect) = self.reconnect.as_ref() else {
            return Err(McpError::Closed(format!(
                "server `{}` connection is closed and has no reconnect source",
                self.name
            )));
        };

        conn.health.state = HealthState::Reconnecting;
        let transport = match reconnect().await {
            Ok(transport) => transport,
            Err(err) => {
                conn.note_connect_failure(&err);
                return Err(err);
            }
        };
        // The fresh transport must pass the full handshake before we trust it;
        // we keep the *original* advertised tool set (reconnect restores
        // connectivity, it does not re-negotiate the surface mid-session).
        //
        // Bound the handshake by `call_timeout`: a respawned server that never
        // answers `initialize` would otherwise hang here forever while holding
        // the connection mutex, wedging the agent turn and every `health()`
        // snapshot — the exact timeout guarantee this path is supposed to keep.
        match tokio::time::timeout(
            self.call_timeout,
            run_handshake(&self.name, transport.as_ref()),
        )
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => {
                conn.note_connect_failure(&err);
                return Err(err);
            }
            Err(_elapsed) => {
                let err = McpError::Closed(format!(
                    "server `{}` did not complete the reconnect handshake within {:.1}s",
                    self.name,
                    self.call_timeout.as_secs_f64(),
                ));
                conn.note_connect_failure(&err);
                return Err(err);
            }
        }
        conn.transport = Some(transport);
        conn.mark_connected();
        Ok(())
    }

    /// Orderly shutdown of the underlying transport (best-effort; an
    /// already-down connection is a no-op).
    pub async fn close(&self) -> Result<(), McpError> {
        let conn = self.conn.lock().await;
        match conn.transport.as_deref() {
            Some(transport) => transport.close().await,
            None => Ok(()),
        }
    }

    /// A point-in-time snapshot of this server's connection health, so the
    /// CLI/TUI/telemetry can render a clear, non-fatal diagnostic when a
    /// server drops and while it is reconnecting. See [`HealthState`] for what
    /// each state promises — in particular, `Live` means the server *answered*,
    /// not merely that it accepted a connection.
    pub async fn health(&self) -> ServerHealth {
        let conn = self.conn.lock().await;
        ServerHealth {
            name: self.name.clone(),
            state: conn.health.state,
            call_failures: conn.health.call_failures,
            connect_failures: conn.health.connect_failures,
            last_error: conn.health.last_error.clone(),
            retry_in: conn.retry_in(),
        }
    }

    /// The configured server name (the namespace segment for its tools).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The negotiated protocol revision (empty until `initialize` succeeds).
    pub fn negotiated_version(&self) -> &str {
        &self.negotiated_version
    }

    /// The server's advertised implementation info, if it sent any.
    pub fn server_info(&self) -> Option<&Implementation> {
        self.server_info.as_ref()
    }

    /// The tools discovered during `initialize` (raw, un-namespaced).
    pub fn tools(&self) -> &[McpToolInfo] {
        &self.tools
    }

    /// How many advertised tools were refused because the server exceeded
    /// this crate's per-server tool cap ([`MAX_TOOLS_PER_SERVER`], #551). `0`
    /// for every well-behaved server.
    ///
    /// This is a **floor**, not a total: discovery stops on the page where the
    /// cap is reached, so anything the server would have listed on later pages
    /// is never counted (or fetched). It is a non-fatal diagnostic — unlike
    /// [`crate::McpToolSet::failed_servers`], the server is connected and its
    /// kept tools route normally.
    ///
    /// Reconnecting does not re-open this question: a reconnect restores
    /// connectivity to the tool set negotiated at the first handshake, so this
    /// count describes that same surface for the life of the client.
    pub fn dropped_tool_count(&self) -> usize {
        self.dropped_tools
    }
}

#[cfg(test)]
impl McpClient {
    /// Attach a reconnect factory for tests, so reconnect can be exercised
    /// over in-memory transports without spawning a real process. Each call to
    /// `f` must yield a *fresh* pre-handshake transport.
    pub(crate) fn with_test_reconnector<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Box<dyn Transport>, McpError>> + Send + 'static,
    {
        self.reconnect = Some(Arc::new(move || Box::pin(f())));
        self
    }
}

/// The negotiated result of a handshake, produced by [`run_handshake`] and
/// used by both the initial [`McpClient::initialize`] and each reconnect.
struct Handshake {
    negotiated_version: String,
    server_info: Option<Implementation>,
    tools: Vec<McpToolInfo>,
    /// Tools refused past [`MAX_TOOLS_PER_SERVER`] (#551) — see
    /// [`McpClient::dropped_tool_count`].
    dropped_tools: usize,
}

/// Build a fresh (pre-handshake) transport for `config`. Shared by the first
/// [`McpClient::connect`] and every later reconnect so both spawn the child
/// with the identical (scrubbed, for stdio) environment. When an
/// [`OAuthManager`] is supplied, every HTTP transport gets its lazy
/// per-server token source (a no-op until a login is stored).
async fn build_transport(
    config: &McpServerConfig,
    timeout: Duration,
    auth: Option<&OAuthManager>,
) -> Result<Box<dyn Transport>, McpError> {
    Ok(match &config.transport {
        McpTransport::Stdio { cmd, args, env } => {
            Box::new(StdioTransport::spawn(&config.name, cmd, args, env).await?)
        }
        McpTransport::Http { url, headers } => {
            let mut transport = HttpTransport::new(&config.name, url, headers, timeout)?;
            if let Some(manager) = auth {
                transport = transport.with_bearer_source(manager.source_for(&config.name));
            }
            Box::new(transport)
        }
    })
}

/// Run `initialize` → negotiate the version → `notifications/initialized` →
/// `tools/list` (all pages) over a fresh transport, returning the negotiated
/// surface. Free-standing so both the first handshake and reconnect share one
/// implementation.
async fn run_handshake(name: &str, transport: &dyn Transport) -> Result<Handshake, McpError> {
    let params = InitializeParams {
        protocol_version: PREFERRED_PROTOCOL_VERSION.to_string(),
        capabilities: serde_json::json!({}),
        client_info: Implementation {
            name: CLIENT_NAME.to_string(),
            version: CLIENT_VERSION.to_string(),
        },
    };
    let raw = transport.request("initialize", to_value(&params)?).await?;
    let result: InitializeResult = serde_json::from_value(raw)
        .map_err(|e| McpError::Protocol(format!("could not decode initialize result: {e}")))?;

    // Negotiate: accept the server's version if we can speak it; a server that
    // omits the field is read leniently as agreeing to our offer.
    let version = if result.protocol_version.is_empty() {
        PREFERRED_PROTOCOL_VERSION.to_string()
    } else {
        result.protocol_version
    };
    if !is_supported_version(&version) {
        return Err(McpError::UnsupportedProtocol {
            offered: version,
            supported: SUPPORTED_PROTOCOL_VERSIONS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        });
    }

    // Complete the handshake, then discover tools.
    transport
        .notify("notifications/initialized", Value::Null)
        .await?;
    let (tools, dropped_tools) = fetch_all_tools(name, transport).await?;
    Ok(Handshake {
        negotiated_version: version,
        server_info: result.server_info,
        tools,
        dropped_tools,
    })
}

/// Serialize a params struct, mapping any (unexpected) failure to a protocol
/// error rather than panicking.
fn to_value<T: serde::Serialize>(value: &T) -> Result<Value, McpError> {
    serde_json::to_value(value).map_err(|e| McpError::Protocol(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::testkit::ScriptedTransport;

    #[tokio::test]
    async fn initialize_negotiates_version_and_lists_tools() {
        let transport = ScriptedTransport::new();
        transport.push_ok(
            "initialize",
            serde_json::json!({
                "protocolVersion": PREFERRED_PROTOCOL_VERSION,
                "serverInfo": { "name": "fixture", "version": "1.0" }
            }),
        );
        transport.push_ok(
            "tools/list",
            serde_json::json!({ "tools": [{ "name": "echo", "inputSchema": { "type": "object" } }] }),
        );
        let mut client = McpClient::new("srv", Box::new(transport));
        client.initialize().await.unwrap();

        assert_eq!(client.negotiated_version(), PREFERRED_PROTOCOL_VERSION);
        assert_eq!(client.server_info().unwrap().name, "fixture");
        assert_eq!(client.tools().len(), 1);
        assert_eq!(client.tools()[0].name, "echo");
    }

    #[tokio::test]
    async fn initialize_accepts_an_older_counter_offer() {
        let transport = ScriptedTransport::new();
        transport.push_ok(
            "initialize",
            serde_json::json!({ "protocolVersion": "2024-11-05" }),
        );
        transport.push_ok("tools/list", serde_json::json!({ "tools": [] }));
        let mut client = McpClient::new("srv", Box::new(transport));
        client.initialize().await.unwrap();
        assert_eq!(client.negotiated_version(), "2024-11-05");
    }

    #[tokio::test]
    async fn initialize_rejects_an_unspeakable_version() {
        let transport = ScriptedTransport::new();
        transport.push_ok(
            "initialize",
            serde_json::json!({ "protocolVersion": "1999-01-01" }),
        );
        let mut client = McpClient::new("srv", Box::new(transport));
        let err = client.initialize().await.unwrap_err();
        assert!(matches!(err, McpError::UnsupportedProtocol { .. }));
    }

    #[tokio::test]
    async fn tools_list_follows_pagination_cursors() {
        let transport = ScriptedTransport::new();
        transport.push_ok(
            "initialize",
            serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
        );
        transport.push_ok(
            "tools/list",
            serde_json::json!({ "tools": [{ "name": "a" }], "nextCursor": "p2" }),
        );
        transport.push_ok(
            "tools/list",
            serde_json::json!({ "tools": [{ "name": "b" }, { "name": "c" }] }),
        );
        let mut client = McpClient::new("srv", Box::new(transport));
        client.initialize().await.unwrap();
        let names: Vec<&str> = client.tools().iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c"]);
    }

    #[tokio::test]
    async fn initialized_notification_is_sent() {
        let transport = ScriptedTransport::new();
        transport.push_ok(
            "initialize",
            serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
        );
        transport.push_ok("tools/list", serde_json::json!({ "tools": [] }));
        let notes = transport.notifications_handle();
        let mut client = McpClient::new("srv", Box::new(transport));
        client.initialize().await.unwrap();
        let sent = notes.lock().unwrap();
        assert!(sent.iter().any(|(m, _)| m == "notifications/initialized"));
    }

    #[tokio::test]
    async fn call_tool_maps_ok_and_is_error() {
        let transport = ScriptedTransport::new();
        transport.push_ok(
            "tools/call",
            serde_json::json!({ "content": [{ "type": "text", "text": "done" }] }),
        );
        transport.push_ok(
            "tools/call",
            serde_json::json!({ "content": [{ "type": "text", "text": "boom" }], "isError": true }),
        );
        let client = McpClient::new("srv", Box::new(transport));

        let ok = client.call_tool("t", serde_json::json!({})).await.unwrap();
        assert_eq!(
            ok,
            ToolOutput::Ok {
                content: "done".into()
            }
        );

        let err = client.call_tool("t", serde_json::json!({})).await.unwrap();
        assert_eq!(
            err,
            ToolOutput::Error {
                message: "boom".into()
            }
        );
    }

    #[tokio::test]
    async fn call_tool_propagates_a_jsonrpc_error() {
        let transport = ScriptedTransport::new();
        transport.push_err(
            "tools/call",
            McpError::JsonRpc {
                code: -32602,
                message: "unknown tool".into(),
                data: None,
            },
        );
        let client = McpClient::new("srv", Box::new(transport));
        let err = client.call_tool("t", Value::Null).await.unwrap_err();
        assert!(matches!(err, McpError::JsonRpc { code: -32602, .. }));
    }

    /// Build a fresh, fully-healthy scripted transport (handshake + one queued
    /// `tools/call` success) — the shape a reconnect factory yields. The
    /// `echo` tool is advertised read-only so the transparent post-drop
    /// retry is permitted.
    fn healthy_transport() -> Box<dyn Transport> {
        let t = ScriptedTransport::new();
        t.push_ok(
            "initialize",
            serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
        );
        t.push_ok(
            "tools/list",
            serde_json::json!({ "tools": [{ "name": "echo", "inputSchema": { "type": "object" },
                "annotations": { "readOnlyHint": true } }] }),
        );
        t.push_ok(
            "tools/call",
            serde_json::json!({ "content": [{ "type": "text", "text": "healed" }] }),
        );
        Box::new(t)
    }

    #[tokio::test]
    async fn a_dropped_connection_transparently_reconnects_and_retries_read_only_tools() {
        // Initial transport: handshake succeeds, then the very first tools/call
        // drops the connection (child exited). `echo` is advertised read-only,
        // so re-sending after the ambiguous drop is safe.
        let initial = ScriptedTransport::new();
        initial.push_ok(
            "initialize",
            serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
        );
        initial.push_ok(
            "tools/list",
            serde_json::json!({ "tools": [{ "name": "echo", "inputSchema": { "type": "object" },
                "annotations": { "readOnlyHint": true } }] }),
        );
        initial.push_err("tools/call", McpError::Closed("child exited".into()));

        let mut client = McpClient::new("srv", Box::new(initial))
            .with_test_reconnector(|| async { Ok(healthy_transport()) });
        client.initialize().await.unwrap();

        // The dropped call self-heals: reconnect + retry happen inside the one
        // call, so the model sees success, not an error.
        let out = client
            .call_tool("echo", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(
            out,
            ToolOutput::Ok {
                content: "healed".into()
            }
        );

        let health = client.health().await;
        assert_eq!(health.state, HealthState::Live);
        assert_eq!(health.call_failures, 0);
        assert_eq!(health.connect_failures, 0);
        assert_eq!(health.consecutive_failures(), 0);
        assert!(health.last_error.is_none());
    }

    /// Witness for #638's first finding. A server that accepts every
    /// connection and then drops every `tools/call` used to report `Live` with
    /// a zeroed failure streak forever: the successful reconnect called
    /// `mark_healthy()`, which wiped the call failures too, so the deck said
    /// healthy while every call failed AND the backoff never grew past zero
    /// (the server was respawned once per call, indefinitely).
    #[tokio::test]
    async fn a_server_that_reconnects_but_fails_every_call_is_not_live_and_backs_off() {
        /// Handshakes fine, advertises a read-only tool (so the transparent
        /// retry is allowed), then drops the connection on `tools/call`.
        fn accepts_but_drops_calls() -> Box<dyn Transport> {
            let t = ScriptedTransport::new();
            t.push_ok(
                "initialize",
                serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
            );
            t.push_ok(
                "tools/list",
                serde_json::json!({ "tools": [{ "name": "echo", "inputSchema": { "type": "object" },
                    "annotations": { "readOnlyHint": true } }] }),
            );
            t.push_err("tools/call", McpError::Closed("child exited".into()));
            Box::new(t)
        }

        let mut client = McpClient::new("flaky", accepts_but_drops_calls())
            .with_test_reconnector(|| async { Ok(accepts_but_drops_calls()) });
        client.initialize().await.unwrap();
        // The handshake is a completed round-trip, so a fresh connect IS Live.
        assert_eq!(client.health().await.state, HealthState::Live);

        // One call: it drops, the reconnect *succeeds*, the retry drops again.
        client
            .call_tool("echo", serde_json::json!({}))
            .await
            .expect_err("every call on this server fails");

        let health = client.health().await;
        assert_eq!(
            health.state,
            HealthState::Down,
            "a server whose every call fails must not read as Live just because \
             it accepts connections"
        );
        assert_eq!(
            health.call_failures, 2,
            "the dropped call and its retry both count"
        );
        assert_eq!(
            health.connect_failures, 0,
            "the reconnect itself keeps succeeding — the two are tracked apart"
        );
        assert!(
            health.consecutive_failures() >= 2,
            "the combined streak is what drives the backoff"
        );
        assert!(
            health.retry_in.is_some_and(|d| d > Duration::ZERO),
            "the backoff clock is armed, so the next call does not respawn \
             immediately: {:?}",
            health.retry_in
        );
        assert!(health.last_error.is_some(), "and the cause is retained");
    }

    #[tokio::test]
    async fn a_reconnect_whose_handshake_hangs_times_out_instead_of_wedging() {
        // Initial transport handshakes, advertises a read-only tool, then the
        // first call drops (child exited) → reconnect fires.
        let initial = ScriptedTransport::new();
        initial.push_ok(
            "initialize",
            serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
        );
        initial.push_ok(
            "tools/list",
            serde_json::json!({ "tools": [{ "name": "echo", "inputSchema": { "type": "object" },
                "annotations": { "readOnlyHint": true } }] }),
        );
        initial.push_err("tools/call", McpError::Closed("child exited".into()));

        // The respawned server never answers `initialize`.
        struct HangHandshake;
        #[async_trait::async_trait]
        impl Transport for HangHandshake {
            async fn request(&self, _method: &str, _params: Value) -> Result<Value, McpError> {
                std::future::pending::<()>().await;
                unreachable!()
            }
            async fn notify(&self, _method: &str, _params: Value) -> Result<(), McpError> {
                Ok(())
            }
            async fn close(&self) -> Result<(), McpError> {
                Ok(())
            }
        }

        let mut client = McpClient::new("srv", Box::new(initial))
            .with_test_reconnector(|| async { Ok(Box::new(HangHandshake) as Box<dyn Transport>) });
        client.initialize().await.unwrap();
        client.set_call_timeout(Duration::from_millis(40));

        // The witness: the whole call must COMPLETE quickly. Before the fix the
        // reconnect handshake awaited unbounded, so this call hung forever and
        // the outer guard below would fire. (call_tool surfaces the original
        // drop error rather than the handshake-timeout message, which is fine —
        // "did not wedge" is the property under test.)
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            client.call_tool("echo", serde_json::json!({})),
        )
        .await
        .expect("reconnect handshake must not hang the call");
        result.expect_err("a hung reconnect handshake surfaces an error, not success");
        assert_eq!(client.health().await.state, HealthState::Down);
    }

    #[tokio::test]
    async fn a_dropped_call_on_a_mutating_tool_is_not_resent() {
        // `send` carries no readOnlyHint/idempotentHint: the drop is
        // ambiguous (the server may have executed the call before the
        // connection died), so the client must NOT re-send it — at-most-once
        // for side-effecting tools. The reconnect still heals the transport
        // for the *next* call.
        let initial = ScriptedTransport::new();
        initial.push_ok(
            "initialize",
            serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
        );
        initial.push_ok(
            "tools/list",
            serde_json::json!({ "tools": [{ "name": "send", "inputSchema": { "type": "object" } }] }),
        );
        initial.push_err("tools/call", McpError::Closed("child exited".into()));

        let healed = ScriptedTransport::new();
        healed.push_ok(
            "initialize",
            serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
        );
        healed.push_ok(
            "tools/list",
            serde_json::json!({ "tools": [{ "name": "send", "inputSchema": { "type": "object" } }] }),
        );
        healed.push_ok(
            "tools/call",
            serde_json::json!({ "content": [{ "type": "text", "text": "second call" }] }),
        );
        let healed: std::sync::Mutex<Option<Box<dyn Transport>>> =
            std::sync::Mutex::new(Some(Box::new(healed)));

        let mut client =
            McpClient::new("srv", Box::new(initial)).with_test_reconnector(move || {
                let t = healed.lock().unwrap().take().expect("one reconnect");
                async move { Ok(t) }
            });
        client.initialize().await.unwrap();

        // The dropped mutating call surfaces as an error naming the ambiguity…
        let err = client
            .call_tool("send", serde_json::json!({}))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("may or may not have executed"),
            "error must explain the ambiguity: {msg}"
        );

        // …while the healed transport still has its queued response, proving
        // the client did not consume it with an automatic re-send; the next
        // call succeeds on the reconnected transport.
        let out = client
            .call_tool("send", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(
            out,
            ToolOutput::Ok {
                content: "second call".into()
            }
        );
    }

    #[tokio::test]
    async fn repeated_failures_back_off_and_fail_fast_without_blocking() {
        let initial = ScriptedTransport::new();
        initial.push_ok(
            "initialize",
            serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }),
        );
        initial.push_ok(
            "tools/list",
            serde_json::json!({ "tools": [{ "name": "echo", "inputSchema": { "type": "object" } }] }),
        );
        initial.push_err("tools/call", McpError::Closed("child exited".into()));

        // Reconnect always fails — the server is genuinely dead.
        let mut client = McpClient::new("srv", Box::new(initial)).with_test_reconnector(|| async {
            Err(McpError::Transport("connection refused".into()))
        });
        client.initialize().await.unwrap();

        // First call drops, then the in-call reconnect also fails: an error.
        assert!(
            client
                .call_tool("echo", serde_json::json!({}))
                .await
                .is_err()
        );

        // The backoff clock is now armed, so the next call fails *fast* — it
        // never spawns or blocks the turn — and names the wait.
        let err = client
            .call_tool("echo", serde_json::json!({}))
            .await
            .unwrap_err();
        let msg = err.user_message();
        assert!(msg.contains("is down"), "fast-fail names the state: {msg}");
        assert!(
            msg.contains("next reconnect attempt"),
            "hints at the backoff wait: {msg}"
        );

        // The health contract (#638), stated as numbers: this server failed one
        // CALL and then could not be re-established at all, so the two counters
        // move independently and the backoff streak is their sum. (The old
        // single `consecutive_failures` field asserted `>= 2` here — the same
        // property, but it could not distinguish this genuinely-unreachable
        // server from one whose reconnects succeed and whose calls all fail,
        // which is exactly the case that used to report `Live`.)
        let health = client.health().await;
        assert_eq!(health.state, HealthState::Down);
        assert_eq!(health.call_failures, 1, "the one `tools/call` that dropped");
        assert_eq!(
            health.connect_failures, 1,
            "plus the reconnect that could not be re-established"
        );
        assert!(
            health.consecutive_failures() >= 2,
            "the combined streak is what armed the backoff: {}",
            health.consecutive_failures()
        );
        assert!(health.last_error.is_some());
    }

    #[tokio::test]
    async fn a_hung_call_times_out_and_arms_reconnect_without_retrying_in_call() {
        // A transport that answers the handshake but hangs forever on any call.
        struct Hang;
        #[async_trait::async_trait]
        impl Transport for Hang {
            async fn request(&self, method: &str, _params: Value) -> Result<Value, McpError> {
                match method {
                    "initialize" => {
                        Ok(serde_json::json!({ "protocolVersion": PREFERRED_PROTOCOL_VERSION }))
                    }
                    "tools/list" => Ok(serde_json::json!({
                        "tools": [{ "name": "slow", "inputSchema": { "type": "object" } }]
                    })),
                    _ => {
                        std::future::pending::<()>().await;
                        unreachable!()
                    }
                }
            }
            async fn notify(&self, _method: &str, _params: Value) -> Result<(), McpError> {
                Ok(())
            }
            async fn close(&self) -> Result<(), McpError> {
                Ok(())
            }
        }

        let mut client = McpClient::new("slow", Box::new(Hang));
        client.initialize().await.unwrap();
        client.set_call_timeout(Duration::from_millis(30));

        // The hung call times out (non-fatal, server-named) and does NOT
        // double-wait on an in-call retry — it just arms reconnect.
        let err = client
            .call_tool("slow", serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(
            err.user_message().contains("timed out"),
            "{}",
            err.user_message()
        );

        let health = client.health().await;
        assert_eq!(health.state, HealthState::Down);
        assert_eq!(health.call_failures, 1);
        assert_eq!(
            health.connect_failures, 0,
            "a hung call is a CALL failure — no reconnect was attempted"
        );
    }
}
