//! Integration tests for the tracker connections (`stella connect`) and the
//! OAuth-backed issue operations, with `wiremock` standing in for GitHub and
//! Linear. Follows `stella-mcp/tests/oauth_integration.rs`: the "browser"
//! round-trip is simulated by GETting the loopback redirect ourselves.

use serde_json::json;
use wiremock::matchers::{body_string_contains, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use stella_tools::issue_ops::{self as ops, CreateParams, IssueFilters};
use stella_tools::issues::IssueBackend;
use stella_tools::tracker_auth::{
    ConnectEvent, ConnectionKind, GitHubDeviceConfig, LinearOAuthConfig, github_device_login,
    linear_oauth_login,
};

// ── GitHub device flow ──────────────────────────────────────────────────────

#[tokio::test]
async fn github_device_flow_polls_through_pending_to_tokens() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/device/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dev-123",
            "user_code": "ABCD-1234",
            "verification_uri": format!("{}/activate", server.uri()),
            "expires_in": 60,
            "interval": 0
        })))
        .expect(1)
        .mount(&server)
        .await;

    // First poll: pending. Second poll: tokens.
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("device_code=dev-123"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({ "error": "authorization_pending" })),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "gho_test_token",
            "scope": "repo",
            "token_type": "bearer"
        })))
        .mount(&server)
        .await;

    let config = GitHubDeviceConfig {
        client_id: "stella-client".into(),
        device_code_url: format!("{}/device/code", server.uri()),
        token_url: format!("{}/token", server.uri()),
    };

    let events = std::sync::Mutex::new(Vec::new());
    let connection = github_device_login(&config, &|event| {
        events.lock().unwrap().push(event);
    })
    .await
    .expect("device flow should succeed");

    assert_eq!(connection.kind, ConnectionKind::OAuth);
    assert_eq!(connection.access_token, "gho_test_token");
    assert_eq!(connection.scope.as_deref(), Some("repo"));
    // The user code + verification URL were surfaced to the UI.
    let events = events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(
        e,
        ConnectEvent::UserCode { code, .. } if code == "ABCD-1234"
    )));
}

#[tokio::test]
async fn github_device_flow_surfaces_denial() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/device/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": "dev-123",
            "user_code": "ABCD-1234",
            "verification_uri": "https://example.test/activate",
            "expires_in": 60,
            "interval": 0
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "error": "access_denied" })))
        .mount(&server)
        .await;

    let config = GitHubDeviceConfig {
        client_id: "stella-client".into(),
        device_code_url: format!("{}/device/code", server.uri()),
        token_url: format!("{}/token", server.uri()),
    };
    let err = github_device_login(&config, &|_| {})
        .await
        .expect_err("denial must be an error");
    assert!(err.contains("denied"), "{err}");
}

// ── Linear authorization-code + PKCE ────────────────────────────────────────

