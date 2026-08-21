//! Turning a ranked path into the block the reader gets — and the graph-name
//! strategy that ranks when no embedder does.
//!
//! # Why each facet is its own labelled line
//!
//! [`stella_core::search`] guarantees the facet *set* at depth `n + 1` is a
//! superset of the set at depth `n`. This module makes the same true of the
//! rendered *text*: every facet writes its own `label: …` line and no facet
//! rewrites another's, so the block at depth `n + 1` contains every line of
//! the block at depth `n` verbatim. That turns monotonicity from a convention
//! into something a test can check by line containment, which is what a depth
//! sweep needs to be interpretable — a rung that quietly reformatted a lower
//! rung's line would make two settings incomparable without failing anything.
//!
//! It costs a little redundancy (`symbols:` names them, `kinds:` types them)
//! and buys a checkable property. That trade is deliberate.
//!
//! # Why this is not a match list
//!
//! Signature, callers, callees and importers are the content here, and the
//! path is a header rather than the payload. A block of `file:line` matches
//! would be assemblable with `grep -n | head`; the ranking's *why* and the
//! graph structure are what no pipeline of shell commands produces.

use std::path::Path;

use stella_core::search::{Depth, Facet, facets_at};
use stella_graph::{CodeGraph, NeighborhoodSymbol};

use super::cache::{self, GatherCache};
use super::engine::Hit;
use super::names;

/// The file's neighborhood, from the session cache when the file's bytes are
/// unchanged since it was gathered, and from the graph otherwise.
///
/// `source` doubles as the cache key: `None` (an unreadable or non-UTF-8
/// file) has no identity to validate against, so it bypasses the cache
/// entirely and is gathered fresh — the conservative direction. A graph the
/// path is unknown to degrades to `None` exactly as the un-cached gather did,
/// and is not cached either: a miss is not a fact worth retaining.
fn gathered_neighborhood(
    cache: &mut GatherCache,
    graph: &CodeGraph,
    path: &str,
    source: Option<&str>,
) -> Option<stella_graph::FileNeighborhood> {
    let identity = source.map(cache::content_identity);
    if let Some(identity) = identity
        && let Some(neighborhood) = cache.lookup(path, &identity)
    {
        return Some(neighborhood);
    }
    let neighborhood = graph.file_neighborhood(Path::new(path)).ok()?;
    cache.gathered += 1;
    if let Some(identity) = identity {
        cache.store(path.to_string(), identity, neighborhood.clone());
    }
    Some(neighborhood)
}

/// Symbols named per hit. Past this the list stops being a summary.
const MAX_SYMBOLS: usize = 8;
/// Symbols carrying a signature, doc, callers or callees. Those facets cost
/// a graph query and a file read apiece, so they cover the leading few.
const MAX_DETAILED_SYMBOLS: usize = 3;
/// Import/importer/caller/callee edges listed per hit.
const MAX_EDGES: usize = 6;
/// Source lines of the leading symbol quoted at [`Facet::Body`].
const MAX_BODY_LINES: usize = 40;

