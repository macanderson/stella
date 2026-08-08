// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **Light mode, and the ink-on-gold fix it required.**
//!
//! It lives in `tests/` rather than beside the unit tests because
//! `src/tests.rs` sits exactly on the 1500-line ratchet (`make gate`'s
//! `file-size` guard) — the same reason `tests/journal_era.rs` gives for
//! being here instead of there.
//!
//! Two things this pins down:
//!
//! 1. **Both wordmark cuts are served and wired into the header.** The dark
//!    cut (`/assets/wordmark.svg`) already had a route test; this adds the
//!    light cut (`/assets/wordmark-light.svg`) and the markup that swaps
//!    between them, so a route that exists but is never referenced — or
//!    markup that references a route that doesn't exist — fails loudly
//!    instead of rendering a broken image.
//! 2. **No selector recolors `--ground`-as-text.** Before this change,
//!    `.tf button[aria-checked="true"]` and `.ctx-row button.on` painted
//!    their selected-state text in `color:var(--ground)` — reusing the page
//!    background token to get ink-on-gold contrast in the (then dark-only)
//!    page. Once `--ground` became theme-dependent, that coupling would have
//!    put light-theme text on a gold fill, which is close to unreadable —
//!    the exact failure mode `--ink` (a fixed token, never repointed by the
//!    theme) exists to prevent. This asserts the coupling is gone for good,
//!    not just fixed today.

use stella_observatory::respond;
use tempfile::TempDir;

#[test]
fn both_wordmark_cuts_are_served_and_referenced() {
    let ws = TempDir::new().unwrap();

    let dark = respond(ws.path(), "/assets/wordmark.svg");
    assert_eq!(dark.content_type, "image/svg+xml");
    assert!(String::from_utf8(dark.body).unwrap().contains("<svg"));

    let light = respond(ws.path(), "/assets/wordmark-light.svg");
    assert_eq!(light.content_type, "image/svg+xml");
    let light_body = String::from_utf8(light.body).unwrap();
    assert!(light_body.contains("<svg"));
    // Ink text, not Paper — this cut is for the Paper ground, not the Void one.
    // Under the nebula kit Ink is `#0B0E1A`: the comet kit used one black for
    // both the dark ground and the light text, and the nebula splits them
    // (ground is Void `#080B1C`) because a canvas wants more blue than a glyph.
    assert!(light_body.contains("#0B0E1A"));

    let page = String::from_utf8(respond(ws.path(), "/").body).unwrap();
    for needle in [
        "src=\"/assets/wordmark.svg\"",
        "src=\"/assets/wordmark-light.svg\"",
        "class=\"bword bword-dark\"",
        "class=\"bword bword-light\"",
        "id=\"themeToggle\"",
        ":root[data-theme=\"light\"]",
    ] {
        assert!(page.contains(needle), "missing {needle}");
    }
}

#[test]
fn no_selector_recolors_the_page_background_as_text() {
    let ws = TempDir::new().unwrap();
    let page = String::from_utf8(respond(ws.path(), "/").body).unwrap();
    assert!(
        !page.contains("color:var(--ground)"),
        "a selector still paints text in --ground; on the light theme that \
         token is a light background, not ink, and this pattern only ever \
         worked because dark mode's --ground happens to be dark"
    );
    for needle in [
        "--ink:#0B0E1A",
        ".tf button[aria-checked=\"true\"]{color:var(--ink)",
        ".ctx-row button.on{color:var(--ink)",
    ] {
        assert!(page.contains(needle), "missing {needle}");
    }
}
