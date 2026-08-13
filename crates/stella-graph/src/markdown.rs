// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Markdown's heading hierarchy, as the sections the semantic index ranks.
//!
//! # Why markdown is indexed at all
//!
//! It was not, and the omission was invisible: `code_graph_files` held zero
//! rows whose path ended `.md`, so 165 markdown files in this workspace —
//! `AGENTS.md`, `CLAUDE.md`, every crate README, all of `docs/spec/` — could
//! not be reached by any search the agent had. The invariants that govern
//! this repository were unsearchable *by the agent that has to obey them*.
//!
//! A question like "why must a new `AgentEvent` variant declare what consumes
//! it" has no answer in the code at all. It is answered by one section of one
//! markdown file, and until that section is a row in the index the honest
//! answer to the query is silence.
//!
//! # Why a line scan and not a tree-sitter grammar
//!
//! Every other language in this crate arrives through a native grammar
//! ([`crate::lang`]), and markdown deliberately does not. The index needs one
//! fact from a markdown file — where each ATX heading starts, and which
//! headings enclose it — and that is a line scan with a fence flag. A grammar
//! would add a build dependency and a syntax tree to recover a fact that is
//! already lexically obvious at the start of a line.
//!
//! The failure direction matters too: a scan that misses a heading yields one
//! coarser section, never a wrong one, and never an error. That is the same
//! best-effort contract every other per-file failure mode here has (`L-L1`).
//!
//! # Why a section, not a file and not a heading
//!
//! A section's body runs from its heading to the **next heading of any
//! level** — not to the end of its own subtree. A subtree-scoped section
//! would make `# AGENTS.md`'s body the entire document, which is the
//! whole-file dilution [`crate::vectors`] exists to escape; a heading with no
//! body would embed a title with no content to disambiguate it. Sibling-
//! scoped sections give every heading exactly the prose written under it.
//!
//! Content *before* the first heading is deliberately not a section. The
//! file-level vector already carries it (`render_file_text` leads with the
//! head of the file), and inventing a `(preamble)` symbol would put a name in
//! `code_graph_symbols.name` that names nothing a reader could cite.

use crate::symbol::{Symbol, SymbolKind};

/// The deepest heading markdown defines. A run of seven or more `#` is not a
/// heading at all, which is why this is a bound and not a clamp.
const MAX_HEADING_LEVEL: usize = 6;

/// The shortest run of backticks or tildes that opens a fenced code block.
const MIN_FENCE: usize = 3;

/// How many leading spaces a construct may carry and still be recognised.
/// Four spaces starts an indented code block instead, so this is CommonMark's
/// bound rather than a tolerance we chose.
const MAX_INDENT: usize = 3;

/// The separator between the levels of a section's breadcrumb.
///
/// A non-ASCII separator on purpose: it cannot occur in a heading by accident
/// the way `/` or `>` can, so splitting a breadcrumb back into its levels is
/// unambiguous, and a reader seeing
/// `Architecture › 8. Provider feature parity` knows immediately that it is a
/// path through a document rather than part of one heading.
pub(crate) const BREADCRUMB_SEPARATOR: &str = " › ";

