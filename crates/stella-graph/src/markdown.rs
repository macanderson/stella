// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! Markdown's heading hierarchy, as the sections the semantic index ranks,
//! and its links, as the import edges the graph stores.
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
//! # Why a link is an import edge
//!
//! `[the context spec](./docs/spec/adaptive-context/context-pr.md)` is
//! exactly the relation `code_graph_imports` models, and the graph already
//! answers both directions of it (`imports_from`, `importers_of`). Extracting
//! doc links makes "which documents reference this one" answerable — the
//! question this repository's whole citation-by-id scheme exists for, after
//! sixteen dead markdown-to-markdown links accumulated under guards that only
//! ever read Rust comments (#3103).
//!
//! # Why a line scan and not a tree-sitter grammar
//!
//! Every other language in this crate arrives through a native grammar
//! ([`crate::lang`]), and markdown deliberately does not. The index needs two
//! facts from a markdown file — where each heading starts (ATX `#` runs and
//! setext underlines), and which files its links name — and both are line
//! scans with a fence flag. A grammar would add a build dependency and a
//! syntax tree to recover facts that are already lexically obvious at the
//! start of a line.
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

use crate::import::ImportSpec;
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
///
/// Public because a consumer that wants to *recognise* a breadcrumb needs the
/// same literal — `stella-tools`' exact-symbol rung admits a query carrying it
/// (#4574), and `chunk_retrieval_witnesses` asserts on section names. Both had
/// no import that could reach it and repeated the bytes instead.
pub const BREADCRUMB_SEPARATOR: &str = " › ";

/// Every headed section of `source`, in document order, as symbols the
/// index stores exactly like any other.
///
/// The `name` is the section's **breadcrumb** — the enclosing headings joined
/// by [`BREADCRUMB_SEPARATOR`], without the file path, matching every other
/// symbol name in this crate (the path is `code_graph_files.path`, and a
/// citation composes the two). Lines are 1-based and inclusive, the same
/// convention `Symbol` carries everywhere else.
///
/// Both heading forms are recognised: ATX (`#` runs) and setext (a paragraph
/// line underlined by `===` or `---`, #3103). Setext is the guarded one —
/// `---` is also a thematic break and also YAML frontmatter's delimiter — so
/// an underline promotes only the non-blank paragraph line directly above
/// it, never a blank, a fence line, frontmatter, or a shape CommonMark does
/// not read as paragraph text ([`breaks_paragraph`]). The failure direction
/// of every guard is the module's usual one: a miss yields one coarser
/// section, never an invented one.
///
/// Pure: no I/O, no clock, no environment.
pub(crate) fn sections(source: &str) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = Vec::new();
    // One entry per open heading level, so the breadcrumb of the next heading
    // is a prefix of this stack. Index 0 is level 1.
    let mut stack: Vec<String> = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    // The previous line, when it was ordinary paragraph text — the only
    // shape a setext underline may promote to a heading.
    let mut paragraph: Option<(u32, String)> = None;
    let frontmatter_end = frontmatter_end(source);
    let mut total_lines: u32 = 0;

    for (index, line) in source.lines().enumerate() {
        let line_no = index as u32 + 1;
        total_lines = line_no;

        // Frontmatter is metadata, not prose: nothing inside it is a
        // heading, and its closing `---` must not read as a setext underline
        // for the metadata line above it.
        if line_no <= frontmatter_end {
            continue;
        }

        // Fence state next: a `#` inside a code block is a shell comment or
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
                    paragraph = None;
                    continue;
                }
            }
        }

        if let Some((level, text)) = atx_heading(line) {
            // Close the previous section at the line above this heading. A
            // heading immediately following another leaves the earlier one
            // with an empty body, which is correct: it had none.
            close_last(&mut out, line_no.saturating_sub(1));
            out.push(open_section(&mut stack, level, text, line_no));
            paragraph = None;
            continue;
        }

        // A setext underline promotes the paragraph line directly above it —
        // only that line: a multi-line paragraph keeps its earlier lines in
        // the enclosing section, deliberately coarser than CommonMark's
        // whole-paragraph heading. With nothing promotable above, the line
        // falls through to be classified like any other — `---` as a
        // thematic break, `===` as ordinary paragraph text.
        if let Some(level) = setext_level(line)
            && let Some((text_line, text)) = paragraph.take()
        {
            close_last(&mut out, text_line.saturating_sub(1));
            out.push(open_section(&mut stack, level, &text, text_line));
            continue;
        }

        paragraph = if line.trim().is_empty() {
            None
        } else if let Some(rest) = after_indent(line) {
            (!breaks_paragraph(rest)).then(|| (line_no, line.trim().to_string()))
        } else {
            // Four-plus spaces directly under an open paragraph is a lazy
            // continuation of it; under anything else it opens an indented
            // code block.
            paragraph
                .is_some()
                .then(|| (line_no, line.trim().to_string()))
        };
    }

    close_last(&mut out, total_lines);
    // A heading with no text (`##` alone) opens a level and names nothing.
    // It is dropped *here*, after the spans are closed, so the section above
    // it still ends where the nameless heading began — filtering earlier
    // would silently extend the previous section over it.
    out.retain(|section| !section.name.is_empty());
    out
}

