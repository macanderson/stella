//! Integration tests for the web family over a wiremock server — the whole
//! HTTP surface (fetch rendering, per-domain auth injection, both search
//! providers, asset extraction, download confinement) without touching the
//! real network.

use std::sync::Arc;

use serde_json::json;
use stella_protocol::tool::ToolOutput;
use stella_tools::registry::Tool;
use stella_tools::web::{
    SearchBackend, SearchProvider, WebAuthConfig, WebAuthState, WebDownload, WebExtractAssets,
    WebFetch, WebSearch,
};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// No per-domain credentials, but the loopback exemption an operator pointing
/// Stella at a local dev server would write.
///
/// Every mock server here binds `127.0.0.1`, which the shipping default
/// refuses (#939). The tests that are not ABOUT the egress guard therefore
/// carry the opt-out rather than the guard being weakened for them — the
/// shipping posture is exercised by [`shipping_default_auth`] below.
fn no_auth() -> Arc<WebAuthState> {
    auth_from(
        r#"
        [egress]
        allow = ["127.0.0.1"]
        "#,
    )
}

/// `web_auth.toml` exactly as it ships when nobody has written one: no
/// credentials, no egress exemptions.
fn shipping_default_auth() -> Arc<WebAuthState> {
    Arc::new(Ok(WebAuthConfig::default()))
}

fn auth_from(toml_text: &str) -> Arc<WebAuthState> {
    Arc::new(Ok(toml::from_str(toml_text).expect("test auth toml")))
}

fn expect_ok(output: ToolOutput) -> String {
    match output {
        ToolOutput::Ok { content } => content,
        ToolOutput::Error { message } => panic!("expected Ok, got error: {message}"),
    }
}

fn expect_error(output: ToolOutput) -> String {
    match output {
        ToolOutput::Error { message } => message,
        ToolOutput::Ok { content } => panic!("expected Error, got: {content}"),
    }
}

#[tokio::test]
async fn web_fetch_renders_html_as_markdown_with_absolute_links() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/post"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<html><head><title>My Post</title></head><body>\
             <h1>Welcome</h1><p>Read <a href=\"/more\">more here</a>.</p></body></html>",
            "text/html; charset=utf-8",
        ))
        .mount(&server)
        .await;

    let root = tempfile::tempdir().unwrap();
    let out = WebFetch(no_auth())
        .execute(
            &json!({"url": format!("{}/post", server.uri())}),
            root.path(),
        )
        .await;
    let content = expect_ok(out);
    assert!(content.contains("# My Post"), "{content}");
    assert!(content.contains("# Welcome"), "{content}");
    assert!(
        content.contains(&format!("[more here]({}/more)", server.uri())),
        "{content}"
    );
    assert!(content.contains("Source:"), "{content}");
}

#[tokio::test]
async fn web_fetch_sends_configured_domain_auth_and_reports_it_by_name_only() {
    let server = MockServer::start().await;
    // The mock only matches when the cookie and extra header arrive — a
    // fetch without the injected auth 404s and fails the test.
    Mock::given(method("GET"))
        .and(path("/paywalled"))
        .and(header("Cookie", "session=secret-cookie-value"))
        .and(header("X-Client", "stella-test"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw("<html><body><p>Members only</p></body></html>", "text/html"),
        )
        .mount(&server)
        .await;

    let auth = auth_from(
        r#"
        [domains."127.0.0.1"]
        cookie = "session=secret-cookie-value"
        [domains."127.0.0.1".headers]
        X-Client = "stella-test"
        [egress]
        allow = ["127.0.0.1"]
        "#,
    );
    let root = tempfile::tempdir().unwrap();
    let out = WebFetch(auth)
        .execute(
            &json!({"url": format!("{}/paywalled", server.uri())}),
            root.path(),
        )
        .await;
    let content = expect_ok(out);
    assert!(content.contains("Members only"), "{content}");
    assert!(
        content.contains("authenticated via web_auth.toml for `127.0.0.1`"),
        "{content}"
    );
    assert!(
        !content.contains("secret-cookie-value"),
        "auth values must never appear in tool output: {content}"
    );
}

#[tokio::test]
async fn web_fetch_refuses_non_http_schemes_and_flags_binary_bodies() {
    let root = tempfile::tempdir().unwrap();
    let message = expect_error(
        WebFetch(no_auth())
            .execute(&json!({"url": "file:///etc/passwd"}), root.path())
            .await,
    );
    assert!(message.contains("only http/https"), "{message}");

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/blob"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(vec![0u8, 159, 146, 150], "application/octet-stream"),
        )
        .mount(&server)
        .await;
    let content = expect_ok(
        WebFetch(no_auth())
            .execute(
                &json!({"url": format!("{}/blob", server.uri())}),
                root.path(),
            )
            .await,
    );
    assert!(content.contains("binary content"), "{content}");
    assert!(content.contains("web_download"), "{content}");
}

