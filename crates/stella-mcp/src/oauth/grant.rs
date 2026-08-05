//! The two credential exchanges: RFC 7591 dynamic client registration, and the
//! RFC 6749 token endpoint (authorization-code grant and refresh alike).
//!
//! Both quote an untrusted server's response body back to the user on failure,
//! so both read it under the transport's shared body cap and truncate what
//! they surface.

use reqwest::Client;
use serde::Deserialize;

use super::{HTTP_TIMEOUT, TokenResponse};
use crate::error::McpError;
use crate::http::{read_capped_body, truncate};

#[derive(Debug, Deserialize)]
struct RegistrationResponse {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

pub(super) async fn register_client(
    http: &Client,
    endpoint: &str,
    redirect_uri: &str,
    client_name: &str,
    scope: Option<&str>,
) -> Result<(String, Option<String>), McpError> {
    let mut body = serde_json::json!({
        "client_name": client_name,
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    if let Some(scope) = scope {
        body["scope"] = serde_json::Value::String(scope.to_string());
    }
    let response = http
        .post(endpoint)
        .json(&body)
        .timeout(HTTP_TIMEOUT)
        .send()
        .await
        .map_err(|e| McpError::Auth(format!("client registration at `{endpoint}` failed: {e}")))?;
    let status = response.status();
    let body = read_capped_body(response, endpoint).await;
    if !status.is_success() {
        return Err(McpError::Auth(format!(
            "client registration at `{endpoint}` returned HTTP {status}: {}",
            truncate(&body.unwrap_or_default(), 300)
        )));
    }
    let body = body.map_err(|e| McpError::Auth(format!("registration response: {e}")))?;
    let reg: RegistrationResponse = serde_json::from_str(&body)
        .map_err(|e| McpError::Auth(format!("malformed registration response: {e}")))?;
    Ok((reg.client_id, reg.client_secret))
}

/// POST a form to the token endpoint; `basic` carries confidential-client
/// credentials when registration issued a secret.
pub(super) async fn token_request(
    http: &Client,
    endpoint: &str,
    form: &[(&str, String)],
    basic: Option<(&str, &str)>,
) -> Result<TokenResponse, McpError> {
    let mut builder = http.post(endpoint).form(form).timeout(HTTP_TIMEOUT);
    if let Some((user, secret)) = basic {
        builder = builder.basic_auth(user, Some(secret));
    }
    let response = builder
        .send()
        .await
        .map_err(|e| McpError::Auth(format!("token request to `{endpoint}` failed: {e}")))?;
    let status = response.status();
    let body = read_capped_body(response, endpoint).await;
    if !status.is_success() {
        // OAuth error bodies ({"error": "...", "error_description": "..."})
        // are safe to surface — they carry codes, not credentials.
        return Err(McpError::Auth(format!(
            "token endpoint `{endpoint}` returned HTTP {status}: {}",
            truncate(&body.unwrap_or_default(), 300)
        )));
    }
    let body = body.map_err(|e| McpError::Auth(format!("token response: {e}")))?;
    serde_json::from_str(&body)
        .map_err(|e| McpError::Auth(format!("malformed token response: {e}")))
}
