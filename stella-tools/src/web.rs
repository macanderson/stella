//! The web family: `web_search`, `web_fetch`, `web_extract_assets`,
//! `web_download`.
//!
//! **Registered by default, switchable off** with `"tools": {"web": "off"}`
//! (any scope), matching the `bash` posture since #710 — the three key-free
//! tools ship on, pinned by `registry`'s
//! `the_key_free_web_family_is_registered_with_no_options_at_all`.
//! `web_search` additionally needs a BYOK provider key (`BRAVE_API_KEY` or
//! `TAVILY_API_KEY`) — no key, no dead schema, exactly like the media tools.
//! A fetched page is untrusted input *and* an uncontrolled egress channel, so
//! an operator who wants egress bounded has to switch the family off rather
//! than decline to switch it on.
//!
//! Logged-in fetches ride the user's own sessions via
//! `~/.stella/web_auth.toml` (override with `STELLA_WEB_AUTH_FILE`):
//! per-domain cookies/headers are injected at request time and never appear
//! in tool output, so the secrets never enter the model's context. reqwest
//! strips `Cookie`/`Authorization` on a cross-host redirect; a secret placed
//! in a custom `[domains.x.headers]` entry is NOT in reqwest's sensitive set
//! and would follow the redirect, so scope custom-header secrets to hosts
//! you trust to redirect.
//!
//! Sub-resources named by a fetched page (`web_extract_assets`' stylesheet
//! list) are fetched SAME-ORIGIN-ONLY with respect to auth: scheme, host and
//! port must all match the page's final URL, or the request goes out bare.
//! Without that fence a page could name `https://internal.corp/secrets.css`
//! and have Stella issue a credentialed request the user never authorized —
//! a confused deputy the SSRF note below does not cover, because the URL is
//! chosen by the page rather than by the operator.
//!
//! No SSRF guard: a session can fetch any http(s) URL the host can reach —
//! including `localhost` and cloud metadata endpoints. It is required for the
//! "fetch my internal tool / dev server" use case, and there is no network
//! allowlist.
//!
//! **The compensating control this used to name is gone.** The exposure was
//! justified here by the family being opt-in, so that reaching a metadata
//! endpoint took a deliberate host action; since #710 the family is
//! registered by default and the only gate is an operator who knows to set
//! `"web": "off"`. Whether that is acceptable, or whether the default surface
//! needs a metadata/loopback denylist, is an open ruling (#615) — this note
//! exists so the next reader does not re-derive the retired guarantee from a
//! stale comment.
//!
//! Fetch/extract are `read_only` (they observe the web, not the workspace)
//! but never `speculation_safe`: every run is real traffic against someone
//! else's server — and `web_search` spends a metered BYOK key — so a stream
//! retry must not be able to run one twice (#923);
//! `web_download` writes through [`crate::resolve_within_root`] and is
//! classified into the file-touch ledger like `write_file`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::Url;
use serde::Deserialize;
use serde_json::Value;
use stella_protocol::tool::{ToolOutput, ToolSchema};

use crate::registry::Tool;
use crate::web_extract;

/// Cap on a fetched page or stylesheet — enough for any real document,
/// small enough that a runaway stream can't balloon memory.
const FETCH_CAP_BYTES: usize = 4 * 1024 * 1024;
/// Cap on a `web_download` body.
const DOWNLOAD_CAP_BYTES: usize = 64 * 1024 * 1024;
/// Default cap on rendered `web_fetch` content, in characters.
const DEFAULT_MAX_LENGTH: usize = 30_000;
/// Stylesheets fetched per `web_extract_assets` call by default.
const DEFAULT_MAX_STYLESHEETS: usize = 8;
/// In-flight stylesheet fetches per `web_extract_assets` call. The sheets
/// were fetched one at a time — up to 24 serial round-trips to what is
/// usually one origin; bounded so a large `max_stylesheets` is not also a
/// 24-connection burst.
const STYLESHEET_FETCH_CONCURRENCY: usize = 6;
/// Per-stylesheet and per-`<meta>` lines rendered by `web_extract_assets`.
/// The manifest lists EVERY sheet and meta tag the page declares, and the
/// page chooses how many it declares — every other section here is already
/// capped, so these two were the remaining unbounded paths from a fetched
/// document into the transcript.
const MAX_RENDERED_STYLESHEETS: usize = 40;
const MAX_RENDERED_META: usize = 12;

const DEFAULT_USER_AGENT: &str = concat!("stella/", env!("CARGO_PKG_VERSION"));

const BRAVE_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
const TAVILY_ENDPOINT: &str = "https://api.tavily.com/search";

