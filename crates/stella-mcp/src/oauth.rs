//! OAuth 2.1 login for streamable-HTTP MCP servers, per the MCP
//! authorization spec (protocol revision `2025-06-18`):
//!
//! - **RFC 9728** protected-resource metadata discovery (the server's 401
//!   `WWW-Authenticate: … resource_metadata="…"` hint, with well-known-path
//!   fallbacks) to find the authorization server;
//! - **RFC 8414** authorization-server metadata (plus the OIDC
//!   `openid-configuration` fallback) for the endpoints;
//! - **RFC 7591** dynamic client registration when the server offers it;
//! - the **authorization-code grant with PKCE (S256)** through the user's
//!   browser and a loopback redirect on `127.0.0.1`;
//! - **RFC 8707** resource indicators (`resource=<canonical server URL>`) so
//!   the issued token is audience-bound to the one MCP server;
//! - the **refresh-token grant**, applied transparently before a request when
//!   the access token is near expiry and once more after a 401.
//!
//! Two halves:
//!
//! 1. [`login`] — the interactive flow. It is UI-agnostic: progress (most
//!    importantly the authorization URL the user must open) is surfaced
//!    through a [`LoginEvent`] callback, so the CLI can print + auto-open the
//!    browser while the deck shows the same flow inline.
//! 2. [`OAuthManager`] / [`OAuthTokenSource`] — the runtime side. The manager
//!    owns the on-disk [`TokenStore`] and hands each HTTP transport a lazy
//!    per-server token source. "Lazy" is load-bearing: a source attached to a
//!    server with no stored tokens yields no header (static-header servers
//!    keep working untouched), and it re-checks the store until tokens
//!    appear — so a login completed mid-session takes effect on the next
//!    tool call without a reconnect.
//!
//! Security: tokens live in an owner-only (0600) JSON file inside an
//! owner-only (0700) directory, at a path chosen by the caller. Every open is
//! `O_NOFOLLOW` and rejects a non-regular or multiply-linked file, so a
//! planted symlink cannot redirect a token write. That hardening is
//! **Unix-only**: on other platforms the store still persists (#617 — it
//! used to refuse), protected only by the OS's inherited ACLs
//! (see `ensure_private_token_parent`). No token, verifier,
//! or client secret is ever logged — [`OAuthTokens`]'s `Debug` is hand-written
//! to redact them, matching the [`crate::config::McpTransport`] convention.
//!
//! This file holds the tokens, their store, the runtime source, and [`login`]
//! itself; the three seams the flow passes through are their own modules —
//! [`discovery`] (who authorizes this server, and where), [`grant`] (client
//! registration and the token endpoint), and [`callback`] (the loopback
//! listener the browser is redirected back to).

mod callback;
mod discovery;
mod grant;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng as _;
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use stella_store::durable::{MODE_PRIVATE, write_atomic};
use tokio::net::TcpListener;

use callback::wait_for_code;
use discovery::{
    canonical_resource, discover_auth_server, discover_protected_resource, is_permitted_issuer,
    origin_of,
};
use grant::{register_client, token_request};

use crate::error::McpError;

/// Refresh an access token this many seconds *before* its stated expiry, so
/// a token never expires mid-request.
const EXPIRY_SKEW_SECS: u64 = 60;

/// How long [`login`] waits for the user to finish the browser round-trip.
pub const DEFAULT_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// Bound on discovery/registration/token HTTP calls.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

// ── Tokens & the on-disk store ───────────────────────────────────────────────

/// Everything needed to authorize requests to one server *and* to refresh
/// without re-running the browser flow. Serialized verbatim into the token
/// store file (owner-only), never into logs — see the redacting `Debug`.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OAuthTokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix seconds after which `access_token` is stale. `None` = the server
    /// gave no `expires_in`; the token is used until a 401 forces a refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// Where the refresh grant is sent.
    pub token_endpoint: String,
    pub client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// The canonical server URL the token is audience-bound to (RFC 8707).
    pub resource: String,
}

