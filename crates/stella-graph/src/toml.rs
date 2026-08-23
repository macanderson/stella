// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 Oxagen, Inc. Commercial licensing: licensing@oxagen.sh

//! A TOML document's table headers, as the sections the semantic index ranks.
//!
//! # Why TOML is indexed at all
//!
//! It was not, and the omission had the shape [`crate::markdown`]'s header
//! describes: `.stella/rules/*.toml` holds this repository's published
//! **context records** — the steering policy that travels with the repository
//! — and `.toml` named no [`crate::Language`], so a record's `statement`,
//! `keywords`, `paths` and `lineage_id` reached no index. The scan rung was
//! taught to see them (#3162, #4456); the three graph-backed rungs could not,
//! because there was nothing for them to rank (#4492).
//!
//! **Only a record is a document.** `.toml` is the extension of every build
//! manifest in a Rust tree, and indexing `Cargo.toml` buys a corpus of
//! dependency version tables that dilutes exactly the records this module was
//! added for. [`crate::admitted::is_context_record`] is the gate, and it is
//! consulted by [`crate::Language::from_path`] rather than here, so every
//! admission site inherits it.
//!
//! # Why a line scan and not a tree-sitter grammar
//!
//! The same argument [`crate::markdown`] makes, and the issue asks for it by
//! name: the index needs one fact from a record — where each table starts —
//! and a table header is lexically obvious at the start of a line. A grammar
//! would add a build dependency to recover it.
//!
//! # Why a table, not a file and not a key
//!
//! A record file is one document with named parts: `[[record]]` carries the
//! statement, `[record.steering.applies_to]` the keywords that select it,
//! `[record.provenance]` where it came from. A file-level chunk would put all
//! of them in one vector, which is the whole-file dilution [`crate::vectors`]
//! exists to escape; a key-level chunk would embed `force = "must"` on its own.
//!
//! A section's body runs from its header to the line above the **next header
//! of any depth**, matching [`crate::markdown`]'s sibling-scoped sections. No
//! breadcrumb stack is needed here: a TOML header key is already absolute
//! (`record.steering.applies_to` names its own path through the document), so
//! the key as written *is* the breadcrumb.
//!
//! Content before the first header — a record file's `schema` and `set_id` —
//! is deliberately not a section, for [`crate::markdown`]'s reason: the
//! file-level vector already carries the head of the file, and a `(preamble)`
//! symbol would name nothing a reader could cite.

// This module shares its name with the `toml` crate `crate::manifest` parses
// with. They do not collide — an unqualified `toml::` inside another module
// resolves through the extern prelude, and this one is only ever
// `crate::toml` — but reach for the full path when you touch either.
use crate::symbol::{Symbol, SymbolKind};

/// How many leading spaces a header may carry and still be recognised.
/// TOML puts no bound on it; this one exists so a deeply indented line inside
/// a value cannot be mistaken for a header, and no real document approaches
/// it.
const MAX_INDENT: usize = 8;

/// Every table header of `source`, in document order, as symbols the index
/// stores exactly like any other.
///
/// The `name` is the header's key **as written**, with its brackets and any
/// surrounding whitespace stripped — `record.steering.applies_to` for
/// `[record.steering.applies_to]`, `record` for `[[record]]`. Quoted key
/// segments keep their quotes, because that is how a reader citing the
/// document would write them. Lines are 1-based and inclusive, the convention
/// `Symbol` carries everywhere else.
///
/// The scan tracks TOML's three string forms and array nesting, so neither a
/// `[` at the start of a line inside a multi-line string nor an element of a
/// multi-line array can mint a section. The failure direction is
/// [`crate::markdown`]'s: a missed header yields one coarser section, never an
/// invented one.
///
/// Pure: no I/O, no clock, no environment.
pub(crate) fn tables(source: &str) -> Vec<Symbol> {
    let mut out: Vec<Symbol> = Vec::new();
    let mut scanner = Scanner::default();
    let mut total_lines: u32 = 0;

    for (index, line) in source.lines().enumerate() {
        let line_no = index as u32 + 1;
        total_lines = line_no;

        // A header is only a header at the top level of the document: inside
        // an open multi-line string or an unclosed array, a bracketed line is
        // part of a value.
        let at_top_level = scanner.at_top_level();
        scanner.consume(line);
        if !at_top_level {
            continue;
        }
        let Some(name) = header_key(line) else {
            continue;
        };
        close_last(&mut out, line_no.saturating_sub(1));
        out.push(Symbol {
            name: name.to_string(),
            kind: SymbolKind::Section,
            start_line: line_no,
            end_line: line_no,
        });
    }

    close_last(&mut out, total_lines);
    // A header with an empty key (`[]`) is not valid TOML and names nothing;
    // it is dropped here, after the spans are closed, so the section above it
    // still ends where it began — filtering earlier would silently extend the
    // previous section over it.
    out.retain(|section| !section.name.is_empty());
    out
}

