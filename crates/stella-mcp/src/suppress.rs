// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Auth-probe suppression (#2687): remember which HTTP servers answered a
//! connect-time `401 Unauthorized` so subsequent sessions skip the doomed
//! round trip instead of re-paying it every startup.
//!
//! Before this, a configured server whose auth the user never granted was
//! connected on *every* session: the `initialize` POST went out, came back
//! 401, and the server landed in `failed_servers` as a generic outage — a
//! wasted round trip surfacing as the wrong diagnosis. Now the 401 is
//! recorded in a small on-disk cache ([`AuthProbeCache`], a sibling of the
//! OAuth token store, so `stella-cli` lands it in `.stella/private/`), and
//! [`connect_gate`] answers "should this server even be dialed?" before a
//! transport is built:
//!
//! - **Usable stored tokens always connect.** A completed login overrides any
//!   probe record — that is how `stella mcp login` clears suppression without
//!   this module ever hearing about it.
//! - **A 401 within [`AUTH_PROBE_TTL`] suppresses the probe.** The server is
//!   skipped entirely — no connection, no round trip — and
//!   [`crate::McpToolSet`] advertises a synthetic `login_required` tool in
//!   its place (`toolset/needs_auth.rs`).
//! - **A stored login that cannot be used silently is skipped too.** Tokens
//!   whose access token is expired with no refresh token *will* fail before
//!   any request is sent; the store knowing that is enough to skip the
//!   connect and surface the login affordance instead.
//! - **Everything re-arms.** TTL lapse restores the probe; a fresh 401
//!   re-records; a successful connect retires the record
//!   ([`crate::McpToolSet::connect_with_auth`] clears it).
//!
//! The cache is an optimization, never an authority: every read **fails
//! open** (missing, unreadable, or corrupt cache ⇒ no suppression ⇒ the
//! probe happens, exactly as before this module existed), and the next
//! recorded 401 rewrites the file wholesale, so a corrupt cache self-heals.
//! It holds server names and timestamps only — no secret — but it lives in
//! the same owner-only directory as the token store and is written with the
//! same durable, owner-only contract, because inventing a second location
//! scheme for MCP local state is how files get orphaned.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use stella_store::durable::{MODE_PRIVATE, write_atomic};

use crate::error::McpError;
use crate::oauth::{OAuthTokens, TokenStore, ensure_private_token_parent};

/// How long a recorded 401 suppresses the connect probe. Long enough that a
/// session-per-minute workflow stops hammering an unauthenticated server,
/// short enough that a server-side grant (or a revoked-then-restored token)
/// is picked up without the user having to know this cache exists.
pub const AUTH_PROBE_TTL: Duration = Duration::from_secs(15 * 60);

/// The cache's file name, created beside the OAuth token store by
/// [`AuthProbeCache::sibling_of`] so both live in the caller's one private
/// MCP state directory (`.stella/private/` for `stella-cli`).
const AUTH_PROBE_CACHE_FILE: &str = "mcp_auth_probes.json";

/// The on-disk file: server name → its most recent connect-time 401.
#[derive(Debug, Default, Serialize, Deserialize)]
struct ProbeFile {
    #[serde(default)]
    servers: BTreeMap<String, ProbeRecord>,
}

/// One server's record. A struct (not a bare timestamp) so a future field —
/// say, the `WWW-Authenticate` realm — extends the file without a format
/// break; unknown fields are already tolerated on read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ProbeRecord {
    /// Unix seconds of the last connect attempt this server answered 401.
    unauthorized_at: u64,
}

/// The on-disk 401 cache. Same write discipline as [`TokenStore`]: the whole
/// file is re-read and atomically rewritten per operation (temp + fsync +
/// rename via `stella_store::durable::write_atomic`, owner-only mode), so a
/// reader never sees a half-written cache. Reads fail open — see the module
/// doc for why that is the correct direction here.
#[derive(Debug, Clone)]
pub struct AuthProbeCache {
    path: PathBuf,
}

