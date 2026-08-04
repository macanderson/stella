//! Provider-native model discovery — "what does this provider's own API
//! say it serves *right now*". The models.dev master list
//! ([`crate::modelsdev`]) is broad but third-party: it can lag a release
//! by days and its gateway coverage (OpenRouter especially) is a curated
//! subset. Each provider's own `/models` endpoint is authoritative and
//! instant, so `stella models refresh` (and the startup auto-sync) overlay
//! it on top of the master list for every provider whose credential is
//! configured.
//!
//! Four wire shapes cover every built-in provider:
//! - **OpenRouter** — `GET {base}/models`, public. The richest listing:
//!   per-token pricing strings, context/completion limits, and
//!   `supported_parameters` (which is where per-model reasoning/tool
//!   support comes from).
//! - **Anthropic** — `GET {base}/v1/models`, `x-api-key` + versioned,
//!   paginated via `after_id`/`has_more`. Ids, display names, and the two
//!   token limits — `max_tokens` (the OUTPUT ceiling) and `max_input_tokens`
//!   (the context window). No pricing.
//! - **Gemini** — `GET {base}/models`, `x-goog-api-key`, paginated via
//!   `pageToken`. Carries token limits, `thinking`, and the generation
//!   methods used to filter out non-chat rows (embeddings, imagen, aqa).
//! - **OpenAI-compatible** — `GET {base}/models`, bearer auth: OpenAI,
//!   xAI, DeepSeek, Z.ai, local servers, custom gateways. Ids, plus whichever
//!   limit fields the particular gateway chose to publish — the shape is
//!   standardized, the field names are not.
//!
//! This module only fetches and parses (same division of labor as
//! `modelsdev`); merging into the on-disk catalog belongs to `stella-cli`.
//! Every function is best-effort by contract: a provider whose listing
//! endpoint is down, missing (404 on a gateway that never implemented it),
//! or shape-drifted returns an `Err(String)` the caller reports and moves
//! past — discovery failure must never fail a refresh of OTHER providers,
//! and never a turn.

use std::time::Duration;

use crate::credential::ApiKey;
use crate::http;

/// One model as reported by its serving provider's own listing endpoint.
/// Everything except the id is optional: most providers report far less
/// than the master list knows, and a missing field must stay "unknown" so
/// the catalog merge can keep the better value it already has.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderModel {
    /// Provider-native slug, sent verbatim as a request's `model`.
    pub id: String,
    pub display_name: Option<String>,
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    /// USD per million tokens (the catalog's unit).
    pub input_usd_per_mtok: Option<f64>,
    pub output_usd_per_mtok: Option<f64>,
    pub cached_input_usd_per_mtok: Option<f64>,
    pub cache_write_usd_per_mtok: Option<f64>,
    /// Whether the model supports reasoning / extended thinking — `None`
    /// when the provider's listing doesn't say.
    pub supports_reasoning: Option<bool>,
    /// Whether the model accepts tool definitions.
    pub supports_tools: Option<bool>,
}

/// Hard ceiling on one listing response body. The largest real listing
/// (OpenRouter's, with per-model pricing and parameter arrays) is a few MB
/// of JSON, so 16 MiB leaves generous headroom while keeping a misbehaving
/// endpoint from growing the process until it is OOM-killed.
const MAX_LISTING_BYTES: usize = 16 * 1024 * 1024;

/// Wall-clock ceiling on one listing request, send through last byte.
/// `http::client`'s bound is per-read, so a slow-but-never-silent body could
/// otherwise stall the caller indefinitely — and one caller is the CLI's
/// *blocking* startup auto-sync. With [`MAX_PAGES`] this also bounds a whole
/// paginated fetch. A healthy listing answers in single-digit seconds; a
/// provider that cannot produce its own model list inside this window is
/// down for sync purposes, and discovery is best-effort by contract.
const LISTING_TIMEOUT: Duration = Duration::from_secs(20);

/// GET `url` and hand back the body, with the provider's own error text on
/// a non-success status. `headers` are (name, value) pairs — the auth
/// vocabulary differs per provider and none of it may end up in the URL
/// (query-string keys leak into logs and proxies).
///
/// The `client` is passed in rather than built here so a paginated fetch
/// reuses ONE connection pool across its rounds. Building a `reqwest::Client`
/// per request compiles a fresh TLS config and starts an empty pool, so a
/// ten-page sync paid ten full TLS handshakes to the same host — the reuse
/// `reqwest::Client` documents itself as existing for.
async fn get_json(
    client: &reqwest::Client,
    label: &str,
    url: &str,
    headers: &[(&str, &str)],
) -> Result<String, String> {
    get_json_bounded(
        client,
        label,
        url,
        headers,
        MAX_LISTING_BYTES,
        LISTING_TIMEOUT,
    )
    .await
}