/// The per-domain auth file, parsed once at registry construction. A parse
/// failure is carried as the `Err` and surfaced as a named error on every
/// web call — a broken secrets file must be loud, never silently
/// unauthenticated.
pub type WebAuthState = Result<WebAuthConfig, String>;

/// `web_auth.toml` — the whole file. Unknown keys are a hard parse error
/// (the Toggle discipline: a typo must be loud, not silently ignored).
#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebAuthConfig {
    #[serde(default)]
    defaults: WebDefaults,
    /// Keyed by registrable domain (`example.com` also matches
    /// `www.example.com`); the longest matching suffix wins.
    #[serde(default)]
    domains: HashMap<String, DomainAuth>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct WebDefaults {
    /// Override the `stella/<version>` User-Agent for every request.
    user_agent: Option<String>,
}

/// One domain's request decoration. Values are secrets: the custom `Debug`
/// on [`WebAuthConfig`] redacts them, and no tool output ever echoes them.
#[derive(Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct DomainAuth {
    /// Sent as the `Cookie` header — paste a logged-in session's cookies.
    cookie: Option<String>,
    /// Sent as the `Authorization` header.
    authorization: Option<String>,
    /// Arbitrary extra headers.
    #[serde(default)]
    headers: HashMap<String, String>,
    /// Per-domain User-Agent override.
    user_agent: Option<String>,
}

impl std::fmt::Debug for WebAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut domains: Vec<&String> = self.domains.keys().collect();
        domains.sort();
        f.debug_struct("WebAuthConfig")
            .field("domains", &domains)
            .field("values", &"<redacted>")
            .finish()
    }
}

impl WebAuthConfig {
    /// Load `$STELLA_WEB_AUTH_FILE`, else `~/.stella/web_auth.toml`.
    /// A missing file is the empty config; an unreadable or unparseable one
    /// is the `Err` every web tool then reports.
    pub fn load_default() -> WebAuthState {
        let path = match std::env::var_os("STELLA_WEB_AUTH_FILE") {
            Some(explicit) => PathBuf::from(explicit),
            None => match std::env::var_os("HOME") {
                Some(home) => PathBuf::from(home).join(".stella").join("web_auth.toml"),
                None => return Ok(Self::default()),
            },
        };
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("cannot parse {}: {e}", path.display()))
    }

    /// The auth entry for `host`: exact match or subdomain, longest
    /// configured suffix winning (`api.example.com` over `example.com`).
    fn for_host(&self, host: &str) -> Option<(&str, &DomainAuth)> {
        self.domains
            .iter()
            .filter(|(domain, _)| host == domain.as_str() || host.ends_with(&format!(".{domain}")))
            .max_by_key(|(domain, _)| domain.len())
            .map(|(domain, auth)| (domain.as_str(), auth))
    }
}

/// The one HTTP client every web fetch shares.
///
/// A `reqwest::Client` owns a connection pool and a TLS configuration, and
/// this used to be rebuilt per request — so `web_extract_assets`, which pulls
/// up to `max_stylesheets` sub-resources from a single origin inside one call,
/// paid a fresh pool and a fresh TLS handshake for each one instead of reusing
/// the connection it had just opened to that very host.
///
/// The per-domain user agent was the only thing that varied between them, and
/// it does not need to be baked into the client: [`fetch_url`] sends it as a
/// per-request header, which overrides this default and, like the default,
/// carries across redirects.
fn shared_client() -> Result<reqwest::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(60))
                .user_agent(DEFAULT_USER_AGENT)
                .redirect(reqwest::redirect::Policy::limited(10))
                .build()
                .map_err(|e| format!("http client: {e}"))
        })
        .clone()
}

/// A fetched response body, capped and annotated.
struct Fetched {
    final_url: Url,
    content_type: String,
    bytes: Vec<u8>,
    truncated: bool,
    /// The `web_auth.toml` domain whose auth decorated the request, if any
    /// — reported by NAME only, never by value.
    authed_domain: Option<String>,
}

