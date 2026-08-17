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

use super::engine::Hit;

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
pub fn render_hit(graph: Option<&CodeGraph>, root: &Path, hit: &Hit, depth: Depth) -> String {
    let facets = facets_at(depth);
    // `Facet::Path` is the header and is present at every depth by
    // construction, so it is written unconditionally rather than looked up.
    let mut block = format!("{}\n    why: {}", hit.path, hit.why);

    let Some(graph) = graph else {
        return block;
    };
    let source = std::fs::read_to_string(root.join(&hit.path)).ok();
    let Ok(neighborhood) = graph.file_neighborhood(Path::new(&hit.path)) else {
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

/// The files that define a symbol the query names **exactly** — a lookup, not
/// a ranking (#3125).
///
/// Ranking is the wrong instrument for this question and measurably so: asked
/// for `ContextFrame`, a name with exactly one definition in its repository,
/// embedding rank returned that definition at **rank 5 of 139** because a
/// symbol name is a handful of tokens against whole files of prose. The graph
/// already holds the answer as a fact.
///
/// A sentence is not a symbol, so a multi-word query is refused before the
/// query runs rather than after it returns nothing. That is the common case,
/// and it keeps this free for every search that is a question.
///
/// The matched symbol rides as the hit's focus, so the detailed facets the
/// renderer pays for describe the definition itself rather than whatever
/// happens to sit first in the file.
pub fn exact_symbol_hits(graph: &CodeGraph, query: &str, limit: usize) -> Vec<Hit> {
    let name = query.trim();
    if name.is_empty() || name.split_whitespace().count() != 1 {
        return Vec::new();
    }
    let Ok(spans) = graph.definition_spans(name) else {
        return Vec::new();
    };

    let mut seen = std::collections::HashSet::new();
    let mut hits = Vec::new();
    for span in spans {
        // `definition_spans` is already `WHERE s.name = ?`, so this only guards
        // the contract rather than filtering: an inexact hit here would be a
        // ranking wearing a certainty's label, which is the one thing this
        // strategy must never do.
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
            break;
        }
    }
    hits
}

/// Rank indexed files by how many query terms appear in their path or their
/// symbol names — the strategy for a workspace with an index but no embedder.
///
/// This is what a path glob cannot do: it searches *symbol* names. It is
/// still a name match, and every answer carrying it says so.
/// Deterministic: score descending, then path ascending.
///
/// Returns the ranked hits **and how many files matched at all**, so the
/// caller can disclose a cut list — `limit` files shown out of a larger match
/// set must never read as "only `limit` files matched".
pub fn name_hits(graph: &CodeGraph, query: &str, limit: usize) -> (Vec<Hit>, usize) {
    let terms = terms_of(query);
    if terms.is_empty() {
        return (Vec::new(), 0);
    }
    let Ok(files) = graph.all_files() else {
        return (Vec::new(), 0);
    };

    let mut scored: Vec<(usize, String)> = files
        .into_iter()
        .filter_map(|path| {
            let symbols = graph
                .file_neighborhood(Path::new(&path))
                .map(|neighborhood| {
                    neighborhood
                        .symbols
                        .iter()
                        .map(|symbol| symbol.name.clone())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            let haystack = format!("{path} {symbols}").to_lowercase();
            let hits = terms.iter().filter(|term| haystack.contains(*term)).count();
            (hits > 0).then_some((hits, path))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let matched = scored.len();
    scored.truncate(limit);

    let hits = scored
        .into_iter()
        .map(|(hits, path)| Hit {
            why: format!(
                "matched {hits} of {} query term(s) in its path or symbol NAMES (not by meaning)",
                terms.len()
            ),
            path,
            focus: None,
        })
        .collect();
    (hits, matched)
}

/// English function words that carry no signal in a path or a symbol name.
///
/// A length floor alone cannot do this job: `the`, `and` and `for` are three
/// characters and pure noise, while `api`, `sql`, `url` and `log` are three
/// characters and exactly what someone is searching for. So the floor stays
/// at three and the handful of words that would otherwise match half the tree
/// are named. Deliberately short — this is a noise filter, not a linguistics
/// project, and every word added is a word a caller can no longer search for.
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "was", "were", "that", "this", "with", "from", "into", "when",
    "where", "what", "which", "does", "how", "its", "not", "but", "all", "any", "can", "has",
    "have", "before", "after", "them", "then", "than",
];

/// Words worth matching on: lowercase, alphanumeric, at least three
/// characters, and not a stopword.
pub fn terms_of(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 3)
        .map(str::to_lowercase)
        .filter(|word| !STOPWORDS.contains(&word.as_str()))
        .collect();
    terms.sort();
    terms.dedup();
    terms
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stopword list filters function words while keeping the short
    /// identifiers people actually search for.
    #[test]
    fn function_words_are_not_search_terms_but_short_identifiers_are() {
        let terms = terms_of("where does the api log its sql errors");
        assert!(terms.contains(&"api".to_string()));
        assert!(terms.contains(&"sql".to_string()));
        assert!(terms.contains(&"log".to_string()));
        assert!(terms.contains(&"errors".to_string()));
        assert!(!terms.contains(&"where".to_string()));
        assert!(!terms.contains(&"does".to_string()));
        assert!(!terms.contains(&"the".to_string()));
        assert!(!terms.contains(&"its".to_string()));
    }
}
