//! Finding out who authorizes an MCP server, and where its endpoints are:
//! RFC 9728 protected-resource metadata (the 401 `WWW-Authenticate` hint, then
//! the well-known paths) and RFC 8414 authorization-server metadata with the
//! OIDC `openid-configuration` fallback.
//!
//! Every URL fetched here is named by an untrusted server, so this is also
//! where the rules about *which* of those URLs may be fetched live —
//! [`is_permitted_issuer`] and the same-origin check on the hint.

use reqwest::{Client, StatusCode, Url};
use serde::Deserialize;

use super::HTTP_TIMEOUT;
use crate::error::McpError;
use crate::http::read_capped_body;

/// RFC 9728 protected-resource metadata (tolerant subset).
#[derive(Debug, Default, Deserialize)]
pub(super) struct ProtectedResourceMeta {
    #[serde(default)]
    pub(super) authorization_servers: Vec<String>,
    #[serde(default)]
    pub(super) scopes_supported: Vec<String>,
}

/// RFC 8414 authorization-server metadata (tolerant subset).
#[derive(Debug, Deserialize)]
pub(super) struct AuthServerMeta {
    pub(super) authorization_endpoint: String,
    pub(super) token_endpoint: String,
    #[serde(default)]
    pub(super) registration_endpoint: Option<String>,
    #[serde(default)]
    pub(super) code_challenge_methods_supported: Vec<String>,
}

/// Find the server's protected-resource metadata: the 401
/// `WWW-Authenticate` hint first, then the well-known locations. `None` when
/// the server publishes none (the server's own origin then acts as issuer).
pub(super) async fn discover_protected_resource(
    http: &Client,
    server_url: &str,
) -> Option<ProtectedResourceMeta> {
    // The spec'd path: an unauthenticated request answered 401 with
    // `WWW-Authenticate: Bearer resource_metadata="…"`.
    //
    // The hint is attacker-controlled data from a server we do not trust, and
    // following it is an outbound GET this process makes on the server's
    // behalf — a plain SSRF primitive if the URL may point anywhere (cloud
    // metadata endpoints, an intranet host). RFC 9728 §3.3 requires the
    // metadata URL to belong to the protected resource, so anything
    // cross-origin is ignored and discovery falls back to the well-known
    // paths under the server's OWN origin.
    if let Ok(response) = http.get(server_url).timeout(HTTP_TIMEOUT).send().await
        && response.status() == StatusCode::UNAUTHORIZED
        && let Some(value) = response
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
        && let Some(url) = parse_resource_metadata_hint(value)
        && is_same_origin(&url, server_url)
        && let Some(meta) = fetch_json::<ProtectedResourceMeta>(http, &url).await
    {
        return Some(meta);
    }
    // Fallbacks: well-known with and without the server's path component.
    for candidate in well_known_candidates(server_url, "oauth-protected-resource") {
        if let Some(meta) = fetch_json::<ProtectedResourceMeta>(http, &candidate).await {
            return Some(meta);
        }
    }
    None
}

/// Resolve the authorization server's endpoints (RFC 8414, then OIDC).
pub(super) async fn discover_auth_server(
    http: &Client,
    issuer: &str,
) -> Result<AuthServerMeta, McpError> {
    let mut candidates = well_known_candidates(issuer, "oauth-authorization-server");
    candidates.extend(well_known_candidates(issuer, "openid-configuration"));
    for candidate in &candidates {
        if let Some(meta) = fetch_json::<AuthServerMeta>(http, candidate).await {
            return Ok(meta);
        }
    }
    Err(McpError::Auth(format!(
        "no authorization-server metadata at `{issuer}` (tried {})",
        candidates.join(", ")
    )))
}