impl OAuthTokens {
    /// Stale (within [`EXPIRY_SKEW_SECS`] of expiry) and therefore due for a
    /// refresh before the next request.
    fn needs_refresh(&self, now_secs: u64) -> bool {
        self.expires_at
            .is_some_and(|at| now_secs.saturating_add(EXPIRY_SKEW_SECS) >= at)
    }

    /// Fold a token response into this record: a rotated refresh token
    /// replaces the old one, an absent one keeps it (RFC 6749 §6).
    fn apply(&mut self, response: TokenResponse, now_secs: u64) {
        self.access_token = response.access_token;
        if response.refresh_token.is_some() {
            self.refresh_token = response.refresh_token;
        }
        self.expires_at = response.expires_in.map(|s| now_secs.saturating_add(s));
        if response.scope.is_some() {
            self.scope = response.scope;
        }
    }
}

impl std::fmt::Debug for OAuthTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthTokens")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("expires_at", &self.expires_at)
            .field("token_endpoint", &self.token_endpoint)
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("scope", &self.scope)
            .field("resource", &self.resource)
            .finish()
    }
}

/// RFC 6749 token response (tolerant subset). Lives beside the record it is
/// folded into rather than beside the request that fetches it ([`grant`]) —
/// [`OAuthTokens::apply`] is the only thing that reads its fields.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

/// The on-disk file: server name → its tokens.
#[derive(Debug, Default, Serialize, Deserialize)]
struct TokenFile {
    #[serde(default)]
    servers: BTreeMap<String, OAuthTokens>,
}

/// The owner-only JSON token file (path chosen by the caller — the CLI puts
/// it at `.stella/private/mcp_oauth.json`). Every operation re-reads the whole
/// file and rewrites it through a temp file + `rename`, so a reader never sees
/// a half-written store and a crash mid-write leaves the previous one intact.
///
/// The read-modify-write is *not* serialized across processes, though: two
/// sessions logging into different servers at the same instant can each write
/// a file built from the pre-login snapshot, and the later `rename` wins. The
/// worst case is one login having to be repeated, which is why this is left
/// unlocked — a lock file would add a stale-lock failure mode to a
/// once-per-login operation.
#[derive(Debug, Clone)]
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    fn load(&self) -> Result<TokenFile, McpError> {
        if std::fs::symlink_metadata(&self.path)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        {
            return Ok(TokenFile::default());
        }
        use std::io::Read as _;
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        let mut file = open_private_token_file(&self.path, options)?;
        let mut text = String::new();
        file.read_to_string(&mut text).map_err(|e| {
            McpError::Auth(format!(
                "cannot read token store {}: {e}",
                self.path.display()
            ))
        })?;
        serde_json::from_str(&text).map_err(|e| {
            McpError::Auth(format!(
                "token store {} is corrupt: {e} — delete it and log in again",
                self.path.display()
            ))
        })
    }

    fn save(&self, file: &TokenFile) -> Result<(), McpError> {
        ensure_private_token_parent(&self.path)?;
        let json = serde_json::to_string_pretty(file)
            .map_err(|e| McpError::Auth(format!("cannot serialize token store: {e}")))?;
        if let Ok(metadata) = std::fs::symlink_metadata(&self.path)
            && (metadata.file_type().is_symlink() || !metadata.is_file())
        {
            return Err(McpError::Auth(format!(
                "token store {} is not a regular file",
                self.path.display()
            )));
        }
        // The workspace's one durable-write contract (#617): pid-tagged temp,
        // fsync, rename, fsync of the parent directory, temp removed on any
        // failure. This used to be a sixth hand-rolled copy of it.
        write_atomic(&self.path, json.as_bytes(), MODE_PRIVATE)
            .map_err(|e| McpError::Auth(e.to_string()))
    }

    /// The stored tokens for `server`, if a login has completed.
    pub fn get(&self, server: &str) -> Result<Option<OAuthTokens>, McpError> {
        Ok(self.load()?.servers.remove(server))
    }

    /// Insert or replace `server`'s tokens (login / refresh persistence).
    pub fn put(&self, server: &str, tokens: &OAuthTokens) -> Result<(), McpError> {
        let mut file = self.load()?;
        file.servers.insert(server.to_string(), tokens.clone());
        self.save(&file)
    }

    /// Drop `server`'s tokens (logout); returns whether any existed.
    pub fn remove(&self, server: &str) -> Result<bool, McpError> {
        let mut file = self.load()?;
        let existed = file.servers.remove(server).is_some();
        if existed {
            self.save(&file)?;
        }
        Ok(existed)
    }

    /// The servers with stored tokens (for "logged in" badges).
    pub fn logged_in_servers(&self) -> Result<Vec<String>, McpError> {
        Ok(self.load()?.servers.into_keys().collect())
    }
}