/// [`get_json`] with the bounds as parameters, so tests can exercise the cap
/// and the deadline without a 16 MiB fixture or a 20-second wait.
///
/// The body is accumulated chunk by chunk under `max_bytes` — the same shape
/// as `stella-media`'s `download_bytes`: an honest `Content-Length` over the
/// cap costs zero bytes, and the chunk loop catches a missing or lying one.
/// `deadline` covers the whole request; the per-read bound `http::client`
/// carries cannot see total elapsed time.
async fn get_json_bounded(
    client: &reqwest::Client,
    label: &str,
    url: &str,
    headers: &[(&str, &str)],
    max_bytes: usize,
    deadline: Duration,
) -> Result<String, String> {
    let mut request = client.get(url).header("Accept", "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let fetch = async {
        let response = request
            .send()
            .await
            .map_err(|e| format!("could not reach {label}: {e}"))?;
        let status = response.status();
        let oversized = || {
            format!(
                "{label} model list exceeds the {} MiB response cap — refusing to buffer it",
                max_bytes / (1024 * 1024)
            )
        };
        if response
            .content_length()
            .is_some_and(|len| len > max_bytes as u64)
        {
            return Err(oversized());
        }
        // Cap the pre-allocation by the declared length so a lying
        // Content-Length cannot make us reserve max_bytes for a small body.
        let reserve = response.content_length().unwrap_or(0).min(max_bytes as u64) as usize;
        let mut bytes: Vec<u8> = Vec::with_capacity(reserve);
        let mut response = response;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("could not read the {label} response: {e}"))?
        {
            if bytes.len().saturating_add(chunk.len()) > max_bytes {
                return Err(oversized());
            }
            bytes.extend_from_slice(&chunk);
        }
        let body = String::from_utf8_lossy(&bytes).into_owned();
        if !status.is_success() {
            let snippet: String = body.chars().take(200).collect();
            return Err(format!("{label} answered HTTP {status}: {snippet}"));
        }
        Ok(body)
    };
    tokio::time::timeout(deadline, fetch).await.map_err(|_| {
        format!(
            "{label} model list did not answer within {}s — skipping this sync",
            deadline.as_secs()
        )
    })?
}

/// Hard cap on pagination rounds for the providers that page. Ten pages at
/// the page sizes requested (≥100/page everywhere) is far beyond any real
/// listing; the cap exists so a server echoing the same page token forever
/// cannot spin the refresh. Hitting it is an ERROR, not a quiet stop — see
/// [`page_cap_error`].
const MAX_PAGES: usize = 10;

/// The error a pagination loop returns when it runs out of rounds with the
/// provider still offering a next page. Exhausting the cap and finishing
/// cleanly used to be indistinguishable: both `break` out of the loop and
/// returned the pages gathered so far as a success, so a provider legitimately
/// serving more than [`MAX_PAGES`] pages silently produced a truncated catalog
/// and a model picker missing rows with no explanation.
fn page_cap_error(label: &str) -> String {
    format!(
        "{label} model list did not finish within {MAX_PAGES} pages — refusing a silently \
         truncated sync"
    )
}

/// `{base}{path}?{params}` with every value percent-encoded. Pagination
/// cursors are provider-controlled opaque tokens, so interpolating one into a
/// query string lets a value containing `&`, `#`, or `+` truncate the query,
/// inject a parameter, or decode wrong — and the failure mode is a wrong or
/// empty page, not an error.
fn build_url(base: &str, path: &str, params: &[(&str, &str)]) -> Result<String, String> {
    let mut url = reqwest::Url::parse(&format!("{base}{path}"))
        .map_err(|e| format!("`{base}` is not a usable base URL: {e}"))?;
    {
        let mut pairs = url.query_pairs_mut();
        for (name, value) in params {
            pairs.append_pair(name, value);
        }
    }
    Ok(url.into())
}

