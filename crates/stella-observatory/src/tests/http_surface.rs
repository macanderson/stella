//! The raw HTTP surface: routing and status codes, percent-decoding, the
//! embedded dashboard's self-containment and CSP, and serving over a real
//! socket. Split out of the parent module for the same reason.

use super::*;

#[test]
fn unknown_route_is_404_and_missing_id_is_400() {
    let ws = TempDir::new().unwrap();
    assert_eq!(respond(ws.path(), "/api/nope").status, "404 Not Found");
    assert_eq!(
        respond(ws.path(), "/api/execution").status,
        "400 Bad Request"
    );
    assert_eq!(
        respond(ws.path(), "/api/execution?id=abc").status,
        "400 Bad Request"
    );
}

#[test]
fn index_and_mark_are_embedded() {
    let ws = TempDir::new().unwrap();
    let index = respond(ws.path(), "/");
    assert_eq!(index.content_type, "text/html; charset=utf-8");
    // Lowercase deliberately: the comet brand kit writes "stella — observatory"
    // (docs/brand/README.md — lowercase always), and this pins the masthead to it.
    assert!(
        String::from_utf8(index.body)
            .unwrap()
            .contains("stella — observatory")
    );
    let mark = respond(ws.path(), "/assets/mark.svg");
    assert_eq!(mark.content_type, "image/svg+xml");
    // The header lockup needs both cuts; a missing wordmark route would
    // render as a broken image rather than failing loudly.
    let wordmark = respond(ws.path(), "/assets/wordmark.svg");
    assert_eq!(wordmark.content_type, "image/svg+xml");
    assert!(String::from_utf8(wordmark.body).unwrap().contains("<svg"));
}

/// The dashboard sends `encodeURIComponent` output, so a scope value with
/// a space, `&` or `/` arrives escaped. Comparing the still-encoded bytes
/// against the stored value silently drilled into an empty scope.
#[test]
fn query_param_percent_decodes_the_value() {
    let q = Some("org=acme%20corp&repo=owner%2Fname&workspace=a+b&empty=");
    assert_eq!(query_param(q, "org").as_deref(), Some("acme corp"));
    assert_eq!(query_param(q, "repo").as_deref(), Some("owner/name"));
    assert_eq!(query_param(q, "workspace").as_deref(), Some("a b"));
    assert_eq!(query_param(q, "empty"), None);
    assert_eq!(query_param(q, "absent"), None);
    assert_eq!(query_param(None, "org"), None);
    // `%20`/`+` decode to a real space: whitespace is a value, not an
    // absent filter.
    assert_eq!(query_param(Some("org=%20"), "org").as_deref(), Some(" "));
    assert_eq!(query_param(Some("org=+"), "org").as_deref(), Some(" "));
}

/// This parses attacker-reachable bytes, so a malformed escape must be
/// inert — never a panic, never a dropped byte. Multi-byte UTF-8 split
/// across escapes must reassemble; invalid bytes become U+FFFD.
#[test]
fn percent_decode_tolerates_malformed_escapes() {
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(percent_decode("%A"), "%A");
    assert_eq!(percent_decode("%ZZ"), "%ZZ");
    assert_eq!(percent_decode("a%%20b"), "a% b");
    // é is two bytes, escaped separately by encodeURIComponent.
    assert_eq!(percent_decode("caf%C3%A9"), "café");
    assert_eq!(percent_decode("%ff"), "\u{fffd}");
    assert_eq!(percent_decode("plain"), "plain");
}

/// Inspecting a turn is a route, not an overlay.
///
/// This asserts structure, not rendering: that the transcript panel and its
/// `#transcript/<id>` route exist, that both drill paths (the Overview's
/// execution table and the Sessions turn table) navigate to it, and that the
/// modal drawer they used to open is gone rather than merely bypassed — a
/// second, stale way in is how two renderings of the same data drift apart.
/// Whether the page then *looks* right is not decidable from Rust; that was
/// verified by driving the served dashboard in a browser.
#[test]
fn inspecting_a_turn_is_a_page_not_a_drawer() {
    for needle in [
        "data-tab=\"transcript\"",              // the panel
        "id=\"panel-transcript\"",              // …addressed by its tab
        "location.hash = \"transcript/\" + id", // the route
        "goTranscript(+tr.dataset.exec)",       // sessions → turn
        "goTranscript(+tr.dataset.id)",         // overview → execution
    ] {
        assert!(
            INDEX_HTML.contains(needle),
            "the transcript page is missing {needle}"
        );
    }
    for gone in ["id=\"drawer\"", "id=\"scrim\"", "openDrawer(", "aria-modal"] {
        assert!(
            !INDEX_HTML.contains(gone),
            "the execution drawer was replaced by the transcript page, but {gone} survives"
        );
    }
}