/// `resource_metadata="…"` out of a `WWW-Authenticate` challenge.
fn parse_resource_metadata_hint(header: &str) -> Option<String> {
    let start = header.find("resource_metadata=")? + "resource_metadata=".len();
    let rest = &header[start..];
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// RFC 8414-style well-known candidates for `url`: the suffix inserted at the
/// origin, first *with* the URL's path appended, then bare.
fn well_known_candidates(url: &str, suffix: &str) -> Vec<String> {
    let origin = origin_of(url);
    let path = Url::parse(url)
        .ok()
        .map(|u| u.path().trim_end_matches('/').to_string())
        .unwrap_or_default();
    let mut out = Vec::new();
    if !path.is_empty() && path != "/" {
        out.push(format!("{origin}/.well-known/{suffix}{path}"));
    }
    out.push(format!("{origin}/.well-known/{suffix}"));
    out
}

/// Whether the authorization server a resource names may be fetched.
///
/// The value arrives inside the resource's own metadata document, so it is
/// attacker-controlled exactly as far as the MCP server is, and it decides the
/// URL of an outbound GET this process makes — the same SSRF primitive the
/// `resource_metadata` hint carries. The hint's rule does not transfer,
/// though: RFC 9728 §3.3 requires that document to sit at the resource's own
/// origin, while an authorization server is a *separate* entity by design
/// (`mcp.example.com` authorized by `auth.example.com` is the ordinary
/// deployment, and the well-known fallback is the only thing that ever reads
/// the resource's own origin as an issuer).
///
/// What still holds is that a resource out on the internet has no business
/// naming an issuer inside the address space this host can reach and the
/// public cannot — the cloud metadata endpoint at `169.254.169.254`, an
/// intranet box, a service bound to loopback. That is refused. A server that
/// is itself local may name a local issuer, which is what a development or
/// self-hosted deployment looks like, and a cleartext issuer for a
/// TLS-protected resource is refused as the downgrade it is.
pub(super) fn is_permitted_issuer(issuer: &str, server_url: &str) -> bool {
    let (Ok(issuer_url), Ok(server)) = (Url::parse(issuer), Url::parse(server_url)) else {
        return false;
    };
    if !matches!(issuer_url.scheme(), "http" | "https") {
        return false;
    }
    if is_same_origin(issuer, server_url) {
        return true;
    }
    if server.scheme() == "https" && issuer_url.scheme() != "https" {
        return false;
    }
    !is_host_local(&issuer_url) || is_host_local(&server)
}

/// Whether a URL's host names the address space a public peer must not be able
/// to steer this client into: loopback, private, link-local (where the cloud
/// metadata endpoint lives), unspecified, or a name resolvers keep local. A
/// host that cannot be read at all counts as local — refusing is the safe
/// direction for a check that could not run.
fn is_host_local(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return true;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return is_local_ip(ip);
    }
    let host = host.to_ascii_lowercase();
    host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
}

fn is_local_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
        }
        // A v4 address written in v6 clothing (`::ffff:169.254.169.254`) is
        // the same address, so it is ruled on as one.
        std::net::IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_local_ip(std::net::IpAddr::V4(v4)),
            None => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_unique_local()
                    || v6.is_unicast_link_local()
            }
        },
    }
}

/// Whether two URLs share a `scheme://host[:port]` origin. Both sides must
/// actually parse: [`origin_of`] echoes an unparseable input back verbatim, so
/// requiring a parse keeps two identical pieces of garbage from comparing
/// equal and passing the check.
fn is_same_origin(a: &str, b: &str) -> bool {
    Url::parse(a).is_ok() && Url::parse(b).is_ok() && origin_of(a) == origin_of(b)
}

/// `scheme://host[:port]` of a URL (falls back to the input on parse failure).
pub(super) fn origin_of(url: &str) -> String {
    Url::parse(url)
        .ok()
        .map(|u| {
            let mut origin = format!("{}://{}", u.scheme(), u.host_str().unwrap_or_default());
            if let Some(port) = u.port() {
                origin.push_str(&format!(":{port}"));
            }
            origin
        })
        .unwrap_or_else(|| url.trim_end_matches('/').to_string())
}

/// The canonical resource indicator for a server URL: scheme+host+port+path,
/// query and fragment dropped (RFC 8707 as profiled by MCP).
pub(super) fn canonical_resource(server_url: &str) -> Result<String, McpError> {
    let mut url = Url::parse(server_url)
        .map_err(|e| McpError::Auth(format!("invalid server URL `{server_url}`: {e}")))?;
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string().trim_end_matches('/').to_string())
}