/// Two URLs share an origin when scheme, host and port all match — the
/// web's own definition, not just the host. `https://x/` and `http://x/` are
/// deliberately different origins: a page that downgrades a sub-resource to
/// plaintext must not take the user's cookie with it.
fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// GET `url_str` with the configured per-domain auth, streaming at most
/// `cap` bytes. Non-2xx is an error carrying a login hint on 401/403.
///
/// `auth_origin` is the confused-deputy fence for PAGE-NAMED sub-resources:
/// when `Some(page)`, the `web_auth.toml` entry is attached only if this URL
/// shares `page`'s origin. It gates the lookup rather than the call site
/// because `WebAuthConfig::for_host` matches subdomains too — an attacker
/// page naming `https://internal.corp/x.css` would otherwise have Stella
/// issue a credentialed request the user never authorized.
async fn fetch_raw(
    url_str: &str,
    auth: &WebAuthConfig,
    cap: usize,
    auth_origin: Option<&Url>,
) -> Result<Fetched, String> {
    let url = Url::parse(url_str).map_err(|e| format!("invalid URL `{url_str}`: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!(
            "only http/https URLs can be fetched — got scheme `{}`",
            url.scheme()
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| format!("URL `{url_str}` has no host"))?
        .to_string();
    // Cross-origin sub-resource: no cookie, no Authorization, no custom
    // headers — and no per-domain User-Agent either, since none of that
    // configuration was meant for a host the fetched page chose.
    let domain_auth = match auth_origin {
        Some(origin) if !same_origin(&url, origin) => None,
        _ => auth.for_host(&host),
    };
    let user_agent = domain_auth
        .and_then(|(_, a)| a.user_agent.as_deref())
        .or(auth.defaults.user_agent.as_deref())
        .unwrap_or(DEFAULT_USER_AGENT);
    let client = shared_client()?;
    // The user agent rides per-request rather than per-client so every fetch can
    // share one connection pool regardless of which domain's UA it needs.
    let mut request = client
        .get(url.clone())
        .header(reqwest::header::USER_AGENT, user_agent);
    let mut authed_domain = None;
    if let Some((domain, entry)) = domain_auth {
        if let Some(cookie) = &entry.cookie {
            request = request.header(reqwest::header::COOKIE, cookie.as_str());
        }
        if let Some(authorization) = &entry.authorization {
            request = request.header(reqwest::header::AUTHORIZATION, authorization.as_str());
        }
        for (name, value) in &entry.headers {
            request = request.header(name.as_str(), value.as_str());
        }
        authed_domain = Some(domain.to_string());
    }
    let mut response = request
        .send()
        .await
        .map_err(|e| format!("fetch of {url} failed: {e}"))?;
    let status = response.status();
    let final_url = response.url().clone();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !status.is_success() {
        let hint = if matches!(status.as_u16(), 401 | 403) && authed_domain.is_none() {
            format!(
                " — if this site needs a login, add a `[domains.\"{host}\"]` entry with your \
                 session cookie to ~/.stella/web_auth.toml"
            )
        } else if matches!(status.as_u16(), 401 | 403) {
            format!(" — the configured auth for `{host}` was sent but rejected; it may be expired")
        } else {
            String::new()
        };
        return Err(format!("HTTP {status} fetching {final_url}{hint}"));
    }
    let mut bytes: Vec<u8> = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("reading {final_url}: {e}"))?
    {
        if bytes.len() + chunk.len() > cap {
            bytes.extend_from_slice(&chunk[..cap - bytes.len()]);
            truncated = true;
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Fetched {
        final_url,
        content_type,
        bytes,
        truncated,
        authed_domain,
    })
}

/// True when the body is worth rendering as text at all.
fn is_texty(content_type: &str, bytes: &[u8]) -> bool {
    let ct = content_type.to_ascii_lowercase();
    ct.starts_with("text/")
        || ct.contains("json")
        || ct.contains("xml")
        || ct.contains("javascript")
        || ct.contains("html")
        || ct.contains("css")
        || ct.contains("svg")
        || (ct.is_empty() && std::str::from_utf8(bytes).is_ok())
}

fn looks_like_html(content_type: &str, body: &str) -> bool {
    let ct = content_type.to_ascii_lowercase();
    ct.contains("html")
        || (ct.is_empty()
            && (body.trim_start().starts_with("<!") || body.trim_start().starts_with("<html")))
}

fn auth_note(fetched: &Fetched) -> String {
    match &fetched.authed_domain {
        Some(domain) => format!(", authenticated via web_auth.toml for `{domain}`"),
        None => String::new(),
    }
}

fn require_auth(state: &WebAuthState) -> Result<&WebAuthConfig, ToolOutput> {
    state.as_ref().map_err(|e| ToolOutput::Error {
        message: format!("web_auth.toml is broken — fix or remove it: {e}"),
    })
}

fn require_str<'a>(input: &'a Value, field: &str) -> Result<&'a str, ToolOutput> {
    input
        .get(field)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| ToolOutput::Error {
            message: format!("`{field}` is required"),
        })
}

// web_search

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchProvider {
    Brave,
    Tavily,
}

impl SearchProvider {
    fn id(self) -> &'static str {
        match self {
            SearchProvider::Brave => "brave",
            SearchProvider::Tavily => "tavily",
        }
    }
}

/// A key-authenticated search backend pinned to one endpoint. Tests point
/// `endpoint` at a mock server via [`SearchBackend::with_endpoint`].
#[derive(Clone)]
pub struct SearchBackend {
    provider: SearchProvider,
    key: String,
    endpoint: String,
}