impl AuthProbeCache {
    /// A cache at an explicit path (tests, or a host with its own layout).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The cache that lives beside `token_store_path` — the constructor
    /// [`crate::OAuthManager`] uses, so the caller's one choice (the token
    /// store path) places both files.
    pub fn sibling_of(token_store_path: &Path) -> Self {
        Self::new(token_store_path.with_file_name(AUTH_PROBE_CACHE_FILE))
    }

    /// Read the whole file, failing open: a missing, unreadable, or corrupt
    /// cache is an empty one. Suppression lost is a probe paid — the
    /// pre-cache behavior — never a server wrongly withheld.
    fn load(&self) -> ProbeFile {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => ProbeFile::default(),
        }
    }

    fn save(&self, file: &ProbeFile) -> Result<(), McpError> {
        ensure_private_token_parent(&self.path)?;
        let json = serde_json::to_string_pretty(file)
            .map_err(|e| McpError::Auth(format!("cannot serialize auth-probe cache: {e}")))?;
        write_atomic(&self.path, json.as_bytes(), MODE_PRIVATE)
            .map_err(|e| McpError::Auth(e.to_string()))
    }

    /// Record that `server` answered 401 at `now_secs`, arming (or re-arming)
    /// its suppression window. Overwrites any earlier record.
    pub fn record_unauthorized(&self, server: &str, now_secs: u64) -> Result<(), McpError> {
        let mut file = self.load();
        file.servers.insert(
            server.to_string(),
            ProbeRecord {
                unauthorized_at: now_secs,
            },
        );
        self.save(&file)
    }

    /// Drop `server`'s record (a successful connect proved the 401 gone);
    /// returns whether one existed.
    pub fn clear(&self, server: &str) -> Result<bool, McpError> {
        let mut file = self.load();
        let existed = file.servers.remove(server).is_some();
        if existed {
            self.save(&file)?;
        }
        Ok(existed)
    }

    /// When `server` last answered 401 (unix seconds), if a record exists.
    pub fn last_unauthorized(&self, server: &str) -> Option<u64> {
        self.load().servers.get(server).map(|r| r.unauthorized_at)
    }
}

/// Whether a recorded 401 at `last_unauthorized` still suppresses the probe
/// at `now_secs`. Pure so the TTL boundary is table-testable; a record from
/// the future (clock stepped backwards) suppresses until the window walks
/// past it, which is bounded by construction.
pub fn probe_suppressed(last_unauthorized: Option<u64>, now_secs: u64, ttl: Duration) -> bool {
    last_unauthorized.is_some_and(|at| now_secs < at.saturating_add(ttl.as_secs()))
}

/// Whether stored tokens are *known* to be unusable without a fresh browser
/// login: the access token is past its stated expiry and no refresh token was
/// granted. Connecting with such a record fails before a byte is sent (the
/// bearer source refuses), so the store knowing this is enough to skip the
/// connect. `None` (no login stored) is `false` — the server may not need
/// auth at all, and only a real 401 may say otherwise.
pub fn tokens_need_login(tokens: Option<&OAuthTokens>, now_secs: u64) -> bool {
    tokens
        .is_some_and(|t| t.refresh_token.is_none() && t.expires_at.is_some_and(|at| now_secs >= at))
}

/// The pre-connect decision for one HTTP server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectGate {
    /// Dial the server normally.
    Connect,
    /// Skip the connect: the server requires authentication the user has not
    /// granted. `reason` is operator-facing prose naming the evidence and the
    /// `stella mcp login` fix.
    AuthRequired { reason: String },
}