/// The first of `names` this row carries as a non-negative integer.
///
/// The OpenAI-compatible "standard" is a shape, not a schema: gateways that
/// agree on `{"data": [{"id": ...}]}` disagree on what the limit fields are
/// called. Trying an ordered list beats picking one name and silently
/// learning nothing from every gateway that chose a different one.
fn first_u64(row: &serde_json::Value, names: &[&str]) -> Option<u64> {
    names.iter().find_map(|name| {
        row.get(*name)
            .and_then(|v| v.as_u64())
            // A published zero is "no limit stated", not "a limit of zero" —
            // and it must not be carried, because downstream a `Some(0)`
            // ceiling is filtered back out to the engine default anyway while
            // still overwriting a real value the master list already knew.
            .filter(|v| *v > 0)
    })
}

// OpenRouter

/// A USD-per-TOKEN decimal string ("0.000003") → USD per million tokens.
/// OpenRouter prices are strings, sometimes "-1" for dynamically-priced
/// rows (the `openrouter/auto` meta-router) — negative means unknown.
fn per_token_str_to_per_mtok(raw: Option<&serde_json::Value>) -> Option<f64> {
    let value = raw?;
    let per_token = match value {
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok()?,
        serde_json::Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    (per_token >= 0.0).then_some(per_token * 1_000_000.0)
}

/// Parse OpenRouter's `GET /models` document.
pub fn parse_openrouter(body: &str) -> Result<Vec<ProviderModel>, String> {
    let root: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| format!("OpenRouter model list is unparseable JSON: {e}"))?;
    let Some(data) = root.get("data").and_then(|d| d.as_array()) else {
        return Err("OpenRouter model list has no `data` array".to_string());
    };
    let mut models = Vec::new();
    for row in data {
        let Some(id) = row
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let pricing = row.get("pricing");
        // `supported_parameters` is per-model ground truth: a model that
        // lists `reasoning` thinks, one that lists `tools` accepts tool
        // definitions — and one that lists neither genuinely supports
        // neither (the field enumerates everything the model accepts).
        let supported: Option<Vec<&str>> = row
            .get("supported_parameters")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|p| p.as_str()).collect());
        let (supports_reasoning, supports_tools) = match &supported {
            Some(params) => (
                Some(
                    params
                        .iter()
                        .any(|p| *p == "reasoning" || *p == "include_reasoning"),
                ),
                Some(params.iter().any(|p| *p == "tools" || *p == "tool_choice")),
            ),
            None => (None, None),
        };
        models.push(ProviderModel {
            id: id.to_string(),
            display_name: row.get("name").and_then(|v| v.as_str()).map(str::to_string),
            context_window: row.get("context_length").and_then(|v| v.as_u64()),
            max_output_tokens: row
                .get("top_provider")
                .and_then(|t| t.get("max_completion_tokens"))
                .and_then(|v| v.as_u64()),
            input_usd_per_mtok: per_token_str_to_per_mtok(pricing.and_then(|p| p.get("prompt"))),
            output_usd_per_mtok: per_token_str_to_per_mtok(
                pricing.and_then(|p| p.get("completion")),
            ),
            cached_input_usd_per_mtok: per_token_str_to_per_mtok(
                pricing.and_then(|p| p.get("input_cache_read")),
            ),
            cache_write_usd_per_mtok: per_token_str_to_per_mtok(
                pricing.and_then(|p| p.get("input_cache_write")),
            ),
            supports_reasoning,
            supports_tools,
        });
    }
    if models.is_empty() {
        return Err("OpenRouter model list contained no models — refusing an empty sync".into());
    }
    Ok(models)
}

/// Fetch OpenRouter's full live model list. The endpoint is public — no
/// key required — but it is only ever called when the user has configured
/// OpenRouter (key present), so discovery never phones a provider the
/// user isn't using.
pub async fn fetch_openrouter(base_url: &str) -> Result<Vec<ProviderModel>, String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let client = http::client();
    let body = get_json(&client, "OpenRouter", &url, &[]).await?;
    parse_openrouter(&body)
}

// Anthropic

