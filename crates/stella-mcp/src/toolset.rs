//! [`McpToolSet`]: the bridge that makes external MCP servers' tools
//! indistinguishable from native tools to the engine. It implements
//! `stella_core::ports::ToolExecutor` — the same port `stella-tools`'
//! `ToolRegistry` implements — so `stella-core::Engine` drives MCP tools and
//! built-in tools through one seam, exactly as `driver.rs` consumes the trait.
//!
//! # Namespacing
//!
//! Every MCP tool is advertised as `mcp__<server>__<tool>` — composed by
//! [`wire_name`], the one place the encoding lives — so tools from different
//! servers (and from the native set) never collide. Server names are required
//! to be non-empty, free of the `__` separator, and free of a leading or
//! trailing `_`; a server whose name violates that is skipped and recorded in
//! [`McpToolSet::failed_servers`]. Those rules make the encoding **injective**
//! (see [`wire_name`]), so the prefix uniquely identifies the server, the
//! remainder is the raw tool name, and routing is unambiguous.
//!
//! Injectivity is also enforced, not merely argued: if two `(server, tool)`
//! pairs ever map to one wire name anyway, route building drops **every**
//! claimant's route and records the clash in
//! [`McpToolSet::wire_name_collisions`], so connect order can never silently
//! pick which third-party server answers a call (#2675).
//!
//! # Composition & fall-through
//!
//! [`McpToolSet::wrapping`] layers the set over an inner native
//! `ToolExecutor`. `execute` routes an `mcp__…` name to its server; any other
//! name falls through to the native executor. An `mcp__…` name that matches no
//! connected server's tool is a model-visible error, never a panic — matching
//! `ToolRegistry`'s contract.
//!
//! # Resilience
//!
//! Every MCP call is bounded by a per-call timeout ([`McpClient`] owns it, so
//! a hang is observable rather than merely cancelled). A dead, hung, or
//! erroring server yields a `ToolOutput::Error` that *names the server*; it
//! never poisons sibling servers or the native tools, and it is never an `Err`
//! out of `execute` (tool failures are model-visible data).
//!
//! **Auto-reconnect (bounded backoff).** A server that drops mid-session is
//! not written off: the next call transparently respawns it (a single blip
//! self-heals within the turn), and repeated failures back off on an
//! increasing, capped interval so a long-dead server degrades gracefully
//! instead of aborting the agent. Per-server state is surfaced by
//! [`McpToolSet::health`] for a non-fatal CLI/TUI/telemetry diagnostic.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use stella_core::mcp_usage::{McpUsageLedger, McpUsageRecord, push_usage};
use stella_core::ports::ToolExecutor;
use stella_protocol::{ToolOutput, ToolSchema};

use crate::client::{McpClient, McpToolInfo, ServerHealth};
use crate::config::{McpServerConfig, McpTransport};
use crate::oauth::OAuthManager;
use crate::suppress::{ConnectGate, connect_gate, unix_now_secs};

mod needs_auth;
mod resources;
pub use needs_auth::login_required_tool_name;
pub use resources::{list_resources_tool_name, read_resource_tool_name};

/// A session-scoped set of server names disabled by the operator. Shared with
/// the CLI/TUI so a toggle takes effect on the next model call — the engine
/// re-reads [`McpToolSet::schemas`] each call, so a disabled server's tools
/// simply disappear from the advertised set (and any stray call errors).
pub type DisabledServers = Arc<Mutex<HashSet<String>>>;

/// The tool-namespace prefix.
const NS_PREFIX: &str = "mcp__";
/// The separator between the `mcp__`, server, and tool segments.
const NS_SEP: &str = "__";
/// Default per-call (and per-connect) timeout when the caller does not set one.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Byte budget for ONE server's contribution to the advertised toolset.
///
/// Ingest already caps each tool individually — `MAX_TOOLS_PER_SERVER` (256),
/// `MAX_TOOL_DESCRIPTION_CHARS` (2,000) and a per-tool `inputSchema` budget in
/// `crate::client::ingest`. What none of them bounds is the **aggregate**:
/// 256 tools times a 2,000-character description is ~512 KB of third-party
/// prose from a single server, at position 0 of every request, before its
/// schemas are counted (#1856).
///
/// Every per-tool cap can hold and the block still exceed the whole
/// recall+memory+rules budget combined, because the caps are per-item and the
/// cost is the sum. 32 KB is roughly 9k tokens: generous for the servers
/// people actually run (a handful of tools each), and a wall for the one that
/// advertises a catalogue.
///
/// Applied to the SORTED segment, so which tools survive is a deterministic
/// function of their names rather than of client order — the byte-stability
/// contract in [`McpToolSet::schemas`] would be worthless if the truncation
/// point moved between processes.
pub const MAX_SERVER_SCHEMA_BYTES: usize = 32 * 1024;

/// What a connected server said about itself during the `initialize`
/// handshake — its own name (which need not match the local alias), version,
/// display title, and free-prose `instructions`.
///
/// Every field is untrusted third-party text: it is rendered to the operator
/// and never fed to the model.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerIdentity {
    /// The server's self-reported name, e.g. `stripe-mcp`. Distinct from the
    /// local alias — this is what the *server* calls itself.
    pub name: Option<String>,
    pub version: Option<String>,
    /// Human display name (MCP 2025-06-18+), when advertised.
    pub title: Option<String>,
    pub website_url: Option<String>,
    /// The handshake `instructions` blob, trimmed-empty filtered out.
    pub instructions: Option<String>,
    /// The protocol revision this session negotiated with it.
    pub protocol_version: String,
}

