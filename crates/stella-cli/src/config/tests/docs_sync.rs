use super::*;

/// The docs' provider cards must not drift from [`PROVIDERS`].
///
/// `website/src/components/provider-catalog.ts` holds one typed record per
/// provider, rendered as the card grid on both the API Providers index and the
/// getting-started walkthrough. Before it existed the same facts were typed
/// into a markdown table on each page, and they had already drifted: Bedrock's
/// "default model" cell read `Claude via Converse` — a wire dialect, not a
/// slug. Consolidating the two copies into one record removed the drift
/// *between the pages*; it did nothing about drift from this file, which is
/// the copy that actually decides what the binary does.
///
/// So: `id`, `env`, and `defaultModel` are pinned here. A renamed env var or a
/// bumped default model now fails a test instead of silently sending readers
/// to a variable Stella stopped reading.
///
/// `dialect` and `blurb` are deliberately NOT pinned — those are prose written
/// for a human ("Anthropic Messages", "OpenAI-compatible"), not the
/// kebab-case `Dialect` wire value, and asserting on them would force the
/// docs to speak the enum's language rather than the reader's.
#[test]
fn provider_cards_match_the_registry() {
    let Some(source) = read_provider_catalog() else {
        return;
    };
    let cards = parse_provider_catalog(&source);
    assert!(
        cards.len() >= PROVIDERS.len(),
        "parsed only {} cards from provider-catalog.ts — the parser has \
         probably gone stale against the file's shape",
        cards.len()
    );

    for provider in PROVIDERS {
        let card = cards
            .iter()
            .find(|c| c.id == provider.id)
            .unwrap_or_else(|| {
                panic!(
                    "provider `{}` has no card in provider-catalog.ts — every \
                     supported provider must appear in the docs grid",
                    provider.id
                )
            });

        assert_eq!(
            card.default_model, provider.default_model,
            "the docs card for `{}` says its default model is `{}`, but \
             PROVIDERS says `{}`",
            provider.id, card.default_model, provider.default_model
        );

        let (primary, aliases) = split_env(&card.env);
        assert_eq!(
            primary, provider.env_var,
            "the docs card for `{}` names `{primary}` as the primary \
             credential variable, but PROVIDERS reads `{}`",
            provider.id, provider.env_var
        );
        assert_eq!(
            aliases, provider.env_var_aliases,
            "the docs card for `{}` lists aliases {aliases:?}, but PROVIDERS \
             has {:?}",
            provider.id, provider.env_var_aliases
        );

        // The link must reach that provider's documentation. Deliberately NOT
        // pinned to one URL shape: the docs moved from ten per-provider pages
        // (`/docs/api-providers/zai`) to one consolidated page with anchors
        // (`/docs/api-providers#zai`), and re-encoding the site's heading slugs
        // here would make every editorial retitle a failing test in the binary
        // — asserting on the website's structure rather than on the fact this
        // file is the authority for. What still cannot drift is what actually
        // breaks a reader: a card pointing outside the API-providers docs, or
        // at the bare index with no anchor to land on.
        assert!(
            card.href.starts_with("/docs/api-providers"),
            "the docs card for `{}` links to `{}`, which is not in the \
             API-providers docs",
            provider.id,
            card.href
        );
        let lands_somewhere = card.href.len() > "/docs/api-providers".len()
            && !card.href.ends_with('#')
            && !card.href.ends_with('/');
        assert!(
            lands_somewhere,
            "the docs card for `{}` links to `{}` — the bare index, with \
             nothing identifying this provider",
            provider.id, card.href
        );
    }

    // …and two providers must never share a destination, which is how a
    // copy-pasted card silently sends readers to the wrong provider's setup.
    let mut hrefs: Vec<&str> = PROVIDERS
        .iter()
        .filter_map(|p| cards.iter().find(|c| c.id == p.id))
        .map(|c| c.href.as_str())
        .collect();
    let total = hrefs.len();
    hrefs.sort_unstable();
    hrefs.dedup();
    assert_eq!(
        hrefs.len(),
        total,
        "two provider cards link to the same page — one of them sends readers \
         to another provider's setup instructions"
    );

    // The reverse direction: a card for a provider that no longer exists sends
    // readers to a page for something they cannot select. `local` is the one
    // legitimate extra — a pseudo-provider that is deliberately absent from
    // PROVIDERS so auto-detection can never pick it.
    for card in &cards {
        assert!(
            PROVIDERS.iter().any(|p| p.id == card.id) || card.id == LOCAL_PROVIDER.id,
            "provider-catalog.ts documents `{}`, which is not a provider \
             Stella supports",
            card.id
        );
    }
}