/// Render one hit at `depth`.
///
/// Total: a missing graph, an unreadable file and a path the index does not
/// know all degrade to fewer lines, never to an error. A search that failed
/// to enrich a hit still found the hit, and losing the whole answer over a
/// detail line would be the worst possible trade.
pub fn render_hit(
    graph: Option<&CodeGraph>,
    root: &Path,
    hit: &Hit,
    depth: Depth,
    cache: &mut GatherCache,
) -> String {
    let facets = facets_at(depth);
    // `Facet::Path` is the header and is present at every depth by
    // construction, so it is written unconditionally rather than looked up.
    let mut block = format!("{}\n    why: {}", hit.path, hit.why);

    let Some(graph) = graph else {
        return block;
    };
    let source = std::fs::read_to_string(root.join(&hit.path)).ok();
    let Some(neighborhood) = gathered_neighborhood(cache, graph, &hit.path, source.as_deref())
    else {
        return block;
    };
    // The matched symbol leads the detailed facets when the ranking named
    // one: the body and signature the answer pays for must describe what the
    // query is about, not whatever sits first in the file.
    let focused: Option<&NeighborhoodSymbol> = hit.focus.as_ref().and_then(|focus| {
        neighborhood
            .symbols
            .iter()
            .find(|symbol| &symbol.name == focus)
    });
    let leading: Vec<&NeighborhoodSymbol> = focused
        .into_iter()
        .chain(
            neighborhood
                .symbols
                .iter()
                .filter(|symbol| focused.is_none_or(|kept| kept.name != symbol.name)),
        )
        .take(MAX_DETAILED_SYMBOLS)
        .collect();

    for facet in facets {
        let line =
            match facet {
                // Written above: it is the block's header, not a labelled line.
                Facet::Path => None,
                Facet::SymbolNames => list_line(
                    "symbols",
                    neighborhood
                        .symbols
                        .iter()
                        .take(MAX_SYMBOLS)
                        .map(|symbol| symbol.name.clone()),
                ),
                Facet::SymbolKinds => list_line(
                    "kinds",
                    neighborhood.symbols.iter().take(MAX_SYMBOLS).map(|symbol| {
                        format!("{} {}:{}", symbol.kind, symbol.name, symbol.start_line)
                    }),
                ),
                Facet::Imports => list_line(
                    "imports",
                    neighborhood.imports.iter().take(MAX_EDGES).cloned(),
                ),
                Facet::Importers => list_line(
                    "imported by",
                    neighborhood.importers.iter().take(MAX_EDGES).cloned(),
                ),
                Facet::Signature => list_line(
                    "signature",
                    leading
                        .iter()
                        .filter_map(|symbol| declaration_line(source.as_deref(), symbol)),
                ),
                Facet::DocComment => list_line(
                    "doc",
                    leading
                        .iter()
                        .filter_map(|symbol| doc_comment(source.as_deref(), symbol)),
                ),
                Facet::Callers => list_line(
                    "callers",
                    leading.iter().flat_map(|symbol| {
                        frame_labels(graph.callers(&symbol.name).unwrap_or_default())
                    }),
                ),
                // Callee lookup is name-based across the whole index, so a
                // common leading symbol (`new`, `default`) drags in every
                // same-named definition's call list. Only the frames about
                // THIS file describe this hit; the rest are noise about
                // other files wearing the same name.
                Facet::Callees => list_line(
                    "callees",
                    leading.iter().flat_map(|symbol| {
                        frame_labels(
                            graph
                                .callees(&symbol.name)
                                .unwrap_or_default()
                                .into_iter()
                                .filter(|frame| frame_is_about(frame, &hit.path))
                                .collect(),
                        )
                    }),
                ),
                Facet::Body => body_block(
                    graph,
                    source.as_deref(),
                    &hit.path,
                    leading.first().copied(),
                ),
            };
        if let Some(line) = line {
            block.push('\n');
            block.push_str(&line);
        }
    }
    block
}

/// `    label: a, b, c` — or nothing at all when there is nothing to say.
///
/// An empty facet writes no line rather than `label: (none)`: a rung that
/// always emits keeps the monotonicity property either way, and silence costs
/// the budget nothing.
fn list_line(label: &str, items: impl Iterator<Item = String>) -> Option<String> {
    let mut seen: Vec<String> = Vec::new();
    for item in items {
        let item = item.trim().to_string();
        if !item.is_empty() && !seen.contains(&item) {
            seen.push(item);
        }
        if seen.len() >= MAX_EDGES.max(MAX_SYMBOLS) {
            break;
        }
    }
    (!seen.is_empty()).then(|| format!("    {label}: {}", seen.join(", ")))
}

/// The frames' human labels — `fn name (path:line)`, already rendered by
/// `stella-graph` for citation (L-C4), so this never re-derives a citation.
///
/// `citation_label` first, `title` as the fallback: a caller frame's title
/// is the generic `caller of {name}`, which deduplicated to a single
/// site-less entry and said nothing — the located label is the citation,
/// exactly as L-C4 says.
fn frame_labels(frames: Vec<stella_graph::ContextFrame>) -> Vec<String> {
    frames
        .into_iter()
        .take(MAX_EDGES)
        .map(|frame| frame.citation_label.unwrap_or(frame.title))
        .collect()
}