#[tokio::test]
async fn web_search_brave_sends_the_token_and_parses_results() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/res/v1/web/search"))
        .and(query_param("q", "stella agent"))
        .and(query_param("count", "2"))
        .and(header("X-Subscription-Token", "brave-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "web": { "results": [
                { "title": "Stella", "url": "https://stella.oxagen.sh", "description": "The agent." },
                { "title": "Repo", "url": "https://github.com/macanderson/stella", "description": "Source." }
            ]}
        })))
        .mount(&server)
        .await;

    let backend = SearchBackend::with_endpoint(
        SearchProvider::Brave,
        "brave-key",
        format!("{}/res/v1/web/search", server.uri()),
    );
    let root = tempfile::tempdir().unwrap();
    let content = expect_ok(
        WebSearch(backend)
            .execute(&json!({"query": "stella agent", "count": 2}), root.path())
            .await,
    );
    assert!(
        content.contains("2 results for \"stella agent\" (brave)"),
        "{content}"
    );
    assert!(content.contains("1. Stella"), "{content}");
    assert!(content.contains("https://stella.oxagen.sh"), "{content}");
    assert!(content.contains("2. Repo"), "{content}");
}

#[tokio::test]
async fn web_search_tavily_posts_the_query_with_a_bearer_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/search"))
        .and(header("Authorization", "Bearer tvly-key"))
        .and(wiremock::matchers::body_string_contains("\"query\":\"rust scraper\""))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                { "title": "scraper crate", "url": "https://docs.rs/scraper", "content": "HTML parsing." }
            ]
        })))
        .mount(&server)
        .await;

    let backend = SearchBackend::with_endpoint(
        SearchProvider::Tavily,
        "tvly-key",
        format!("{}/search", server.uri()),
    );
    let root = tempfile::tempdir().unwrap();
    let content = expect_ok(
        WebSearch(backend)
            .execute(&json!({"query": "rust scraper"}), root.path())
            .await,
    );
    assert!(
        content.contains("1 results for \"rust scraper\" (tavily)"),
        "{content}"
    );
    assert!(content.contains("https://docs.rs/scraper"), "{content}");
}