impl std::fmt::Debug for SearchBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchBackend")
            .field("provider", &self.provider)
            .field("key", &"<redacted>")
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

impl SearchBackend {
    pub fn with_endpoint(
        provider: SearchProvider,
        key: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            key: key.into(),
            endpoint: endpoint.into(),
        }
    }
}

/// Detect a BYOK search backend from the environment: `BRAVE_API_KEY` wins
/// (a dedicated web-search API), then `TAVILY_API_KEY`.
pub fn detect_search_backend() -> Option<SearchBackend> {
    detect_search_backend_with(|name| std::env::var(name).ok())
}

/// [`detect_search_backend`] with the env lookup injectable for tests.
pub fn detect_search_backend_with(env: impl Fn(&str) -> Option<String>) -> Option<SearchBackend> {
    for (var, provider, endpoint) in [
        ("BRAVE_API_KEY", SearchProvider::Brave, BRAVE_ENDPOINT),
        ("TAVILY_API_KEY", SearchProvider::Tavily, TAVILY_ENDPOINT),
    ] {
        if let Some(key) = env(var).filter(|k| !k.trim().is_empty()) {
            return Some(SearchBackend::with_endpoint(provider, key, endpoint));
        }
    }
    None
}

pub struct WebSearch(pub SearchBackend);

#[async_trait]
impl Tool for WebSearch {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_search".into(),
            description: "Search the web and get ranked results (title, URL, snippet). Use for \
                          anything the workspace can't answer: current docs, libraries, news, \
                          designs to reference. Follow up with web_fetch to read a result."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    },
                    "count": {
                        "type": "integer",
                        "description": "Results to return, 1-20 (default 8)"
                    }
                },
                "required": ["query"]
            }),
            read_only: true,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, _root: &std::path::Path) -> ToolOutput {
        let query = match require_str(input, "query") {
            Ok(q) => q,
            Err(e) => return e,
        };
        let count = input
            .get("count")
            .and_then(|v| v.as_u64())
            .unwrap_or(8)
            .clamp(1, 20);
        let results = match self.0.provider {
            SearchProvider::Brave => brave_search(&self.0, query, count).await,
            SearchProvider::Tavily => tavily_search(&self.0, query, count).await,
        };
        match results {
            Ok(results) if results.is_empty() => ToolOutput::Ok {
                content: format!("no results for \"{query}\" ({})", self.0.provider.id()),
            },
            Ok(results) => {
                let mut content = format!(
                    "{} results for \"{query}\" ({}):\n",
                    results.len(),
                    self.0.provider.id()
                );
                for (idx, r) in results.iter().enumerate() {
                    content.push_str(&format!(
                        "\n{}. {}\n   {}\n   {}\n",
                        idx + 1,
                        r.title,
                        r.url,
                        r.snippet
                    ));
                }
                ToolOutput::Ok { content }
            }
            Err(message) => ToolOutput::Error { message },
        }
    }
}

struct SearchHit {
    title: String,
    url: String,
    snippet: String,
}

async fn brave_search(
    backend: &SearchBackend,
    query: &str,
    count: u64,
) -> Result<Vec<SearchHit>, String> {
    let client = shared_client()?;
    let response = client
        .get(&backend.endpoint)
        .query(&[("q", query), ("count", &count.to_string())])
        .header("X-Subscription-Token", backend.key.as_str())
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("Brave search failed: {e}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let preview = crate::exec::truncate_preview(&body, 300);
        return Err(format!("Brave search: HTTP {status}: {preview}"));
    }
    let json: Value =
        serde_json::from_str(&body).map_err(|e| format!("Brave returned non-JSON: {e}"))?;
    let empty = Vec::new();
    let results = json
        .pointer("/web/results")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    Ok(results
        .iter()
        .map(|r| SearchHit {
            title: r.get("title").and_then(|v| v.as_str()).unwrap_or("").into(),
            url: r.get("url").and_then(|v| v.as_str()).unwrap_or("").into(),
            snippet: r
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into(),
        })
        .collect())
}

async fn tavily_search(
    backend: &SearchBackend,
    query: &str,
    count: u64,
) -> Result<Vec<SearchHit>, String> {
    let client = shared_client()?;
    let response = client
        .post(&backend.endpoint)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", backend.key),
        )
        .json(&serde_json::json!({ "query": query, "max_results": count }))
        .send()
        .await
        .map_err(|e| format!("Tavily search failed: {e}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        let preview = crate::exec::truncate_preview(&body, 300);
        return Err(format!("Tavily search: HTTP {status}: {preview}"));
    }
    let json: Value =
        serde_json::from_str(&body).map_err(|e| format!("Tavily returned non-JSON: {e}"))?;
    let empty = Vec::new();
    let results = json
        .get("results")
        .and_then(|v| v.as_array())
        .unwrap_or(&empty);
    Ok(results
        .iter()
        .map(|r| SearchHit {
            title: r.get("title").and_then(|v| v.as_str()).unwrap_or("").into(),
            url: r.get("url").and_then(|v| v.as_str()).unwrap_or("").into(),
            snippet: r
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .into(),
        })
        .collect())
}

