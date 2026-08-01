//! Witness for #450: the docs cannot silently drift from the tool registry.
//!
//! The counts in `agent-tools/index.mdx` ("N always-on tools, up to M native")
//! used to be hand-maintained integers. Two parallel PRs each bumping them off
//! the same base merge without a conflict marker and land a
//! plausible-but-wrong number — and unlike the registry's own pins, nothing
//! caught it: the docs just drifted. (They had. Before this test, the
//! read-only partition prose in `index.mdx` was missing `diagnostics` and
//! `repo_diff`.)
//!
//! Every assertion here derives its expectation from
//! [`stella_tools::catalog`], the one place a tool is declared. Adding a tool
//! without documenting it — or documenting it with the wrong access marker —
//! fails this test by name.

use stella_tools::catalog::{self, Availability};

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The docs live in a sibling crate, so resolve up out of this one.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("stella-tools has a parent directory")
        .to_path_buf()
}

/// The docs tree, or `None` when it isn't present — a packaged/vendored
/// `stella-tools` ships without its sibling crates, and this suite is a
/// repo-hygiene gate, not a runtime invariant.
///
/// Note the granularity: a *missing tree* skips, but a missing **file inside
/// a present tree** still fails. Renaming `index.mdx` must not silently
/// disable the gate.
fn docs_root() -> Option<PathBuf> {
    let dir = repo_root().join("website");
    dir.is_dir().then_some(dir)
}

fn read_doc(relative: &str) -> Option<String> {
    let path = docs_root()?.join(relative);
    Some(
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("{} is missing or unreadable: {e}", path.display())),
    )
}

/// Every `` `name` `` span in `text`, in order, with the backticks stripped.
fn backticked(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else { break };
        out.push(after[..close].to_string());
        rest = &after[close + 1..];
    }
    out
}

/// The tool names the docs enumerate in a prose sentence — everything
/// backticked between `marker` and the end of that line.
fn names_in_sentence(text: &str, marker: &str) -> BTreeSet<String> {
    let start = text
        .find(marker)
        .unwrap_or_else(|| panic!("the docs no longer contain the marker {marker:?}"));
    let line = text[start..].lines().next().expect("a marker has a line");
    backticked(line).into_iter().collect()
}

/// The integer written as `{before}N{after}`.
///
/// Both sides are required so the needle can't drift onto an unrelated
/// sentence: `"These "` alone would happily match new prose added above the
/// catalog and then fail with a baffling message. Scans every occurrence and
/// takes the first that satisfies the whole shape.
fn count_between(text: &str, before: &str, after: &str) -> usize {
    for (offset, _) in text.match_indices(before) {
        let rest = &text[offset + before.len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() || !rest[digits.len()..].starts_with(after) {
            continue;
        }
        return digits.parse().expect("a run of ASCII digits parses");
    }
    panic!("the docs no longer contain the phrase {before:?}<number>{after:?}")
}

/// The opening tag starting at the front of `text`, i.e. everything before the
/// first `>` that is not inside a double-quoted attribute value.
///
/// The quote tracking is not pedantry: a card's `when=` prose is free text and
/// may legitimately contain a `>`. Stopping at the first raw `>` would truncate
/// the tag mid-attribute and lose the marker that follows it.
fn opening_tag(text: &str) -> &str {
    let mut in_quotes = false;
    for (i, ch) in text.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '>' if !in_quotes => return &text[..i],
            _ => {}
        }
    }
    panic!("a <ToolCard element is never closed with `>`: {text:.80}")
}