/// Close the most recently opened section at `end`, clamped so a section
/// with an empty body keeps an orderable span.
fn close_last(out: &mut [Symbol], end: u32) {
    if let Some(previous) = out.last_mut() {
        previous.end_line = end.max(previous.start_line);
    }
}

/// Open a section at `level` named `text` under whatever the breadcrumb
/// stack has open, starting at `start_line`. A jump from level 2 to level 4
/// opens level 4 under whatever level 2 was open — the document's own
/// nesting, not a repaired one.
fn open_section(stack: &mut Vec<String>, level: usize, text: &str, start_line: u32) -> Symbol {
    stack.truncate(level.saturating_sub(1));
    while stack.len() < level.saturating_sub(1) {
        stack.push(String::new());
    }
    stack.push(text.to_string());
    Symbol {
        name: stack
            .iter()
            .filter(|part| !part.is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join(BREADCRUMB_SEPARATOR),
        kind: SymbolKind::Section,
        start_line,
        end_line: start_line,
    }
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

/// The 1-based number of the line closing the document's YAML frontmatter,
/// or 0 when it has none. Frontmatter is a `---` on line 1 closed by a later
/// `---` or `...`; an opening delimiter that is never closed is an ordinary
/// thematic break, not frontmatter — reading it as an unclosed block would
/// swallow every heading in the document behind one stray dash run.
fn frontmatter_end(source: &str) -> u32 {
    let mut lines = source.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return 0;
    }
    for (index, line) in lines.enumerate() {
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            return index as u32 + 2;
        }
    }
    0
}

/// The heading level a setext underline confers — `===` is level 1, `---`
/// level 2 — or `None` when `line` is not underline-shaped: one marker
/// repeated, nothing after it but whitespace, at most [`MAX_INDENT`] spaces
/// before. Whether the shape *acts* as an underline is the caller's call,
/// because the same line is a thematic break when no paragraph precedes it.
fn setext_level(line: &str) -> Option<usize> {
    let rest = after_indent(line)?;
    let (marker, level) = match rest.as_bytes().first() {
        Some(b'=') => (b'=', 1),
        Some(b'-') => (b'-', 2),
        _ => return None,
    };
    let run = rest.bytes().take_while(|byte| *byte == marker).count();
    rest[run..].trim().is_empty().then_some(level)
}

/// Line shapes (past their indent) CommonMark does not read as paragraph
/// text, so a `---` under one is a thematic break rather than a setext
/// underline: thematic breaks themselves, blockquotes, list items, and
/// pipe-table rows. The failure direction decides the close calls: a shape
/// wrongly kept here costs one missed heading, while a shape wrongly read
/// as a paragraph invents a section that does not exist.
fn breaks_paragraph(rest: &str) -> bool {
    if is_thematic_break(rest) {
        return true;
    }
    let bytes = rest.as_bytes();
    match bytes.first() {
        Some(b'>' | b'|') => true,
        Some(b'-' | b'+' | b'*') => bytes.get(1).is_none_or(|byte| matches!(byte, b' ' | b'\t')),
        Some(b'0'..=b'9') => {
            let digits = rest
                .bytes()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            matches!(bytes.get(digits), Some(b'.' | b')'))
                && bytes
                    .get(digits + 1)
                    .is_none_or(|byte| matches!(byte, b' ' | b'\t'))
        }
        _ => false,
    }
}