#[cfg(unix)]
fn ensure_private_token_parent(path: &Path) -> Result<PathBuf, McpError> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let parent = path
        .parent()
        .ok_or_else(|| McpError::Auth(format!("token path {} has no parent", path.display())))?;
    match std::fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(McpError::Auth(format!(
                "token directory {} is not a real directory",
                parent.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true).mode(0o700);
            builder.create(parent).map_err(|e| {
                McpError::Auth(format!(
                    "cannot create token directory {}: {e}",
                    parent.display()
                ))
            })?;
        }
        Err(error) => {
            return Err(McpError::Auth(format!(
                "cannot inspect token directory {}: {error}",
                parent.display()
            )));
        }
    }
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).map_err(|e| {
        McpError::Auth(format!(
            "cannot restrict token directory {}: {e}",
            parent.display()
        ))
    })?;
    Ok(parent.to_path_buf())
}

/// Without POSIX modes there is no 0700 to enforce, so the directory is
/// created (inheriting the parent's ACL) and checked for being a real
/// directory. Refusing outright — this arm's previous behaviour — meant MCP
/// OAuth simply did not work off unix, which stores the secret somewhere
/// Stella does not manage rather than storing it under the OS's own
/// permissions (#617).
#[cfg(not(unix))]
fn ensure_private_token_parent(path: &Path) -> Result<PathBuf, McpError> {
    let parent = path
        .parent()
        .ok_or_else(|| McpError::Auth(format!("token store {} has no parent", path.display())))?;
    match std::fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(McpError::Auth(format!(
                "token directory {} is not a real directory",
                parent.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(parent).map_err(|e| {
                McpError::Auth(format!(
                    "cannot create token directory {}: {e}",
                    parent.display()
                ))
            })?;
        }
        Err(error) => {
            return Err(McpError::Auth(format!(
                "cannot inspect token directory {}: {error}",
                parent.display()
            )));
        }
    }
    Ok(parent.to_path_buf())
}

#[cfg(unix)]
fn open_private_token_file(
    path: &Path,
    mut options: std::fs::OpenOptions,
) -> Result<std::fs::File, McpError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    options.mode(0o600);
    options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options
        .open(path)
        .map_err(|e| McpError::Auth(format!("cannot open token store {}: {e}", path.display())))?;
    let metadata = file.metadata().map_err(|e| {
        McpError::Auth(format!(
            "cannot inspect token store {}: {e}",
            path.display()
        ))
    })?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(McpError::Auth(format!(
            "token store {} is not a single-link regular file",
            path.display()
        )));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|e| McpError::Auth(format!("cannot restrict {}: {e}", path.display())))?;
    Ok(file)
}

/// The regular-file check with none of the unix-only hardening — no
/// `O_NOFOLLOW`, no link count, no 0600. See
/// [`ensure_private_token_parent`] for why this reads the file instead of
/// refusing to.
#[cfg(not(unix))]
fn open_private_token_file(
    path: &Path,
    options: std::fs::OpenOptions,
) -> Result<std::fs::File, McpError> {
    let file = options
        .open(path)
        .map_err(|e| McpError::Auth(format!("cannot open token store {}: {e}", path.display())))?;
    let metadata = file.metadata().map_err(|e| {
        McpError::Auth(format!(
            "cannot inspect token store {}: {e}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(McpError::Auth(format!(
            "token store {} is not a regular file",
            path.display()
        )));
    }
    Ok(file)
}