/// One page of Anthropic's `GET /v1/models`.
fn parse_anthropic_page(body: &str) -> Result<(Vec<ProviderModel>, Option<String>), String> {
    let root: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| format!("Anthropic model list is unparseable JSON: {e}"))?;
    let Some(data) = root.get("data").and_then(|d| d.as_array()) else {
        return Err("Anthropic model list has no `data` array".to_string());
    };
    let models = data
        .iter()
        .filter_map(|row| {
            let id = row
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())?;
            Some(ProviderModel {
                id: id.to_string(),
                display_name: row
                    .get("display_name")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                // The two limits Anthropic's listing publishes. Note the
                // names do NOT match the shape every other provider uses:
                // the output ceiling is `max_tokens` (the request parameter
                // it bounds), and the context window is `max_input_tokens`.
                // There is no `context_window` field to fall back on.
                //
                // Read here because #1290 found the first-party route had no
                // way to learn a ceiling at all: this parser kept the id and
                // the display name and dropped everything else, so the seed
                // was the ONLY source of an Anthropic ceiling and a wrong seed
                // stayed wrong through every refresh. It was wrong — Sonnet 5
                // read 64000 against a real 128000 — and no amount of
                // refreshing could have corrected it.
                context_window: row.get("max_input_tokens").and_then(|v| v.as_u64()),
                max_output_tokens: row.get("max_tokens").and_then(|v| v.as_u64()),
                ..ProviderModel::default()
            })
        })
        .collect();
    // `has_more` and `last_id` must agree. Folding a missing cursor into
    // `None` reopened, in a second guise, exactly the hole [`page_cap_error`]
    // closed: the caller reads `None` as "the listing finished", so a page
    // that announces more rows but omits the cursor to reach them produced a
    // truncated catalog reported as a clean sync. Refuse it by name instead.
    let has_more = root.get("has_more").and_then(|v| v.as_bool()) == Some(true);
    let last_id = root
        .get("last_id")
        .and_then(|v| v.as_str())
        .filter(|id| !id.is_empty())
        .map(str::to_string);
    let next = match (has_more, last_id) {
        (true, None) => {
            return Err(
                "Anthropic model list set `has_more` without a usable `last_id` cursor \
                        — refusing a silently truncated sync"
                    .to_string(),
            );
        }
        (true, cursor) => cursor,
        (false, _) => None,
    };
    Ok((models, next))
}

/// Fetch every model the Anthropic API serves this key. Ids and display
/// names only — the API's listing carries no pricing or capability data,
/// so the catalog merge keeps whatever the master list already knows.
pub async fn fetch_anthropic(
    base_url: &str,
    api_key: &ApiKey,
) -> Result<Vec<ProviderModel>, String> {
    let base = base_url.trim_end_matches('/');
    let client = http::client();
    let mut models = Vec::new();
    let mut after: Option<String> = None;
    let mut finished = false;
    for _ in 0..MAX_PAGES {
        let mut params: Vec<(&str, &str)> = vec![("limit", "1000")];
        if let Some(id) = &after {
            params.push(("after_id", id.as_str()));
        }
        let url = build_url(base, "/v1/models", &params)?;
        let body = get_json(
            &client,
            "Anthropic",
            &url,
            &[
                ("x-api-key", api_key.reveal()),
                ("anthropic-version", "2023-06-01"),
            ],
        )
        .await?;
        let (mut page, next) = parse_anthropic_page(&body)?;
        models.append(&mut page);
        match next {
            Some(id) => after = Some(id),
            None => {
                finished = true;
                break;
            }
        }
    }
    if !finished {
        return Err(page_cap_error("Anthropic"));
    }
    if models.is_empty() {
        return Err("Anthropic model list contained no models — refusing an empty sync".into());
    }
    Ok(models)
}

// Gemini

/// One page of Gemini's `GET /models` (the `ListModels` surface). Rows
/// that can't serve chat (`generateContent` absent from
/// `supportedGenerationMethods`: embeddings, imagen, aqa) are dropped —
/// they would be unusable-but-selectable, the exact bug this module fixes.
fn parse_gemini_page(body: &str) -> Result<(Vec<ProviderModel>, Option<String>), String> {
    let root: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| format!("Gemini model list is unparseable JSON: {e}"))?;
    let Some(rows) = root.get("models").and_then(|d| d.as_array()) else {
        return Err("Gemini model list has no `models` array".to_string());
    };
    let models = rows
        .iter()
        .filter_map(|row| {
            let name = row.get("name").and_then(|v| v.as_str())?;
            let id = name.strip_prefix("models/").unwrap_or(name);
            if id.is_empty() {
                return None;
            }
            let methods: Vec<&str> = row
                .get("supportedGenerationMethods")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|m| m.as_str()).collect())
                .unwrap_or_default();
            if !methods.contains(&"generateContent") {
                return None;
            }
            Some(ProviderModel {
                id: id.to_string(),
                display_name: row
                    .get("displayName")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                context_window: row.get("inputTokenLimit").and_then(|v| v.as_u64()),
                max_output_tokens: row.get("outputTokenLimit").and_then(|v| v.as_u64()),
                // The listing's `thinking` flag (present on 2.5+ era rows);
                // absent means unknown, not "no".
                supports_reasoning: row.get("thinking").and_then(|v| v.as_bool()),
                ..ProviderModel::default()
            })
        })
        .collect();
    let next = root
        .get("nextPageToken")
        .and_then(|v| v.as_str())
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    Ok((models, next))
}