/// `***`, `---`, `___`: at least three of one marker with nothing but
/// whitespace between and after them.
fn is_thematic_break(rest: &str) -> bool {
    let mut marker = None;
    let mut count = 0usize;
    for ch in rest.chars() {
        match ch {
            ' ' | '\t' => {}
            '-' | '_' | '*' => {
                if *marker.get_or_insert(ch) != ch {
                    return false;
                }
                count += 1;
            }
            _ => return false,
        }
    }
    count >= 3
}

/// Every workspace link target of `source`, in document order, as the raw
/// import specifiers [`crate::parse`] hands to [`crate::import::resolve`].
///
/// Two syntaxes produce an edge: inline links and images (`[text](target)`,
/// `![alt](target)`) and reference definitions (`[label]: target`). A target
/// with a URL scheme (`https:`, `mailto:`) or a protocol-relative `//` names
/// nothing in the tree and is skipped — it is not a workspace edge — as is a
/// bare `#fragment` self link; a GFM footnote definition (`[^1]: prose`) is
/// prose, not a link. Fenced blocks, indented code blocks, and frontmatter
/// are skipped with the same tracking [`sections`] uses.
///
/// Pure: no I/O, no clock, no environment — resolution against the
/// filesystem happens in [`crate::import::resolve`], which is what keeps the
/// stored chunk hash a valid cache key ([`crate::vectors`]).
pub(crate) fn links(source: &str) -> Vec<ImportSpec> {
    let mut out: Vec<ImportSpec> = Vec::new();
    let mut fence: Option<(u8, usize)> = None;
    let frontmatter_end = frontmatter_end(source);

    for (index, line) in source.lines().enumerate() {
        if (index as u32) < frontmatter_end {
            continue;
        }
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
        // Four-plus spaces is an indented code block: a link-shaped string
        // inside one is example text, and an invented edge is the failure
        // direction this module avoids (a missed one is only a coarser
        // graph).
        let Some(rest) = after_indent(line) else {
            continue;
        };
        if let Some(target) = reference_definition_target(rest) {
            // The remainder of a definition line is an optional title,
            // which may itself contain link syntax; it names nothing.
            push_target(&mut out, target);
            continue;
        }
        inline_targets(rest, &mut out);
    }
    out
}

/// The destination of a reference definition (`[label]: target`) at the head
/// of `rest`, or `None`. A label opening with `^` is a GFM footnote — its
/// "target" is the first word of its prose, not a link.
fn reference_definition_target(rest: &str) -> Option<&str> {
    let label = rest.strip_prefix('[')?;
    if label.starts_with('^') {
        return None;
    }
    let close = find_unescaped(label, b']')?;
    if close == 0 {
        return None;
    }
    destination(label[close + 1..].strip_prefix(':')?.trim_start())
}

/// Push the destination of every inline link or image on `rest`. The scan
/// keys on `](`; the bracket-depth guard keeps a stray `](` in prose from
/// minting an edge, and the text between the brackets is never needed —
/// only the destination after them.
fn inline_targets(rest: &str, out: &mut Vec<ImportSpec>) {
    let bytes = rest.as_bytes();
    let mut depth = 0usize;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            // An escaped bracket is literal text. (Skipping one byte of a
            // multi-byte character is safe: continuation bytes match no
            // arm below, and no slice is taken at this position.)
            b'\\' => index += 1,
            b'[' => depth += 1,
            b']' if depth > 0 => {
                depth -= 1;
                if bytes.get(index + 1) == Some(&b'(') {
                    let body = &rest[index + 2..];
                    if let Some(target) = destination(body) {
                        push_target(out, target);
                    }
                    // Resume after the link's closing `)`. Without this the
                    // scan walks back into the destination and title, where a
                    // `](...)`-shaped substring in the title would mint a
                    // spurious edge — the failure direction this module avoids.
                    if let Some(close) = link_close(body.as_bytes()) {
                        index += 2 + close;
                    }
                }
            }
            _ => {}
        }
        index += 1;
    }
}