/// One wire name claimed by more than one `(server, tool)` pair (#2675).
///
/// The `namespace_rejection` rules make [`wire_name`] injective, so this should
/// never occur — but MCP servers are third-party and their tool lists are
/// untrusted, and a misroute here delivers a call meant for one server to a
/// different one. When it does occur, **every** claimant's route is dropped
/// (never a connect-order winner) and this record is the operator-visible
/// diagnostic, in the [`McpToolSet::failed_servers`] /
/// [`McpToolSet::over_advertising_servers`] style: bounded, named, non-fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireNameCollision {
    /// The contested `mcp__…` name. Not advertised and not callable this
    /// session.
    pub wire_name: String,
    /// Every `(server, tool)` pair that encodes to `wire_name`, in client
    /// connect order. Always at least two entries, always distinct servers
    /// (one server's duplicate tool names are deduped at ingest).
    pub claimants: Vec<(String, String)>,
}

/// A set of connected MCP servers exposed to the engine as one
/// `ToolExecutor`, optionally composed over an inner native executor.
pub struct McpToolSet {
    clients: Vec<McpClient>,
    /// Namespaced tool name -> (client index, raw tool name). A wire name
    /// claimed by more than one client is absent here and recorded in
    /// `collisions` instead — routing never has a connect-order winner.
    routes: HashMap<String, (usize, String)>,
    /// Wire names more than one `(server, tool)` pair claimed, with every
    /// claimant. See [`McpToolSet::wire_name_collisions`].
    collisions: Vec<WireNameCollision>,
    /// Servers that could not be connected or had invalid names: `(name,
    /// reason)`. They advertise no tools but never block the rest.
    failed: Vec<(String, String)>,
    /// Servers skipped before connect — or refused with an HTTP 401 — because
    /// they require authentication the user has not granted (#2687): `(name,
    /// reason)`. Each advertises one synthetic [`needs_auth`] tool instead of
    /// its real ones.
    auth_required: Vec<(String, String)>,
    native: Option<Arc<dyn ToolExecutor>>,
    /// Where each successful MCP call is recorded (server, tool, reason, time)
    /// for the `mcp_usage` telemetry table. `None` = no telemetry (a no-op).
    usage: Option<McpUsageLedger>,
    /// Server names disabled for this session. A disabled server's tools are
    /// hidden from `schemas()` and its calls error, without disconnecting it.
    disabled: Option<DisabledServers>,
    /// Connected server names opted into the Best-of-N candidate allowlist
    /// (`.stella/mcp.toml`'s `candidate_safe = true`, issue #248 Phase 1).
    /// Populated from the configs at connect time; see
    /// [`McpToolSet::for_candidates`].
    candidate_safe: HashSet<String>,
}

impl McpToolSet {
    /// Connect every server in `configs`. Connection is best-effort and
    /// isolated: a server that fails to connect (bad spawn, handshake error,
    /// timeout, duplicate/invalid name) is recorded in
    /// [`McpToolSet::failed_servers`] and contributes nothing — it never
    /// blocks the others. `timeout` bounds both each connect and (until
    /// overridden with [`McpToolSet::with_call_timeout`]) each later call.
    pub async fn connect(configs: &[McpServerConfig], timeout: Duration) -> Self {
        Self::connect_with_auth(configs, timeout, None).await
    }

