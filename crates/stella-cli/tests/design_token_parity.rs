//! The web instrument surfaces carry one palette, and this test is what makes
//! that true rather than merely intended.
//!
//! Four files describe the same design system in three vocabularies:
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

/// The two loopback auth landing pages are one stylesheet, kept in two places.
///
/// `stella-mcp` (OAuth) and `stella-tools` (tracker auth) each run a one-shot
/// listener that serves a two-state page, and they share no dependency to put
/// a stylesheet behind. Copying it is the right call for six values; letting
/// the copies drift is not, and "keep these two files identical" written in a
/// comment is the same non-defence the observatory/arenabench contract was
/// before this file existed.
#[test]
fn the_two_auth_landing_pages_share_one_stylesheet() {
    let mcp = read("crates/stella-mcp/src/oauth/callback_page.css");
    let tools = read("crates/stella-tools/src/tracker_auth_page.css");
    assert_eq!(
        mcp, tools,
        "crates/stella-mcp/src/oauth/callback_page.css and \
         crates/stella-tools/src/tracker_auth_page.css must stay byte-identical \
         — edit one and copy it over the other"
    );
}

/// Those pages carry the instrument palette too, for the tokens they use.
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

/// Identity gold is the brand's own value, not one mixed at the call site.
///
/// The rule permits gold on identity; it does not permit *a* gold. Both values
/// have to come from `docs/brand/css/tokens.css`, or the system grows a fourth
/// gold the way it grew a fourth look. The marketing mock this system was
/// drawn from had already invented `#C99A22`, which is 1.07:1 against `--warn`
/// — indistinguishable from it, and the reason the shipped value is not that.
#[test]
fn identity_gold_comes_from_the_brand_ramp() {
    let brand = read("docs/brand/css/tokens.css");
    let ramp = declarations(&brand);

    let phosphor = ramp
        .get("--stella-gold-500")
        .expect("brand tokens define --stella-gold-500");
    let deep = ramp
        .get("--stella-gold-800")
        .expect("brand tokens define --stella-gold-800");

    let observatory = read("crates/stella-observatory/src/assets/index.html");
    let dark = declarations(between(&observatory, "BEGIN palette", "END palette"));
    let light = declarations(between(&observatory, r#":root[data-theme="light"]"#, "\n}"));

    assert_eq!(
        dark.get("--identity"),
        Some(phosphor),
        "dark identity must be the brand's Phosphor Gold (--stella-gold-500)"
    );
    assert_eq!(
        light.get("--identity"),
        Some(deep),
        "light identity must be --stella-gold-800. NOT --stella-gold-deep \
         (#a37200): that token is documented for small gold text on light \
         surfaces and measures 4.05:1 there, under WCAG AA (#2591)."
    );
}