// ── Runtime: manager + per-server token source ──────────────────────────────

/// Hands each HTTP transport a lazy per-server [`OAuthTokenSource`] over one
/// shared [`TokenStore`]. Constructed once per session by the host and passed
/// to `McpToolSet::connect_with_auth`.
#[derive(Debug)]
pub struct OAuthManager {
    store: TokenStore,
    http: Client,
}

impl OAuthManager {
    pub fn new(store_path: impl Into<PathBuf>) -> Self {
        Self {
            store: TokenStore::new(store_path),
            http: http_client(),
        }
    }

    /// A token source for `server`. Always `Some` — the source is lazy and
    /// yields no bearer while the store has no tokens for the server, so it
    /// is safe (and intended) to attach to every HTTP transport.
    pub fn source_for(&self, server: &str) -> Arc<OAuthTokenSource> {
        Arc::new(OAuthTokenSource {
            server: server.to_string(),
            store: self.store.clone(),
            http: self.http.clone(),
            state: tokio::sync::Mutex::new(None),
        })
    }

    /// Whether a completed login is stored for `server`.
    pub fn logged_in(&self, server: &str) -> bool {
        self.store.get(server).ok().flatten().is_some()
    }

    /// The underlying store (login persistence, logout).
    pub fn store(&self) -> &TokenStore {
        &self.store
    }
}

/// Per-server bearer supplier consulted by the HTTP transport on every
/// request. Refreshes ahead of expiry and persists rotated tokens; after a
/// 401 the transport calls [`OAuthTokenSource::refreshed_bearer`] for one
/// forced refresh + retry.
pub struct OAuthTokenSource {
    server: String,
    store: TokenStore,
    http: Client,
    /// `None` until tokens are first seen in the store. A source that found
    /// no tokens re-reads the store on each request (cheap; only while
    /// logged out) so a mid-session login is picked up without a reconnect.
    state: tokio::sync::Mutex<Option<OAuthTokens>>,
}

impl OAuthTokenSource {
    /// The current bearer, refreshed if near expiry — or `None` when no
    /// login is stored (the transport then sends only its static headers).
    pub async fn bearer(&self) -> Result<Option<String>, McpError> {
        let mut state = self.state.lock().await;
        if state.is_none() {
            *state = self.store.get(&self.server)?;
        }
        let Some(tokens) = state.as_mut() else {
            return Ok(None);
        };
        if tokens.needs_refresh(now_secs()) {
            self.refresh(tokens).await?;
        }
        Ok(Some(tokens.access_token.clone()))
    }

    /// Force a refresh (the 401 recovery path). Errors if no login is stored
    /// or the grant has no refresh token — both mean "log in again".
    pub async fn refreshed_bearer(&self) -> Result<String, McpError> {
        let mut state = self.state.lock().await;
        if state.is_none() {
            *state = self.store.get(&self.server)?;
        }
        let Some(tokens) = state.as_mut() else {
            return Err(self.reauth_error("no OAuth login is stored"));
        };
        self.refresh(tokens).await?;
        Ok(tokens.access_token.clone())
    }

    async fn refresh(&self, tokens: &mut OAuthTokens) -> Result<(), McpError> {
        let Some(refresh_token) = tokens.refresh_token.clone() else {
            return Err(
                self.reauth_error("the access token expired and no refresh token was granted")
            );
        };
        let mut form = vec![
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh_token),
            ("client_id", tokens.client_id.clone()),
            ("resource", tokens.resource.clone()),
        ];
        if let Some(scope) = &tokens.scope {
            form.push(("scope", scope.clone()));
        }
        let response = token_request(
            &self.http,
            &tokens.token_endpoint,
            &form,
            tokens
                .client_secret
                .as_deref()
                .map(|s| (tokens.client_id.as_str(), s)),
        )
        .await
        .map_err(|e| self.reauth_error(&format!("token refresh failed: {e}")))?;
        tokens.apply(response, now_secs());
        // Persist best-effort: a write failure must not fail the request the
        // fresh in-memory token can still serve.
        let _ = self.store.put(&self.server, tokens);
        Ok(())
    }

    fn reauth_error(&self, why: &str) -> McpError {
        McpError::Auth(format!(
            "server `{}`: {why} — run `stella mcp login {}` (or press `o` on it in the deck's MCP tab)",
            self.server, self.server
        ))
    }
}