/// Whether a frame describes `path` itself, by its file URI. Suffix-matched
/// on a `/` boundary because the URI is absolute and the hit path is
/// workspace-relative.
fn frame_is_about(frame: &stella_graph::ContextFrame, path: &str) -> bool {
    frame
        .uri
        .as_deref()
        .is_some_and(|uri| uri.ends_with(&format!("/{path}")))
}

/// The symbol's declaration line, as written in the file.
fn declaration_line(source: Option<&str>, symbol: &NeighborhoodSymbol) -> Option<String> {
    let line = line_at(source?, symbol.start_line)?;
    Some(line.trim().to_string())
}

/// The comment block immediately above the declaration, joined to one line.
///
/// Walks upward from the declaration over `///`, `//!`, `//`, `#` and `*`
/// openers, which covers every language the indexer parses without a
/// per-language table — a doc comment recognised loosely is a doc comment
/// recognised, and the cost of a false positive here is one quoted line.
pub fn doc_comment(source: Option<&str>, symbol: &NeighborhoodSymbol) -> Option<String> {
    let lines: Vec<&str> = source?.lines().collect();
    // `start_line` is 1-based, so the declaration sits at index
    // `start_line - 1` and the line above it at `start_line - 2`.
    let mut index = usize::try_from(symbol.start_line).ok()?.checked_sub(2)?;
    let mut collected: Vec<&str> = Vec::new();
    loop {
        let candidate = lines.get(index)?.trim();
        // A Rust attribute (`#[test]`, `#![allow]`) sits between a
        // declaration and its doc comment and is not prose: walk past it
        // without quoting it, so `#[must_use]` never opens a doc line.
        let is_attribute = candidate.starts_with("#[") || candidate.starts_with("#!");
        let is_comment = candidate.starts_with("///")
            || candidate.starts_with("//!")
            || candidate.starts_with("//")
            || candidate.starts_with('#')
            || candidate.starts_with('*')
            || candidate.starts_with("/*");
        if !is_attribute {
            if !is_comment {
                break;
            }
            collected.push(candidate.trim_start_matches(['/', '!', '#', '*', ' ']));
        }
        let Some(next) = index.checked_sub(1) else {
            break;
        };
        index = next;
    }
    collected.reverse();
    let joined = collected.join(" ");
    let joined = joined.trim();
    (!joined.is_empty()).then(|| joined.to_string())
}

/// The leading symbol's source span, quoted and line-numbered.
///
/// The span comes from the code graph (`definition_spans`), never from a
/// guessed offset.
fn body_block(
    graph: &CodeGraph,
    source: Option<&str>,
    path: &str,
    symbol: Option<&NeighborhoodSymbol>,
) -> Option<String> {
    let symbol = symbol?;
    let source = source?;
    let span = graph
        .definition_spans(&symbol.name)
        .ok()?
        .into_iter()
        .find(|span| span.path == path)?;
    let start = usize::try_from(span.start_line).ok()?.max(1);
    let end = usize::try_from(span.end_line)
        .ok()?
        .min(start.saturating_add(MAX_BODY_LINES).saturating_sub(1));
    let quoted: Vec<String> = (start..=end)
        .filter_map(|number| {
            line_at(source, u32::try_from(number).ok()?)
                .map(|text| format!("    {number:>5} | {text}"))
        })
        .collect();
    if quoted.is_empty() {
        return None;
    }
    let elided = u32::try_from(end).is_ok_and(|end| end < span.end_line);
    let mut block = format!("    body of `{}` ({path}:{start}-{end}):", symbol.name);
    for line in quoted {
        block.push('\n');
        block.push_str(&line);
    }
    if elided {
        block.push_str(&format!(
            "\n    … {} further line(s) in the file",
            span.end_line - u32::try_from(end).unwrap_or(span.end_line)
        ));
    }
    Some(block)
}

