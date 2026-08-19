//! The web instrument surfaces carry one palette, and this test is what makes
//! that true rather than merely intended.
//!
//! Four files describe the same instrument design system in three vocabularies:
//!
//! | file | names it uses |
//! |---|---|
//! | `crates/stella-observatory/src/assets/index.html` | `--ground` `--text-2` `--accent` |
//! | `crates/stella-cli/src/export.rs` | the same (adopted deliberately) |
//! | `arenabench/ui/app/globals.css` | `--background` `--muted` `--accent` |
//! | `arenabench/web/app/globals.css` | `--bg` `--dim` `--accent` |
//!
//! The Observatory's delimited palette block is the single definition; the
//! other three are derivations. Until this test existed the contract was a
//! sentence in a comment — "a value that differs between them is a bug in
//! whichever moved last" — which is a description of drift, not a defence
//! against it, and the export dashboard had already drifted two whole
//! recolours behind while nothing failed.
//!
//! `/export` was briefly dropped from this matrix (#2614), on the stated
//! ground that it "derives its dark palette from `stella_tui::theme`". That
//! was true of #2606's file and is not true of the one shipping: the same
//! commit that dropped it also removed `dark_tokens()` entirely and wrote the
//! instrument palette into `:root` literally. So the row is restored, and it
//! passed on the very commit that removed it — nothing had to change in
//! `export.rs` to put it back. Dropping a row is how the drift that motivated
//! this file happens: the dashboard is a standalone artifact users mail
//! around, it cannot be eyeballed beside the Observatory, and it is the copy
//! of the product most readers ever see.
//!
//! Its *grammar* is genuinely its own — the compact transcript row, the tabs —
//! and nothing here constrains that. This matrix is about colour values only.
//!
//! Why a test and not a `make gate` step: a gate step is five coupled edits
//! (`GATE_STEPS`, the AGENTS.md block, the CONTRIBUTING.md block, the script,
//! `check-gate-parity.sh`) and one more shared cell for two concurrent PRs to
//! collide on. `make gate` already runs `make test`, so this rung costs
//! nothing new and cannot red `main` on its own. The exemplar is
//! `crates/stella-model/src/provider_parity.rs`, which enforces invariant #8
//! the same way: a matrix, checked from both sides, in a plain test.
//!
//! Why it lives in `stella-cli`: the export dashboard is the surface that
//! drifted, `stella-cli` is a binary crate with no `lib.rs` (so an in-crate
//! test cannot be imported from elsewhere), and this test reads all four files
//! as text rather than linking any of them. It needs no crate it does not
//! already have.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The workspace root, derived from this crate's manifest directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root resolves from CARGO_MANIFEST_DIR")
}

fn read(rel: &str) -> String {
    let path = workspace_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Every `--token: #hex` declaration in `css`, lowercased, last write winning.
///
/// Lowercased because the four files disagree on hex case and always have —
/// the Rust surfaces write `#EDEDED`, the TypeScript ones `#ededed`. That is a
/// difference in a string, not in a colour, and a parity test that failed on
/// it would be measuring the wrong thing.
///
/// "Last write wins" is what makes a *scheme* extractable at all: each file
/// declares its tokens once per scheme, so scanning a scheme's own block gives
/// that scheme's values.
fn declarations(css: &str) -> BTreeMap<String, String> {
    let mut found = BTreeMap::new();
    for (index, _) in css.match_indices("--") {
        let rest = &css[index..];
        let Some(colon) = rest.find(':') else {
            continue;
        };
        let name = &rest[2..colon];
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            continue;
        }
        let value = rest[colon + 1..]
            .split([';', '\n'])
            .next()
            .unwrap_or("")
            .trim();
        if let Some(hex) = value.strip_prefix('#')
            && hex.len() == 6
            && hex.bytes().all(|b| b.is_ascii_hexdigit())
        {
            found.insert(
                format!("--{name}"),
                format!("#{}", hex.to_ascii_lowercase()),
            );
        }
    }
    found
}