// web_fetch

pub struct WebFetch(pub Arc<WebAuthState>);

#[async_trait]
impl Tool for WebFetch {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_fetch".into(),
            description: "Fetch a URL and read it. HTML is rendered as markdown with absolute \
                          links (default), or as plain text or raw HTML via `format`. Sites \
                          configured in web_auth.toml are fetched with the user's own login. \
                          For binary files use web_download instead."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The http(s) URL to fetch"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["markdown", "text", "html"],
                        "description": "Rendering for HTML pages (default markdown)"
                    },
                    "max_length": {
                        "type": "integer",
                        "description": "Character cap on the returned content (default 30000)"
                    }
                },
                "required": ["url"]
            }),
            read_only: true,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, _root: &std::path::Path) -> ToolOutput {
        let auth = match require_auth(&self.0) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let url = match require_str(input, "url") {
            Ok(u) => u,
            Err(e) => return e,
        };
        let format = input
            .get("format")
            .and_then(|v| v.as_str())
            .unwrap_or("markdown");
        let max_length = input
            .get("max_length")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_LENGTH)
            // The upper clamp is what a context window can actually absorb
            // (#616): 120k chars ≈ 30k tokens. The old 400k ceiling let one
            // page displace most of a session's budget.
            .clamp(200, 120_000);

        let fetched = match fetch_raw(url, auth, FETCH_CAP_BYTES, None).await {
            Ok(f) => f,
            Err(message) => return ToolOutput::Error { message },
        };
        if !is_texty(&fetched.content_type, &fetched.bytes) {
            return ToolOutput::Ok {
                content: format!(
                    "{} is binary content ({}, {} bytes{}) — use web_download to save it \
                     into the workspace",
                    fetched.final_url,
                    if fetched.content_type.is_empty() {
                        "unknown type"
                    } else {
                        &fetched.content_type
                    },
                    fetched.bytes.len(),
                    if fetched.truncated {
                        "+, truncated"
                    } else {
                        ""
                    },
                ),
            };
        }
        let body = String::from_utf8_lossy(&fetched.bytes);
        let (title, mut content) = if looks_like_html(&fetched.content_type, &body) {
            match format {
                "html" => (None, body.into_owned()),
                "text" => web_extract::html_to_text(&body),
                _ => web_extract::html_to_markdown(&body, Some(&fetched.final_url)),
            }
        } else {
            (None, body.into_owned())
        };

        let total_chars = content.chars().count();
        let mut truncation_note = String::new();
        if total_chars > max_length {
            content = content.chars().take(max_length).collect();
            truncation_note = format!(
                "\n\n[truncated at {max_length} of {total_chars} chars — raise `max_length` \
                 or fetch a more specific page]"
            );
        } else if fetched.truncated {
            truncation_note = format!(
                "\n\n[response body exceeded the {} MB fetch cap and was cut off]",
                FETCH_CAP_BYTES / (1024 * 1024)
            );
        }

        let header = match title {
            Some(title) => format!("# {title}\n"),
            None => String::new(),
        };
        ToolOutput::Ok {
            content: format!(
                "{header}Source: {} ({}, {} bytes{})\n\n{content}{truncation_note}",
                fetched.final_url,
                if fetched.content_type.is_empty() {
                    "unknown type"
                } else {
                    &fetched.content_type
                },
                fetched.bytes.len(),
                auth_note(&fetched),
            ),
        }
    }
}

// web_extract_assets

pub struct WebExtractAssets(pub Arc<WebAuthState>);

