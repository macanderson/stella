// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **Light mode, and the ink-on-accent fix it required.**
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
//!    background token to get readable contrast on a selected fill in the
//!    (then dark-only) page. Once `--ground` became theme-dependent, that
//!    coupling put light-theme text on a light fill, which is close to
//!    unreadable — the exact failure mode `--ink` exists to prevent. This
//!    asserts the coupling is gone for good, not just fixed today.
//!
//!    **`--ink` is no longer a fixed hex, and that is the point of the
//!    change that moved it.** It was one value on every theme because it sat
//!    on a gold fill, and gold is dark enough for ink either way. The page's
//!    accent is now the *text* colour, so "selected" is an inversion of the
//!    page rather than a hue: the fill takes `--accent` and `--ink` is
//!    whatever the page's ground is, which necessarily flips with the theme
//!    (`#0A0A0C` on the dark page, `#FFFFFF` on the paper one).
//!    Pinning the old literal here would pin the old design, so this now
//!    asserts the *invariant* the literal was standing in for: `--ink` is
//!    declared in the dark root and re-pointed in both light gates, and the
//!    selectors that need an on-accent colour still reach for it rather than
//!    for `--ground`.

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
    // Obsidian text, not Paper — this cut is for the Paper ground, not the
    // dark one. The literal moves with the brand kit; it was `#0B0B0C` under
    // v1.0 and is the v3.0 ground.
    assert!(light_body.contains("#070B10"));

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
        // Declared on the ink page, and re-pointed by BOTH light gates — the
        // media query and the explicit attribute. Two declarations is the
        // load-bearing count: a light theme that inherited the dark `--ink`
        // would paint dark text on the dark selected fill, which is the same
        // unreadable pair this test was written to catch, arrived at from the
        // other direction.
        "--ink:#0A0A0C",
        "--ink:#FFFFFF",
        ".tf button[aria-checked=\"true\"]{color:var(--ink)",
        // Was .ctx-row button.on before the call inspector replaced each
        // row's own two buttons with a single view toggle; the ink-on-accent
        // inversion pattern this test pins moved with it.
        ".ctx-toggle button.on{color:var(--ink)",
    ] {
        assert!(page.contains(needle), "missing {needle}");
    }
    assert_eq!(
        page.matches("--ink:#FFFFFF").count(),
        2,
        "--ink must be re-pointed by both light gates (the \
         prefers-color-scheme media query and [data-theme=\"light\"]); one \
         alone leaves the other inheriting the ink-page value"
    );
}