    /// [`McpToolSet::connect`], with an optional [`OAuthManager`] that gives
    /// every HTTP server a lazy OAuth bearer source (see [`crate::oauth`]).
    /// Hosts that support `stella mcp login` pass one; everything else is
    /// unchanged.
    pub async fn connect_with_auth(
        configs: &[McpServerConfig],
        timeout: Duration,
        auth: Option<Arc<OAuthManager>>,
    ) -> Self {
        let mut clients = Vec::new();
        let mut failed = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let candidate_safe: HashSet<String> = configs
            .iter()
            .filter(|c| c.candidate_safe)
            .map(|c| c.name.clone())
            .collect();

        let mut auth_required = Vec::new();
        for config in configs {
            if let Some(reason) = namespace_rejection(&config.name) {
                failed.push((config.name.clone(), reason.into()));
                continue;
            }
            if !seen.insert(config.name.clone()) {
                failed.push((config.name.clone(), "duplicate server name".into()));
                continue;
            }
            // Auth-probe suppression (#2687): an HTTP server known to need a
            // login the user has not granted is not dialed at all — no
            // transport, no round trip. See [`crate::suppress`] for the gate.
            let gated_http =
                auth.is_some() && matches!(config.transport, McpTransport::Http { .. });
            if let Some(manager) = auth.as_deref().filter(|_| gated_http)
                && let ConnectGate::AuthRequired { reason } = connect_gate(
                    manager.store(),
                    manager.probe_cache(),
                    &config.name,
                    unix_now_secs(),
                )
            {
                auth_required.push((config.name.clone(), reason));
                continue;
            }
            match tokio::time::timeout(
                timeout,
                McpClient::connect_with_auth(config, timeout, auth.clone()),
            )
            .await
            {
                Ok(Ok(client)) => {
                    if let Some(manager) = auth.as_deref().filter(|_| gated_http) {
                        // A connect that succeeded proves the 401 gone —
                        // retire any probe record so a later logout re-arms
                        // from a fresh 401, never a stale timestamp.
                        // Best-effort: losing the clear costs one TTL wait.
                        let _ = manager.probe_cache().clear(&config.name);
                    }
                    clients.push(client);
                }
                Ok(Err(err)) if gated_http && err.is_auth_error() => {
                    // A fresh 401 (or an unusable stored login): arm the
                    // suppression window and surface the login affordance
                    // instead of a generic failure. Best-effort write — a
                    // failed record only means the next session probes again.
                    if let Some(manager) = auth.as_deref() {
                        let _ = manager
                            .probe_cache()
                            .record_unauthorized(&config.name, unix_now_secs());
                    }
                    auth_required.push((config.name.clone(), err.user_message()));
                }
                Ok(Err(err)) => failed.push((config.name.clone(), err.user_message())),
                Err(_) => failed.push((
                    config.name.clone(),
                    format!("connect timed out after {}ms", timeout.as_millis()),
                )),
            }
        }

        let mut set = Self {
            clients,
            routes: HashMap::new(),
            collisions: Vec::new(),
            failed,
            auth_required,
            native: None,
            usage: None,
            disabled: None,
            candidate_safe,
        };
        set.rebuild_routes();
        set
    }

    /// Build from already-connected clients. Invalid or duplicate server names
    /// are dropped into [`McpToolSet::failed_servers`]. Uses
    /// [`DEFAULT_CALL_TIMEOUT`] until [`McpToolSet::with_call_timeout`].
    pub fn from_clients(clients: Vec<McpClient>) -> Self {
        let mut kept = Vec::new();
        let mut failed = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        for client in clients {
            let name = client.name().to_string();
            if let Some(reason) = namespace_rejection(&name) {
                failed.push((name, reason.into()));
                continue;
            }
            if !seen.insert(name.clone()) {
                failed.push((name, "duplicate server name".into()));
                continue;
            }
            kept.push(client);
        }

        let mut set = Self {
            clients: kept,
            routes: HashMap::new(),
            collisions: Vec::new(),
            failed,
            auth_required: Vec::new(),
            native: None,
            usage: None,
            disabled: None,
            candidate_safe: HashSet::new(),
        };
        set.rebuild_routes();
        set
    }

    /// Compose over an inner native `ToolExecutor`. Any non-`mcp__` tool name
    /// falls through to it.
    #[must_use]
    pub fn wrapping(mut self, native: Arc<dyn ToolExecutor>) -> Self {
        self.native = Some(native);
        self
    }