#[tokio::test]
async fn web_extract_assets_mines_stylesheets_into_design_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r##"<html><head><title>Brand</title>
                <link rel="stylesheet" href="/css/site.css">
                <style>.hero { color: #ff6a3d }</style>
                </head><body><img src="/img/logo.png"></body></html>"##,
            "text/html",
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/css/site.css"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r##":root { --color-accent: #ff6a3d; }
                body { color: #ff6a3d; font-family: "Inter", sans-serif; }
                @font-face { font-family: "Inter"; src: url("/fonts/inter.woff2"); }"##,
            "text/css",
        ))
        .mount(&server)
        .await;

    let root = tempfile::tempdir().unwrap();
    let content = expect_ok(
        WebExtractAssets(no_auth())
            .execute(&json!({"url": server.uri()}), root.path())
            .await,
    );
    assert!(content.contains("Asset manifest"), "{content}");
    assert!(content.contains("site.css (fetched"), "{content}");
    // #ff6a3d appears in the inline block, the body rule, and the token.
    assert!(content.contains("- #ff6a3d (3)"), "{content}");
    assert!(content.contains("Inter"), "{content}");
    assert!(content.contains("--color-accent: #ff6a3d"), "{content}");
    assert!(content.contains("/fonts/inter.woff2"), "{content}");
    assert!(content.contains("/img/logo.png"), "{content}");
    assert!(content.contains("web_download"), "{content}");
}

/// The confused deputy: `web_extract_assets` follows stylesheet URLs the
/// FETCHED PAGE names. Both mock servers live on `127.0.0.1`, so one
/// `[domains."127.0.0.1"]` entry covers both by host — exactly the shape
/// that let a page name an internal host and have Stella send the user's
/// stored cookie there. A sub-resource carries auth only on the page's own
/// origin (scheme + host + PORT).
#[tokio::test]
async fn a_page_named_cross_origin_stylesheet_gets_no_stored_credentials() {
    let internal = MockServer::start().await;
    // Mounted first, so it wins if the credential is (wrongly) sent.
    Mock::given(method("GET"))
        .and(path("/secrets.css"))
        .and(header("Cookie", "session=secret-cookie-value"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(":root { --leaked-secret: #010203; }", "text/css"),
        )
        .mount(&internal)
        .await;
    Mock::given(method("GET"))
        .and(path("/secrets.css"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(":root { --sent-bare: #040506; }", "text/css"),
        )
        .mount(&internal)
        .await;

    let page = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/own.css"))
        .and(header("Cookie", "session=secret-cookie-value"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(":root { --same-origin: #070809; }", "text/css"),
        )
        .mount(&page)
        .await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(
                r##"<html><head><title>Trap</title>
                    <link rel="stylesheet" href="{}/secrets.css">
                    <link rel="stylesheet" href="/own.css">
                    </head><body></body></html>"##,
                internal.uri()
            ),
            "text/html",
        ))
        .mount(&page)
        .await;

    let auth = auth_from(
        r#"
        [domains."127.0.0.1"]
        cookie = "session=secret-cookie-value"
        [egress]
        allow = ["127.0.0.1"]
        "#,
    );
    let root = tempfile::tempdir().unwrap();
    let content = expect_ok(
        WebExtractAssets(auth)
            .execute(&json!({"url": page.uri()}), root.path())
            .await,
    );

    assert!(
        content.contains("--sent-bare: #040506"),
        "the cross-origin sheet was fetched WITHOUT the stored cookie: {content}"
    );
    assert!(
        !content.contains("--leaked-secret"),
        "a page-named cross-origin sub-resource must never carry the user's \
         stored credentials: {content}"
    );
    // The page's own origin still gets its configured auth — the fence is
    // same-origin, not a blanket refusal.
    assert!(
        content.contains("--same-origin: #070809"),
        "same-origin sub-resources keep their auth: {content}"
    );
}

#[tokio::test]
async fn web_download_writes_inside_the_root_and_rejects_escapes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/fonts/inter.woff2"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(vec![1u8, 2, 3, 4], "font/woff2"))
        .mount(&server)
        .await;

    let root = tempfile::tempdir().unwrap();
    let url = format!("{}/fonts/inter.woff2", server.uri());
    let content = expect_ok(
        WebDownload(no_auth())
            .execute(
                &json!({"url": url, "path": ".stella/artifacts/web/inter.woff2"}),
                root.path(),
            )
            .await,
    );
    assert!(content.contains("4 bytes"), "{content}");
    let written = root.path().join(".stella/artifacts/web/inter.woff2");
    assert_eq!(std::fs::read(&written).unwrap(), vec![1u8, 2, 3, 4]);

    let message = expect_error(
        WebDownload(no_auth())
            .execute(&json!({"url": url, "path": "../outside.bin"}), root.path())
            .await,
    );
    assert!(message.contains("escapes the workspace root"), "{message}");
}