async fn fetch_json<T: serde::de::DeserializeOwned>(http: &Client, url: &str) -> Option<T> {
    let response = http.get(url).timeout(HTTP_TIMEOUT).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = read_capped_body(response, url).await.ok()?;
    serde_json::from_str(&body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn www_authenticate_hint_is_extracted() {
        assert_eq!(
            parse_resource_metadata_hint(
                r#"Bearer realm="mcp", resource_metadata="https://srv/.well-known/oauth-protected-resource""#
            )
            .as_deref(),
            Some("https://srv/.well-known/oauth-protected-resource")
        );
        assert_eq!(parse_resource_metadata_hint("Bearer realm=\"x\""), None);
    }

    #[test]
    fn resource_metadata_hint_must_be_same_origin() {
        // RFC 9728 §3.3: only the resource's own origin may publish its
        // metadata. A hostile MCP server pointing the hint at an intranet or
        // cloud-metadata host must not turn this client into its fetcher.
        assert!(is_same_origin(
            "https://srv.example.com/.well-known/oauth-protected-resource",
            "https://srv.example.com/mcp"
        ));
        assert!(!is_same_origin(
            "http://169.254.169.254/latest/meta-data/",
            "https://srv.example.com/mcp"
        ));
        // Same host, different scheme/port are different origins.
        assert!(!is_same_origin(
            "http://srv.example.com/x",
            "https://srv.example.com/mcp"
        ));
        assert!(!is_same_origin(
            "https://srv.example.com:8443/x",
            "https://srv.example.com/mcp"
        ));
        // Unparseable input never matches, even against itself.
        assert!(!is_same_origin("not a url", "not a url"));
    }

    #[test]
    fn a_named_issuer_may_be_cross_origin_but_never_reaches_local_address_space() {
        // The ordinary deployment: the resource and its authorization server
        // are different hosts, which is what RFC 9728 exists to express.
        assert!(is_permitted_issuer(
            "https://auth.example.com",
            "https://mcp.example.com/mcp"
        ));
        assert!(is_permitted_issuer(
            "https://mcp.example.com",
            "https://mcp.example.com/mcp"
        ));
        // The SSRF shape: a server out on the internet naming an issuer only
        // this host can reach. The GET would be made on that server's behalf.
        for inside in [
            "http://169.254.169.254/latest/meta-data/",
            "https://169.254.169.254/",
            "https://[::ffff:169.254.169.254]/",
            "http://127.0.0.1:9000",
            "https://10.1.2.3",
            "https://192.168.1.1",
            "https://[fd00::1]",
            "https://localhost:8080",
            "https://vault.internal",
        ] {
            assert!(
                !is_permitted_issuer(inside, "https://mcp.example.com/mcp"),
                "{inside} must not be fetched for a public resource"
            );
        }
        // A local server may name a local issuer — self-hosted and development
        // deployments run both on loopback, on different ports.
        assert!(is_permitted_issuer(
            "http://127.0.0.1:9000",
            "http://127.0.0.1:8931/mcp"
        ));
        // Cleartext issuer for a TLS-protected resource is a downgrade.
        assert!(!is_permitted_issuer(
            "http://auth.example.com",
            "https://mcp.example.com/mcp"
        ));
        // Neither a non-HTTP scheme nor an unparseable value is a URL to fetch.
        assert!(!is_permitted_issuer(
            "file:///etc/passwd",
            "https://mcp.example.com/mcp"
        ));
        assert!(!is_permitted_issuer(
            "not a url",
            "https://mcp.example.com/mcp"
        ));
    }

    #[test]
    fn well_known_candidates_are_path_aware() {
        assert_eq!(
            well_known_candidates("https://srv.example.com/mcp", "oauth-protected-resource"),
            vec![
                "https://srv.example.com/.well-known/oauth-protected-resource/mcp".to_string(),
                "https://srv.example.com/.well-known/oauth-protected-resource".to_string(),
            ]
        );
        assert_eq!(
            well_known_candidates("https://auth.example.com", "oauth-authorization-server"),
            vec!["https://auth.example.com/.well-known/oauth-authorization-server".to_string()]
        );
    }

    #[test]
    fn canonical_resource_drops_query_and_fragment() {
        assert_eq!(
            canonical_resource("https://SRV.example.com/mcp?x=1#frag").unwrap(),
            "https://srv.example.com/mcp"
        );
    }
}