#[async_trait]
impl Tool for WebExtractAssets {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_extract_assets".into(),
            description: "Fetch a page and mine its design assets: stylesheets, scripts, \
                          images, fonts, plus design tokens distilled from the CSS — colors \
                          and font families by frequency, custom properties, @font-face \
                          sources. The starting point for building a design system from an \
                          existing site; save individual assets with web_download."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The http(s) page to analyze"
                    },
                    "max_stylesheets": {
                        "type": "integer",
                        "description": "External stylesheets to fetch and analyze (default 8)"
                    }
                },
                "required": ["url"]
            }),
            read_only: true,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, _root: &std::path::Path) -> ToolOutput {
        let auth = match require_auth(&self.0) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let url = match require_str(input, "url") {
            Ok(u) => u,
            Err(e) => return e,
        };
        let max_stylesheets = input
            .get("max_stylesheets")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_STYLESHEETS)
            .clamp(0, 24);

        let fetched = match fetch_raw(url, auth, FETCH_CAP_BYTES, None).await {
            Ok(f) => f,
            Err(message) => return ToolOutput::Error { message },
        };
        let body = String::from_utf8_lossy(&fetched.bytes);
        if !looks_like_html(&fetched.content_type, &body) {
            return ToolOutput::Error {
                message: format!(
                    "{} is not an HTML page ({}) — web_extract_assets needs a page to parse",
                    fetched.final_url,
                    if fetched.content_type.is_empty() {
                        "unknown type"
                    } else {
                        &fetched.content_type
                    },
                ),
            };
        }
        let manifest = web_extract::extract_assets(&body, &fetched.final_url);

        // Distill design tokens from the inline blocks plus the first N
        // external stylesheets, fetched with the same per-domain auth.
        let mut css = web_extract::CssAccumulator::default();
        for inline in &manifest.inline_css {
            css.add_css(inline, Some(&fetched.final_url));
        }
        // Fetched concurrently (bounded — one slow CDN must not serialize
        // the rest), then folded in manifest order so notes and token
        // accumulation stay deterministic. Same-origin only: the stylesheet
        // list comes from the FETCHED PAGE, so a hostile page could
        // otherwise point Stella's stored credentials at an internal host
        // of its choosing.
        use futures_util::StreamExt as _;
        let page_url = &fetched.final_url;
        let results: Vec<(String, Result<Fetched, String>)> = futures_util::stream::iter(
            manifest
                .stylesheets
                .iter()
                .take(max_stylesheets)
                .cloned()
                .map(|sheet_url| async move {
                    let result = fetch_raw(&sheet_url, auth, FETCH_CAP_BYTES, Some(page_url)).await;
                    (sheet_url, result)
                }),
        )
        .buffered(STYLESHEET_FETCH_CONCURRENCY)
        .collect()
        .await;
        let mut sheet_notes: Vec<String> = Vec::new();
        for (sheet_url, result) in results {
            match result {
                Ok(sheet) => {
                    let text = String::from_utf8_lossy(&sheet.bytes);
                    if let Ok(sheet_base) = Url::parse(&sheet_url) {
                        css.add_css(&text, Some(&sheet_base));
                    } else {
                        css.add_css(&text, Some(&fetched.final_url));
                    }
                    sheet_notes.push(format!(
                        "- {sheet_url} (fetched, {:.1} KB)",
                        sheet.bytes.len() as f64 / 1024.0
                    ));
                }
                Err(e) => sheet_notes.push(format!("- {sheet_url} (fetch failed: {e})")),
            }
        }
        for sheet_url in manifest.stylesheets.iter().skip(max_stylesheets) {
            sheet_notes.push(format!(
                "- {sheet_url} (not fetched — over `max_stylesheets`)"
            ));
        }
        let tokens = css.finish();

        let mut out = format!("# Asset manifest for {}\n", fetched.final_url);
        if let Some(title) = &manifest.title {
            out.push_str(&format!("Title: {title}\n"));
        }
        out.push_str(&format!(
            "Fetched: {} bytes{}\n",
            fetched.bytes.len(),
            auth_note(&fetched)
        ));
        for (name, value) in manifest.meta.iter().take(MAX_RENDERED_META) {
            out.push_str(&format!("{name}: {value}\n"));
        }
        if manifest.meta.len() > MAX_RENDERED_META {
            out.push_str(&format!(
                "… {} more meta values\n",
                manifest.meta.len() - MAX_RENDERED_META
            ));
        }

        out.push_str(&format!(
            "\n## Stylesheets ({} external, {} inline blocks)\n",
            manifest.stylesheets.len(),
            manifest.inline_css.len()
        ));
        for note in sheet_notes.iter().take(MAX_RENDERED_STYLESHEETS) {
            out.push_str(note);
            out.push('\n');
        }
        if sheet_notes.len() > MAX_RENDERED_STYLESHEETS {
            out.push_str(&format!(
                "- … {} more (raise `max_stylesheets` to analyze them)\n",
                sheet_notes.len() - MAX_RENDERED_STYLESHEETS
            ));
        }

        out.push_str("\n## Design tokens\n");
        push_ranked(&mut out, "Colors (by frequency)", &tokens.colors, 24);
        push_ranked(&mut out, "Font families", &tokens.font_families, 12);
        if !tokens.font_faces.is_empty() {
            out.push_str("\n### @font-face\n");
            for face in tokens.font_faces.iter().take(12) {
                out.push_str(&format!(
                    "- \"{}\" ← {}\n",
                    face.family,
                    face.sources.join(", ")
                ));
            }
        }
        if !tokens.custom_props.is_empty() {
            out.push_str(&format!(
                "\n### CSS custom properties ({} shown of {})\n",
                tokens.custom_props.len().min(60),
                tokens.custom_props.len()
            ));
            for (name, value) in tokens.custom_props.iter().take(60) {
                out.push_str(&format!("- {name}: {value}\n"));
            }
        }

        push_list(&mut out, "Preloaded fonts", &manifest.fonts, 20);
        push_list(&mut out, "Images", &manifest.images, 40);
        push_list(&mut out, "Scripts", &manifest.scripts, 20);

        out.push_str(
            "\nURLs are absolute — save any of them into the workspace with web_download \
             (e.g. under .stella/artifacts/web/).",
        );
        ToolOutput::Ok { content: out }
    }
}