impl std::fmt::Debug for OAuthTokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthTokenSource")
            .field("server", &self.server)
            .finish_non_exhaustive()
    }
}

// ── Interactive login flow ───────────────────────────────────────────────────

/// Progress surfaced by [`login`] so any UI (CLI printer, deck tab) can show
/// the flow. The `AuthorizeUrl` event is the one the user must act on.
#[derive(Debug, Clone)]
pub enum LoginEvent {
    /// A human-readable progress line ("discovering authorization server…").
    Status(String),
    /// Open this URL in a browser to approve access. Sent exactly once.
    AuthorizeUrl(String),
}

/// Knobs for [`login`]. `Default` is right for almost every caller.
#[derive(Debug, Clone)]
pub struct LoginOptions {
    /// How long to wait for the browser round-trip before giving up.
    pub timeout: Duration,
    /// Scopes to request; `None` uses the resource metadata's
    /// `scopes_supported` (all of them), or omits the parameter entirely.
    pub scope: Option<String>,
    /// `client_name` sent during dynamic registration.
    pub client_name: String,
}

impl Default for LoginOptions {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_LOGIN_TIMEOUT,
            scope: None,
            client_name: "stella".to_string(),
        }
    }
}

/// Run the full browser login for `server_url` and return tokens ready for
/// [`TokenStore::put`]. Blocks (async) until the user approves in the
/// browser, the authorization server reports an error, or `options.timeout`
/// elapses.
///
/// `server_name` is the caller's storage key. This function deliberately does
/// nothing with it: the flow is driven entirely by `server_url`, and nothing
/// here logs, so accepting the name only keeps the call site symmetrical with
/// the [`TokenStore::put`] that follows.
pub async fn login(
    server_name: &str,
    server_url: &str,
    options: &LoginOptions,
    notify: &mut (dyn FnMut(LoginEvent) + Send),
) -> Result<OAuthTokens, McpError> {
    let http = http_client();
    let resource = canonical_resource(server_url)?;

    // 1. Who authorizes this server? (RFC 9728, with fallbacks.)
    notify(LoginEvent::Status(
        "discovering authorization server…".to_string(),
    ));
    let prm = discover_protected_resource(&http, server_url).await;
    let named = prm
        .as_ref()
        .map(|m| m.authorization_servers.as_slice())
        .unwrap_or_default();
    let issuer = match named.first() {
        Some(first) => named
            .iter()
            .find(|candidate| is_permitted_issuer(candidate, server_url))
            .cloned()
            .ok_or_else(|| {
                McpError::Auth(format!(
                    "`{server_url}` names authorization server `{first}`, which this client \
                     will not fetch — an issuer inside the loopback/private/link-local space \
                     (or reached in cleartext from an https resource) is refused"
                ))
            })?,
        None => origin_of(server_url),
    };
    let scope = options.scope.clone().or_else(|| {
        prm.as_ref()
            .and_then(|m| (!m.scopes_supported.is_empty()).then(|| m.scopes_supported.join(" ")))
    });

    // 2. The authorization server's endpoints (RFC 8414 / OIDC discovery).
    let meta = discover_auth_server(&http, &issuer).await?;
    if !meta.code_challenge_methods_supported.is_empty()
        && !meta
            .code_challenge_methods_supported
            .iter()
            .any(|m| m == "S256")
    {
        return Err(McpError::Auth(format!(
            "authorization server `{issuer}` does not support PKCE S256 (offers {:?})",
            meta.code_challenge_methods_supported
        )));
    }

    // 3. A loopback redirect target, bound before registration so the exact
    //    redirect URI can be registered.
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| McpError::Auth(format!("cannot bind loopback listener: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| McpError::Auth(format!("cannot read loopback address: {e}")))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    // 4. A client id: dynamic registration when offered (RFC 7591), else the
    //    server must accept public clients with an unregistered redirect —
    //    surfaced as a clear error if it refuses at the authorize step.
    let (client_id, client_secret) = match &meta.registration_endpoint {
        Some(endpoint) => {
            notify(LoginEvent::Status("registering client…".to_string()));
            register_client(
                &http,
                endpoint,
                &redirect_uri,
                &options.client_name,
                scope.as_deref(),
            )
            .await?
        }
        None => (options.client_name.clone(), None),
    };

    // 5. PKCE + state, then the URL the user opens.
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes::<48>());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = URL_SAFE_NO_PAD.encode(random_bytes::<24>());

    let mut authorize = Url::parse(&meta.authorization_endpoint)
        .map_err(|e| McpError::Auth(format!("bad authorization_endpoint: {e}")))?;
    authorize
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("resource", &resource);
    if let Some(scope) = &scope {
        authorize.query_pairs_mut().append_pair("scope", scope);
    }
    notify(LoginEvent::AuthorizeUrl(authorize.to_string()));

    // 6. Wait for the browser to bounce back with the code.
    notify(LoginEvent::Status(
        "waiting for approval in the browser…".to_string(),
    ));
    let code = tokio::time::timeout(options.timeout, wait_for_code(listener, &state))
        .await
        .map_err(|_| {
            McpError::Auth(format!(
                "login timed out after {}s waiting for the browser",
                options.timeout.as_secs()
            ))
        })??;

    // 7. Exchange the code for tokens.
    notify(LoginEvent::Status(
        "exchanging code for tokens…".to_string(),
    ));
    let form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", client_id.clone()),
        ("code_verifier", verifier),
        ("resource", resource.clone()),
    ];
    let response = token_request(
        &http,
        &meta.token_endpoint,
        &form,
        client_secret.as_deref().map(|s| (client_id.as_str(), s)),
    )
    .await?;

    let now = now_secs();
    let mut tokens = OAuthTokens {
        access_token: String::new(),
        refresh_token: None,
        expires_at: None,
        token_endpoint: meta.token_endpoint,
        client_id,
        client_secret,
        scope,
        resource,
    };
    tokens.apply(response, now);
    // Accepted for call-site symmetry only — see this function's doc comment.
    let _ = server_name;
    Ok(tokens)
}