/// The 1-based `number`th line of `source`.
fn line_at(source: &str, number: u32) -> Option<&str> {
    let index = usize::try_from(number).ok()?.checked_sub(1)?;
    source.lines().nth(index)
}

/// Rust definition keywords a query may lead with before naming the symbol.
///
/// Ordered longest-first only for readability; [`symbol_terms`] strips
/// whichever ones lead, repeatedly, so `pub async fn` peels in three steps.
const DEFINITION_KEYWORDS: &[&str] = &[
    "macro_rules!",
    "pub(crate)",
    "pub(super)",
    "pub(self)",
    "unsafe",
    "static",
    "struct",
    "trait",
    "union",
    "async",
    "const",
    "enum",
    "impl",
    "type",
    "mod",
    "pub",
    "fn",
];

/// Whether `s` is a bare Rust identifier — the only shape the code graph can
/// be asked about, and the guard that keeps regex fragments out.
///
/// A query alternative like `rate.limit`, `429` or `PendingChunk\b` is a
/// pattern, not a name; looking one up would always miss, and letting it
/// through would cost a graph round-trip per junk term.
fn is_bare_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_alphabetic() || c == '_')
        && chars.all(|c| c.is_alphanumeric() || c == '_')
}

/// The symbol names a query is asking about, in the order it named them.
///
/// # Why a query is decomposed at all
///
/// [`exact_symbol_hits`] used to take the whole query as one name, so it
/// answered only for a query that was already exactly one bare identifier.
/// That is not the shape callers write. Measured over a real session, every
/// one of the agent's twenty-one code searches was an exact-identifier
/// lookup, and most named **several** symbols at once in regex alternation
/// (`warm_chunks_opened|embed_and_store_chunk_file|ChunkWarmOutcome`) or led
/// with the definition keyword (`pub fn store_chunk_vectors`). Every one of
/// them missed this rung entirely and fell through to embedding rank — the
/// instrument the doc above says is measurably wrong for this question —
/// even though each individual term was an exact graph fact. The session
/// used `rg` instead, twenty-one times out of twenty-one.
///
/// # Why these two decompositions and no others
///
/// `|` is unambiguous: it cannot occur in a Rust identifier and carries no
/// meaning in a natural-language question, so splitting on it reads an
/// explicit "any of these" rather than inferring an intent. The leading
/// keywords are unambiguous in the same way — a question does not open with
/// `pub struct`. Both preserve the rule the strategy exists for: **a
/// sentence is still not a symbol**, so an alternative that is not a bare
/// identifier after stripping is dropped rather than guessed at, and a
/// prose query decomposes to nothing and stays free.
///
/// Partial decomposition is deliberate: `429|retry|backoff` yields
/// `[retry, backoff]`. The junk term costs nothing and the two real names
/// are still facts.
pub fn symbol_terms(query: &str) -> Vec<&str> {
    let mut terms = Vec::new();
    for alternative in query.split('|') {
        let mut term = alternative.trim();
        // Peel leading keywords one at a time so `pub async fn name` reduces
        // the same way `fn name` does.
        while let Some((head, rest)) = term.split_once(char::is_whitespace) {
            if !DEFINITION_KEYWORDS.contains(&head) {
                break;
            }
            term = rest.trim_start();
        }
        if is_bare_identifier(term) && !terms.contains(&term) {
            terms.push(term);
        }
    }
    terms
}