fn push_ranked(out: &mut String, heading: &str, ranked: &[(String, usize)], cap: usize) {
    if ranked.is_empty() {
        return;
    }
    out.push_str(&format!("\n### {heading}\n"));
    for (value, count) in ranked.iter().take(cap) {
        out.push_str(&format!("- {value} ({count})\n"));
    }
    if ranked.len() > cap {
        out.push_str(&format!("- … {} more\n", ranked.len() - cap));
    }
}

fn push_list(out: &mut String, heading: &str, items: &[String], cap: usize) {
    if items.is_empty() {
        return;
    }
    out.push_str(&format!("\n## {heading} ({})\n", items.len()));
    for item in items.iter().take(cap) {
        out.push_str(&format!("- {item}\n"));
    }
    if items.len() > cap {
        out.push_str(&format!("- … {} more\n", items.len() - cap));
    }
}

// web_download

pub struct WebDownload(pub Arc<WebAuthState>);

#[async_trait]
impl Tool for WebDownload {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "web_download".into(),
            description: "Download a URL to a file inside the workspace (images, fonts, \
                          stylesheets, archives — anything web_fetch reports as binary). \
                          Generated assets belong under .stella/artifacts/web/."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The http(s) URL to download"
                    },
                    "path": {
                        "type": "string",
                        "description": "Workspace-relative destination file path"
                    }
                },
                "required": ["url", "path"]
            }),
            read_only: false,
            speculation_safe: false,
        }
    }

    async fn execute(&self, input: &Value, root: &std::path::Path) -> ToolOutput {
        let auth = match require_auth(&self.0) {
            Ok(a) => a,
            Err(e) => return e,
        };
        let url = match require_str(input, "url") {
            Ok(u) => u,
            Err(e) => return e,
        };
        let path = match require_str(input, "path") {
            Ok(p) => p,
            Err(e) => return e,
        };
        let Some(full) = crate::resolve_within_root(root, path) else {
            return ToolOutput::Error {
                message: format!("path `{path}` escapes the workspace root"),
            };
        };
        let mut fetched = match fetch_raw(url, auth, DOWNLOAD_CAP_BYTES, None).await {
            Ok(f) => f,
            Err(message) => return ToolOutput::Error { message },
        };
        if fetched.truncated {
            return ToolOutput::Error {
                message: format!(
                    "{} exceeds the {} MB download cap — nothing was written",
                    fetched.final_url,
                    DOWNLOAD_CAP_BYTES / (1024 * 1024)
                ),
            };
        }
        // `tokio::fs` like every other file-writing tool in the crate: this
        // is an async `execute` and the payload is up to DOWNLOAD_CAP_BYTES,
        // so a blocking write here parks a runtime worker for 64 MB. A
        // let-chain condition cannot host the `.await`, hence the `match`.
        let parent_ready = match full.parent() {
            Some(parent) => tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| format!("cannot create {}: {e}", parent.display())),
            None => Ok(()),
        };
        if let Err(message) = parent_ready {
            return ToolOutput::Error { message };
        }
        // The same durable replacement `write_file`/`edit_file` use, for the
        // same reason ([`crate::durable_write`]): a download lands on the same
        // workspace paths they do, and `tokio::fs::write` opens with
        // `O_TRUNC` — a re-download that fails mid-write would leave the
        // existing artifact truncated with the replacement nowhere on disk.
        // `mem::take` hands the body over without a second 64 MB copy; the
        // rest of `fetched` (final URL, content type, auth provenance) is
        // still needed for the report below.
        let bytes = std::mem::take(&mut fetched.bytes);
        let byte_count = bytes.len();
        if let Err(e) = crate::durable_write::write_file_durably(full.clone(), bytes).await {
            return ToolOutput::Error {
                message: format!("cannot write {}: {e}", full.display()),
            };
        }
        ToolOutput::Ok {
            content: format!(
                "downloaded {} → {path} ({byte_count} bytes, {}{})",
                fetched.final_url,
                if fetched.content_type.is_empty() {
                    "unknown type"
                } else {
                    &fetched.content_type
                },
                auth_note(&fetched),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_have_the_right_names_and_read_only_partition() {
        let auth: Arc<WebAuthState> = Arc::new(Ok(WebAuthConfig::default()));
        let backend =
            SearchBackend::with_endpoint(SearchProvider::Brave, "k", "https://example.test");
        for (schema, read_only) in [
            (WebSearch(backend).schema(), true),
            (WebFetch(auth.clone()).schema(), true),
            (WebExtractAssets(auth.clone()).schema(), true),
            (WebDownload(auth).schema(), false),
        ] {
            assert!(schema.name.starts_with("web_"), "{}", schema.name);
            assert_eq!(schema.read_only, read_only, "{}", schema.name);
        }
    }

    #[test]
    fn search_backend_detection_prefers_brave_and_skips_blank_keys() {
        let backend = detect_search_backend_with(|name| match name {
            "BRAVE_API_KEY" => Some("bk".into()),
            "TAVILY_API_KEY" => Some("tk".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(backend.provider, SearchProvider::Brave);

        let backend = detect_search_backend_with(|name| match name {
            "BRAVE_API_KEY" => Some("   ".into()),
            "TAVILY_API_KEY" => Some("tk".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(backend.provider, SearchProvider::Tavily);

        assert!(detect_search_backend_with(|_| None).is_none());
    }

    #[test]
    fn domain_auth_matches_subdomains_and_prefers_the_longest_suffix() {
        let config: WebAuthConfig = toml::from_str(
            r#"
            [domains."example.com"]
            cookie = "base"
            [domains."api.example.com"]
            cookie = "api"
            "#,
        )
        .unwrap();
        assert_eq!(config.for_host("example.com").unwrap().0, "example.com");
        assert_eq!(config.for_host("www.example.com").unwrap().0, "example.com");
        assert_eq!(
            config.for_host("api.example.com").unwrap().0,
            "api.example.com"
        );
        assert_eq!(
            config.for_host("v2.api.example.com").unwrap().0,
            "api.example.com"
        );
        assert!(config.for_host("example.org").is_none());
        assert!(config.for_host("notexample.com").is_none());
    }

    #[test]
    fn auth_config_debug_never_leaks_values_and_typos_are_loud() {
        let config: WebAuthConfig = toml::from_str(
            r#"
            [domains."example.com"]
            cookie = "secret-session-value"
            authorization = "Bearer secret-token"
            "#,
        )
        .unwrap();
        let debug = format!("{config:?}");
        assert!(!debug.contains("secret"), "{debug}");
        assert!(debug.contains("example.com"), "{debug}");

        let typo: Result<WebAuthConfig, _> = toml::from_str(
            r#"
            [domains."example.com"]
            cokie = "oops"
            "#,
        );
        assert!(typo.is_err(), "unknown keys must be a loud parse error");
    }

    /// The fence `web_extract_assets` puts between a page and the user's
    /// stored credentials. A stylesheet URL the PAGE chose only carries auth
    /// when it is the page's own origin — scheme, host AND port.
    #[test]
    fn same_origin_is_scheme_host_and_port() {
        let page = Url::parse("https://example.com/docs/index.html").unwrap();
        for same in [
            "https://example.com/a.css",
            "https://example.com:443/nested/a.css",
        ] {
            assert!(
                same_origin(&Url::parse(same).unwrap(), &page),
                "{same} is the page's own origin"
            );
        }
        for different in [
            // The confused deputy from the audit: a page naming an internal
            // host to have Stella send that host's stored cookie.
            "https://internal.corp/secrets.css",
            // `for_host` matches subdomains, which is why the fence must sit
            // at the lookup rather than at the caller.
            "https://cdn.example.com/a.css",
            // A downgrade must not carry the cookie either.
            "http://example.com/a.css",
            "https://example.com:8443/a.css",
        ] {
            assert!(
                !same_origin(&Url::parse(different).unwrap(), &page),
                "{different} must not be treated as the page's origin"
            );
        }
    }

    #[test]
    fn search_backend_debug_never_leaks_the_key() {
        let backend = SearchBackend::with_endpoint(
            SearchProvider::Tavily,
            "tvly-secret",
            "https://api.tavily.com/search",
        );
        let debug = format!("{backend:?}");
        assert!(!debug.contains("tvly-secret"), "{debug}");
    }
}