/// Close the most recently opened section at `end`, clamped so a table with
/// no keys under it keeps an orderable span.
fn close_last(out: &mut [Symbol], end: u32) {
    if let Some(previous) = out.last_mut() {
        previous.end_line = end.max(previous.start_line);
    }
}

/// The key of the table header on `line`, or `None` when the line is not one.
///
/// Both forms are recognised — `[table]` and `[[array-of-tables]]` — and they
/// yield the same key, because they name the same path through the document
/// and a reader citing one writes the key, not the bracket count. A quoted
/// segment is skipped whole, so a `]` inside `["a]b"]` does not end the header
/// early. Anything after the closing bracket other than whitespace or a `#`
/// comment means this is a value, not a header.
fn header_key(line: &str) -> Option<&str> {
    let rest = after_indent(line)?;
    let (body, closing) = match rest.strip_prefix("[[") {
        Some(body) => (body, "]]"),
        None => (rest.strip_prefix('[')?, "]"),
    };
    let end = unquoted_index_of(body, closing)?;
    let after = body[end + closing.len()..].trim_start();
    if !after.is_empty() && !after.starts_with('#') {
        return None;
    }
    Some(body[..end].trim())
}

/// The byte index of the first occurrence of `needle` in `body` that is not
/// inside a quoted key segment, or `None`.
///
/// `needle` is `]` or `]]`, so its first byte is ASCII and a match can never
/// land mid-character — which is what makes the slice below safe on a key
/// carrying non-ASCII text.
fn unquoted_index_of(body: &str, needle: &str) -> Option<usize> {
    let lead = *needle.as_bytes().first()?;
    let bytes = body.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            quote @ (b'"' | b'\'') => {
                index += 1;
                while index < bytes.len() && bytes[index] != quote {
                    // Only a basic string honours backslash escapes; a literal
                    // string ends at its first `'`.
                    if quote == b'"' && bytes[index] == b'\\' {
                        index += 1;
                    }
                    index += 1;
                }
            }
            byte if byte == lead && body[index..].starts_with(needle) => return Some(index),
            _ => {}
        }
        index += 1;
    }
    None
}

/// The line's content past its leading whitespace, or `None` when there is
/// more of it than [`MAX_INDENT`].
fn after_indent(line: &str) -> Option<&str> {
    let indent = line
        .bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count();
    (indent <= MAX_INDENT).then(|| &line[indent..])
}

/// The lexical state that carries across lines: an open multi-line string and
/// the depth of unclosed arrays. Both are what tell a header apart from a
/// bracketed line inside a value.
#[derive(Default)]
struct Scanner {
    /// The delimiter of the open multi-line string (`"""` or `'''`), if any.
    multiline: Option<u8>,
    /// How many `[` have been opened by array values and not yet closed.
    depth: usize,
}

impl Scanner {
    /// Whether the *next* line begins at the document's top level, where a
    /// bracketed line is a table header rather than part of a value.
    fn at_top_level(&self) -> bool {
        self.multiline.is_none() && self.depth == 0
    }

    /// Fold one line into the state. A header line balances its own brackets,
    /// so a document of nothing but headers never leaves [`Self::depth`] above
    /// zero.
    fn consume(&mut self, line: &str) {
        let bytes = line.as_bytes();
        let mut index = 0;
        while index < bytes.len() {
            if let Some(delimiter) = self.multiline {
                if bytes[index] == delimiter && line[index..].starts_with(triple(delimiter)) {
                    self.multiline = None;
                    index += 3;
                    continue;
                }
                // A backslash-escaped delimiter inside a multi-line basic
                // string does not close it.
                if delimiter == b'"' && bytes[index] == b'\\' {
                    index += 1;
                }
                index += 1;
                continue;
            }
            match bytes[index] {
                b'#' => return,
                quote @ (b'"' | b'\'') => {
                    if line[index..].starts_with(triple(quote)) {
                        self.multiline = Some(quote);
                        index += 3;
                        continue;
                    }
                    index += 1;
                    while index < bytes.len() && bytes[index] != quote {
                        if quote == b'"' && bytes[index] == b'\\' {
                            index += 1;
                        }
                        index += 1;
                    }
                }
                b'[' => self.depth += 1,
                b']' => self.depth = self.depth.saturating_sub(1),
                _ => {}
            }
            index += 1;
        }
    }
}

