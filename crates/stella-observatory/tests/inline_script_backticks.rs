// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **A backtick in an HTML comment is a whole-dashboard outage.**
//!
//! The dashboard's markup is generated from JavaScript template literals, so a
//! stretch of HTML inside the page's inline `<script>` is *string content*, not
//! markup — and the delimiter of that string is the backtick. An HTML comment
//! written there in the ordinary house style, with a path or a crate name
//! wrapped in markdown backticks, closes the template literal at its first
//! backtick. Everything after it parses as expression syntax and the script
//! dies with a `SyntaxError` before its first statement runs.
//!
//! The failure has no partial mode. One bad character takes down the entire
//! inline script, so every panel renders empty while the server keeps answering
//! every `/api/*` route with a `200` and real rows. That is what shipped: the
//! transcript comment added beside the `rendered transcript` link carried
//! `` `/transcript` `` and `` `stella-transcript` ``, and the dashboard showed
//! no data at all.
//!
//! Nothing else in the crate can see this. The Rust compiler and clippy do not
//! read the asset, `dashboard_html_has_no_external_references` only greps for
//! hosts, and the golden route payloads assert on JSON the server produced
//! without ever executing the page. So this test reads the served asset and
//! asserts the one property the outage violated, which is mechanically exact:
//! **no HTML comment inside an inline `<script>` may contain a backtick.**
//!
//! It deliberately does not attempt a full template-literal balance check.
//! That needs a JavaScript parser to be correct, and a half-right one would
//! either miss real breakage or reject legitimate code — a guard that has to be
//! argued with gets deleted. This property is narrow, it is the property that
//! actually broke, and it is decidable by reading characters.

/// The page exactly as the binary embeds and serves it.
const INDEX_HTML: &str = include_str!("../src/assets/index.html");

/// Byte ranges of each inline `<script>` body in `html`.
///
/// The observatory's page carries no external scripts (that is its own gate),
/// so every script body found here is inline code that must parse.
fn inline_script_bodies(html: &str) -> Vec<(usize, &str)> {
    let mut bodies = Vec::new();
    let mut cursor = 0usize;

    while let Some(open_rel) = html[cursor..].find("<script") {
        let open = cursor + open_rel;
        let Some(gt_rel) = html[open..].find('>') else {
            break;
        };
        let body_start = open + gt_rel + 1;
        let Some(close_rel) = html[body_start..].find("</script>") else {
            break;
        };
        let body_end = body_start + close_rel;
        bodies.push((body_start, &html[body_start..body_end]));
        cursor = body_end;
    }

    bodies
}

/// 1-based line number of `offset` within `html`, for a failure a human can act on.
fn line_of(html: &str, offset: usize) -> usize {
    html[..offset].bytes().filter(|b| *b == b'\n').count() + 1
}

#[test]
fn inline_script_html_comments_carry_no_backtick() {
    let bodies = inline_script_bodies(INDEX_HTML);
    assert!(
        !bodies.is_empty(),
        "found no inline <script> in the served page — the scanner is broken, \
         not the page"
    );

    let mut offenders = Vec::new();

    for (base, body) in &bodies {
        let mut cursor = 0usize;
        while let Some(open_rel) = body[cursor..].find("<!--") {
            let open = cursor + open_rel;
            let Some(close_rel) = body[open..].find("-->") else {
                // An unterminated comment is its own defect, but it is not the
                // one this test owns; leave it to the parse gates.
                break;
            };
            let close = open + close_rel + "-->".len();
            let comment = &body[open..close];

            if comment.contains('`') {
                offenders.push((line_of(INDEX_HTML, base + open), comment.to_string()));
            }
            cursor = close;
        }
    }

    assert!(
        offenders.is_empty(),
        "an HTML comment inside the page's inline <script> contains a backtick. \
         The comment sits inside a JavaScript template literal, so that backtick \
         closes the literal early and the whole script fails to parse — every \
         panel renders empty while the API keeps returning 200. Drop the \
         backticks from the comment.\n\n{}",
        offenders
            .iter()
            .map(|(line, text)| format!(
                "  src/assets/index.html:{line}\n    {}",
                text.replace('\n', "\n    ")
            ))
            .collect::<Vec<_>>()
            .join("\n\n")
    );
}