    /// Flag `names` as Best-of-N candidate-safe (issue #248 Phase 1) —
    /// [`McpToolSet::connect_with_auth`] does this from `.stella/mcp.toml`
    /// automatically; this is for [`McpToolSet::from_clients`] callers (tests,
    /// or any future non-config-driven construction) that need to set it
    /// explicitly.
    #[must_use]
    pub fn with_candidate_safe_servers<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.candidate_safe = names.into_iter().map(Into::into).collect();
        self
    }

    /// Record every successful MCP call into `ledger` (server, tool, reason,
    /// call time) for the `mcp_usage` telemetry table. Without this the set
    /// still works — telemetry is simply not collected.
    #[must_use]
    pub fn with_usage_ledger(mut self, ledger: McpUsageLedger) -> Self {
        self.usage = Some(ledger);
        self
    }

    /// Consult `disabled` (a shared, session-scoped set of server names) so a
    /// disabled server's tools are hidden and its calls error, live, without a
    /// reconnect. The set is shared with the CLI/TUI, which toggles it.
    #[must_use]
    pub fn with_disabled_servers(mut self, disabled: DisabledServers) -> Self {
        self.disabled = Some(disabled);
        self
    }

    /// Whether `server` is currently disabled for this session.
    fn is_disabled(&self, server: &str) -> bool {
        self.disabled.as_ref().is_some_and(|set| {
            set.lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains(server)
        })
    }

    /// Whether `namespaced` (an `mcp__server__tool` name) routes to a
    /// currently-connected, non-disabled server flagged `candidate_safe` —
    /// the Best-of-N allowlist check (issue #248 Phase 1). A name that
    /// doesn't route to any server (including every plain native tool name,
    /// which never matches an `mcp__…` route) is `false`.
    pub fn is_candidate_safe_tool(&self, namespaced: &str) -> bool {
        self.routes.get(namespaced).is_some_and(|(idx, _)| {
            let name = self.clients[*idx].name();
            self.candidate_safe.contains(name) && !self.is_disabled(name)
        })
    }

    /// Build a Best-of-N candidate's tool surface (issue #248 Phase 1):
    /// `native` (the candidate's own snapshot-rooted registry + custom
    /// tools) composed with a READ-ONLY, filtered view over `self` — only
    /// tools from `candidate_safe`-flagged servers are advertised or
    /// callable; the same connected clients answer every candidate's calls,
    /// no per-candidate subprocess. Requires `Arc<Self>` (not `&self`) so the
    /// returned view can outlive the session's own borrow of the set — see
    /// `stella-cli/src/candidate_ws.rs`'s module doc for the full rationale
    /// and why every OTHER server (and `ask_user`) stays withheld.
    pub fn for_candidates(self: &Arc<Self>, native: Arc<dyn ToolExecutor>) -> CandidateMcpView {
        CandidateMcpView {
            inner: Arc::clone(self),
            native,
        }
    }

    /// The orchestrator's Best-of-N pre-fetch (issue #248 Phase 1): call
    /// every connected, non-disabled, `candidate_safe`-flagged server's
    /// ZERO-required-input tools ONCE, concatenating their output as shared
    /// context every candidate can start from — the common "candidates all
    /// need the same DB schema" case, at the cost of one round trip instead
    /// of N. A tool whose schema requires input is skipped outright: this
    /// never synthesizes an argument, it only ever calls a tool with a
    /// genuinely empty `{}`. `None` when nothing is safe to call, or every
    /// call errored or came back empty — a prefetch miss, never a panic.
    pub async fn prefetch_candidate_context(&self) -> Option<String> {
        let mut sections = Vec::new();
        for schema in self.schemas() {
            if !self.is_candidate_safe_tool(&schema.name)
                || !accepts_empty_input(&schema.input_schema)
            {
                continue;
            }
            let empty = Value::Object(serde_json::Map::new());
            if let ToolOutput::Ok { content } = self.execute(&schema.name, &empty).await
                && !content.trim().is_empty()
            {
                sections.push(format!("### {}\n{}", schema.name, content.trim()));
            }
        }
        (!sections.is_empty()).then(|| sections.join("\n\n"))
    }

    /// Override the per-call timeout (default [`DEFAULT_CALL_TIMEOUT`]),
    /// propagating it to every connected server so the client owns the bound
    /// (and can treat a hang as a reconnect trigger).
    #[must_use]
    pub fn with_call_timeout(mut self, timeout: Duration) -> Self {
        for client in &mut self.clients {
            client.set_call_timeout(timeout);
        }
        self
    }

    /// Per-server connection health, for a non-fatal CLI/TUI/telemetry
    /// diagnostic (which servers are live, reconnecting, or backing off).
    pub async fn health(&self) -> Vec<ServerHealth> {
        let mut out = Vec::with_capacity(self.clients.len() + self.auth_required.len());
        for client in &self.clients {
            out.push(client.health().await);
        }
        // Auth-suppressed servers were never dialed, so no client exists to
        // ask — their rows are synthesized (#2687).
        for (name, reason) in &self.auth_required {
            out.push(needs_auth::auth_required_health(name, reason));
        }
        out
    }

    /// Servers that were not connected, as `(name, reason)`.
    pub fn failed_servers(&self) -> &[(String, String)] {
        &self.failed
    }

    /// Servers skipped before connect — or refused with an HTTP 401 —
    /// because they require authentication (#2687), as `(name, reason)`.
    /// Deliberately not folded into [`McpToolSet::failed_servers`]: callers
    /// render that as "server unavailable", and the actionable state here is
    /// "run `stella mcp login <name>`". Each advertises one synthetic
    /// [`login_required_tool_name`] tool in place of its real ones.
    pub fn auth_required_servers(&self) -> &[(String, String)] {
        &self.auth_required
    }

    /// What `server` said about itself during the handshake, or `None` if it
    /// is not connected this session.
    ///
    /// The live counterpart of the registry's [`crate::config::ServerCard`]:
    /// the card is what the *publisher* wrote and survives across sessions on
    /// disk, this is what the *process on the other end of the pipe* claims
    /// right now. A hand-written `mcp.toml` entry has no card at all, so for
    /// those servers this is the only identity that exists.
    pub fn identity(&self, server: &str) -> Option<ServerIdentity> {
        let client = self.clients.iter().find(|c| c.name() == server)?;
        let info = client.server_info();
        Some(ServerIdentity {
            name: info.map(|i| i.name.clone()).filter(|n| !n.is_empty()),
            version: info.map(|i| i.version.clone()).filter(|v| !v.is_empty()),
            title: info.and_then(|i| i.title.clone()),
            website_url: info.and_then(|i| i.website_url.clone()),
            instructions: client.instructions().map(str::to_string),
            protocol_version: client.negotiated_version().to_string(),
        })
    }

    /// `server`'s advertised tools, un-namespaced, as discovered at handshake.
    ///
    /// Read from the client rather than from [`McpToolSet::schemas`] on
    /// purpose: `schemas` answers "what can the model call right now", so it
    /// goes empty the moment a server is disabled for the session. An operator
    /// inspecting a server they just switched off still needs to see what it
    /// offers — that is usually *why* they are looking.
    pub fn advertised_tools(&self, server: &str) -> &[McpToolInfo] {
        self.clients
            .iter()
            .find(|c| c.name() == server)
            .map(McpClient::tools)
            .unwrap_or_default()
    }

    /// Connected servers whose advertised tool list was **truncated** at
    /// ingest, as `(name, dropped)` — the tools past
    /// [`crate::MAX_TOOLS_PER_SERVER`] were refused so one over-advertising server
    /// cannot eat the model's context (#551).
    ///
    /// This is the bounded record of *who* truncated: at most one entry per
    /// connected server, and it holds a borrowed name plus a count, never the
    /// dropped tools themselves.
    ///
    /// Deliberately **not** folded into [`McpToolSet::failed_servers`] (#638):
    /// these servers are connected and their kept tools route normally, and
    /// callers render that list as "server unavailable", which would be a lie
    /// here. Each `dropped` count is a floor — discovery stops at the cap.
    /// Empty for every well-behaved server.
    pub fn over_advertising_servers(&self) -> Vec<(&str, usize)> {
        self.clients
            .iter()
            .filter(|c| c.dropped_tool_count() > 0)
            .map(|c| (c.name(), c.dropped_tool_count()))
            .collect()
    }

    /// Wire names claimed by more than one `(server, tool)` pair, with every
    /// claimant — sorted by wire name (#2675). Each contested name routes
    /// nowhere this session: dropping every claimant is the only answer that
    /// does not let connect order decide which third-party server receives a
    /// call meant for another.
    ///
    /// Deliberately **not** folded into [`McpToolSet::failed_servers`], for
    /// the same reason [`McpToolSet::over_advertising_servers`] is not: the
    /// claimant servers are connected and their uncontested tools route
    /// normally, so rendering them "unavailable" would be a lie. Empty
    /// whenever every server name passes `namespace_rejection`, which makes
    /// [`wire_name`] injective — this is the defense-in-depth record for the
    /// day that guarantee is weakened.
    pub fn wire_name_collisions(&self) -> &[WireNameCollision] {
        &self.collisions
    }

    /// How many advertised tools this session refused across *every* connected
    /// server — the single number a status line can carry ("2 servers
    /// truncated, 31 tools dropped") without iterating
    /// [`McpToolSet::over_advertising_servers`] itself. `0` when nothing was
    /// truncated, which is the case for every well-behaved server.
    ///
    /// A floor, for the same reason each per-server count is: discovery stops
    /// on the page where the cap bites, so tools the server would have listed
    /// later are never counted. A non-zero value means *at least* this many
    /// tools exist that the model was never told about — the operator-visible
    /// half of the cap.
    pub fn dropped_tool_count(&self) -> usize {
        self.clients.iter().map(McpClient::dropped_tool_count).sum()
    }

    /// How many servers are live.
    pub fn connected_count(&self) -> usize {
        self.clients.len()
    }

    /// The live servers' names, in connect order — the counterpart of
    /// [`McpToolSet::failed_servers`] for a caller's connect-outcome report.
    pub fn connected_names(&self) -> Vec<&str> {
        self.clients.iter().map(|c| c.name()).collect()
    }

    /// Close every connected server's transport in order (best-effort).
    pub async fn close_all(&self) {
        for client in &self.clients {
            let _ = client.close().await;
        }
    }

    /// (Re)build the routing map from the current clients. Server names are
    /// validated unique + namespaceable, which makes [`wire_name`] injective
    /// and every wire name single-claimant. Should two `(server, tool)` pairs
    /// map to one wire name anyway, **every** claimant's route is dropped and
    /// the clash recorded in [`McpToolSet::wire_name_collisions`] — before
    /// #2675 the later-connected client silently won the route, which handed
    /// a third-party server a way to shadow another server's tools by
    /// choosing colliding names.
    fn rebuild_routes(&mut self) {
        self.routes.clear();
        self.collisions.clear();
        // BTreeMap so the collision report is ordered by wire name, not by
        // hash order — diagnostics are rendered to the operator and must not
        // shuffle between runs.
        let mut claims: BTreeMap<String, Vec<(usize, String)>> = BTreeMap::new();
        for (idx, client) in self.clients.iter().enumerate() {
            for tool in client.tools() {
                claims
                    .entry(wire_name(client.name(), &tool.name))
                    .or_default()
                    .push((idx, tool.name.clone()));
            }
        }
        for (name, pairs) in claims {
            match pairs.as_slice() {
                [(idx, tool)] => {
                    self.routes.insert(name, (*idx, tool.clone()));
                }
                _ => self.collisions.push(WireNameCollision {
                    wire_name: name,
                    claimants: pairs
                        .into_iter()
                        .map(|(idx, tool)| (self.clients[idx].name().to_string(), tool))
                        .collect(),
                }),
            }
        }
    }

    /// Route one MCP call, mapping every failure mode to a server-named
    /// `ToolOutput::Error`. The per-call timeout and auto-reconnect now live in
    /// [`McpClient::call_tool`] (so a hang can trigger a reconnect); this layer
    /// only names the server and turns an `Err` into model-visible data.
    async fn execute_mcp(&self, client: &McpClient, raw_tool: &str, input: &Value) -> ToolOutput {
        match client.call_tool(raw_tool, input.clone()).await {
            Ok(output) => {
                // Record the successful call for the `mcp_usage` telemetry
                // table. `reason` is best-effort: external MCP tools rarely
                // carry one, so it is usually empty.
                if let Some(ledger) = &self.usage {
                    let reason = input
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    push_usage(ledger, McpUsageRecord::now(client.name(), raw_tool, reason));
                }
                output
            }
            Err(err) => ToolOutput::Error {
                message: format!(
                    "mcp server `{}` failed calling `{raw_tool}`: {}",
                    client.name(),
                    err.user_message()
                ),
            },
        }
    }
}

