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
    // Lowercase: the comet brand kit writes "stella — observatory"
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

/// The turn page's sub-agents plane: the fan-out section lists the turn's
/// delegate children, clicking one makes the page assume that child
/// (`#transcript/<id>/sub/<agent>`), a back button climbs to the parent
/// turn, and ← / → cycle the fan-out. Whether the page then *looks* right is
/// not decidable from Rust — same bargain as the page-not-drawer test above.
#[test]
fn a_turns_sub_agents_are_a_focusable_plane_of_the_transcript_page() {
    for needle in [
        "/api/execution-subagents",         // the endpoint the page folds from
        "sect(\"tx-subagents\"",            // the fan-out section on the parent view
        "renderSubagentFocus",              // the page assumed by one child
        "/sub/",                            // the child's address in the route
        "id=\"tx-sub-up\"",                 // back to the parent turn
        "id=\"tx-sub-prev\"",               // ← previous sub-agent…
        "id=\"tx-sub-next\"",               // …next sub-agent →
        "goSubagent(txSub.exec, txSub.ids", // the arrow keys cycle the fan-out
    ] {
        assert!(
            INDEX_HTML.contains(needle),
            "the sub-agents plane is missing {needle}"
        );
    }
}

/// The session plane is cards with a clock rail, and holds no table.
///
/// Both lists were tables inside a horizontal scroller, so a phone showed the
/// prompt column and hid outcome, cost and every count behind a swipe — and
/// neither table printed a clock at all, which is half of what a session
/// replay is for. Rust cannot decide whether the result *looks* right; it can
/// decide that the markup a table needs is gone and the markup the cards and
/// the rail need is there.
#[test]
fn a_session_replay_is_cards_with_a_clock_rail() {
    for needle in [
        "class=\"ses-cards\" id=\"ses-list\"", // the list container, no longer a scroller
        "class=\"ses-card",                    // one card per session
        "id=\"ses-filter\"",                   // eighty sessions, six on a screen
        "class=\"tl-turn\"",                   // one card per turn
        "class=\"tl-when\"",                   // …with the wall clock beside it
        "turnGapHtml",                         // …and the idle gap between two
        "const clockOf",                       // local time, from the stamp
    ] {
        assert!(
            INDEX_HTML.contains(needle),
            "the session plane is missing {needle}"
        );
    }
    for gone in [
        "<th class=\"num\">Tok in·out</th>", // the sixteen-column turn table
        "<th class=\"num\">Turns</th>",      // the twelve-column session table
        "function turnRowHtml",              // its row builder
    ] {
        assert!(
            !INDEX_HTML.contains(gone),
            "the session tables were replaced by cards, but {gone} survives"
        );
    }
}

/// A phone gets one scrolling row of tabs, and a rail that does not stand
/// between the reader and the transcript.
///
/// Eleven tabs wrapped to four rows of sticky chrome at 390px, and the
/// section rail — a sidebar in a one-column grid — stacked above the content
/// as a list of section names to scroll past. Both are settled in the sheet,
/// so both are checkable here.
#[test]
fn narrow_screens_get_one_tab_row_and_a_rail_beside_the_content() {
    for needle in [
        "@media(max-width:760px)",  // the narrow layer exists
        ".tabs{flex-wrap:nowrap",   // one row, scrolled, never wrapped
        ":root{--navh:45px}",       // …and everything sticky clears it
        "@media(max-width:980px)",  // the rail's own breakpoint
        ".tx-rail{top:var(--navh)", // it sticks under the bar
        "flex-direction:row",       // as a strip, not a stacked list
    ] {
        assert!(
            INDEX_HTML.contains(needle),
            "the narrow-screen layout is missing {needle}"
        );
    }
}

/// On a wide screen the picker sits beside the replay it controls.
///
/// One column is right for a phone and wrong at 1400px: the list took the
/// full width and the replay started below the fold, so choosing a session
/// meant scrolling to find out what the choice did. The layout is settled in
/// the sheet, and the scroll-on-click is settled in the script — both are
/// checkable here; whether it *looks* right is not.
#[test]
fn a_wide_screen_puts_the_picker_beside_the_replay() {
    for needle in [
        "class=\"ses-cols\"",                  // the two-column container
        "class=\"card section-gap ses-pick\"", // the picker becomes a rail
        "class=\"ses-replay\"",                // …and the replay is its own column
        "@media(min-width:1080px)",            // the breakpoint that turns it on
        ".ses-pick{position:sticky",           // the rail stays while the list scrolls
        "matchMedia(\"(min-width:1080px)\")",  // …so a click must not scroll the page
    ] {
        assert!(
            INDEX_HTML.contains(needle),
            "the wide-screen session layout is missing {needle}"
        );
    }
}

/// A table that scrolls sideways says so, and keeps the cell that names the
/// row.
///
/// Ten containers on this page overflow at phone width and none of them
/// admitted it: the columns simply stopped, with no edge and no hint that
/// more were to the right. The shadow pair is pure CSS on the scroller
/// itself, so it covers every table at once rather than the ones somebody
/// remembered to wrap.
#[test]
fn a_sideways_table_admits_it_and_holds_its_first_column() {
    for needle in [
        "no-repeat local",                // the pair that covers the shadow at an edge
        "no-repeat scroll",               // …and the pair that shows while there is more
        ".scroll-x table th:first-child", // the identity column stays put
        "position:sticky;left:0",
        "max-width:45vw", // …without eating the screen it made swipeable
    ] {
        assert!(
            INDEX_HTML.contains(needle),
            "the scrolling-table affordance is missing {needle}"
        );
    }
}