/// The files that define a symbol the query names **exactly** — a lookup, not
/// a ranking (#3125).
///
/// Ranking is the wrong instrument for this question and measurably so: asked
/// for `ContextFrame`, a name with exactly one definition in its repository,
/// embedding rank returned that definition at **rank 5 of 139** because a
/// symbol name is a handful of tokens against whole files of prose. The graph
/// already holds the answer as a fact.
///
/// A sentence is not a symbol, so a query that names none is refused before
/// any lookup runs rather than after it returns nothing. That is the common
/// case, and it keeps this free for every search that is a question. Which
/// names a query does name is [`symbol_terms`]'s job — it reads an explicit
/// alternation and a leading definition keyword, and nothing else.
///
/// The matched symbol rides as the hit's focus, so the detailed facets the
/// renderer pays for describe the definition itself rather than whatever
/// happens to sit first in the file.
pub fn exact_symbol_hits(graph: &CodeGraph, query: &str, limit: usize) -> Vec<Hit> {
    let mut seen = std::collections::HashSet::new();
    let mut hits = Vec::new();
    // Term order, not graph order: the caller named these in a sequence and a
    // certainty found for the first should not be displaced by one found for
    // the third.
    for name in symbol_terms(query) {
        let Ok(spans) = graph.definition_spans(name) else {
            continue;
        };
        for span in spans {
            // `definition_spans` is already `WHERE s.name = ?`, so this only
            // guards the contract rather than filtering: an inexact hit here
            // would be a ranking wearing a certainty's label, which is the one
            // thing this strategy must never do.
            if span.name != name || !seen.insert(span.path.clone()) {
                continue;
            }
            hits.push(Hit {
                why: format!(
                    "EXACT name match — `{}` ({}) is DEFINED here at line {}. This is a code-graph \
                     fact, not a similarity score.",
                    span.name, span.kind, span.start_line
                ),
                path: span.path,
                focus: Some(span.name),
            });
            if hits.len() >= limit {
                return hits;
            }
        }
    }
    hits
}