/// The value of a double-quoted JSX attribute in an opening tag.
fn attr(tag: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Every `<ToolCard name="…" access="…">` in `text`, paired with whether its
/// access marker says read-only.
///
/// The catalog used to be a stack of markdown tables and this parser split on
/// `|`. A five-column table is a desktop-only artifact — on a phone it either
/// overflows the viewport or crushes every cell to two words — so the catalog
/// is now a grid of `<ToolCard>` elements. The gate is unchanged in substance:
/// the tool *name* and its *access marker* still come out of the doc and still
/// get compared against `catalog.rs`. Only the syntax being read moved.
///
/// Every failure mode here is a panic rather than a skip. A silently-ignored
/// card would drop a tool out of the set-equality checks below, turning real
/// drift into a vacuous pass — which is the exact class of bug this file
/// exists to prevent.
fn documented_tools(text: &str) -> Vec<(String, bool)> {
    let mut out = vec![];
    for (offset, _) in text.match_indices("<ToolCard") {
        let tag = opening_tag(&text[offset..]);
        let name = attr(tag, "name")
            .unwrap_or_else(|| panic!("a <ToolCard is missing its `name` attribute: {tag}"));
        let access = attr(tag, "access")
            .unwrap_or_else(|| panic!("<ToolCard name=\"{name}\"> is missing `access`"));
        let read_only = match access.as_str() {
            "read" => true,
            "mutating" => false,
            other => panic!(
                "<ToolCard name=\"{name}\"> has access=\"{other}\" — expected \
                 exactly `read` or `mutating`"
            ),
        };
        out.push((name, read_only));
    }
    out
}

const INDEX: &str = "content/docs/agent-tools/index.mdx";
const PERMISSIONS: &str = "content/docs/agent-tools/permissions.mdx";

/// The headline counts are derived, not typed. This is the exact failure the
/// #336 wave-1 merge produced: four PRs, each +1, landing as +1.
#[test]
fn docs_counts_match_the_catalog() {
    let Some(index) = read_doc(INDEX) else { return };

    let always_on = catalog::always_on().len();
    let native = catalog::native().len();

    assert_eq!(
        count_between(&index, "These ", " tools register in every session"),
        always_on,
        "the \"These N tools register in every session\" count is stale — \
         catalog::always_on() has {always_on}"
    );
    assert_eq!(
        count_between(&index, "All told: ", " always-on tools"),
        always_on,
        "the \"All told: N always-on tools\" count is stale — \
         catalog::always_on() has {always_on}"
    );
    assert_eq!(
        count_between(&index, "up to ", " native tools"),
        native,
        "the \"up to M native tools\" ceiling is stale — catalog::native() \
         has {native}"
    );
}

/// Adding a tool to the registry without adding its docs row fails here, by
/// name — the second failure class from the #336 wave-1 merge.
#[test]
fn every_catalog_tool_is_documented_and_vice_versa() {
    let Some(index) = read_doc(INDEX) else { return };

    let documented: BTreeSet<String> = documented_tools(&index)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let declared: BTreeSet<String> = catalog::CATALOG
        .iter()
        .map(|entry| entry.name.to_string())
        .collect();

    let undocumented: Vec<&String> = declared.difference(&documented).collect();
    assert!(
        undocumented.is_empty(),
        "declared in catalog.rs but absent from {INDEX}: {undocumented:?}"
    );
    let unknown: Vec<&String> = documented.difference(&declared).collect();
    assert!(
        unknown.is_empty(),
        "documented in {INDEX} but not declared in catalog.rs: {unknown:?}"
    );
}

/// The docs' per-tool Read-only/Mutating marker is the same partition the
/// engine speculates on. A row that disagrees tells a reader a mutating tool
/// is safe to run.
#[test]
fn documented_access_markers_match_the_catalog() {
    let Some(index) = read_doc(INDEX) else { return };

    for (name, documented_read_only) in documented_tools(&index) {
        let entry = catalog::get(&name)
            .unwrap_or_else(|| panic!("`{name}` is documented but not declared in catalog.rs"));
        assert_eq!(
            documented_read_only,
            entry.read_only,
            "{INDEX} marks `{name}` as {} but the catalog says {}",
            if documented_read_only {
                "Read-only"
            } else {
                "Mutating"
            },
            if entry.read_only {
                "read_only: true"
            } else {
                "read_only: false"
            },
        );
    }
}

/// The prose partition in `index.mdx` covers the whole surface, session layer
/// included. It was missing `diagnostics` and `repo_diff` when this test was
/// written — exactly the silent doc drift #450 is about.
#[test]
fn index_read_only_prose_matches_the_catalog() {
    let Some(index) = read_doc(INDEX) else { return };

    let documented = names_in_sentence(&index, "The read-only partition is exactly:");
    let expected: BTreeSet<String> = catalog::read_only()
        .into_iter()
        .map(str::to_string)
        .collect();
    assert_eq!(
        documented, expected,
        "the read-only prose in {INDEX} disagrees with catalog::read_only()"
    );
}

/// The speculated set is the catalog's second partition (#923): read-only
/// AND speculation-safe. Prose drift here is worse than for read_only —
/// a name wrongly listed tells a reader their metered tool may be billed
/// twice per step, and a name wrongly missing hides real overlap.
#[test]
fn index_speculation_safe_prose_matches_the_catalog() {
    let Some(index) = read_doc(INDEX) else { return };

    let documented = names_in_sentence(&index, "The speculation-safe subset is exactly:");
    let expected: BTreeSet<String> = catalog::speculation_safe()
        .into_iter()
        .map(str::to_string)
        .collect();
    assert_eq!(
        documented, expected,
        "the speculation-safe prose in {INDEX} disagrees with catalog::speculation_safe()"
    );
}

/// `permissions.mdx` scopes its list to the built-in catalog — the native
/// registry, without the CLI's session layer.
#[test]
fn permissions_read_only_prose_matches_the_native_catalog() {
    let Some(permissions) = read_doc(PERMISSIONS) else {
        return;
    };

    let documented = names_in_sentence(&permissions, "The authoritative read-only set");
    let expected: BTreeSet<String> = catalog::CATALOG
        .iter()
        .filter(|entry| entry.read_only && entry.availability.is_native())
        .map(|entry| entry.name.to_string())
        .collect();
    assert_eq!(
        documented, expected,
        "the read-only prose in {PERMISSIONS} disagrees with the native \
         read-only catalog"
    );
}

/// The session layer is counted in prose as a word, so it needs its own pin.
#[test]
fn session_tool_count_prose_matches_the_catalog() {
    let Some(index) = read_doc(INDEX) else { return };

    let session = catalog::names_where(|a| a == Availability::Session).len();
    let spelled = match session {
        6 => "**six**",
        n => panic!(
            "the session-tool count changed to {n} — update the prose in \
             {INDEX} (\"layers **six** more tools\", \"plus the six session \
             tools\") and this test's spelling table"
        ),
    };
    assert!(
        index.contains(&format!("layers {spelled} more tools")),
        "{INDEX} no longer spells the session-tool count as {spelled}"
    );
    assert!(
        index.contains("plus the six session tools"),
        "{INDEX} no longer spells the session-tool count in the summary line"
    );
}

/// The parser is what makes the rest of this file meaningful: if it silently
/// matched nothing, every set-equality above would pass vacuously.
#[test]
fn the_card_parser_actually_finds_tools() {
    let sample = "\
Some prose about `read_file` that must not be mistaken for a card.

<CardGrid size=\"md\">
  <ToolCard name=\"read_file\" access=\"read\">
    Read a file, with 1-based line numbers.
  </ToolCard>
  <ToolCard name=\"write_file\" access=\"mutating\">
    Create a file or overwrite an existing one.
  </ToolCard>
  <ToolCard name=\"web_search\" access=\"read\" when=\"a search key is set (limit > 0)\">
    Search the web.
  </ToolCard>
</CardGrid>
";
    assert_eq!(
        documented_tools(sample),
        vec![
            ("read_file".to_string(), true),
            ("write_file".to_string(), false),
            // The `>` inside `when=` must not truncate the tag before `access`.
            ("web_search".to_string(), true),
        ]
    );
    assert_eq!(backticked("a `b` c `d`"), vec!["b", "d"]);
    // The decoy `up to 9 native things` must be skipped for the real phrase.
    let prose = "up to 9 native things. All told: 42 always-on tools, up to 58 native tools.";
    assert_eq!(count_between(prose, "up to ", " native tools"), 58);
    assert_eq!(count_between(prose, "All told: ", " always-on tools"), 42);
}

/// An access marker the component itself would reject must fail loudly here
/// rather than silently dropping the tool from the set-equality checks.
#[test]
#[should_panic(expected = "expected exactly `read` or `mutating`")]
fn an_unknown_access_marker_is_a_hard_failure() {
    documented_tools("<ToolCard name=\"read_file\" access=\"Read-only\">x</ToolCard>");
}
