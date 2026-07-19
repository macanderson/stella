//! Minimal GitHub REST v3 client for the OAuth-connected issue backend.
//!
//! Exists so a `stella connect github` login works end-to-end without the
//! `gh` binary: the [`crate::issues::IssueBackend::GitHubApi`] variant routes
//! issue operations through this client instead of shelling out. Scope is
//! deliberately tiny — issues, comments, labels, assignees — not a general
//! GitHub SDK.

use serde_json::Value;

use crate::exec;

const DEFAULT_API_BASE: &str = "https://api.github.com";
const TIMEOUT_SECS: u64 = 60;

/// A token-authenticated client pinned to one API base. Tests point
/// `api_base` at a mock server; production uses [`GitHubRest::new`].
#[derive(Clone)]
pub struct GitHubRest {
    token: String,
    pub api_base: String,
}

impl std::fmt::Debug for GitHubRest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubRest")
            .field("token", &"<redacted>")
            .field("api_base", &self.api_base)
            .finish()
    }
}

impl GitHubRest {
    pub fn new(token: impl Into<String>) -> Self {
        Self::with_base(token, DEFAULT_API_BASE)
    }

    pub fn with_base(token: impl Into<String>, api_base: impl Into<String>) -> Self {
        let mut api_base = api_base.into();
        while api_base.ends_with('/') {
            api_base.pop();
        }
        Self {
            token: token.into(),
            api_base,
        }
    }

    /// One JSON request. `path` starts with `/`; `body` present → sent as
    /// JSON. Non-2xx responses become readable errors carrying GitHub's
    /// `message` field when present.
    pub async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, String> {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
            .build()
            .map_err(|e| format!("http client: {e}"))?;
        let mut builder = client
            .request(method.clone(), format!("{}{path}", self.api_base))
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("User-Agent", "stella-cli");
        if let Some(body) = body {
            builder = builder.json(body);
        }
        let response = builder
            .send()
            .await
            .map_err(|e| format!("GitHub request failed: {e}"))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            let message = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    v.get("message")
                        .and_then(|m| m.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| {
                    let mut preview = text.clone();
                    preview.truncate(300);
                    preview
                });
            let hint = if status.as_u16() == 401 {
                " — token may be expired; run `stella connect github` again"
            } else {
                ""
            };
            return Err(format!(
                "GitHub {method} {path}: HTTP {status}: {message}{hint}"
            ));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).map_err(|e| format!("GitHub returned non-JSON: {e}"))
    }
}

/// The `owner/repo` slug of the workspace's `origin` remote — how the
/// OAuth-connected backend learns which repository's issues to operate on
/// (the `gh` CLI resolves this the same way).
pub async fn repo_slug(root: &std::path::Path) -> Result<String, String> {
    let (code, output) = exec::run("git remote get-url origin", root, 10).await?;
    if code != 0 {
        return Err(format!(
            "cannot resolve the GitHub repository: `git remote get-url origin` \
             failed (exit {code}): {output}"
        ));
    }
    parse_remote_slug(output.trim()).ok_or_else(|| {
        format!(
            "cannot parse an owner/repo slug from remote `{}`",
            output.trim()
        )
    })
}

/// Extract `owner/repo` from the common remote URL shapes:
/// `git@github.com:owner/repo.git`, `https://github.com/owner/repo(.git)`,
/// `ssh://git@github.com/owner/repo.git`.
pub fn parse_remote_slug(url: &str) -> Option<String> {
    let trimmed = url.trim().trim_end_matches('/');
    let path = if let Some((_, rest)) = trimmed.split_once("://") {
        // https:// or ssh:// — drop the host segment.
        let (_host, path) = rest.split_once('/')?;
        path
    } else if let Some((_, path)) = trimmed.split_once(':') {
        // scp-like git@host:owner/repo.git
        path
    } else {
        return None;
    };
    let path = path.trim_matches('/').trim_end_matches(".git");
    let mut segments = path.rsplit('/');
    let repo = segments.next()?;
    let owner = segments.next()?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_slugs_parse_from_every_common_shape() {
        for (url, expected) in [
            (
                "git@github.com:macanderson/stella.git",
                "macanderson/stella",
            ),
            (
                "https://github.com/macanderson/stella.git",
                "macanderson/stella",
            ),
            (
                "https://github.com/macanderson/stella",
                "macanderson/stella",
            ),
            (
                "https://github.com/macanderson/stella/",
                "macanderson/stella",
            ),
            (
                "ssh://git@github.com/macanderson/stella.git",
                "macanderson/stella",
            ),
            ("https://ghe.example.com/team/project.git", "team/project"),
        ] {
            assert_eq!(parse_remote_slug(url).as_deref(), Some(expected), "{url}");
        }
        assert!(parse_remote_slug("not a url").is_none());
        assert!(parse_remote_slug("").is_none());
    }

    #[test]
    fn debug_never_leaks_the_token() {
        let client = GitHubRest::new("secret-token");
        let debug = format!("{client:?}");
        assert!(!debug.contains("secret-token"), "{debug}");
    }

    #[test]
    fn trailing_slash_on_base_is_normalized() {
        let client = GitHubRest::with_base("t", "https://api.example.test///");
        assert_eq!(client.api_base, "https://api.example.test");
    }
}