/// The Overview's execution list is the session timeline's twin, so it is
/// drawn by the same card rather than by a twelve-column table.
#[test]
fn the_overview_lists_executions_as_cards() {
    for needle in [
        "function execCardHtml",        // the card
        "goTranscript(+tr.dataset.id)", // …still opens the turn it names
        "const dayShort",               // the 56px rail needs a date that fits
    ] {
        assert!(
            INDEX_HTML.contains(needle),
            "the Overview execution list is missing {needle}"
        );
    }
    for gone in [
        "<th scope=\"col\">kind</th>",   // the twelve-column table it replaced
        "<th scope=\"col\">prompt</th>", // …whose headers exist nowhere else
    ] {
        assert!(
            !INDEX_HTML.contains(gone),
            "the Overview execution table was replaced by cards, but {gone} survives"
        );
    }
}

/// The transcript host must not be styled by the masthead's status dot.
///
/// `paintRenderedTranscript` puts `live` on the host so the shared stylesheet
/// shows its prompt-inspect controls (`:host(.live)`). The dashboard's own
/// `.live` rule is the connection indicator, and its `display:inline-flex`
/// made that host a content-sized box: at 390px the rendered transcript came
/// out wider than the card holding it and had its right-hand column clipped.
#[test]
fn the_transcript_host_is_not_dressed_as_the_connection_indicator() {
    assert!(
        INDEX_HTML.contains(".headmeta .live{display:inline-flex"),
        "the connection indicator's rule must be scoped to the masthead"
    );
    assert!(
        !INDEX_HTML.contains("\n.live{display:inline-flex"),
        "an unscoped .live rule also matches the transcript host, which wears \
         that class so the shared stylesheet shows its inspect controls"
    );
    assert!(
        INDEX_HTML.contains("#tx-render{display:block"),
        "the transcript host states its own display"
    );
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
        let _ = serve(
            root,
            0,
            std::sync::Arc::new(crate::NoContributions),
            move |addr| {
                let _ = tx.send(addr);
            },
        )
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
        let _ = serve(
            root,
            0,
            std::sync::Arc::new(crate::NoContributions),
            move |addr| {
                let _ = tx.send(addr);
            },
        )
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

/// A live stream must release its one-shot connection permit the moment its
/// route is known. It must not hold that permit until the tab closes.
///
/// `serve_with_capacity` shrinks the pool to a single permit. That keeps the
/// proof cheap: one live connection and one ordinary request, not the
/// `MAX_LIVE_CONNECTIONS` sockets the real pool would need. With the bug,
/// the live stream below holds that lone permit for as long as its socket
/// stays open. The accept loop then wedges at the second connection's
/// `acquire_owned`, so an ordinary `GET /api/meta` — which would otherwise
/// answer in microseconds — never gets a response. The bounded read below
/// turns that hang into a failing assertion instead of a stuck test.
#[tokio::test]
async fn a_live_stream_releases_its_permit_before_its_long_life_begins() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let ws = TempDir::new().unwrap();
    let root = ws.path().to_path_buf();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let _ = serve_with_capacity(
            root,
            0,
            std::sync::Arc::new(crate::NoContributions),
            move |addr| {
                let _ = tx.send(addr);
            },
            1, // one permit: a single leaked one is enough to wedge the loop
        )
        .await;
    });
    let addr = rx.await.unwrap();

    // Open the live stream and read its response head. By the time that
    // head arrives, `respond_to_head` has already dropped the permit — it
    // does so before ever calling `live::serve_stream`, which is what
    // writes this head. The socket is then left open, unread, for the rest
    // of the test: what matters is that the *server* still treats it as a
    // live subscription, not that this client does anything with it.
    let live_stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut live_reader = BufReader::new(live_stream);
    live_reader
        .get_mut()
        .write_all(b"GET /api/v1/live HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let mut status = String::new();
    live_reader.read_line(&mut status).await.unwrap();
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "stream refused: {status}"
    );
    loop {
        let mut line = String::new();
        live_reader.read_line(&mut line).await.unwrap();
        if line.trim().is_empty() {
            break; // end of the response head; the stream body follows
        }
    }

    // The single permit is now either free (fixed) or gone for good (bug).
    // This connection is reachable at all only if the accept loop is still
    // calling `accept()`, and answered only if a permit was free to take —
    // both fail together the moment the live stream above leaks its own.
    let mut plain = tokio::net::TcpStream::connect(addr).await.unwrap();
    plain
        .write_all(b"GET /api/meta HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let mut response = String::new();
    let answered =
        tokio::time::timeout(Duration::from_secs(2), plain.read_to_string(&mut response)).await;
    assert!(
        answered.is_ok(),
        "an ordinary request hung behind the live stream's permit, which \
         must be released before the stream's long life begins rather than \
         held for it"
    );
    assert!(
        response.starts_with("HTTP/1.1 200 OK"),
        "head was: {response}"
    );

    drop(live_reader);
    server.abort();
}