/// The page must be fully self-contained: any http(s) URL in the HTML
/// would be an outbound fetch from the user's browser — a phone-home.
#[test]
fn dashboard_html_has_no_external_references() {
    for needle in ["http://", "https://", "//cdn", "@import", "integrity="] {
        assert!(
            !INDEX_HTML.contains(needle),
            "embedded dashboard must not reference {needle}"
        );
    }
}

#[tokio::test]
async fn serves_over_a_real_socket() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let ws = seeded_workspace();
    let root = ws.path().to_path_buf();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let _ = serve(root, 0, move |addr| {
            let _ = tx.send(addr);
        })
        .await;
    });
    let addr = rx.await.unwrap();
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream
        // A loopback Host: the DNS-rebinding gate (host_is_local, its
        // own unit test above) would 403 a non-local name.
        .write_all(b"GET /api/overview HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let mut body = String::new();
    stream.read_to_string(&mut body).await.unwrap();
    assert!(body.starts_with("HTTP/1.1 200 OK"));
    assert!(body.contains("\"runs\":2"));
    // The no-phone-home guarantee, enforced by the browser rather than
    // only by `dashboard_html_has_no_external_references`.
    assert!(
        body.contains("\r\nX-Content-Type-Options: nosniff\r\n"),
        "head was: {body}"
    );
    assert!(
        body.contains(&format!("\r\nContent-Security-Policy: {CSP}\r\n")),
        "head was: {body}"
    );
    server.abort();
}

/// The CSP must keep the embedded page working: it is one document with
/// inline script, an inline `<style>`, and inline `style="…"` attributes,
/// so dropping `'unsafe-inline'` from either directive would silently
/// break it. Same-origin `/assets/*.svg` needs `img-src 'self'`.
#[test]
fn csp_admits_everything_the_embedded_dashboard_actually_uses() {
    for directive in [
        "script-src 'self' 'unsafe-inline'",
        "style-src 'self' 'unsafe-inline'",
        "img-src 'self'",
        "connect-src 'self'",
        "frame-ancestors 'none'",
    ] {
        assert!(CSP.contains(directive), "CSP is missing {directive}");
    }
    // A header value may not carry a newline — it would split the head.
    assert!(!CSP.contains('\r') && !CSP.contains('\n'));
    assert!(INDEX_HTML.contains("<style>") && INDEX_HTML.contains("<script>"));
    // Every asset the page names must be same-origin *and* actually routed.
    // Asserted over whichever attribute carries it rather than a fixed
    // `src="…"`: the mark moved from an `<img src>` to a `<link rel=icon
    // href>`, which is the same same-origin fetch and the same CSP question,
    // but silently failed a literal-string check. What matters is that no
    // reference points off-origin and none points at a 404.
    let root = tempfile::tempdir().expect("temp workspace");
    for attr in ["src=\"", "href=\""] {
        for (_, rest) in INDEX_HTML
            .match_indices(attr)
            .map(|(i, m)| (i, &INDEX_HTML[i + m.len()..]))
        {
            let target = rest.split('"').next().unwrap_or_default();
            assert!(
                target.starts_with('/') || target.starts_with('#'),
                "the page must reference nothing off-origin, found {target:?}"
            );
            if target.starts_with('/') {
                assert_ne!(
                    respond(root.path(), target).status,
                    "404 Not Found",
                    "the page references {target:?}, which no route serves"
                );
            }
        }
    }
}

/// Padding the request line to the 8 KiB head cap used to be served: the
/// route still parsed out of the truncated path, while every header —
/// `Host` included — sat past the cap, and an absent Host is allowed. That
/// handed a rebound page the whole dashboard cross-origin. A head with no
/// terminator inside the cap is now refused before routing.
///
/// Sized to *exactly* the cap so the server consumes every byte the client
/// wrote: closing a socket with unread bytes still in its receive queue
/// sends an RST, which would race the client's read of the response.
#[tokio::test]
async fn unterminated_request_head_is_refused_not_routed() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let ws = seeded_workspace();
    let root = ws.path().to_path_buf();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let _ = serve(root, 0, move |addr| {
            let _ = tx.send(addr);
        })
        .await;
    });
    let addr = rx.await.unwrap();
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let prefix = "GET /api/overview?pad=";
    let suffix = " HTTP/1.1\r\n";
    let pad = "a".repeat(8192 - prefix.len() - suffix.len());
    let request = format!("{prefix}{pad}{suffix}");
    assert_eq!(request.len(), 8192, "fills the cap without terminating");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut body = String::new();
    stream.read_to_string(&mut body).await.unwrap();
    assert!(
        body.starts_with("HTTP/1.1 431"),
        "an unterminated head must be refused, got: {}",
        body.lines().next().unwrap_or_default()
    );
    assert!(!body.contains("\"runs\""), "no telemetry may leak");
    server.abort();
}