/// The offset of the `)` that closes an inline link whose body — its
/// destination and optional title — begins at the head of `body`, or `None`
/// if none closes it. A quoted title is skipped whole so a `)` inside it
/// (or a `](...)` link-shaped substring anywhere in it) does not end the
/// link early or mint an edge.
fn link_close(body: &[u8]) -> Option<usize> {
    let mut index = 0;
    let mut depth = 0usize;
    while index < body.len() {
        match body[index] {
            b'\\' => index += 1,
            quote @ (b'"' | b'\'') => {
                index += 1;
                while index < body.len() && body[index] != quote {
                    if body[index] == b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
            }
            b'(' => depth += 1,
            b')' if depth == 0 => return Some(index),
            b')' => depth -= 1,
            _ => {}
        }
        index += 1;
    }
    None
}

/// The link destination at the head of `text`: an angle-bracketed target up
/// to its `>`, or a plain one up to the first whitespace or unbalanced `)` —
/// CommonMark allows balanced parentheses inside a plain destination.
fn destination(text: &str) -> Option<&str> {
    if let Some(inner) = text.strip_prefix('<') {
        return inner.find('>').map(|end| &inner[..end]);
    }
    let mut depth = 0usize;
    for (index, ch) in text.char_indices() {
        match ch {
            c if c.is_whitespace() => return Some(&text[..index]),
            '(' => depth += 1,
            ')' => {
                if depth == 0 {
                    return Some(&text[..index]);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    Some(text)
}

/// Record `target` as an edge unless it cannot name a file in the tree: a
/// URL (scheme-ful or protocol-relative), a `#fragment` self link, or
/// nothing at all.
fn push_target(out: &mut Vec<ImportSpec>, target: &str) {
    if target.is_empty()
        || target.starts_with('#')
        || target.starts_with("//")
        || has_url_scheme(target)
    {
        return;
    }
    out.push(ImportSpec::MarkdownLink {
        specifier: target.to_string(),
    });
}

/// Whether `target` opens with an RFC 3986 scheme (`https:`, `mailto:`, …):
/// an ASCII letter, then letters, digits, `+`, `-`, or `.`, then `:`.
fn has_url_scheme(target: &str) -> bool {
    let mut chars = target.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_alphabetic()) {
        return false;
    }
    for c in chars {
        match c {
            ':' => return true,
            c if c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.') => {}
            _ => return false,
        }
    }
    false
}

/// The index of the first unescaped `needle` byte in `text`.
fn find_unescaped(text: &str, needle: u8) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            byte if byte == needle => return Some(index),
            _ => index += 1,
        }
    }
    None
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
        let source = "# A\n\n## B\n\n```\n# C\n```\n\n### D\nlast\nSetext\n---\n";
        let lines = source.lines().count() as u32;
        for section in sections(source) {
            assert!(
                section.start_line <= section.end_line,
                "{section:?} has an inverted span"
            );
            assert!(section.end_line <= lines, "{section:?} runs past the file");
        }
    }

    #[test]
    fn a_setext_underline_promotes_the_paragraph_line_above_it() {
        let source = "Top\n===\nbody\nMiddle\n---\nmore\n";
        assert_eq!(names(source), vec!["Top", "Top › Middle"]);
    }

    #[test]
    fn setext_and_atx_headings_nest_in_one_breadcrumb() {
        let source = "Top\n===\n\n## Inner\ntext\nAlso two\n---\nmore\n";
        assert_eq!(names(source), vec!["Top", "Top › Inner", "Top › Also two"]);
    }

    /// A setext heading occupies two lines; the section starts at the text
    /// line, and the section above it must end *above* that text line.
    #[test]
    fn a_setext_section_starts_at_its_text_line() {
        let source = "# Top\nTitle\n===\nbody\n";
        let found = sections(source);
        assert_eq!((found[0].start_line, found[0].end_line), (1, 1));
        assert_eq!((found[1].start_line, found[1].end_line), (2, 4));
    }

    /// The underline promotes the single line above it — a deliberately
    /// coarser read than CommonMark's whole-paragraph heading, whose earlier
    /// lines stay in the enclosing section.
    #[test]
    fn only_the_line_directly_above_the_underline_is_promoted() {
        assert_eq!(names("first line\nsecond line\n===\n"), vec!["second line"]);
    }

    /// The frontmatter block is metadata: neither its own closing delimiter
    /// nor a dash run directly after it may promote a metadata line.
    #[test]
    fn a_dash_run_after_frontmatter_is_not_an_underline() {
        let source = "---\nid: doc\n---\n---\n\n# Real\n";
        assert_eq!(names(source), vec!["Real"]);
    }

    /// An opening `---` that is never closed is a thematic break, not an
    /// unclosed frontmatter block swallowing the rest of the document.
    #[test]
    fn an_unclosed_frontmatter_delimiter_is_an_ordinary_dash_run() {
        assert_eq!(names("---\nprose\n\n# Real\n"), vec!["Real"]);
    }

    #[test]
    fn a_dash_run_after_a_blank_line_is_a_thematic_break() {
        let source = "# Top\nprose\n\n---\nmore\n";
        assert_eq!(names(source), vec!["Top"]);
    }

    #[test]
    fn a_dash_run_inside_a_fence_is_not_an_underline() {
        assert_eq!(names("```\nText\n---\n```\n"), Vec::<String>::new());
    }

    /// The closing fence line is not a paragraph, so a dash run directly
    /// under it stays a thematic break.
    #[test]
    fn a_dash_run_directly_under_a_closing_fence_is_not_an_underline() {
        assert_eq!(names("```\ncode\n```\n---\n"), Vec::<String>::new());
    }

    /// CommonMark reads the `---` under a non-paragraph shape as a thematic
    /// break; promoting one would invent a section named `- item`.
    #[test]
    fn a_dash_run_under_a_non_paragraph_shape_is_not_an_underline() {
        assert_eq!(names("- item\n---\n"), Vec::<String>::new());
        assert_eq!(names("> quote\n---\n"), Vec::<String>::new());
        assert_eq!(names("| header |\n---\n"), Vec::<String>::new());
    }

    fn targets(source: &str) -> Vec<String> {
        links(source)
            .into_iter()
            .map(|spec| match spec {
                ImportSpec::MarkdownLink { specifier } => specifier,
                other => panic!("markdown yields only link specs, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn inline_links_and_reference_definitions_become_edges() {
        let source = "# Doc\n\nSee [the spec](./spec/context.md) and \
                      [the readme](../README.md).\n\n[cite]: docs/other.md\n";
        assert_eq!(
            targets(source),
            vec!["./spec/context.md", "../README.md", "docs/other.md"]
        );
    }

    #[test]
    fn urls_fragments_and_footnotes_are_not_workspace_edges() {
        let source = "[web](https://example.com/x.md)\n[proto]: //cdn.example.com/y\n\
                      [mail](mailto:a@b.c)\n[self](#section)\n[^1]: a footnote, not a link\n";
        assert_eq!(targets(source), Vec::<String>::new());
    }

    #[test]
    fn a_link_inside_a_fence_or_indented_block_is_example_text() {
        let source = "```md\n[x](./fenced.md)\n```\n\n    [y](./indented.md)\n";
        assert_eq!(targets(source), Vec::<String>::new());
    }

    #[test]
    fn images_titles_and_angle_bracket_destinations_are_read() {
        let source = "![diagram](./assets/flow.svg)\n[a](./b.md \"Title\")\n\
                      [c](<./with space.md>)\n";
        assert_eq!(
            targets(source),
            vec!["./assets/flow.svg", "./b.md", "./with space.md"]
        );
    }

    /// The specifier stays as written — it is a label — and resolution is
    /// where the `#fragment` comes off ([`crate::import::resolve`]).
    #[test]
    fn a_fragment_suffix_rides_the_specifier() {
        assert_eq!(targets("[a](./b.md#section)\n"), vec!["./b.md#section"]);
    }

    #[test]
    fn a_stray_close_bracket_paren_in_prose_is_not_a_link() {
        assert_eq!(
            targets("prose with ](./not-a-link.md) in it\n"),
            Vec::<String>::new()
        );
    }

    /// A `](...)`-shaped substring inside a link *title* names nothing; the
    /// scan must resume past the whole link so the title cannot mint an edge.
    #[test]
    fn a_link_shaped_substring_in_a_title_is_not_an_edge() {
        assert_eq!(
            targets("[link](./real.md \"note: [brackets](./other.md)\")\n"),
            vec!["./real.md"]
        );
    }

    #[test]
    fn links_inside_frontmatter_are_metadata() {
        assert_eq!(
            targets("---\nsee: [x](./meta.md)\n---\n[real](./real.md)\n"),
            vec!["./real.md"]
        );
    }
}