/// Compose the wire name for a server/tool pair: `mcp__<server>__<tool>`.
///
/// **This is the one place the encoding lives** — no other `format!` may
/// build an `mcp__…` name, so a future tool-contract registry (#2716) can
/// call this single function and its collision check covers every advertised
/// name.
///
/// **Injectivity.** Over server names accepted by `namespace_rejection`
/// (non-empty, no `__`, no leading or trailing `_`), the encoding is
/// injective: two distinct `(server, tool)` pairs cannot share a wire name.
/// Equality would force one server to be the other plus a trailing `_`
/// (`wire_name("acme_", "status") == wire_name("acme", "_status")` is the
/// whole collision family — any longer suffix puts a non-`_` byte where the
/// other name needs the `__` separator), and a trailing `_` is rejected.
/// [`split_wire_name`] is the inverse. Defense in depth for the day the name
/// rule is weakened lives in [`McpToolSet::wire_name_collisions`] (#2675).
pub fn wire_name(server: &str, tool: &str) -> String {
    format!("{NS_PREFIX}{server}{NS_SEP}{tool}")
}

/// The inverse of [`wire_name`]: `(server, tool)` for an `mcp__…` name, or
/// `None` for a name outside the MCP namespace.
///
/// Splits at the **first** `__` after the prefix, which is exact because
/// accepted server names never contain the separator. The tool half may
/// contain `__` freely — only the server segment is constrained.
pub fn split_wire_name(namespaced: &str) -> Option<(&str, &str)> {
    namespaced.strip_prefix(NS_PREFIX)?.split_once(NS_SEP)
}