/// Every ATX-headed section of `source`, in document order, as symbols the
/// index stores exactly like any other.
///
/// The `name` is the section's **breadcrumb** — the enclosing headings joined
/// by [`BREADCRUMB_SEPARATOR`], without the file path, matching every other
/// symbol name in this crate (the path is `code_graph_files.path`, and a
/// citation composes the two). Lines are 1-based and inclusive, the same
/// convention `Symbol` carries everywhere else.
///
/// Pure: no I/O, no clock, no environment. Setext headings (`===`/`---`
/// underlines) are **not** recognised — `---` is also a thematic break and
/// also YAML frontmatter's delimiter, and guessing between them wrong would
/// invent sections rather than miss them (#3103).
pub(crate) fn sections(source: &str) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = Vec::new();
    // One entry per open heading level, so the breadcrumb of the next heading
    // is a prefix of this stack. Index 0 is level 1.
    let mut stack: Vec<String> = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    let mut total_lines: u32 = 0;

    for (index, line) in source.lines().enumerate() {
        let line_no = index as u32 + 1;
        total_lines = line_no;

        // Fence state first: a `#` inside a code block is a shell comment or
        // a Rust attribute, not a heading, and a document that opens a fence
        // and never closes it must not resume finding headings.
        match fence {
            Some((ch, open_len)) => {
                if closes_fence(line, ch, open_len) {
                    fence = None;
                }
                continue;
            }
            None => {
                if let Some(marker) = fence_marker(line) {
                    fence = Some(marker);
                    continue;
                }
            }
        }

        let Some((level, text)) = atx_heading(line) else {
            continue;
        };

        // Close the previous section at the line above this heading. A
        // heading immediately following another leaves the earlier one with
        // an empty body, which is correct: it had none.
        if let Some(previous) = out.last_mut() {
            previous.end_line = line_no.saturating_sub(1).max(previous.start_line);
        }

        // A jump from level 2 to level 4 opens level 4 under whatever level 2
        // was open — the document's own nesting, not a repaired one.
        stack.truncate(level.saturating_sub(1));
        while stack.len() < level.saturating_sub(1) {
            stack.push(String::new());
        }
        stack.push(text.to_string());

        out.push(Symbol {
            name: stack
                .iter()
                .filter(|level| !level.is_empty())
                .cloned()
                .collect::<Vec<_>>()
                .join(BREADCRUMB_SEPARATOR),
            kind: SymbolKind::Section,
            start_line: line_no,
            end_line: line_no,
        });
    }

    if let Some(last) = out.last_mut() {
        last.end_line = total_lines.max(last.start_line);
    }
    // A heading with no text (`##` alone) opens a level and names nothing.
    // It is dropped *here*, after the spans are closed, so the section above
    // it still ends where the nameless heading began — filtering earlier
    // would silently extend the previous section over it.
    out.retain(|section| !section.name.is_empty());
    out
}

/// The heading level and text of an ATX heading line, or `None`.
///
/// Requires whitespace (or end of line) after the `#` run, which is what
/// separates `# Title` from `#hashtag`, and strips the optional closing run
/// so `## Title ##` and `## Title` produce the same breadcrumb.
fn atx_heading(line: &str) -> Option<(usize, &str)> {
    let rest = after_indent(line)?;
    let hashes = rest.bytes().take_while(|byte| *byte == b'#').count();
    if hashes == 0 || hashes > MAX_HEADING_LEVEL {
        return None;
    }
    let after = &rest[hashes..];
    if !after.is_empty() && !after.starts_with([' ', '\t']) {
        return None;
    }
    let text = after.trim().trim_end_matches('#').trim();
    // `##` alone is a heading with no text; it opens a level but names
    // nothing, and an empty breadcrumb level is dropped when the name is
    // joined.
    Some((hashes, text))
}

/// The fence character and length if `line` opens a fenced code block.
fn fence_marker(line: &str) -> Option<(u8, usize)> {
    let rest = after_indent(line)?;
    let ch = *rest.as_bytes().first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let len = rest.bytes().take_while(|byte| *byte == ch).count();
    (len >= MIN_FENCE).then_some((ch, len))
}

/// Whether `line` closes a fence opened with `ch` repeated `open_len` times.
///
/// A closing fence must use the same character, be at least as long, and
/// carry nothing but whitespace after the run — otherwise the ```` ```rust ````
/// opening an inner block would close the outer one.
fn closes_fence(line: &str, ch: u8, open_len: usize) -> bool {
    let Some(rest) = after_indent(line) else {
        return false;
    };
    let len = rest.bytes().take_while(|byte| *byte == ch).count();
    len >= open_len && rest[len..].trim().is_empty()
}

