// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! **The page-clipper gate (#2026).**
//!
//! The dashboard clips long text in two places — once on the server, in
//! `db::truncate`, and again in the browser, in `clip`
//! ([`src/assets/index.html`](../src/assets/index.html)). Three Rust copies of
//! the server-side clipper were folded into one in #1999. The browser copy
//! could not be folded with them, and it is the one that had actually drifted:
//! it counted `String.prototype.length` — UTF-16 **code units** — and cut with
//! `slice`, so a string whose n-th code unit fell inside a surrogate pair was
//! cut into a **lone surrogate** and rendered as `�`. Every prompt, title and
//! reflection on the page is user-authored text that can contain an emoji.
//!
//! Nothing could catch that. The page is JavaScript inside a Rust crate: the
//! compiler does not read it, clippy does not read it, and the crate's other
//! two page assertions
//! (`dashboard_html_has_no_external_references`, the golden route payloads)
//! never execute it. A duplicated decision that no gate can see is the one
//! that drifts — which is exactly what happened here.
//!
//! So this suite executes it. It pulls `clip` out of the page **as served by
//! [`respond`](stella_observatory::respond)**, not out of a copy or a
//! re-`include_str!`, runs it under `node` against a corpus built in Rust, and
//! asserts the contract below. An edit to that arrow function now fails at
//! `cargo test`.
//!
//! ## The contract
//!
//! For `clip(s, n)`, where `v` is `s` coerced to a string and a *character* is
//! a Unicode scalar value (Rust's `char`, JavaScript's code point):
//!
//! 1. **Well-formed output.** Never a lone surrogate. This is the defect.
//! 2. **Short strings are untouched.** `v` has `<= n` characters → `v` back,
//!    byte for byte, with no ellipsis.
//! 3. **Long strings are cut on a character boundary.** Otherwise the first
//!    `n` characters of `v`, then `'…'`.
//! 4. **Non-strings still work.** Several fields arrive from JSON a bad row
//!    can leave `null`; `clip` coerces rather than throwing.
//!
//! Rules 2 and 3 are `db::truncate`'s contract, restated here rather than
//! imported: `db::truncate` is `pub(crate)`, so an integration test cannot
//! call it. The restatement is deliberate and is what an oracle should be —
//! an independent derivation, since an oracle that shared the implementation
//! could not observe a bug in it. The tradeoff is honest: this suite pins the
//! *contract*, so if `db::truncate`'s own behaviour is ever changed, this
//! fails and the author must change both — it does not silently follow.
//!
//! ## Why `node`, and what happens without it
//!
//! Executing JavaScript needs a JavaScript engine, and embedding one
//! (`boa`, `quickjs`) is a large dependency for one arrow function. Shelling
//! out to a binary and skipping when it is absent is this workspace's
//! established idiom for exactly this — `stella-tools`' git tests and
//! `stella-tools/src/registry/tests/fence.rs`'s `make` test both do it. CI's
//! runners ship node, so this gates there.

use std::process::Command;

use serde_json::{Value, json};

/// The page as the browser receives it — not `include_str!` of the asset, so
/// a change to how the route serves it cannot leave this suite testing a file
/// nobody is shipping.
fn served_page() -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let res = stella_observatory::respond(dir.path(), "/");
    assert_eq!(res.status, "200 OK", "the dashboard route must serve");
    String::from_utf8(res.body).expect("the page is UTF-8")
}