#[tokio::test]
async fn linear_oauth_round_trips_the_browser_and_exchanges_the_code() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("grant_type=authorization_code"))
        .and(body_string_contains("code=auth-code-42"))
        .and(body_string_contains("code_verifier="))
        .and(body_string_contains("client_secret=shh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "lin_oauth_token",
            "token_type": "Bearer",
            "expires_in": 3600,
            "scope": "read,write"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let config = LinearOAuthConfig {
        client_id: "stella-linear".into(),
        client_secret: Some("shh".into()),
        authorize_url: format!("{}/authorize", server.uri()),
        token_url: format!("{}/token", server.uri()),
    };

    // Capture the authorize URL, then play the browser: hit the loopback
    // redirect with the code and the same state.
    let (url_tx, url_rx) = std::sync::mpsc::channel::<String>();
    let browser = tokio::spawn(async move {
        let authorize_url = tokio::task::spawn_blocking(move || url_rx.recv())
            .await
            .expect("join")
            .expect("authorize URL must be emitted");
        let parsed = reqwest::Url::parse(&authorize_url).expect("authorize URL parses");
        let params: std::collections::HashMap<_, _> = parsed.query_pairs().collect();
        let redirect_uri = params
            .get("redirect_uri")
            .expect("redirect_uri")
            .to_string();
        let state = params.get("state").expect("state").to_string();
        assert!(params.contains_key("code_challenge"));
        assert_eq!(
            params.get("code_challenge_method").map(AsRef::as_ref),
            Some("S256")
        );
        reqwest::get(format!("{redirect_uri}?code=auth-code-42&state={state}"))
            .await
            .expect("redirect GET succeeds");
    });

    let connection = linear_oauth_login(&config, &move |event| {
        if let ConnectEvent::AuthorizeUrl(url) = event {
            let _ = url_tx.send(url);
        }
    })
    .await
    .expect("linear oauth should succeed");
    browser.await.expect("browser task");

    assert_eq!(connection.kind, ConnectionKind::OAuth);
    assert_eq!(connection.access_token, "lin_oauth_token");
    assert!(connection.expires_at.is_some());
    assert_eq!(connection.client_id.as_deref(), Some("stella-linear"));
}

// ── GitHub REST backend (OAuth-connected, no `gh` binary) ───────────────────

async fn git_workspace_with_origin() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for args in [
        vec!["init", "-q"],
        vec![
            "remote",
            "add",
            "origin",
            "https://github.com/octo/repo.git",
        ],
    ] {
        let status = tokio::process::Command::new("git")
            .args(&args)
            .current_dir(dir.path())
            .status()
            .await
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }
    dir
}

#[tokio::test]
async fn github_rest_backend_lists_creates_and_mutates_issues() {
    let server = MockServer::start().await;
    let root = git_workspace_with_origin().await;
    let backend = IssueBackend::GitHubApi {
        token: "gho_test".into(),
        api_base: server.uri(),
    };

    // list: PRs are filtered out of the issues listing.
    Mock::given(method("GET"))
        .and(path("/repos/octo/repo/issues"))
        .and(query_param("state", "open"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {
                "number": 1, "title": "Real issue", "state": "open",
                "labels": [{ "name": "bug" }],
                "assignees": [{ "login": "octocat" }],
                "html_url": "https://github.com/octo/repo/issues/1",
                "updated_at": "2026-07-18T00:00:00Z"
            },
            {
                "number": 2, "title": "A PR", "state": "open",
                "labels": [], "assignees": [],
                "html_url": "https://github.com/octo/repo/pull/2",
                "pull_request": { "url": "..." }
            }
        ])))
        .mount(&server)
        .await;
    let issues = ops::list_issues(&backend, root.path(), &IssueFilters::default())
        .await
        .expect("list");
    assert_eq!(issues.len(), 1, "the PR must be filtered out");
    assert_eq!(issues[0].key, "#1");
    assert_eq!(issues[0].assignee.as_deref(), Some("@octocat"));

    // create with labels + assignee.
    Mock::given(method("POST"))
        .and(path("/repos/octo/repo/issues"))
        .and(body_string_contains("Fix the flaky test"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "number": 7, "title": "Fix the flaky test", "state": "open",
            "labels": [{ "name": "bug" }],
            "assignees": [{ "login": "octocat" }],
            "html_url": "https://github.com/octo/repo/issues/7"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let created = ops::create_issue(
        &backend,
        root.path(),
        &CreateParams {
            title: "Fix the flaky test".into(),
            body: "details".into(),
            labels: vec!["bug".into()],
            assignee: Some("@octocat".into()),
            team: None,
        },
    )
    .await
    .expect("create");
    assert_eq!(created.key, "#7");

    // status change closes via PATCH.
    Mock::given(method("PATCH"))
        .and(path("/repos/octo/repo/issues/7"))
        .and(body_string_contains("closed"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "number": 7 })))
        .expect(1)
        .mount(&server)
        .await;
    ops::set_status(&backend, root.path(), "#7", "done")
        .await
        .expect("close");

    // comment.
    Mock::given(method("POST"))
        .and(path("/repos/octo/repo/issues/7/comments"))
        .and(body_string_contains("on it"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 1 })))
        .expect(1)
        .mount(&server)
        .await;
    ops::add_comment(&backend, root.path(), "#7", "on it")
        .await
        .expect("comment");

    // label + member search filter client-side, case-insensitively.
    Mock::given(method("GET"))
        .and(path("/repos/octo/repo/labels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "name": "Bug", "color": "d73a4a", "description": "Something broken" },
            { "name": "docs", "color": "0075ca", "description": "" }
        ])))
        .mount(&server)
        .await;
    let labels = ops::search_labels(&backend, root.path(), "bug", 10)
        .await
        .expect("labels");
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].name, "Bug");

    Mock::given(method("GET"))
        .and(path("/repos/octo/repo/assignees"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "login": "octocat" },
            { "login": "hubot" }
        ])))
        .mount(&server)
        .await;
    let members = ops::search_members(&backend, root.path(), "octo", 10)
        .await
        .expect("members");
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].handle, "@octocat");
}