/// Decide whether `server` should be dialed at all, from the token store and
/// the probe cache. Read-only: recording a fresh 401 and retiring a record on
/// a successful connect are the *caller's* transitions
/// ([`crate::McpToolSet::connect_with_auth`]), so this stays a pure question
/// over two reads.
pub fn connect_gate(
    store: &TokenStore,
    probes: &AuthProbeCache,
    server: &str,
    now_secs: u64,
) -> ConnectGate {
    // A store read error is treated as "no tokens": the gate then falls to
    // the probe record alone, and a connect that proceeds will surface the
    // store's own (better) error through the normal connect path.
    let tokens = store.get(server).ok().flatten();
    if tokens.is_some() && !tokens_need_login(tokens.as_ref(), now_secs) {
        // A completed login always wins over a stale 401 record — this is how
        // `stella mcp login` clears suppression without a back-channel.
        return ConnectGate::Connect;
    }
    if probe_suppressed(probes.last_unauthorized(server), now_secs, AUTH_PROBE_TTL) {
        return ConnectGate::AuthRequired {
            reason: format!(
                "server `{server}` refused the last connect with HTTP 401 (within the last \
                 {} minutes) — authorize with `stella mcp login {server}` (or `o` on it in \
                 the deck's MCP tab)",
                AUTH_PROBE_TTL.as_secs() / 60
            ),
        };
    }
    if tokens_need_login(tokens.as_ref(), now_secs) {
        return ConnectGate::AuthRequired {
            reason: format!(
                "server `{server}`'s stored OAuth login has expired and granted no refresh \
                 token — log in again with `stella mcp login {server}` (or `o` on it in the \
                 deck's MCP tab)"
            ),
        };
    }
    ConnectGate::Connect
}