/// Rank indexed files by NAME — the strategy for a workspace with an index
/// but no embedder.
///
/// This is the graph-facing half: gather every indexed file's path and symbol
/// names, hand them to [`names::rank`], and dress the result as hits. The
/// ranking model itself — stemming, word units, rarity and repetition
/// weighting, and why each exists — lives in [`names`], which is a pure
/// function a probe can measure without an index (#3138).
///
/// This is what a path glob cannot do: it searches *symbol* names. It is
/// still a name match, and every answer carrying it says so.
/// Deterministic: score descending, then path ascending.
///
/// Returns the ranked hits **and how many files matched at all**, so the
/// caller can disclose a cut list — `limit` files shown out of a larger match
/// set must never read as "only `limit` files matched".
pub fn name_hits(graph: &CodeGraph, query: &str, limit: usize) -> (Vec<Hit>, usize) {
    let terms = names::stems_of(query);
    if terms.is_empty() {
        return (Vec::new(), 0);
    }
    let Ok(files) = graph.all_files() else {
        return (Vec::new(), 0);
    };

    let corpus: Vec<names::IndexedNames> = files
        .into_iter()
        .map(|path| {
            let symbols = graph
                .file_neighborhood(Path::new(&path))
                .map(|neighborhood| {
                    neighborhood
                        .symbols
                        .iter()
                        .map(|symbol| symbol.name.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            names::IndexedNames { path, symbols }
        })
        .collect();

    let mut scored = names::rank(&corpus, query);
    let matched = scored.len();
    scored.truncate(limit);

    let hits = scored
        .into_iter()
        .map(|scored| Hit {
            why: format!(
                "matched {} of {} query term(s) as whole words in its path or symbol NAMES (not \
                 by meaning)",
                scored.matched_terms,
                terms.len()
            ),
            path: scored.path,
            focus: None,
        })
        .collect();
    (hits, matched)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single bare identifier is the shape that already worked, and it must
    /// keep decomposing to itself — the rung's whole contract rests on it.
    #[test]
    fn a_bare_identifier_decomposes_to_itself() {
        assert_eq!(symbol_terms("ContextFrame"), vec!["ContextFrame"]);
        assert_eq!(symbol_terms("  ContextFrame  "), vec!["ContextFrame"]);
    }

    /// A sentence is still not a symbol. This is the rule the decomposition
    /// had to preserve: a question must cost no graph round-trip at all.
    #[test]
    fn prose_still_names_no_symbol() {
        assert!(symbol_terms("where is the retry logic").is_empty());
        assert!(symbol_terms("how does compaction decide what to evict").is_empty());
        assert!(symbol_terms("").is_empty());
        assert!(symbol_terms("   ").is_empty());
    }

    /// The dominant real shape: several exact names in regex alternation.
    #[test]
    fn alternation_names_every_symbol_it_lists() {
        assert_eq!(
            symbol_terms("warm_chunks_opened|embed_and_store_chunk_file|ChunkWarmOutcome"),
            vec![
                "warm_chunks_opened",
                "embed_and_store_chunk_file",
                "ChunkWarmOutcome"
            ]
        );
        assert_eq!(
            symbol_terms("EMBED_BATCH|MAX_FILES_PER_CHUNK_PASS"),
            vec!["EMBED_BATCH", "MAX_FILES_PER_CHUNK_PASS"]
        );
    }

    /// The second real shape: the caller leads with the definition keyword.
    #[test]
    fn a_leading_definition_keyword_is_peeled() {
        assert_eq!(
            symbol_terms("pub fn store_chunk_vectors"),
            vec!["store_chunk_vectors"]
        );
        assert_eq!(symbol_terms("pub struct CodeGraph"), vec!["CodeGraph"]);
        assert_eq!(symbol_terms("impl CodeGraph"), vec!["CodeGraph"]);
        assert_eq!(
            symbol_terms("pub async fn warm_chunk_vectors"),
            vec!["warm_chunk_vectors"]
        );
        assert_eq!(
            symbol_terms("pub(crate) const RANK_CEILING"),
            vec!["RANK_CEILING"]
        );
    }

    /// Both shapes at once, which is how they actually arrive.
    #[test]
    fn keywords_are_peeled_from_every_alternative() {
        assert_eq!(
            symbol_terms("pub fn store_chunk_vectors|pub struct CodeGraph|impl CodeGraph"),
            vec!["store_chunk_vectors", "CodeGraph"],
            "the repeated type is named once"
        );
    }

    /// A regex fragment is a pattern, not a name — dropped, without taking
    /// the real names beside it down with it.
    #[test]
    fn pattern_fragments_are_dropped_not_guessed_at() {
        assert_eq!(
            symbol_terms("429|rate.limit|retry|backoff"),
            vec!["retry", "backoff"]
        );
        assert_eq!(
            symbol_terms(r"pub struct PendingChunk\b|pub struct ChunkVector"),
            vec!["ChunkVector"]
        );
    }

    /// The witness that the call site actually consults the decomposition: a
    /// two-symbol alternation returns BOTH definitions. Under the single-name
    /// lookup this replaces, the whole query was one candidate name, nothing
    /// in the graph was called `alpha_symbol|beta_symbol`, and the answer was
    /// empty — so the exact rung was skipped and the search fell through to
    /// embedding rank.
    #[test]
    fn an_alternation_query_returns_every_definition_it_names() {
        let workspace = tempfile::tempdir().expect("tempdir");
        for (path, body) in [
            ("src/alpha.rs", "pub fn alpha_symbol() -> u8 { 1 }\n"),
            ("src/beta.rs", "pub fn beta_symbol() -> u8 { 2 }\n"),
        ] {
            let file = workspace.path().join(path);
            std::fs::create_dir_all(file.parent().expect("a parent")).expect("mkdir");
            std::fs::write(&file, body).expect("write");
        }
        let root = workspace.path().canonicalize().expect("canonicalize");
        let graph = CodeGraph::open(&root, &root.join("codegraph.db")).expect("open");
        graph.index_all().expect("index");

        let hits = exact_symbol_hits(&graph, "alpha_symbol|beta_symbol", 10);

        let focuses: Vec<_> = hits.iter().filter_map(|h| h.focus.as_deref()).collect();
        assert!(
            focuses.contains(&"alpha_symbol") && focuses.contains(&"beta_symbol"),
            "both names in the alternation are code-graph facts, so both must be returned as \
             certainties — got {focuses:?}"
        );

        // And the single-name path is unchanged by the decomposition.
        let one = exact_symbol_hits(&graph, "alpha_symbol", 10);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].focus.as_deref(), Some("alpha_symbol"));
    }
}