#[tokio::test]
async fn github_rest_401_names_the_reconnect_fix() {
    let server = MockServer::start().await;
    let root = git_workspace_with_origin().await;
    let backend = IssueBackend::GitHubApi {
        token: "gho_expired".into(),
        api_base: server.uri(),
    };
    Mock::given(method("GET"))
        .and(path("/repos/octo/repo/issues"))
        .respond_with(
            ResponseTemplate::new(401).set_body_json(json!({ "message": "Bad credentials" })),
        )
        .mount(&server)
        .await;
    let err = ops::list_issues(&backend, root.path(), &IssueFilters::default())
        .await
        .expect_err("401 must error");
    assert!(err.contains("Bad credentials"), "{err}");
    assert!(err.contains("stella connect github"), "{err}");
}

// ── Linear GraphQL backend ──────────────────────────────────────────────────

#[tokio::test]
async fn linear_backend_lists_searches_labels_and_members() {
    let server = MockServer::start().await;
    let backend = IssueBackend::Linear {
        api_key: "Bearer lin_oauth_token".into(),
        api_url: format!("{}/graphql", server.uri()),
    };
    let root = std::env::temp_dir();

    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("issues(filter"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "issues": { "nodes": [{
                "identifier": "ENG-42",
                "title": "Fix the flaky test",
                "url": "https://linear.app/x/issue/ENG-42",
                "updatedAt": "2026-07-18T00:00:00Z",
                "state": { "name": "In Progress" },
                "assignee": { "name": "Mona", "displayName": "mona" },
                "labels": { "nodes": [{ "name": "bug" }] }
            }] } }
        })))
        .mount(&server)
        .await;
    let issues = ops::list_issues(&backend, &root, &IssueFilters::default())
        .await
        .expect("list");
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].key, "ENG-42");
    assert_eq!(issues[0].state, "In Progress");
    assert_eq!(issues[0].labels, vec!["bug"]);

    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("issueLabels"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "issueLabels": { "nodes": [
                { "name": "bug", "color": "#eb5757", "description": "Something broken" }
            ] } }
        })))
        .mount(&server)
        .await;
    let labels = ops::search_labels(&backend, &root, "bu", 10)
        .await
        .expect("labels");
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].name, "bug");

    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(body_string_contains("users(filter"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "users": { "nodes": [
                { "name": "Mona Lisa", "displayName": "mona", "email": "mona@example.com", "active": true },
                { "name": "Gone Person", "displayName": "gone", "email": "gone@example.com", "active": false }
            ] } }
        })))
        .mount(&server)
        .await;
    let members = ops::search_members(&backend, &root, "mona", 10)
        .await
        .expect("members");
    assert_eq!(members.len(), 1, "inactive users are dropped");
    assert_eq!(members[0].handle, "mona@example.com");
    assert_eq!(members[0].name.as_deref(), Some("mona"));
}