/// Cut `src` out of the page, from `const clip =` to the brace that closes it.
///
/// Brace-counted rather than line-counted so reformatting the function does
/// not quietly extract half of it; the body contains no braces inside string
/// literals, which is what makes the naive count sound.
fn extract_clip(page: &str) -> String {
    let start = page
        .find("const clip =")
        .expect("index.html must define `clip` (did it get renamed?)");
    let rest = &page[start..];
    let mut depth = 0usize;
    let mut seen_open = false;
    for (i, c) in rest.char_indices() {
        match c {
            '{' => {
                depth += 1;
                seen_open = true;
            }
            '}' => {
                depth -= 1;
                if depth == 0 && seen_open {
                    let end = i + c.len_utf8();
                    let tail = rest[end..].strip_prefix(';').map_or(end, |_| end + 1);
                    return rest[..tail].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("`clip` in index.html has unbalanced braces");
}

/// Rule 2 + rule 3 of the contract — `db::truncate`'s behaviour, restated.
fn oracle(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(n).collect();
    out.push('…');
    out
}

/// Strings whose clip boundary is worth probing, each swept across every `n`
/// from 0 to a few past its length so the cut lands *inside* every character.
fn corpus() -> Vec<(String, usize)> {
    let subjects = [
        String::new(),
        "plain ascii".to_string(),
        // The defect: a 2-code-unit character. Placed at several offsets so
        // some `n` is guaranteed to fall inside it.
        "\u{1F44D}".to_string(),
        format!("{}\u{1F44D}tail", "a".repeat(5)),
        "\u{1F44D}\u{1F44D}\u{1F44D}".to_string(),
        "mixed \u{1F600} emoji \u{1F680} run".to_string(),
        // Astral-plane non-emoji: CJK extension B.
        "\u{20000}\u{20001}\u{20002}".to_string(),
        // BMP multi-byte in UTF-8 but single-unit in UTF-16 — the case where
        // Rust bytes and JS units disagree in the *other* direction.
        "日本語のテキスト".to_string(),
        // A combining mark: two scalar values, one grapheme. Both clippers
        // may split this, and agreeing that they do is the point.
        "e\u{0301}e\u{0301}e\u{0301}".to_string(),
        "\u{1F3F3}\u{FE0F}\u{200D}\u{1F308}".to_string(),
    ];
    let mut cases = Vec::new();
    for s in subjects {
        let span = s.chars().count() + 3;
        for n in 0..=span {
            cases.push((s.clone(), n));
        }
    }
    cases
}

#[test]
fn page_clip_matches_db_truncate_and_never_splits_a_surrogate_pair() {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("skipping page-clipper test: `node` not available");
        return;
    }

    let clip_src = extract_clip(&served_page());

    let mut cases: Vec<Value> = corpus()
        .into_iter()
        .map(|(s, n)| json!({ "s": s, "n": n, "want": oracle(&s, n) }))
        .collect();
    // Rule 4: the fields these clip arrive from JSON, and a bad row can leave
    // one null. Coercion is part of the contract, so it is part of the gate.
    cases.push(json!({ "s": Value::Null, "n": 5, "want": "" }));
    cases.push(json!({ "s": 1234567, "n": 3, "want": "123…" }));
    cases.push(json!({ "s": 12, "n": 5, "want": "12" }));

    let dir = tempfile::tempdir().expect("tempdir");
    let cases_path = dir.path().join("cases.json");
    let script_path = dir.path().join("check.js");
    std::fs::write(&cases_path, Value::Array(cases).to_string()).expect("write cases");

    // Two details in the driver are load-bearing.
    //
    // `isWellFormed` is Node 20+; where it is missing the regex is the same
    // check — an unpaired high or low surrogate anywhere in the output.
    //
    // `show` escapes every reported string to bare ASCII, and without it this
    // suite cannot report its own headline failure: a lone surrogate is not
    // representable in a Rust `String`, so serde_json rejects the driver's
    // output and the panic reads "unexpected end of hex escape" instead of
    // naming the defect. Comparison still uses the raw values; only the
    // diagnostic is escaped, so a failure prints `\ud83d` and says so.
    let script = format!(
        r#"const fs = require("fs");
{clip_src}
const wellFormed = s => typeof s.isWellFormed === "function"
  ? s.isWellFormed()
  : !/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/.test(s);
const show = s => {{
  let o = "";
  for (let i = 0; i < s.length; i++) {{
    const u = s.charCodeAt(i);
    o += (u >= 0x20 && u < 0x7f) ? s[i] : "\\u" + u.toString(16).padStart(4, "0");
  }}
  return o;
}};
const bad = [];
for (const c of JSON.parse(fs.readFileSync(process.argv[2], "utf8"))) {{
  const shown = show(String(c.s ?? ""));
  let got;
  try {{ got = clip(c.s, c.n); }}
  catch (e) {{ bad.push({{ n: c.n, why: "threw: " + e.message, s: shown }}); continue; }}
  if (got !== c.want) {{
    bad.push({{ n: c.n, why: "mismatch", s: shown, want: show(c.want), got: show(got) }});
  }} else if (!wellFormed(got)) {{
    bad.push({{ n: c.n, why: "lone surrogate", s: shown, got: show(got) }});
  }}
}}
process.stdout.write(JSON.stringify(bad));
"#
    );
    std::fs::write(&script_path, script).expect("write script");

    let out = Command::new("node")
        .arg(&script_path)
        .arg(&cases_path)
        .output()
        .expect("run node");
    assert!(
        out.status.success(),
        "node failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let failures: Vec<Value> =
        serde_json::from_slice(&out.stdout).expect("the driver emits a JSON array");
    assert!(
        failures.is_empty(),
        "the page's `clip` disagrees with `db::truncate` in {} case(s).\n\
         `n` counts code points, not UTF-16 code units — see index.html:1064 \
         and the module docs.\nFirst few: {}",
        failures.len(),
        Value::Array(failures.into_iter().take(6).collect())
    );
}

#[test]
fn the_page_defines_exactly_one_clipper() {
    // Two definitions would mean the extraction above is silently testing
    // whichever came first — the failure mode that let three Rust copies
    // accumulate before #1999.
    let page = served_page();
    assert_eq!(
        page.matches("const clip =").count(),
        1,
        "index.html must define `clip` exactly once"
    );
}

/// The extractor is load-bearing: if it silently grabbed the wrong text the
/// suite above would pass while testing nothing.
#[test]
fn extractor_takes_the_whole_function() {
    let src = extract_clip(&served_page());
    assert!(src.starts_with("const clip ="), "starts at the definition");
    assert!(src.ends_with("};"), "runs to the closing brace: {src}");
    assert_eq!(
        src.matches('{').count(),
        src.matches('}').count(),
        "balanced: {src}"
    );
}

/// Pins the defect by shape, so the guard survives a machine with no `node`.
///
/// The test above is the real one — it proves behaviour — but it skips when
/// `node` is absent, and a regression that only a skipped test catches is not
/// caught. This one always runs.
#[test]
fn code_unit_clipping_does_not_come_back() {
    let src = extract_clip(&served_page());
    assert!(
        !src.contains("v.length > n"),
        "`clip` is counting UTF-16 code units again — that splits surrogate \
         pairs and puts a lone surrogate in the DOM (#2026): {src}"
    );
    assert!(
        src.contains("Array.from"),
        "`clip` must iterate code points, not code units (#2026): {src}"
    );
}