/// A card's pinned fields, as written in the catalog record.
struct ProviderCard {
    id: String,
    href: String,
    env: String,
    default_model: String,
}

/// The docs tree, or `None` when it isn't checked out — same posture as
/// `stella-tools/tests/docs_in_sync.rs`: a repo-hygiene gate, not a runtime
/// invariant. Note the granularity — a missing *tree* skips, but a missing
/// file inside a present tree panics, so renaming the module cannot silently
/// disable this test.
///
/// The records used to live in `provider-cards.tsx` beside the rendering.
/// #4588 split them into this plain `.ts` module so the markdown export path
/// (`src/lib/page-markdown.ts`, exercised by `node --test`, which strips types
/// but cannot parse JSX) could read them too. The card component now only
/// imports the catalog, so this test kept parsing a file with no records left
/// in it and turned `main` red on a zero-card count.
fn read_provider_catalog() -> Option<String> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?;
    if !root.join("website").is_dir() {
        return None;
    }
    // One literal, spelled in full, because `scripts/check-website-inputs.sh`
    // finds a website input by grepping the Rust sources for it. A path built
    // from two `join` calls is invisible to that grep, so the guard would be
    // unable to see this read even though the file it reads is declared (#4662).
    let path = root.join("website/src/components/provider-catalog.ts");
    Some(
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is missing or unreadable: {e}", path.display())),
    )
}

/// Every `{ id: "…", …, env: "…", defaultModel: "…" }` record inside the
/// `PROVIDER_CATALOG` array.
///
/// Anchored on the `export const` declaration rather than on the bare name,
/// because the bare name also matches an `import { PROVIDER_CATALOG }` line —
/// which is exactly what the card component became when #4588 moved the
/// records out. Matching the import left the parser scanning a file with no
/// records and reporting zero rather than failing on the file it was handed.
fn parse_provider_catalog(source: &str) -> Vec<ProviderCard> {
    let start = source
        .find("export const PROVIDER_CATALOG")
        .expect("provider-catalog.ts no longer declares `export const PROVIDER_CATALOG`");
    let body = &source[start..];

    let mut out = vec![];
    for (offset, _) in body.match_indices("id: \"") {
        // Each record runs to the start of the next one; the last runs to the
        // end. Bounding the field search this way keeps a missing field in one
        // record from silently picking up the next record's value.
        let rest = &body[offset..];
        let end = rest[1..].find("id: \"").map_or(rest.len(), |next| next + 1);
        let record = &rest[..end];

        let Some(id) = ts_field(record, "id") else {
            continue;
        };
        out.push(ProviderCard {
            href: ts_field(record, "href").unwrap_or_else(|| panic!("the `{id}` card has no href")),
            env: ts_field(record, "env").unwrap_or_else(|| panic!("the `{id}` card has no env")),
            default_model: ts_field(record, "defaultModel")
                .unwrap_or_else(|| panic!("the `{id}` card has no defaultModel")),
            id,
        });
    }
    out
}

/// The value of a `key: "value"` field in a TypeScript object literal.
fn ts_field(record: &str, key: &str) -> Option<String> {
    let needle = format!("{key}: \"");
    let start = record.find(&needle)? + needle.len();
    let rest = &record[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Split a card's `env` string into its primary variable and its aliases.
///
/// The docs render aliases in parentheses — `GEMINI_API_KEY (GOOGLE_API_KEY)`
/// — because that is how a reader wants to see "this one, or that one".
fn split_env(env: &str) -> (&str, Vec<&str>) {
    match env.split_once('(') {
        Some((primary, aliases)) => (
            primary.trim(),
            aliases
                .trim_end_matches(')')
                .split(',')
                .map(str::trim)
                .filter(|a| !a.is_empty())
                .collect(),
        ),
        None => (env.trim(), vec![]),
    }
}