#[tokio::test]
async fn web_fetch_names_the_auth_file_hint_on_a_401() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/private"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let root = tempfile::tempdir().unwrap();
    let message = expect_error(
        WebFetch(no_auth())
            .execute(
                &json!({"url": format!("{}/private", server.uri())}),
                root.path(),
            )
            .await,
    );
    assert!(message.contains("HTTP 401"), "{message}");
    assert!(message.contains("web_auth.toml"), "{message}");
}

/// The shipping default refuses a loopback destination — the acceptance
/// clause of #939, for both tools it names.
///
/// The mock server exists so the failure mode is unambiguous: the URL is a
/// real, live, reachable page. Before the egress guard both calls succeeded
/// and `web_download` landed the body in the workspace.
#[tokio::test]
async fn web_fetch_and_web_download_refuse_loopback_under_the_shipping_default() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/secret"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<html><body><p>INTERNAL-SERVICE-BODY</p></body></html>",
            "text/html",
        ))
        .mount(&server)
        .await;

    let root = tempfile::tempdir().unwrap();
    let url = format!("{}/secret", server.uri());

    let message = expect_error(
        WebFetch(shipping_default_auth())
            .execute(&json!({"url": url}), root.path())
            .await,
    );
    assert!(message.contains("loopback"), "{message}");
    assert!(
        message.contains("[egress] allow"),
        "the refusal must name its own opt-out: {message}"
    );
    assert!(
        !message.contains("INTERNAL-SERVICE-BODY"),
        "the body must never be reached: {message}"
    );

    let message = expect_error(
        WebDownload(shipping_default_auth())
            .execute(
                &json!({"url": url, "path": ".stella/artifacts/web/secret.html"}),
                root.path(),
            )
            .await,
    );
    assert!(message.contains("loopback"), "{message}");
    assert!(
        !root
            .path()
            .join(".stella/artifacts/web/secret.html")
            .exists(),
        "a refused download must not land in the workspace"
    );
}

/// The metadata endpoints the issue names, refused before a packet leaves.
///
/// Every case asserts the guard's own sentinel as well as the class, because
/// a machine that simply cannot route to `169.254.169.254` also produces an
/// error — and an error for the wrong reason is not the property under test.
/// The guard must refuse these on a host where they *are* reachable.
#[tokio::test]
async fn web_fetch_refuses_the_metadata_name_families_by_name() {
    let root = tempfile::tempdir().unwrap();
    for (url, expected) in [
        (
            "http://metadata.google.internal/computeMetadata/v1/",
            ".internal",
        ),
        ("http://169.254.169.254/latest/meta-data/", "link-local"),
        ("http://[fd00:ec2::254]/latest/meta-data/", "unique-local"),
        ("http://localhost:3000/", "loopback"),
        ("http://[::ffff:127.0.0.1]:3000/", "loopback"),
        ("http://10.1.2.3/admin", "private"),
    ] {
        let message = expect_error(
            WebFetch(shipping_default_auth())
                .execute(&json!({ "url": url }), root.path())
                .await,
        );
        assert!(
            message.starts_with("stella egress guard: "),
            "{url} must be refused BY THE GUARD, not by the network: {message}"
        );
        assert!(
            message.contains(expected),
            "{url} must be refused as {expected}: {message}"
        );
    }
}

