// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **Light mode, and the ink-on-accent fix it required.**
//!
//! It lives in `tests/` rather than beside the unit tests because
//! `src/tests.rs` sits exactly on the 1500-line ratchet (`make gate`'s
//! `file-size` guard) — the same reason `tests/journal_era.rs` gives for
//! being here instead of there.
//!
//! Three things this pins down:
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
//! 3. **The page's two light gates declare one scheme.** See
//!    [`the_two_light_gates_declare_one_scheme`] — the media query had kept
//!    the pre-v5.0 cool-graphite neutrals while the explicit `data-theme`
//!    gate moved to the product ramp, so which cast of the page a reader got
//!    depended on whether they used their OS preference or the toggle
//!    (#4072).

use std::collections::BTreeMap;

use stella_observatory::respond;
use tempfile::TempDir;

/// The slice of `css` between two markers, exclusive of the trailing one.
fn between<'a>(css: &'a str, start: &str, end: &str) -> &'a str {
    let from = css
        .find(start)
        .unwrap_or_else(|| panic!("marker `{start}` not found"));
    let rest = &css[from + start.len()..];
    let to = rest
        .find(end)
        .unwrap_or_else(|| panic!("marker `{end}` not found after `{start}`"));
    &rest[..to]
}

/// `css` with every `/* … */` comment removed. These blocks carry more prose
/// than declarations, and that prose quotes contrast ratios (`4.43:1`) and
/// measurements (`needs: 6.84`) — so a parser that splits on `:` before
/// stripping comments reads a sentence as a declaration and, worse, swallows
/// the real declaration that follows it.
fn strip_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(open) = rest.find("/*") {
        out.push_str(&rest[..open]);
        match rest[open..].find("*/") {
            Some(close) => rest = &rest[open + close + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Every `--token: value` declaration in a block, custom properties only.
fn declarations(block: &str) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for decl in strip_comments(block).split(';') {
        let Some((name, value)) = decl.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if !name.starts_with("--") {
            continue;
        }
        let value = value.trim().to_ascii_uppercase();
        if value.is_empty() {
            continue;
        }
        found.insert(name.to_string(), value);
    }
    found
}

/// **The page's two light gates are one scheme, not two.**
///
/// The OS-preference gate and the explicit `data-theme` gate paint the same
/// page for different readers — one who left their desktop on light, one who
/// clicked the toggle — so a role that differs between them is the same
/// design shipping in two casts, and only the second is checked: the
/// cross-surface matrix (`crates/stella-cli/tests/design_token_parity.rs`)
/// reads its canon from `:root[data-theme="light"]` alone and has never
/// looked at the media query.
///
/// It had drifted exactly the way an unchecked half does. The media query
/// still carried the pre-v5.0 cool-graphite ramp — `--text-2 #4D535A`,
/// `--text-3 #828C97`, `--hairline #E7EBF0`, a gold `--identity` — against
/// the attribute gate's product neutrals, seventeen roles apart. The worst of
/// them was an *absence*: the media query never re-pointed `--text-emph`, so
/// an OS-light reader got the dark scheme's `#BFC1CC` on white — 1.68:1 on
/// `--surface`, which is not a contrast failure so much as invisible ink.
///
/// This asserts the invariant `crates/stella-cli/src/export.rs` states for
/// its own two gates and holds: no colour is defined only inside the media
/// query, and no colour disagrees across them.
#[test]
fn the_two_light_gates_declare_one_scheme() {
    let ws = TempDir::new().unwrap();
    let page = String::from_utf8(respond(ws.path(), "/").body).unwrap();

    let dark = declarations(between(&page, "BEGIN palette", "END palette"));
    let media = declarations(between(
        &page,
        r#":root:not([data-theme="dark"]){"#,
        "\n  }",
    ));
    let attr = declarations(between(&page, r#":root[data-theme="light"]{"#, "\n}"));

    assert!(
        media.len() > 20 && attr.len() > 20,
        "the light blocks did not parse: media={}, attr={}",
        media.len(),
        attr.len()
    );

    let mut drift = Vec::new();
    for (token, want) in &attr {
        match media.get(token) {
            Some(got) if got == want => {}
            Some(got) => drift.push(format!("  {token}: media {got} vs attribute {want}")),
            None => {
                let inherited = dark
                    .get(token)
                    .map(|v| format!("inherits the ink page's {v}"))
                    .unwrap_or_else(|| "is undeclared".to_string());
                drift.push(format!(
                    "  {token}: absent from the media query, {inherited}"
                ));
            }
        }
    }
    for token in media.keys() {
        if !attr.contains_key(token) {
            drift.push(format!("  {token}: absent from the attribute gate"));
        }
    }

    assert!(
        drift.is_empty(),
        "the two light gates disagree on {} role(s); an OS-light reader and a \
         reader who clicked the toggle see different casts of the same page:\n{}",
        drift.len(),
        drift.join("\n")
    );
}

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
    // Ink text, not Paper — this cut is for the Paper ground, not the dark
    // one. The literal moves with the brand kit; v5.0 splits the two roles the
    // older kits conflated, so the value here is `ink` (text on a light ground)
    // and NOT `bg` (the dark canvas). They were one hex before, which is
    // exactly why asserting the wrong one would still have looked plausible.
    assert!(light_body.contains("#141416"));

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