/// The line's content past its leading spaces, or `None` when there are more
/// than [`MAX_INDENT`] of them — four spaces opens an indented code block,
/// inside which neither a heading nor a fence is recognised.
fn after_indent(line: &str) -> Option<&str> {
    let spaces = line.bytes().take_while(|byte| *byte == b' ').count();
    (spaces <= MAX_INDENT).then(|| &line[spaces..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(source: &str) -> Vec<String> {
        sections(source)
            .into_iter()
            .map(|symbol| symbol.name)
            .collect()
    }

    #[test]
    fn a_heading_path_becomes_a_breadcrumb() {
        let source = "# Top\nbody\n## Middle\nmore\n### Leaf\nleaf body\n";
        assert_eq!(
            names(source),
            vec!["Top", "Top › Middle", "Top › Middle › Leaf"]
        );
    }

    /// The property the whole module exists for: a section's span is its own
    /// prose, not its subtree's, so a top heading is not the whole file.
    #[test]
    fn a_section_ends_at_the_next_heading_of_any_level() {
        let source = "# Top\nline two\nline three\n## Middle\nline five\n";
        let found = sections(source);
        assert_eq!((found[0].start_line, found[0].end_line), (1, 3));
        assert_eq!((found[1].start_line, found[1].end_line), (4, 5));
    }

    #[test]
    fn a_hash_inside_a_fenced_block_is_not_a_heading() {
        let source = "# Real\n```sh\n# not a heading\n```\n## Also real\n";
        assert_eq!(names(source), vec!["Real", "Real › Also real"]);
    }

    /// A short fence inside a longer one is content, not a close — otherwise
    /// the headings after it would be read out of a code block.
    #[test]
    fn a_shorter_inner_fence_does_not_close_a_longer_one() {
        let source = "# Real\n````\n```\n# hidden\n```\n````\n## Visible\n";
        assert_eq!(names(source), vec!["Real", "Real › Visible"]);
    }

    #[test]
    fn an_unclosed_fence_swallows_the_rest_of_the_document() {
        let source = "# Real\n```\n# hidden\n## also hidden\n";
        assert_eq!(names(source), vec!["Real"]);
    }

    #[test]
    fn a_closing_hash_run_is_not_part_of_the_name() {
        assert_eq!(names("## Title ##\n"), vec!["Title"]);
    }

    #[test]
    fn a_hash_with_no_space_is_not_a_heading() {
        assert_eq!(names("#hashtag\ntext\n"), Vec::<String>::new());
    }

    #[test]
    fn seven_hashes_are_not_a_heading() {
        assert_eq!(names("####### deep\n"), Vec::<String>::new());
    }

    /// Four spaces is an indented code block in CommonMark, so what looks
    /// like a heading inside one is not one.
    #[test]
    fn an_indented_code_block_hides_a_heading() {
        assert_eq!(names("# Real\n\n    # indented\n"), vec!["Real"]);
    }

    #[test]
    fn a_skipped_level_nests_under_what_is_actually_open() {
        assert_eq!(
            names("## Two\n#### Four\n"),
            vec!["Two", "Two › Four"],
            "the empty level 3 contributes no breadcrumb segment"
        );
    }

    #[test]
    fn content_before_the_first_heading_is_not_a_section() {
        let found = sections("---\nid: doc\n---\n\nintro prose\n\n# First\nbody\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].start_line, 7);
    }

    #[test]
    fn a_document_with_no_headings_has_no_sections() {
        assert_eq!(sections("just prose\nand more\n"), Vec::new());
    }

    /// Every section must carry a usable span — `start <= end`, inside the
    /// document — or an excerpt read against it would panic or read nothing.
    #[test]
    fn every_span_is_orderable_and_inside_the_document() {
        let source = "# A\n\n## B\n\n```\n# C\n```\n\n### D\nlast\n";
        let lines = source.lines().count() as u32;
        for section in sections(source) {
            assert!(
                section.start_line <= section.end_line,
                "{section:?} has an inverted span"
            );
            assert!(section.end_line <= lines, "{section:?} runs past the file");
        }
    }
}