/// The redirect-hop witness, and the reason `host:port` allow entries exist.
///
/// Both servers are on `127.0.0.1`; only the first one's PORT is exempted. A
/// guard that checked the original URL only would follow the 302 straight into
/// the destination it was built to refuse — which is exactly what a public URL
/// 302ing to the metadata endpoint does.
#[tokio::test]
async fn a_redirect_to_a_denied_destination_is_refused_at_the_hop() {
    let denied = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/metadata"))
        .respond_with(ResponseTemplate::new(200).set_body_raw("SHOULD-NEVER-BE-READ", "text/plain"))
        // Verified when the server drops: the denied hop is never requested,
        // so the refusal happened before the connection, not after the read.
        .expect(0)
        .mount(&denied)
        .await;

    let allowed = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/hop"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/metadata", denied.uri()).as_str()),
        )
        // …and the first hop IS requested, so the refusal is the redirect
        // policy's and not the entry-point check refusing the whole fetch.
        .expect(1)
        .mount(&allowed)
        .await;

    let allowed_port = allowed.address().port();
    let auth = auth_from(&format!(
        r#"
        [egress]
        allow = ["127.0.0.1:{allowed_port}"]
        "#
    ));

    let root = tempfile::tempdir().unwrap();
    let message = expect_error(
        WebFetch(auth)
            .execute(
                &json!({"url": format!("{}/hop", allowed.uri())}),
                root.path(),
            )
            .await,
    );
    assert!(
        !message.contains("SHOULD-NEVER-BE-READ"),
        "the denied hop's body must never be reached: {message}"
    );
    assert!(message.contains("loopback"), "{message}");
    assert!(
        message.contains("[egress] allow"),
        "the refusal must survive reqwest's opaque redirect error: {message}"
    );
}

/// The exempted host still works — the guard is a fence, not a wall.
#[tokio::test]
async fn an_allow_listed_loopback_host_still_fetches() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/dev"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<html><body><p>my dev server</p></body></html>",
            "text/html",
        ))
        .mount(&server)
        .await;

    let port = server.address().port();
    let auth = auth_from(&format!(
        r#"
        [egress]
        allow = ["127.0.0.1:{port}"]
        "#
    ));
    let root = tempfile::tempdir().unwrap();
    let content = expect_ok(
        WebFetch(auth)
            .execute(
                &json!({"url": format!("{}/dev", server.uri())}),
                root.path(),
            )
            .await,
    );
    assert!(content.contains("my dev server"), "{content}");
}

/// The same fence via a NAME rather than a literal address — the one path in
/// this file that actually runs the guarded DNS resolver end to end.
///
/// `localhost` is refused by name under the default; allow-listing it lets the
/// URL check through, and the request then only completes if the guarded
/// resolver returns usable addresses for it. The resolver's REFUSAL half is
/// unit-tested in `web_egress` (it needs a name that resolves privately while
/// passing the URL check, which no offline hostname provides).
#[tokio::test]
async fn an_allow_listed_name_resolves_through_the_guarded_resolver() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/dev"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<html><body><p>named dev server</p></body></html>",
            "text/html",
        ))
        .mount(&server)
        .await;

    let port = server.address().port();
    let auth = auth_from(
        r#"
        [egress]
        allow = ["localhost"]
        "#,
    );
    let root = tempfile::tempdir().unwrap();
    let content = expect_ok(
        WebFetch(auth)
            .execute(
                &json!({"url": format!("http://localhost:{port}/dev")}),
                root.path(),
            )
            .await,
    );
    assert!(content.contains("named dev server"), "{content}");
}

/// A malformed `[egress] allow` entry is a loud parse failure, like every
/// other key in `web_auth.toml` — never a silently inert exemption that
/// leaves the operator believing their dev server is reachable.
#[test]
fn a_malformed_egress_allow_entry_fails_the_whole_file() {
    let error = toml::from_str::<WebAuthConfig>(
        r#"
        [egress]
        allow = ["127.0.0.1:not-a-port"]
        "#,
    )
    .expect_err("a bad port must not parse");
    assert!(error.to_string().contains("not a number"), "{error}");
}