/// The three-character opening/closing delimiter for a multi-line string of
/// `quote`'s kind.
fn triple(quote: u8) -> &'static str {
    if quote == b'"' { "\"\"\"" } else { "'''" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(source: &str) -> Vec<String> {
        tables(source)
            .into_iter()
            .map(|symbol| symbol.name)
            .collect()
    }

    /// The shape every context record has (`docs/spec/adaptive-context/context-pr.md`
    /// §6.1): a preamble, an array-of-tables entry, and its dotted sub-tables.
    #[test]
    fn a_context_records_tables_become_sections() {
        let source = "schema = \"context-record/v0.1\"\n\
                      set_id = \"macanderson.stella\"\n\
                      \n\
                      [[record]]\n\
                      lineage_id = \"ctx.demo\"\n\
                      statement = \"a provider adapter never reaches into the engine\"\n\
                      \n\
                      [record.steering]\n\
                      force = \"must\"\n\
                      \n\
                      [record.steering.applies_to]\n\
                      keywords = [\"ports\", \"adapter\"]\n";
        assert_eq!(
            names(source),
            vec!["record", "record.steering", "record.steering.applies_to"]
        );
    }

    /// The property the module exists for: a section's span is its own keys,
    /// not its sub-tables', so `[[record]]` is not the whole file.
    #[test]
    fn a_section_ends_at_the_next_header_of_any_depth() {
        let source = "[[record]]\nstatement = \"x\"\n[record.truth]\nbasis = \"decree\"\n";
        let found = tables(source);
        assert_eq!((found[0].start_line, found[0].end_line), (1, 2));
        assert_eq!((found[1].start_line, found[1].end_line), (3, 4));
    }

    #[test]
    fn content_before_the_first_header_is_not_a_section() {
        let found = tables("schema = \"v0.1\"\n\n[[record]]\nstatement = \"x\"\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].start_line, 3);
    }

    #[test]
    fn a_document_with_no_headers_has_no_sections() {
        assert_eq!(tables("schema = \"v0.1\"\nset_id = \"x\"\n"), Vec::new());
    }

    /// The failure this scan's cross-line state exists to prevent: a
    /// bracketed line inside a multi-line string is prose, and reading it as a
    /// header would invent a section named after somebody's example.
    #[test]
    fn a_bracketed_line_inside_a_multiline_string_is_not_a_header() {
        let source = "[[record]]\n\
                      statement = \"\"\"\n\
                      [not.a.table]\n\
                      still prose\n\
                      \"\"\"\n\
                      [record.truth]\n\
                      basis = \"decree\"\n";
        assert_eq!(names(source), vec!["record", "record.truth"]);
    }

    #[test]
    fn a_literal_multiline_string_is_tracked_too() {
        let source = "[a]\nx = '''\n[not.a.table]\n'''\n[b]\n";
        assert_eq!(names(source), vec!["a", "b"]);
    }

    /// A multi-line array's elements can themselves be arrays, and an element
    /// line then begins with `[`. Array depth is what tells it from a header.
    #[test]
    fn an_element_of_a_multiline_array_is_not_a_header() {
        let source = "[record.steering.applies_to]\n\
                      paths = [\n\
                      [\"crates/**\"],\n\
                      [\"docs/**\"],\n\
                      ]\n\
                      [record.truth]\n";
        assert_eq!(
            names(source),
            vec!["record.steering.applies_to", "record.truth"]
        );
    }

    #[test]
    fn a_commented_out_header_is_not_a_header() {
        assert_eq!(names("[a]\n# [b]\nx = 1\n"), vec!["a"]);
    }

    /// An inline array on a header's own line would leave the depth unbalanced
    /// if the header's brackets were counted as array brackets.
    #[test]
    fn a_header_followed_by_an_inline_array_leaves_the_scan_at_top_level() {
        let source = "[a]\nkeywords = [\"x\", \"y\"]\n[b]\nz = 1\n";
        assert_eq!(names(source), vec!["a", "b"]);
    }

    #[test]
    fn a_quoted_key_segment_may_contain_a_bracket() {
        assert_eq!(names("[\"a]b\".c]\nx = 1\n"), vec!["\"a]b\".c"]);
    }

    /// `[a] = 1` is a value, not a header; only whitespace or a comment may
    /// follow the closing bracket.
    #[test]
    fn a_bracketed_key_with_a_value_after_it_is_not_a_header() {
        assert_eq!(names("[a] = 1\n"), Vec::<String>::new());
        assert_eq!(names("[a] # a note\n"), vec!["a"]);
    }

    #[test]
    fn an_array_of_tables_and_a_table_yield_the_same_key() {
        assert_eq!(names("[[record]]\n"), vec!["record"]);
        assert_eq!(names("[record]\n"), vec!["record"]);
    }

    /// Every section must carry a usable span — `start <= end`, inside the
    /// document — or an excerpt read against it would read nothing.
    #[test]
    fn every_span_is_orderable_and_inside_the_document() {
        let source = "schema = \"v\"\n[[record]]\nstatement = \"x\"\n[record.truth]\n";
        let lines = source.lines().count() as u32;
        for section in tables(source) {
            assert!(
                section.start_line <= section.end_line,
                "{section:?} has an inverted span"
            );
            assert!(section.end_line <= lines, "{section:?} runs past the file");
        }
    }
}