/// Which server a namespaced tool belongs to, or `""` for a name outside the
/// MCP namespace — the total-function convenience over [`split_wire_name`]
/// that `budget_segment`'s fold wants.
fn server_of(namespaced: &str) -> &str {
    split_wire_name(namespaced).map_or("", |(server, _)| server)
}

/// Roughly what one schema costs in the serialized tools block: its name, its
/// description, and its `inputSchema`.
///
/// Approximate on purpose — the exact cost depends on the provider's own JSON
/// envelope, which this crate does not model. A budget is a bound, not an
/// accounting, and being a few bytes out either way changes nothing about
/// whether a 512 KB catalogue is refused.
fn schema_cost(schema: &ToolSchema) -> usize {
    schema.name.len()
        + schema.description.len()
        + serde_json::to_string(&schema.input_schema).map_or(0, |s| s.len())
}

/// Hold each server's contribution to a SORTED MCP segment under
/// [`MAX_SERVER_SCHEMA_BYTES`], returning what survives and, per server, how
/// many tools the budget cut.
///
/// One function with two callers ([`McpToolSet::schemas`] takes the schemas,
/// [`McpToolSet::over_budget_servers`] takes the counts) rather than a second
/// copy of the rule for reporting — two implementations of one fold is exactly
/// the drift #1613 was filed for.
///
/// The input is already sorted by namespaced name, which groups each server's
/// tools into a contiguous run (the server name is the prefix) AND fixes the
/// order within it. So the truncation point is a deterministic function of the
/// tool names, not of client order or connection timing — without that, the
/// byte-stability contract above would survive the sort and then be lost here.
fn budget_segment(sorted: Vec<ToolSchema>) -> (Vec<ToolSchema>, Vec<(String, usize)>) {
    let mut kept = Vec::with_capacity(sorted.len());
    let mut elided: Vec<(String, usize)> = Vec::new();
    let mut current = String::new();
    let mut spent = 0usize;

    for schema in sorted {
        let server = server_of(&schema.name);
        if server != current {
            current = server.to_string();
            spent = 0;
        }
        let cost = schema_cost(&schema);
        // The first tool of a server is always admitted, however large: a
        // server whose single tool exceeds the budget should advertise that
        // one tool, not vanish. Ingest's per-tool caps already bound it.
        if spent > 0 && spent + cost > MAX_SERVER_SCHEMA_BYTES {
            match elided.last_mut() {
                Some((name, count)) if *name == current => *count += 1,
                _ => elided.push((current.clone(), 1)),
            }
            continue;
        }
        spent += cost;
        kept.push(schema);
    }
    (kept, elided)
}

impl McpToolSet {
    /// Servers whose advertised tools were trimmed to fit
    /// [`MAX_SERVER_SCHEMA_BYTES`], as `(server, tools dropped)`.
    ///
    /// Deliberately separate from [`Self::over_advertising_servers`], which
    /// reports the per-tool COUNT cap applied at ingest. These are two
    /// different walls and a reader needs to know which one it hit: 300 tools
    /// trips the first, twelve verbose ones trip this. Empty for every server
    /// that fits, which is nearly all of them.
    #[must_use]
    pub fn over_budget_servers(&self) -> Vec<(String, usize)> {
        budget_segment(self.mcp_segment()).1
    }