// ── Small helpers ────────────────────────────────────────────────────────────

/// The one HTTP client every OAuth exchange shares.
///
/// Built once, for two reasons. Each `Client` owns a connection pool and a TLS
/// configuration, so building one per call discarded connection reuse and
/// repaid TLS setup on every discovery, token grant and refresh.
///
/// And `build()` is fallible — a TLS backend that will not initialize returns
/// `Err`, where `unwrap_or_default()` used to hand back a `Client` with **no**
/// timeout at all, silently converting the 30s bound into an unbounded wait on
/// exactly the runs already in trouble. Every request site therefore carries
/// its own `.timeout(HTTP_TIMEOUT)`: reqwest honours a per-request deadline
/// whatever the client default is, so the bound holds even on that fallback.
fn http_client() -> Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            Client::builder()
                .timeout(HTTP_TIMEOUT)
                .build()
                .unwrap_or_default()
        })
        .clone()
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    rand::rng().fill_bytes(&mut bytes);
    bytes
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_debug_redacts_all_secrets() {
        let tokens = OAuthTokens {
            access_token: "at-secret".into(),
            refresh_token: Some("rt-secret".into()),
            expires_at: Some(123),
            token_endpoint: "https://auth/token".into(),
            client_id: "cid".into(),
            client_secret: Some("cs-secret".into()),
            scope: Some("mcp".into()),
            resource: "https://srv/mcp".into(),
        };
        let shown = format!("{tokens:?}");
        for secret in ["at-secret", "rt-secret", "cs-secret"] {
            assert!(!shown.contains(secret), "leaked `{secret}`: {shown}");
        }
        // Non-secrets stay visible for diagnostics.
        assert!(shown.contains("cid") && shown.contains("https://auth/token"));
    }

    #[test]
    fn needs_refresh_respects_skew_and_absence() {
        let mut tokens = OAuthTokens {
            access_token: "a".into(),
            refresh_token: None,
            expires_at: None,
            token_endpoint: "t".into(),
            client_id: "c".into(),
            client_secret: None,
            scope: None,
            resource: "r".into(),
        };
        // No expiry → never proactively refreshed.
        assert!(!tokens.needs_refresh(1_000));
        tokens.expires_at = Some(1_000 + EXPIRY_SKEW_SECS);
        assert!(tokens.needs_refresh(1_000));
        tokens.expires_at = Some(1_000 + EXPIRY_SKEW_SECS + 1);
        assert!(!tokens.needs_refresh(1_000));
    }

    #[test]
    fn apply_keeps_old_refresh_token_when_response_omits_it() {
        let mut tokens = OAuthTokens {
            access_token: "old".into(),
            refresh_token: Some("keep-me".into()),
            expires_at: None,
            token_endpoint: "t".into(),
            client_id: "c".into(),
            client_secret: None,
            scope: None,
            resource: "r".into(),
        };
        tokens.apply(
            TokenResponse {
                access_token: "new".into(),
                refresh_token: None,
                expires_in: Some(3600),
                scope: None,
            },
            100,
        );
        assert_eq!(tokens.access_token, "new");
        assert_eq!(tokens.refresh_token.as_deref(), Some("keep-me"));
        assert_eq!(tokens.expires_at, Some(3700));
    }

    #[test]
    fn token_store_roundtrip_and_logout() {
        let dir = std::env::temp_dir().join(format!("stella-oauth-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = TokenStore::new(dir.join("mcp_oauth.json"));

        assert!(store.get("srv").unwrap().is_none());
        let tokens = OAuthTokens {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: Some(9),
            token_endpoint: "https://auth/token".into(),
            client_id: "c".into(),
            client_secret: None,
            scope: None,
            resource: "https://srv/mcp".into(),
        };
        store.put("srv", &tokens).unwrap();
        assert_eq!(store.get("srv").unwrap(), Some(tokens));
        assert_eq!(store.logged_in_servers().unwrap(), vec!["srv".to_string()]);

        assert!(store.remove("srv").unwrap());
        assert!(!store.remove("srv").unwrap());
        assert!(store.get("srv").unwrap().is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn token_store_file_is_owner_only() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = std::env::temp_dir().join(format!("stella-oauth-mode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = TokenStore::new(dir.join("mcp_oauth.json"));
        store
            .put(
                "s",
                &OAuthTokens {
                    access_token: "a".into(),
                    refresh_token: None,
                    expires_at: None,
                    token_endpoint: "t".into(),
                    client_id: "c".into(),
                    client_secret: None,
                    scope: None,
                    resource: "r".into(),
                },
            )
            .unwrap();
        let mode = std::fs::metadata(dir.join("mcp_oauth.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);

        let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o777, 0o700);

        let outside = dir.with_extension("outside.json");
        std::fs::write(&outside, "{}\n").unwrap();
        std::fs::remove_file(dir.join("mcp_oauth.json")).unwrap();
        symlink(&outside, dir.join("mcp_oauth.json")).unwrap();
        assert!(
            store
                .put(
                    "s",
                    &OAuthTokens {
                        access_token: "b".into(),
                        refresh_token: None,
                        expires_at: None,
                        token_endpoint: "t".into(),
                        client_id: "c".into(),
                        client_secret: None,
                        scope: None,
                        resource: "r".into(),
                    },
                )
                .is_err(),
            "token store must reject a symlink target"
        );
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "{}\n");
        let _ = std::fs::remove_file(&outside);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