/// Unix seconds now — the `now_secs` the non-test callers pass to the pure
/// functions above (tests pass fixed values instead).
pub(crate) fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_cache(tag: &str) -> (PathBuf, AuthProbeCache) {
        let dir =
            std::env::temp_dir().join(format!("stella-auth-probe-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cache = AuthProbeCache::sibling_of(&dir.join("mcp_oauth.json"));
        (dir, cache)
    }

    fn tokens(refresh: Option<&str>, expires_at: Option<u64>) -> OAuthTokens {
        OAuthTokens {
            access_token: "a".into(),
            refresh_token: refresh.map(str::to_string),
            expires_at,
            token_endpoint: "https://auth/token".into(),
            client_id: "c".into(),
            client_secret: None,
            scope: None,
            resource: "https://srv/mcp".into(),
        }
    }

    #[test]
    fn a_probe_record_suppresses_until_the_ttl_boundary_and_not_past_it() {
        let ttl = AUTH_PROBE_TTL;
        assert!(
            !probe_suppressed(None, 1_000, ttl),
            "no record, no suppression"
        );
        assert!(
            probe_suppressed(Some(1_000), 1_000, ttl),
            "immediately after"
        );
        assert!(
            probe_suppressed(Some(1_000), 1_000 + ttl.as_secs() - 1, ttl),
            "one second inside the window"
        );
        assert!(
            !probe_suppressed(Some(1_000), 1_000 + ttl.as_secs(), ttl),
            "the boundary itself restores the probe"
        );
        // A future-dated record (clock stepped back) suppresses but is bounded.
        assert!(probe_suppressed(Some(2_000), 1_000, ttl));
        assert!(!probe_suppressed(Some(2_000), 2_000 + ttl.as_secs(), ttl));
    }

    #[test]
    fn tokens_need_login_only_for_expired_records_with_no_refresh_token() {
        let now = 10_000;
        assert!(
            !tokens_need_login(None, now),
            "no login stored is not 'needs login'"
        );
        assert!(
            !tokens_need_login(Some(&tokens(None, None)), now),
            "no stated expiry: used until a 401 says otherwise"
        );
        assert!(
            !tokens_need_login(Some(&tokens(None, Some(now + 1))), now),
            "still valid"
        );
        assert!(
            !tokens_need_login(Some(&tokens(Some("r"), Some(now - 1))), now),
            "expired but refreshable: the transport refreshes silently"
        );
        assert!(
            tokens_need_login(Some(&tokens(None, Some(now))), now),
            "expired with no refresh token: a connect is doomed before it sends"
        );
    }

    #[test]
    fn cache_roundtrips_records_and_clears() {
        let (dir, cache) = temp_cache("roundtrip");
        assert_eq!(cache.last_unauthorized("srv"), None);
        cache.record_unauthorized("srv", 42).unwrap();
        assert_eq!(cache.last_unauthorized("srv"), Some(42));
        // Re-recording overwrites (a fresh 401 re-arms from now, not then).
        cache.record_unauthorized("srv", 99).unwrap();
        assert_eq!(cache.last_unauthorized("srv"), Some(99));
        assert!(cache.clear("srv").unwrap());
        assert!(!cache.clear("srv").unwrap(), "second clear finds nothing");
        assert_eq!(cache.last_unauthorized("srv"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_cache_fails_open_and_self_heals_on_the_next_record() {
        let (dir, cache) = temp_cache("corrupt");
        cache.record_unauthorized("srv", 42).unwrap();
        let path = dir.join(AUTH_PROBE_CACHE_FILE);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(
            cache.last_unauthorized("srv"),
            None,
            "corrupt reads as empty — suppression lost, probe paid, never a wrong withhold"
        );
        cache.record_unauthorized("other", 7).unwrap();
        assert_eq!(
            cache.last_unauthorized("other"),
            Some(7),
            "rewrite heals the file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gate_decision_table() {
        let (dir, probes) = temp_cache("gate");
        let store = TokenStore::new(dir.join("mcp_oauth.json"));
        let now = 50_000;

        // Nothing known: connect (the probe is how we learn).
        assert_eq!(
            connect_gate(&store, &probes, "srv", now),
            ConnectGate::Connect
        );

        // Fresh 401, no login: suppressed, and the reason names the fix.
        probes.record_unauthorized("srv", now - 60).unwrap();
        match connect_gate(&store, &probes, "srv", now) {
            ConnectGate::AuthRequired { reason } => {
                assert!(reason.contains("stella mcp login srv"), "{reason}");
                assert!(reason.contains("401"), "{reason}");
            }
            other => panic!("expected AuthRequired, got {other:?}"),
        }

        // A completed login overrides the record.
        store.put("srv", &tokens(Some("r"), None)).unwrap();
        assert_eq!(
            connect_gate(&store, &probes, "srv", now),
            ConnectGate::Connect
        );

        // TTL lapse restores the probe (no tokens for this server).
        probes
            .record_unauthorized("stale", now - AUTH_PROBE_TTL.as_secs() - 1)
            .unwrap();
        assert_eq!(
            connect_gate(&store, &probes, "stale", now),
            ConnectGate::Connect
        );

        // Expired, refresh-less tokens skip before connect even with no 401
        // on record — the store alone knows the connect is doomed.
        store.put("dead", &tokens(None, Some(now - 1))).unwrap();
        match connect_gate(&store, &probes, "dead", now) {
            ConnectGate::AuthRequired { reason } => {
                assert!(reason.contains("expired"), "{reason}");
                assert!(reason.contains("stella mcp login dead"), "{reason}");
            }
            other => panic!("expected AuthRequired, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_file_serde_roundtrip_is_exact() {
        let mut file = ProbeFile::default();
        file.servers.insert(
            "srv".into(),
            ProbeRecord {
                unauthorized_at: 123,
            },
        );
        let json = serde_json::to_string(&file).unwrap();
        let back: ProbeFile = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.servers.get("srv"),
            Some(&ProbeRecord {
                unauthorized_at: 123
            })
        );
        // Tolerant of a future field, like every inbound type in this crate.
        let forward: ProbeFile = serde_json::from_str(
            r#"{ "servers": { "srv": { "unauthorized_at": 5, "realm": "x" } } }"#,
        )
        .unwrap();
        assert_eq!(forward.servers["srv"].unauthorized_at, 5);
    }

    #[test]
    fn sibling_of_places_the_cache_next_to_the_token_store() {
        let cache = AuthProbeCache::sibling_of(Path::new("/x/private/mcp_oauth.json"));
        assert_eq!(
            cache.path,
            Path::new("/x/private/mcp_auth_probes.json"),
            "one caller choice (the token store path) places both files"
        );
    }
}