    /// This set's MCP tools, namespaced and sorted, before the byte budget.
    ///
    /// The one place the segment is built. [`ToolExecutor::schemas`] budgets it
    /// and returns the schemas; [`Self::over_budget_servers`] budgets it and
    /// returns the counts. Deriving the counts from the already-budgeted list
    /// would report zero every time — the honest input to both questions is
    /// what the servers advertised, not what survived.
    fn mcp_segment(&self) -> Vec<ToolSchema> {
        let mut mcp = Vec::new();
        for (idx, client) in self.clients.iter().enumerate() {
            // A disabled server advertises nothing this session — the engine
            // re-reads schemas each model call, so the model stops seeing its
            // tools the moment it is toggled off.
            if self.is_disabled(client.name()) {
                continue;
            }
            for tool in client.tools() {
                let namespaced = wire_name(client.name(), &tool.name);
                // Only advertise tools that actually route back to this client
                // — a collided wire name has no route at all (see
                // `wire_name_collisions`), so no claimant advertises it.
                if self.routes.get(&namespaced).map(|(i, _)| *i) == Some(idx) {
                    mcp.push(ToolSchema {
                        name: namespaced,
                        description: tool.description.clone(),
                        input_schema: tool.input_schema.clone(),
                        // External MCP tools are unknown — treat as mutating,
                        // the safe direction (never auto-parallelized). And
                        // never speculated: the server's request count and
                        // rate limit are not ours to spend twice (#923).
                        read_only: false,
                        speculation_safe: false,
                    });
                }
            }
        }
        // Namespaced names are unique by construction (`routes` is keyed on
        // them), so this is a total order — no tie for the sort to resolve
        // arbitrarily and reintroduce the nondeterminism.
        mcp.sort_by(|a, b| a.name.cmp(&b.name));
        mcp
    }
}

#[async_trait]
impl ToolExecutor for McpToolSet {
    /// The advertised toolset, as two segments in a fixed order: the native
    /// tools, then this set's MCP tools.
    ///
    /// **Both the segment order and the order within each segment are a
    /// cross-process contract, not a presentation choice.** This list is
    /// serialized verbatim at position 0 of the prompt prefix, and prompt
    /// caching is a byte-level prefix match — so two stella processes in the
    /// same workspace inside the cache TTL share the tools+system entry only
    /// if they emit the same bytes. `ToolRegistry::schemas` sorts the native
    /// segment for exactly that reason; this decorator used to concatenate
    /// after it without re-sorting, which put the whole guarantee back at the
    /// mercy of `self.clients` order and of which server finished connecting
    /// first (#1848).
    ///
    /// Sorting the MCP segment by its namespaced name — rather than trusting
    /// client order — is what makes the answer independent of both.
    fn schemas(&self) -> Vec<ToolSchema> {
        let mut schemas = Vec::new();
        // Native tools first — they are the base layer the MCP set augments.
        if let Some(native) = &self.native {
            schemas.extend(native.schemas());
        }
        schemas.extend(budget_segment(self.mcp_segment()).0);
        // Each auth-suppressed server advertises exactly one placeholder that
        // says how to log in (#2687) — subject to the same live disable.
        needs_auth::extend_schemas(self, &mut schemas);
        // Each resources-capable server advertises `list_resources` /
        // `read_resource` (#2678) — same live disable, real tools win a name.
        resources::extend_schemas(self, &mut schemas);
        schemas
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        if let Some((idx, raw_tool)) = self.routes.get(name) {
            let client = &self.clients[*idx];
            if self.is_disabled(client.name()) {
                return ToolOutput::Error {
                    message: format!(
                        "mcp server `{}` is disabled for this session — tool `{name}` unavailable",
                        client.name()
                    ),
                };
            }
            return self.execute_mcp(client, raw_tool, input).await;
        }
        // The synthetic needs-auth tool (#2687): answered locally — there is
        // no connection to the suppressed server to route anything over.
        if let Some(output) = needs_auth::route(self, name) {
            return output;
        }
        // The synthetic resource tools (#2678): not in the routing map, but
        // they do drive a wire call on the named server's live client.
        if let Some(output) = resources::route(self, name, input).await {
            return output;
        }
        // A namespaced name we don't recognize is an MCP miss, not a native
        // tool — never fall through to native for it.
        if name.starts_with(NS_PREFIX) {
            return ToolOutput::Error {
                message: format!(
                    "unknown MCP tool `{name}` — not advertised by any connected server"
                ),
            };
        }
        match &self.native {
            Some(native) => native.execute(name, input).await,
            None => ToolOutput::Error {
                message: format!("unknown tool `{name}` (no native tools configured)"),
            },
        }
    }