/// The slice of `css` between two markers, exclusive of them.
fn between<'a>(css: &'a str, start: &str, end: &str) -> &'a str {
    let from = css
        .find(start)
        .unwrap_or_else(|| panic!("marker `{start}` not found"));
    let to = css[from..]
        .find(end)
        .unwrap_or_else(|| panic!("marker `{end}` not found after `{start}`"));
    &css[from..from + to]
}

/// The roles the system defines. A role is what a colour is *for*; the three
/// vocabularies spell them differently, and renaming any of those is a far
/// larger change than this contract needs to force.
const ROLES: [&str; 9] = [
    "ground", "surface", "text", "text-2", "text-3", "ok", "bad", "warn", "identity",
];

/// The Observatory is the single definition. Each entry is
/// `(role, its dark value, its light value)`.
fn canonical() -> Vec<(&'static str, String, String)> {
    let observatory = read("crates/stella-observatory/src/assets/index.html");
    let dark = declarations(between(&observatory, "BEGIN palette", "END palette"));
    // The explicit gate, not the media query: identical values, and it is the
    // one a reader's own toggle reaches.
    let light = declarations(between(&observatory, r#":root[data-theme="light"]"#, "\n}"));

    let role = |name: &str| -> (String, String) {
        (
            dark.get(name)
                .unwrap_or_else(|| panic!("observatory dark scheme defines no {name}"))
                .clone(),
            light
                .get(name)
                .unwrap_or_else(|| panic!("observatory light scheme defines no {name}"))
                .clone(),
        )
    };

    ROLES
        .into_iter()
        .map(|r| {
            let (dark, light) = role(&format!("--{r}"));
            (r, dark, light)
        })
        .collect()
}

/// A derived surface: where it lives, and how it spells each canonical role.
struct Surface {
    file: &'static str,
    /// The slice of the file holding the dark scheme, as `(start, end)` markers.
    dark: (&'static str, &'static str),
    /// The slice holding the light scheme.
    light: (&'static str, &'static str),
    /// `canonical role -> this file's token name`. A role absent from the map
    /// is one this surface legitimately does not carry.
    names: &'static [(&'static str, &'static str)],
}

fn surfaces() -> Vec<Surface> {
    vec![
        Surface {
            file: "crates/stella-cli/src/export.rs",
            dark: (":root {{", "  }}"),
            light: (r#":root[data-theme="light"] {{"#, "  }}"),
            names: &[
                ("ground", "--ground"),
                ("surface", "--surface"),
                ("text", "--text"),
                ("text-2", "--text-2"),
                ("text-3", "--text-3"),
                ("ok", "--ok"),
                ("bad", "--bad"),
                ("warn", "--warn"),
                ("identity", "--identity"),
            ],
        },
        Surface {
            file: "arenabench/ui/app/globals.css",
            dark: (".dark {", "\n}"),
            // Light is `:root` here: next-themes stamps `.dark` on <html>, so
            // the un-suffixed scheme is the paper one.
            light: (":root {", "\n}"),
            names: &[
                ("ground", "--background"),
                ("surface", "--panel-2"),
                ("text", "--foreground"),
                ("text-2", "--muted"),
                ("text-3", "--dim"),
                ("ok", "--ok"),
                ("bad", "--bad"),
                ("warn", "--warn"),
                // No `identity`. Not an omission — a posture. The arena scores
                // stella as one seat among several, so its mark does not
                // belong in the chrome every seat is judged under; #2577
                // removed the wordmark from the topbar and the four brand cuts
                // with it. Both arenabench files say so where the token would
                // otherwise sit.
            ],
        },
        // The shared transcript page. The Observatory's `render_transcript`
        // route serves it, so a reader clicking a run in the dashboard lands
        // on it — which is what makes it an instrument surface rather than a
        // published one, and puts it outside #2594's carve-out. It is also the
        // standalone artifact people mail around.
        //
        // It was outside this matrix until #3630, and had drifted exactly the
        // way an unchecked surface does: ground `#0a0a0a` against the
        // instrument's `#070B10`, and a different green, red and amber, so
        // "passed" was one colour in the dashboard and another in the
        // transcript of the same run. The file's own header comment claimed
        // the values WERE the instrument palette the whole time — which is the
        // argument for the row rather than for a third alignment pass.
        //
        // No `identity`: a transcript renders a run, and nothing on it is a
        // brand mark. Its two categorical hues (`--cyan`, `--violet`, tool
        // kind and speaker) are deliberately outside the instrument palette
        // and so outside this matrix; the file's header carries that decision
        // and its reasoning.
        Surface {
            file: "crates/stella-transcript/src/html/transcript.css",
            dark: (":root {", "\n}"),
            light: ("@media (prefers-color-scheme: light)", "\n  }"),
            names: &[
                ("ground", "--bg"),
                ("surface", "--panel"),
                ("text", "--ink"),
                ("text-2", "--dim"),
                ("text-3", "--faint"),
                ("ok", "--green"),
                ("bad", "--red"),
                ("warn", "--amber"),
            ],
        },
        Surface {
            file: "arenabench/web/app/globals.css",
            dark: (":root {", "\n}"),
            light: ("@media (prefers-color-scheme: light)", "\n  }"),
            names: &[
                ("ground", "--bg"),
                ("text", "--text"),
                ("text-3", "--dim"),
                ("ok", "--ok"),
                ("bad", "--bad"),
                // No `identity`, for the same reason as the client above.
            ],
        },
        // The two published benchmark pages. They are the reference the
        // instrument palette was drawn FROM, and the report already agreed
        // with the Observatory value for value — it simply had nothing
        // holding it there, which is the drift condition, not the absence of
        // it. The index did not agree: it carried a warm editorial palette and
        // a bronze accent while the report one click away was achromatic, so a
        // reader crossing that link watched the page change temperature.
        //
        // Both take the explicit `data-theme` gates rather than the media
        // query, for the same reason `canonical()` does: identical values, and
        // it is the gate a reader's own toggle reaches.
        //
        // These are published surfaces, and #2594 is right that a published
        // surface is not automatically an instrument — but an index to a set
        // of measurements, and the measurements themselves, are. Neither
        // carries `identity`: the benchmark scores stella against another
        // harness, so the same reasoning that keeps stella's mark out of the
        // arena's chrome applies here.
        Surface {
            file: "docs/benchmarks/index.html",
            dark: (r#":root[data-theme="dark"]{"#, "}"),
            light: (r#":root[data-theme="light"]{"#, "}"),
            names: &[
                ("ground", "--bg"),
                ("surface", "--sub"),
                ("text", "--ink"),
                ("text-2", "--ink-2"),
                ("text-3", "--ink-3"),
                ("ok", "--pass"),
                // No `bad`: an index lists reports, and none of them is a
                // failure state. No `warn` for the same reason.
            ],
        },
        Surface {
            file: "docs/benchmarks/terminal-bench-2-1-glm-5-2.html",
            dark: (r#":root[data-theme="dark"]{"#, "}"),
            light: (r#":root[data-theme="light"]{"#, "}"),
            names: &[
                ("ground", "--bg"),
                ("surface", "--sub"),
                ("text", "--ink"),
                ("text-2", "--ink-2"),
                ("text-3", "--ink-3"),
                ("ok", "--pass"),
                ("bad", "--fail"),
            ],
        },
    ]
}

/// The arena carries no stella identity, and that stays true on purpose.
///
/// A posture recorded only as an absence in `surfaces()` is a posture nobody
/// can see being broken: adding `--identity` back to either arenabench file
/// would make every existing assertion pass, because the parity test only
/// checks roles a surface claims. This asserts the absence itself, so the
/// decision in #2577 — stella's mark does not belong in the chrome every seat
/// is judged under — cannot be undone by accident while restyling.
#[test]
fn the_arena_carries_no_stella_identity() {
    for file in [
        "arenabench/ui/app/globals.css",
        "arenabench/web/app/globals.css",
    ] {
        let css = read(file);
        assert!(
            !declarations(&css).contains_key("--identity"),
            "{file} declares --identity. The arena judges stella as one seat \
             among several, so it carries no stella identity (#2577). If that \
             decision is being reversed, reverse it here too — do not let a \
             restyle do it silently."
        );
    }
}

/// Every surface agrees with the Observatory on every role it carries, in both
/// schemes.
///
/// This is the whole contract. It fails loudly and names the file, the role,
/// the two values and which scheme — because the failure a reader will hit is
/// "someone tuned one hex and three surfaces did not follow", and a diff of
/// two colour codes with no label is unreadable.
#[test]
fn every_web_surface_agrees_with_the_observatory_palette() {
    let canon = canonical();
    let mut divergences = Vec::new();

    for surface in surfaces() {
        let text = read(surface.file);
        let dark = declarations(between(&text, surface.dark.0, surface.dark.1));
        let light = declarations(between(&text, surface.light.0, surface.light.1));

        for (role, want_dark, want_light) in &canon {
            let Some((_, token)) = surface.names.iter().find(|(r, _)| r == role) else {
                continue;
            };
            for (scheme, table, want) in [("dark", &dark, want_dark), ("light", &light, want_light)]
            {
                match table.get(*token) {
                    None => divergences.push(format!(
                        "{}: {scheme} scheme declares no `{token}` (role `{role}`)",
                        surface.file
                    )),
                    Some(got) if got != want => divergences.push(format!(
                        "{}: {scheme} `{token}` (role `{role}`) is {got}, \
                         the observatory says {want}",
                        surface.file
                    )),
                    Some(_) => {}
                }
            }
        }
    }

    assert!(
        divergences.is_empty(),
        "the web instrument surfaces have drifted from the observatory, \
         which is the single definition:\n  {}\n\nFix the derived file, or \
         change the observatory and let this test tell you what follows.",
        divergences.join("\n  ")
    );
}

/// The MCP OAuth landing page carries the instrument palette too, for the
/// tokens it uses.
#[test]
fn the_auth_landing_pages_use_the_instrument_palette() {
    let canon = canonical();
    let css = read("crates/stella-mcp/src/oauth/callback_page.css");
    let dark = declarations(between(&css, ":root{", "}"));
    let light = declarations(between(&css, "@media (prefers-color-scheme:light)", "}}"));

    for (role, want_dark, want_light) in &canon {
        // The page has no surfaces, no series and no --text-2-vs-warn
        // distinction to draw; it carries exactly what a two-state message
        // needs.
        if !matches!(
            *role,
            "ground" | "text" | "text-2" | "ok" | "bad" | "identity"
        ) {
            continue;
        }
        let token = format!("--{role}");
        assert_eq!(
            dark.get(&token),
            Some(want_dark),
            "auth landing page dark `{token}` disagrees with the observatory"
        );
        assert_eq!(
            light.get(&token),
            Some(want_light),
            "auth landing page light `{token}` disagrees with the observatory"
        );
    }
}

/// The four surfaces carry the Instrument system's craft layer, not only its
/// colours.
///
/// The palette contract above was written first and caught nothing when the
/// export dashboard was reverted, because a revert does not stop at the hexes:
/// #2606 restored the pre-design-system stylesheet wholesale and took the mono
/// stack, the square corners and the wordmark with it. Every one of those is a
/// visible property of the shipped page and none of them was anybody's
/// contract, so the only thing that failed was a marker lookup — and it failed
/// with `marker not found`, which reads as a broken test rather than a
/// reverted design.
///
/// So the properties are asserted directly. Each is one a reader can see, each
/// has actually regressed, and each is spelled per-surface because the four
/// files legitimately say the same thing four ways.
#[test]
fn every_web_surface_carries_the_instrument_scale() {
    // `(file, how this file names the face, how it pins the corner)`.
    let surfaces: [(&str, &str, &str); 4] = [
        (
            "crates/stella-observatory/src/assets/index.html",
            "\"JetBrains Mono\"",
            "--radius:0",
        ),
        (
            "crates/stella-cli/src/export.rs",
            "\"JetBrains Mono\"",
            "--radius: 0",
        ),
        (
            "arenabench/ui/app/globals.css",
            "\"JetBrains Mono\"",
            "--radius-sm: 0",
        ),
        (
            "arenabench/web/app/globals.css",
            "\"JetBrains Mono\"",
            "border-radius: 0",
        ),
    ];

    let mut missing = Vec::new();
    for (file, face, corner) in surfaces {
        let text = read(file);
        if !text.contains(face) {
            missing.push(format!(
                "{file}: does not name the brand's mono face. One face is the \
                 whole type system, and this page is a measurement — a \
                 proportional digit in a column of costs is a defect."
            ));
        }
        if !text.contains(corner) {
            missing.push(format!(
                "{file}: does not pin `{corner}`. A rounded corner says \
                 \"surface\" where a hairline says \"boundary\", and these \
                 pages are all boundaries."
            ));
        }
        // Figures are measurements and may not change width between renders.
        // In a mono face this is already true; the declaration is what keeps
        // it true when the stack degrades to the platform fallback, which the
        // Observatory and the export both do by design (they may not fetch a
        // font).
        if !text.contains("tabular-nums") {
            missing.push(format!(
                "{file}: never declares `tabular-nums`, so a figure may reflow \
                 between renders once the mono face is unavailable."
            ));
        }
        // A stated preference for less motion wins on every surface. All four
        // are pages people watch while a run is in flight.
        if !text.contains("prefers-reduced-motion") {
            missing.push(format!("{file}: does not honour `prefers-reduced-motion`."));
        }
    }

    assert!(
        missing.is_empty(),
        "the web instrument surfaces have lost part of the design system:\n  {}",
        missing.join("\n  ")
    );
}

/// `text` with every comment blanked out, newlines preserved.
///
/// Blanked rather than removed so a reported line number still points at the
/// line a reader will open. Both comment syntaxes are covered because the four
/// surfaces are two HTML/CSS files, one CSS-in-Rust file, and one Tailwind
/// stylesheet: `/* … */` spans the CSS in all four, and `//` catches Rust doc
/// comments in `export.rs`.
///
/// This exists because these files DOCUMENT the rules they follow. The
/// Observatory explains its square corners in a comment that spells
/// `border-radius:var(--radius)`, and a scanner reading raw text reports that
/// sentence as a violation of the rule it is describing. A guard that fires on
/// its own documentation trains a reader to ignore it.
fn without_comments(text: &str) -> String {
    let bytes: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < bytes.len() {
        let two: String = bytes[index..(index + 2).min(bytes.len())].iter().collect();
        if two == "/*" {
            while index < bytes.len() {
                let closing: String = bytes[index..(index + 2).min(bytes.len())].iter().collect();
                if closing == "*/" {
                    out.push_str("  ");
                    index += 2;
                    break;
                }
                out.push(if bytes[index] == '\n' { '\n' } else { ' ' });
                index += 1;
            }
        } else if two == "//" {
            while index < bytes.len() && bytes[index] != '\n' {
                out.push(' ');
                index += 1;
            }
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    out
}

/// No surface ships a rounded corner.
///
/// Separate from the test above because that one asks whether the *decision*
/// is recorded (`--radius: 0` is present) and this one asks whether it is
/// *honoured*. #2606 shipped both at once: the token was gone and 8px, 6px,
/// 3px and 2px corners were back, and a test that only checked for the token
/// would have passed on a page with the token and a stray `border-radius: 6px`
/// underneath it.
#[test]
fn no_web_surface_ships_a_rounded_corner() {
    let mut rounded = Vec::new();
    for file in [
        "crates/stella-observatory/src/assets/index.html",
        "crates/stella-cli/src/export.rs",
        "arenabench/ui/app/globals.css",
        "arenabench/web/app/globals.css",
    ] {
        let text = without_comments(&read(file));
        for (index, _) in text.match_indices("border-radius") {
            let rest = &text[index..];
            let Some(colon) = rest.find(':') else {
                continue;
            };
            let value = rest[colon + 1..]
                .split([';', '\n', '}'])
                .next()
                .unwrap_or("")
                .trim();
            // Zero in any spelling, or a radius token — those are pinned to 0
            // by the test above, so a rule that defers to one is honest. The
            // per-corner form has to be checked part by part: the one rule in
            // the tree that uses it is `0 var(--radius-sm) var(--radius-sm) 0`,
            // which is square in all four corners and which a whole-value
            // comparison reads as rounded.
            let square = value
                .split_whitespace()
                .all(|part| part == "0" || part.starts_with("var(--radius"));
            if !square {
                let line = text[..index].matches('\n').count() + 1;
                rounded.push(format!("{file}:{line}: `border-radius: {value}`"));
            }
        }
    }

    assert!(
        rounded.is_empty(),
        "a rounded corner is a soft edge on an instrument:\n  {}",
        rounded.join("\n  ")
    );
}

/// Every custom property the Observatory references is one it defines.
///
/// The Observatory is the single definition, and it is also the file most
/// likely to grow a reference to a token that was renamed somewhere else. A
/// `var(--x)` with no `--x` paints nothing at all — the rule does not fall
/// back, it silently drops — and neither rustc nor any test that checks the
/// palette block for expected NAMES can see it. Enumerating the references
/// rather than listing the expected names is what makes this catch the next
/// rename too; `export/tests.rs` carries the same check for the same reason,
/// after a rename there shipped three elements painted with nothing.
#[test]
fn the_observatory_defines_every_token_it_references() {
    let raw = read("crates/stella-observatory/src/assets/index.html");
    // Definitions are looked up in the raw text (a token may legitimately be
    // introduced beside a comment), but references are read from the
    // comment-free text — see `without_comments`.
    let html = without_comments(&raw);

    // A name is ASCII letters, digits and dashes, and nothing else. Belt and
    // braces with the comment stripping above: a `var(--…)` written in prose
    // outside a comment — in a string, or in the page's own help text — would
    // otherwise be read as a custom property named `--…` and reported as
    // undefined. A predicate meeting typographic punctuation in prose it was
    // only meant to read code through is a recurring shape in this repo.
    let mut referenced: Vec<String> = Vec::new();
    let mut rest = html.as_str();
    while let Some(start) = rest.find("var(--") {
        rest = &rest[start + 4..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        // `var(--x` must be followed by `)` or `,` (a fallback) to be a real
        // reference; anything else is prose that happens to start the same way.
        let next = rest[name.len()..].chars().next();
        if name.len() > 2 && matches!(next, Some(')') | Some(',')) {
            referenced.push(name);
        }
    }
    referenced.sort();
    referenced.dedup();

    assert!(
        !referenced.is_empty(),
        "the Observatory uses custom properties; this test read none, so it \
         is measuring the wrong thing"
    );

    let undefined: Vec<&String> = referenced
        .iter()
        .filter(|name| !html.contains(&format!("{name}:")))
        .collect();

    assert!(
        undefined.is_empty(),
        "the Observatory references custom properties it never defines, so \
         those rules paint nothing: {undefined:?}"
    );
}

/// The identity hue is the brand's own value, not one mixed at the call site.
///
/// The rule permits the brand hue on identity; it does not permit *a* cyan.
/// Both values have to come from `docs/brand/css/tokens.css`, or the system
/// grows a fourth accent the way it grew a fourth look. The marketing mock the
/// bronze system was drawn from had already invented `#C99A22`, which was
/// 1.07:1 against `--warn` — indistinguishable from it, and the reason the
/// shipped value was not that.
///
/// The token family is `--stella-brand-*` since the v3.0 ion recolour; it was
/// `--stella-gold-*` before, which is why this test reads role names rather
/// than hue names now.
#[test]
fn identity_comes_from_the_brand_ramp() {
    let brand = read("docs/brand/css/tokens.css");
    let ramp = declarations(&brand);

    let ion = ramp
        .get("--stella-brand-500")
        .expect("brand tokens define --stella-brand-500");
    let deep = ramp
        .get("--stella-brand-800")
        .expect("brand tokens define --stella-brand-800");

    let observatory = read("crates/stella-observatory/src/assets/index.html");
    let dark = declarations(between(&observatory, "BEGIN palette", "END palette"));
    let light = declarations(between(&observatory, r#":root[data-theme="light"]"#, "\n}"));

    assert_eq!(
        dark.get("--identity"),
        Some(ion),
        "dark identity must be the brand's Ion (--stella-brand-500)"
    );
    assert_eq!(
        light.get("--identity"),
        Some(deep),
        "light identity must be --stella-brand-800. NOT --stella-brand-deep, \
         which is documented for small brand text on light surfaces: identity \
         has to clear contrast on --surface, not only on the page ground, and \
         brand-800 measures 4.93:1 there and 5.20:1 as the identity fill pair \
         (#2591)."
    );
}