/// Fetch every chat-capable model the Gemini API serves this key.
pub async fn fetch_gemini(base_url: &str, api_key: &ApiKey) -> Result<Vec<ProviderModel>, String> {
    let base = base_url.trim_end_matches('/');
    let client = http::client();
    let mut models: Vec<ProviderModel> = Vec::new();
    let mut token: Option<String> = None;
    let mut finished = false;
    for _ in 0..MAX_PAGES {
        let mut params: Vec<(&str, &str)> = vec![("pageSize", "1000")];
        if let Some(t) = &token {
            params.push(("pageToken", t.as_str()));
        }
        let url = build_url(base, "/models", &params)?;
        let body = get_json(
            &client,
            "Gemini",
            &url,
            &[("x-goog-api-key", api_key.reveal())],
        )
        .await?;
        let (mut page, next) = parse_gemini_page(&body)?;
        models.append(&mut page);
        match next {
            Some(t) => token = Some(t),
            None => {
                finished = true;
                break;
            }
        }
    }
    if !finished {
        return Err(page_cap_error("Gemini"));
    }
    if models.is_empty() {
        return Err("Gemini model list contained no chat models — refusing an empty sync".into());
    }
    Ok(models)
}

// OpenAI-compatible (OpenAI, xAI, DeepSeek, Z.ai, local, custom gateways)