    /// Forwarded: this is a decorator, and a decorator that let the default
    /// `0.0` stand would silently drop sub-agent spend out of the parent's
    /// budget (see the port's contract).
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        // MCP servers are remote and spawn no sub-agents; only the native
        // layer beneath this set can.
        self.native
            .as_ref()
            .map_or(0.0, |native| native.drain_sub_agent_spend_usd())
    }

    /// Forwarded for the same reason as the spend drain above: a swallowed
    /// wait request silently turns parked waits (#1471) back into
    /// model-step polling.
    fn drain_wait_request(&self) -> Option<stella_core::WaitRequest> {
        self.native
            .as_ref()
            .and_then(|native| native.drain_wait_request())
    }

    /// Forwarded from the native layer, which owns the process table. An
    /// external MCP server's own long-running state is not reported here and
    /// deliberately so: the end-of-turn assertion (#2764) names things the
    /// model can act on with `restart_process`, and a remote server's children
    /// are not among them.
    fn live_services(&self) -> Vec<stella_core::LiveService> {
        self.native
            .as_ref()
            .map_or_else(Vec::new, |native| native.live_services())
    }

    /// Forwarded from the native layer: letting the empty default stand would
    /// silently serialize its sibling spawns (see the port's contract).
    /// External MCP tools never carry this claim — the port pins their
    /// concurrency to `read_only`, which they are advertised without.
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        self.native
            .as_ref()
            .map_or_else(Default::default, |native| native.parallel_safe_names())
    }
}

/// A Best-of-N candidate's tool surface (issue #248 Phase 1): built by
/// [`McpToolSet::for_candidates`]. `execute`/`schemas` route an `mcp__…` name
/// through `inner` ONLY when [`McpToolSet::is_candidate_safe_tool`] allows
/// it; every other name — including a `mcp__…` miss on a withheld server —
/// falls to `native` or a loud, model-visible error, never to `inner`'s own
/// (real-tree-rooted) native layer, which this view never advertises or
/// calls.
pub struct CandidateMcpView {
    inner: Arc<McpToolSet>,
    native: Arc<dyn ToolExecutor>,
}

#[async_trait]
impl ToolExecutor for CandidateMcpView {
    fn schemas(&self) -> Vec<ToolSchema> {
        let mut schemas = self.native.schemas();
        // `inner.schemas()` also carries `inner`'s own native layer (the
        // SESSION's real-tree tools) if it wraps one — but those never match
        // an `mcp__…` route, so `is_candidate_safe_tool` (route lookup only)
        // naturally excludes them here without special-casing.
        schemas.extend(
            self.inner
                .schemas()
                .into_iter()
                .filter(|s| self.inner.is_candidate_safe_tool(&s.name)),
        );
        schemas
    }

    async fn execute(&self, name: &str, input: &Value) -> ToolOutput {
        if name.starts_with(NS_PREFIX) {
            return if self.inner.is_candidate_safe_tool(name) {
                self.inner.execute(name, input).await
            } else {
                ToolOutput::Error {
                    message: format!(
                        "mcp tool `{name}` is withheld from Best-of-N candidates — its \
                         server is not marked `candidate_safe` in .stella/mcp.toml"
                    ),
                }
            };
        }
        self.native.execute(name, input).await
    }

    /// Forwarded: this is a decorator, and a decorator that let the default
    /// `0.0` stand would silently drop sub-agent spend out of the parent's
    /// budget (see the port's contract).
    fn drain_sub_agent_spend_usd(&self) -> f64 {
        self.inner.drain_sub_agent_spend_usd()
    }

    /// Forwarded so a swallowed wait request cannot turn parked waits
    /// (#1471) back into model-step polling — to `native`, not `inner`:
    /// remote MCP tools never deposit wait requests, and the native layer
    /// this view executes (`ci_status` included) is the only depositor.
    fn drain_wait_request(&self) -> Option<stella_core::WaitRequest> {
        self.native.drain_wait_request()
    }

    /// Forwarded from the candidate's own `native` layer, for the same
    /// reason `drain_wait_request` is: it is the executor that owns a
    /// process table, and the one every non-`mcp__` name routes to above.
    fn live_services(&self) -> Vec<stella_core::LiveService> {
        self.native.live_services()
    }

    /// Forwarded from the candidate's own `native` layer — the executor every
    /// non-`mcp__` name routes to in `execute` above. `inner`'s set would name
    /// tools this view never runs, and MCP tools never carry the claim.
    fn parallel_safe_names(&self) -> std::collections::HashSet<String> {
        self.native.parallel_safe_names()
    }
}

/// Whether `schema` can be called with an empty `{}` — no `required` array,
/// or an empty one. Used ONLY to decide whether [`McpToolSet::prefetch_candidate_context`]
/// may call a tool blind; never to synthesize a value for a required field.
fn accepts_empty_input(schema: &Value) -> bool {
    schema
        .get("required")
        .and_then(Value::as_array)
        .is_none_or(|required| required.is_empty())
}

/// Why `name` may not be used as a namespace segment, or `None` if it may.
///
/// Three rules, each protecting [`wire_name`]'s injectivity: the name must be
/// non-empty, must not contain the `__` separator (which would make the
/// server prefix ambiguous), and must not start or end with `_` — a trailing
/// `_` is exactly what lets two pairs collide
/// (`wire_name("acme_", "status") == wire_name("acme", "_status")`, #2675),
/// and a leading `_` is rejected symmetrically so the boundary between the
/// `mcp__` prefix and the server segment stays visually and lexically crisp.
fn namespace_rejection(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        Some("server name is empty")
    } else if name.contains(NS_SEP) {
        Some("server name contains the reserved `__` separator")
    } else if name.starts_with('_') || name.ends_with('_') {
        Some(
            "server name starts or ends with `_`, which would make \
             `mcp__<server>__<tool>` wire names ambiguous",
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