/// Parse the OpenAI-shape `GET /models` document: `{"data": [{"id": …}]}`.
pub fn parse_openai_compatible(label: &str, body: &str) -> Result<Vec<ProviderModel>, String> {
    let root: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| format!("{label} model list is unparseable JSON: {e}"))?;
    let Some(data) = root.get("data").and_then(|d| d.as_array()) else {
        return Err(format!("{label} model list has no `data` array"));
    };
    let models: Vec<ProviderModel> = data
        .iter()
        .filter_map(|row| {
            let id = row
                .get("id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())?;
            Some(ProviderModel {
                id: id.to_string(),
                display_name: row
                    .get("display_name")
                    .or_else(|| row.get("name"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                context_window: first_u64(row, &["context_length", "context_window"]),
                // The output ceiling, where the gateway publishes one. Only
                // the two UNAMBIGUOUS names are read (#1290): a bare
                // `max_tokens` means the output cap on some OpenAI-shaped
                // listings and the whole context window on others, and
                // guessing wrong in the "it's the output cap" direction seeds
                // a ceiling several times the model's real one — which the
                // engine would then ask for on the wire and the provider
                // would reject. A missing ceiling costs a fallback to the
                // engine default; a wrong one costs every request.
                max_output_tokens: first_u64(row, &["max_output_tokens", "max_completion_tokens"]),
                ..ProviderModel::default()
            })
        })
        .collect();
    if models.is_empty() {
        return Err(format!(
            "{label} model list contained no models — refusing an empty sync"
        ));
    }
    Ok(models)
}

/// Fetch the model list from any OpenAI-compatible endpoint. `label` names
/// the provider in error messages.
pub async fn fetch_openai_compatible(
    label: &str,
    base_url: &str,
    api_key: &ApiKey,
) -> Result<Vec<ProviderModel>, String> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    // `Zeroizing`, not a bare `String`: this is a fresh heap copy of the
    // secret that the `ApiKey`'s own wipe-on-drop cannot reach, and a plain
    // `format!` would leave the whole `Bearer …` line legible in freed memory
    // for the rest of the process.
    let auth = zeroize::Zeroizing::new(format!("Bearer {}", api_key.reveal()));
    let client = http::client();
    let body = get_json(&client, label, &url, &[("Authorization", auth.as_str())]).await?;
    parse_openai_compatible(label, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPENROUTER_FIXTURE: &str = r#"{
        "data": [
            {
                "id": "anthropic/claude-sonnet-4.5",
                "name": "Anthropic: Claude Sonnet 4.5",
                "context_length": 1000000,
                "pricing": {
                    "prompt": "0.000003",
                    "completion": "0.000015",
                    "input_cache_read": "0.0000003",
                    "input_cache_write": "0.00000375"
                },
                "top_provider": { "max_completion_tokens": 64000 },
                "supported_parameters": ["tools", "tool_choice", "reasoning", "max_tokens"]
            },
            {
                "id": "mistralai/mistral-7b-instruct",
                "name": "Mistral 7B Instruct",
                "context_length": 32768,
                "pricing": { "prompt": "0.00000006", "completion": "0.00000006" },
                "supported_parameters": ["max_tokens", "temperature"]
            },
            {
                "id": "openrouter/auto",
                "name": "Auto Router",
                "pricing": { "prompt": "-1", "completion": "-1" }
            },
            { "name": "row with no id is skipped" }
        ]
    }"#;

    #[test]
    fn openrouter_parses_pricing_limits_and_capability_parameters() {
        let models = parse_openrouter(OPENROUTER_FIXTURE).expect("fixture parses");
        assert_eq!(models.len(), 3);
        let sonnet = &models[0];
        assert_eq!(sonnet.id, "anthropic/claude-sonnet-4.5");
        // Per-token strings scaled to the catalog's USD-per-Mtok unit.
        assert_eq!(sonnet.input_usd_per_mtok, Some(3.0));
        assert_eq!(sonnet.output_usd_per_mtok, Some(15.0));
        assert_eq!(sonnet.cached_input_usd_per_mtok, Some(0.3));
        assert_eq!(sonnet.cache_write_usd_per_mtok, Some(3.75));
        assert_eq!(sonnet.context_window, Some(1_000_000));
        assert_eq!(sonnet.max_output_tokens, Some(64_000));
        assert_eq!(sonnet.supports_reasoning, Some(true));
        assert_eq!(sonnet.supports_tools, Some(true));

        // supported_parameters present without reasoning/tools → hard "no",
        // which is what lets the effort picker exclude these models.
        let mistral = &models[1];
        assert_eq!(mistral.supports_reasoning, Some(false));
        assert_eq!(mistral.supports_tools, Some(false));

        // Dynamic pricing ("-1") and no supported_parameters → unknown.
        let auto = &models[2];
        assert_eq!(auto.input_usd_per_mtok, None);
        assert_eq!(auto.supports_reasoning, None);
    }

    #[test]
    fn openrouter_rejects_shapes_that_are_not_the_model_list() {
        assert!(parse_openrouter("{}").is_err());
        assert!(parse_openrouter(r#"{"data": []}"#).is_err());
        assert!(parse_openrouter("not json").is_err());
    }

    #[test]
    fn anthropic_page_parses_ids_and_pagination_cursor() {
        // Shaped after the real document (fields verified against the live
        // endpoint on 2026-08-03, #1290): the OUTPUT ceiling is `max_tokens`
        // and the context window is `max_input_tokens`. Neither name matches
        // what any other provider in this module uses, which is why reading
        // them needs a fixture rather than an assumption.
        let body = r#"{
            "data": [
                {
                    "type": "model",
                    "id": "claude-fable-5",
                    "display_name": "Claude Fable 5",
                    "max_tokens": 128000,
                    "max_input_tokens": 1000000
                },
                {"type": "model", "id": "claude-haiku-4-5-20251001", "max_tokens": 64000},
                {"type": "model", "id": "claude-legacy-no-limits"}
            ],
            "has_more": true,
            "last_id": "claude-haiku-4-5-20251001"
        }"#;
        let (models, next) = parse_anthropic_page(body).expect("page parses");
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "claude-fable-5");
        assert_eq!(models[0].display_name.as_deref(), Some("Claude Fable 5"));
        assert_eq!(models[0].max_output_tokens, Some(128_000));
        assert_eq!(models[0].context_window, Some(1_000_000));
        // Per model, never shared: the same page carries a model whose real
        // ceiling is half its sibling's. A parser that read one row's limit
        // and applied it family-wide would pass every other assertion here.
        assert_eq!(models[1].max_output_tokens, Some(64_000));
        // A row that omits the limits stays unknown rather than inheriting a
        // neighbour's — `None` lets the catalog merge keep whatever the
        // master list already knew for it.
        assert_eq!(models[2].max_output_tokens, None);
        assert_eq!(models[2].context_window, None);
        // The listing still carries no pricing or capability data.
        assert_eq!(models[0].supports_reasoning, None);
        assert_eq!(next.as_deref(), Some("claude-haiku-4-5-20251001"));

        let (_, done) =
            parse_anthropic_page(r#"{"data": [{"id": "m"}], "has_more": false}"#).expect("parses");
        assert_eq!(done, None);
    }

    #[test]
    fn gemini_page_keeps_chat_models_and_drops_the_rest() {
        let body = r#"{
            "models": [
                {
                    "name": "models/gemini-3-pro",
                    "displayName": "Gemini 3 Pro",
                    "inputTokenLimit": 1000000,
                    "outputTokenLimit": 65536,
                    "thinking": true,
                    "supportedGenerationMethods": ["generateContent", "countTokens"]
                },
                {
                    "name": "models/text-embedding-004",
                    "supportedGenerationMethods": ["embedContent"]
                },
                {
                    "name": "models/gemini-2.0-flash-lite",
                    "supportedGenerationMethods": ["generateContent"]
                }
            ],
            "nextPageToken": "tok-2"
        }"#;
        let (models, next) = parse_gemini_page(body).expect("page parses");
        assert_eq!(models.len(), 2, "embedding row is dropped");
        assert_eq!(models[0].id, "gemini-3-pro", "models/ prefix stripped");
        assert_eq!(models[0].context_window, Some(1_000_000));
        assert_eq!(models[0].max_output_tokens, Some(65_536));
        assert_eq!(models[0].supports_reasoning, Some(true));
        assert_eq!(
            models[1].supports_reasoning, None,
            "no thinking flag → unknown"
        );
        assert_eq!(next.as_deref(), Some("tok-2"));
    }

    #[test]
    fn openai_compatible_parses_bare_id_lists() {
        let body = r#"{"object": "list", "data": [
            {"id": "gpt-5.5", "object": "model", "owned_by": "openai"},
            {"id": "grok-4"},
            {"id": ""}
        ]}"#;
        let models = parse_openai_compatible("OpenAI", body).expect("parses");
        assert_eq!(models.len(), 2, "empty id dropped");
        assert_eq!(models[0].id, "gpt-5.5");
        assert_eq!(models[0].supports_reasoning, None);
        assert!(parse_openai_compatible("OpenAI", r#"{"data": []}"#).is_err());
        assert!(parse_openai_compatible("OpenAI", r#"{"models": []}"#).is_err());
    }

    /// The OpenAI-compatible shape is standardized; its limit FIELD NAMES are
    /// not. Read the unambiguous ones, and — the part that matters — refuse to
    /// read a bare `max_tokens`, which means the output cap on some gateways
    /// and the whole context window on others. Reading it the wrong way round
    /// seeds a ceiling many times the model's real one, which the engine then
    /// asks for on the wire and the provider rejects: a missing ceiling costs
    /// one fallback, a wrong one costs every request (#1290).
    #[test]
    fn openai_compatible_reads_unambiguous_limits_and_refuses_the_ambiguous_one() {
        let body = r#"{"object": "list", "data": [
            {"id": "a", "max_output_tokens": 32000, "context_window": 400000},
            {"id": "b", "max_completion_tokens": 8192, "context_length": 128000},
            {"id": "c", "max_tokens": 999999},
            {"id": "d", "max_output_tokens": 0}
        ]}"#;
        let models = parse_openai_compatible("Gateway", body).expect("parses");
        assert_eq!(models[0].max_output_tokens, Some(32_000));
        assert_eq!(models[0].context_window, Some(400_000));
        assert_eq!(models[1].max_output_tokens, Some(8_192));
        assert_eq!(models[1].context_window, Some(128_000));
        // The ambiguous name is deliberately NOT read.
        assert_eq!(
            models[2].max_output_tokens, None,
            "a bare `max_tokens` is not a reliable output cap on this shape"
        );
        // A published zero is "no limit stated", not a ceiling of zero — and
        // carrying it would overwrite a real value the master list knew.
        assert_eq!(models[3].max_output_tokens, None);
    }

    #[tokio::test]
    async fn fetch_openrouter_hits_the_models_route_and_parses() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_string(OPENROUTER_FIXTURE))
            .mount(&server)
            .await;
        let models = fetch_openrouter(&format!("{}/api/v1", server.uri()))
            .await
            .expect("fetches");
        assert_eq!(models.len(), 3);
    }

    #[tokio::test]
    async fn fetch_anthropic_paginates_with_after_id_and_sends_auth_headers() {
        use wiremock::matchers::{header, method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("x-api-key", "sk-test"))
            .and(header("anthropic-version", "2023-06-01"))
            .and(query_param("after_id", "claude-a"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"data": [{"id": "claude-b"}], "has_more": false}"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("x-api-key", "sk-test"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"data": [{"id": "claude-a"}], "has_more": true, "last_id": "claude-a"}"#,
            ))
            .mount(&server)
            .await;

        let models = fetch_anthropic(&server.uri(), &ApiKey::new("sk-test"))
            .await
            .expect("fetches both pages");
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["claude-a", "claude-b"]);
    }

    /// Pagination cursors are provider-controlled opaque tokens. One carrying
    /// `&` or `#` used to be interpolated raw into the query string, which
    /// truncates the query and injects a parameter — and the failure mode is a
    /// wrong or empty page, never an error. The cursor must arrive at the
    /// server byte-for-byte as the provider issued it.
    #[tokio::test]
    async fn fetch_anthropic_percent_encodes_a_cursor_carrying_query_syntax() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        const CURSOR: &str = "claude-a&limit=1#x";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(query_param("after_id", CURSOR))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"data": [{"id": "claude-b"}], "has_more": false}"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                r#"{{"data": [{{"id": "claude-a"}}], "has_more": true, "last_id": "{CURSOR}"}}"#
            )))
            .mount(&server)
            .await;

        let models = fetch_anthropic(&server.uri(), &ApiKey::new("sk-test"))
            .await
            .expect("fetches both pages");
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["claude-a", "claude-b"]);
    }

    /// Exhausting `MAX_PAGES` used to be indistinguishable from a clean
    /// finish: both broke out of the loop and returned the pages gathered so
    /// far as a success, so a provider serving more pages than the cap
    /// silently produced a truncated catalog. It must fail, naming the cap.
    #[tokio::test]
    async fn pagination_that_never_terminates_errors_instead_of_truncating() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let gemini = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"models": [{"name": "models/gemini-x", "supportedGenerationMethods": ["generateContent"]}], "nextPageToken": "always-more"}"#,
            ))
            .mount(&gemini)
            .await;
        let err = fetch_gemini(&gemini.uri(), &ApiKey::new("k"))
            .await
            .expect_err("a never-ending cursor is an error, not a partial success");
        assert!(err.contains("Gemini") && err.contains("10"), "{err}");

        let anthropic = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"data": [{"id": "claude-a"}], "has_more": true, "last_id": "always-more"}"#,
            ))
            .mount(&anthropic)
            .await;
        let err = fetch_anthropic(&anthropic.uri(), &ApiKey::new("sk-test"))
            .await
            .expect_err("a never-ending cursor is an error, not a partial success");
        assert!(err.contains("Anthropic") && err.contains("10"), "{err}");
    }

    /// The response cap: a body over `max_bytes` is refused by name instead
    /// of buffered whole — the OOM shape the cap exists to prevent. Driven
    /// through the bounded variant so the test doesn't need a 16 MiB fixture.
    #[tokio::test]
    async fn a_listing_body_over_the_cap_is_refused_not_buffered() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("x".repeat(4096)))
            .mount(&server)
            .await;
        let err = get_json_bounded(
            &http::client(),
            "OpenAI",
            &server.uri(),
            &[],
            1024,
            Duration::from_secs(5),
        )
        .await
        .expect_err("an oversized body must be refused");
        assert!(err.contains("OpenAI") && err.contains("cap"), "{err}");
    }

    /// The wall-clock deadline: `http::client`'s per-read bound cannot see
    /// total elapsed time, and one caller of these fetches is the CLI's
    /// BLOCKING startup auto-sync — a stalled listing must fail within the
    /// deadline, not hang the launch.
    #[tokio::test]
    async fn a_listing_that_stalls_past_the_deadline_errors_instead_of_hanging() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("{}")
                    .set_delay(Duration::from_secs(5)),
            )
            .mount(&server)
            .await;
        let err = get_json_bounded(
            &http::client(),
            "Gemini",
            &server.uri(),
            &[],
            MAX_LISTING_BYTES,
            Duration::from_millis(100),
        )
        .await
        .expect_err("a stalled listing must time out");
        assert!(
            err.contains("Gemini") && err.contains("did not answer"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn fetch_surfaces_http_errors_with_the_provider_named() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error": "bad key"}"#))
            .mount(&server)
            .await;
        let err = fetch_openai_compatible("xAI", &server.uri(), &ApiKey::new("k"))
            .await
            .unwrap_err();
        assert!(
            err.contains("xAI") && err.contains("401"),
            "named error: {err}"
        );
    }
}
