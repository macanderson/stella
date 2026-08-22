//! The grammar-backed half of the shared lexer: tree-sitter over one line.
//!
//! ## Why a grammar, and why here
//!
//! The hand-written scan in [`super::lang`] recognised six languages and no
//! more, so a `read_file` of a Go, Java, C, SQL or PHP file rendered as flat
//! text on all three transcript surfaces, and even inside the six it guessed —
//! a keyword table cannot tell a type from a variable, or a call from a
//! reference. #4283 decided the upgrade and, more importantly, decided *where*
//! it lands: here, at the bottom, so the deck, the export grid and the
//! Observatory gain it in one change. A deck-local highlighter would have
//! bought the deck accuracy by making the three surfaces disagree again, which
//! is the drift #3644 and #4036 each closed once.
//!
//! ## Why tree-sitter and not syntect
//!
//! Two reasons, and the second is the one that settles it:
//!
//! - **It is already resident.** The root `[workspace.dependencies]` carries
//!   `tree-sitter` and all eight grammar crates used here, because
//!   `stella-graph` indexes the code graph with them — every one is a default
//!   feature of that crate, and `stella-cli` takes it with defaults, so the
//!   shipping binary already compiles this exact C. `cargo deny check licenses`
//!   has therefore already passed on all of it.
//! - **Grammars are compiled in, so there is no runtime asset load.**
//!   syntect's default path reads grammar and theme files at startup, which
//!   invariant #2 forbids in this layer. Everything below is a pure function
//!   over borrowed text.
//!
//! ## Why per line, and why that is not a parser instantiation per line
//!
//! [`super::tokenize`]'s contract is stateless and line-oriented, because a
//! diff hunk can slice a file anywhere and a transcript body is middle-elided
//! before anything renders it. That contract is kept: each line is parsed on
//! its own. Tree-sitter is error-tolerant by construction — a fragment that is
//! not a whole item still lexes its leaves correctly, wrapped in an `ERROR`
//! node whose children keep their token kinds — so a line taken out of context
//! degrades to slightly-off *structure*, never to wrong tokens.
//!
//! What would have made that ruinous is `Parser::new()` per line: constructing
//! a parser and installing a language dwarfs the parse itself. So parsers are
//! built once per grammar per thread and reused ([`with_parser`]). This matters
//! more than it looks — the deck caches highlighted tail rows across frames
//! (`an_unchanged_tail_is_highlighted_once_not_once_per_frame`), and a
//! per-frame parser build would have made a cache that still holds look like
//! one that does not.
//!
//! ## Losslessness, structurally
//!
//! The contract every surface leans on is that concatenating the run texts
//! reproduces the line. That is not asserted here, it is *constructed*:
//! [`runs`] walks the leaves in source order and fills every byte between them,
//! so the output covers `[0, code.len())` exactly once. A leaf that fails a
//! sanity check (zero width, out of order, not on a `char` boundary) is skipped
//! without advancing the cursor, so its bytes are still emitted — untagged
//! rather than dropped.

use std::cell::RefCell;

use tree_sitter::{Node, Parser};

use super::{Runs, Tok};

/// A grammar compiled into this crate.
///
/// Deliberately *not* one-to-one with [`super::Lang`]: several languages share
/// a grammar (every TypeScript/JavaScript dialect takes the TSX one), and the
/// three languages with no resident grammar — Markdown, TOML, JSON — have no
/// variant here at all and stay with the hand-written scan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Grammar {
    Rust,
    /// TypeScript, TSX, JavaScript and JSX.
    ///
    /// One grammar for all four rather than `LANGUAGE_TYPESCRIPT` beside
    /// `LANGUAGE_TSX`: this crate holds a single `Lang::TsJs` because the
    /// palettes downstream cannot tell the dialects apart anyway, and TSX is
    /// the choice that keeps JSX prose from lexing as an unterminated string —
    /// the failure `a_contraction_apostrophe_does_not_swallow_the_line_as_a_string`
    /// pins. The cost is the `<T>expr` type assertion, which TSX reads as a JSX
    /// element; it is rare, and it mis-tints rather than mis-renders.
    Tsx,
    Python,
    Go,
    Java,
    C,
    Sql,
    Php,
}

impl Grammar {
    /// Every grammar, for the tests that assert each one arms and holds its own
    /// parser slot.
    ///
    /// `#[cfg(test)]` rather than `#[allow(dead_code)]`: nothing ships a call to
    /// it, so the allow would be asserting the lint is wrong when it is right
    /// (AGENTS.md § "Code style"). A later production caller becomes a build
    /// error here rather than silently re-justifying a suppression.
    #[cfg(test)]
    pub(super) const ALL: [Grammar; 8] = [
        Grammar::Rust,
        Grammar::Tsx,
        Grammar::Python,
        Grammar::Go,
        Grammar::Java,
        Grammar::C,
        Grammar::Sql,
        Grammar::Php,
    ];

    /// This grammar's slot in the per-thread parser cache.
    fn slot(self) -> usize {
        match self {
            Grammar::Rust => 0,
            Grammar::Tsx => 1,
            Grammar::Python => 2,
            Grammar::Go => 3,
            Grammar::Java => 4,
            Grammar::C => 5,
            Grammar::Sql => 6,
            Grammar::Php => 7,
        }
    }

    /// The tree-sitter language.
    ///
    /// `LANGUAGE_PHP` rather than `LANGUAGE_PHP_ONLY` for the same reason
    /// `stella-graph` picks it: a real `.php` file opens in HTML and the
    /// PHP-only grammar cannot parse it.
    fn language(self) -> tree_sitter::Language {
        match self {
            Grammar::Rust => tree_sitter_rust::LANGUAGE.into(),
            Grammar::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Grammar::Python => tree_sitter_python::LANGUAGE.into(),
            Grammar::Go => tree_sitter_go::LANGUAGE.into(),
            Grammar::Java => tree_sitter_java::LANGUAGE.into(),
            Grammar::C => tree_sitter_c::LANGUAGE.into(),
            Grammar::Sql => tree_sitter_sequel::LANGUAGE.into(),
            Grammar::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        }
    }
}

thread_local! {
    /// One parser per grammar per thread, built on first use.
    ///
    /// `RefCell` rather than a lock because a `Parser` is not `Sync` and does
    /// not need to be: nothing here escapes the call, so the borrow is taken
    /// and released inside [`with_parser`] and can never be held across one.
    static PARSERS: RefCell<[Option<Parser>; 8]> =
        RefCell::new(std::array::from_fn(|_| None));
}

/// Run `f` against this thread's parser for `g`, building it on first use.
///
/// `None` when the grammar will not install — a version skew between
/// `tree-sitter` and a grammar crate is the only way that happens, and it is a
/// build-time fact rather than a runtime one, so the caller renders the line
/// plain rather than treating it as an error worth naming.
fn with_parser<T>(g: Grammar, f: impl FnOnce(&mut Parser) -> T) -> Option<T> {
    PARSERS.with(|cell| {
        let mut slots = cell.borrow_mut();
        let slot = &mut slots[g.slot()];
        if slot.is_none() {
            let mut parser = Parser::new();
            if parser.set_language(&g.language()).is_err() {
                return None;
            }
            *slot = Some(parser);
        }
        slot.as_mut().map(f)
    })
}

/// Tokenize one line under `g`, or `None` when the grammar would not install.
///
/// Lossless and panic-free by construction — see the module doc.
pub(super) fn runs(code: &str, g: Grammar) -> Option<Runs> {
    let tree = with_parser(g, |parser| parser.parse(code.as_bytes(), None))??;

    let mut out = Emit::new(code);
    let mut cursor = tree.walk();
    // Depth-first, leaves in source order. `goto_first_child` descends while
    // there is one; at a leaf we emit, then climb until a sibling exists.
    // Failing to climb means we are back at the root and the walk is done.
    loop {
        if cursor.goto_first_child() {
            continue;
        }
        out.leaf(cursor.node());
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Some(out.finish());
            }
        }
    }
}

/// Accumulates runs over the source, filling the gaps between leaves.
struct Emit<'a> {
    code: &'a str,
    runs: Runs,
    /// Bytes before this are already emitted. The invariant that makes the
    /// output lossless: it only ever moves forward, and it reaches
    /// `code.len()` in [`Emit::finish`].
    cursor: usize,
    /// Untagged text awaiting a tagged run to interrupt it, so punctuation and
    /// indentation cost one run rather than one per token.
    plain: String,
}

impl<'a> Emit<'a> {
    fn new(code: &'a str) -> Self {
        Self {
            code,
            runs: Runs::new(),
            cursor: 0,
            plain: String::new(),
        }
    }

    fn flush(&mut self) {
        if !self.plain.is_empty() {
            self.runs.push((std::mem::take(&mut self.plain), None));
        }
    }

    /// Emit one leaf, plus whatever untagged text precedes it.
    fn leaf(&mut self, node: Node<'_>) {
        let (start, end) = (node.start_byte(), node.end_byte());
        // Zero-width (a `MISSING` node tree-sitter inserted to recover), out of
        // order, past the end, or off a `char` boundary. Skipping without
        // moving the cursor leaves the bytes to the next gap fill or to
        // `finish`, so the line still round-trips.
        if end <= start
            || start < self.cursor
            || end > self.code.len()
            || !self.code.is_char_boundary(start)
            || !self.code.is_char_boundary(end)
        {
            return;
        }
        if start > self.cursor {
            self.plain.push_str(&self.code[self.cursor..start]);
        }
        match classify(node) {
            Some(tok) => {
                self.flush();
                // Coalesce with the previous run when it wears the same class.
                // Grammars split one literal into several leaves — `'` +
                // `string_fragment` + `'` in TypeScript, `string_start` +
                // `string_content` + `string_end` in Python — and a reader
                // wants `'ok'`, not three runs that happen to abut. It also
                // keeps the run list short, which every surface pays for by
                // the cell.
                match self.runs.last_mut() {
                    Some((text, last)) if *last == Some(tok) => {
                        text.push_str(&self.code[start..end]);
                    }
                    _ => self
                        .runs
                        .push((self.code[start..end].to_string(), Some(tok))),
                }
            }
            None => self.plain.push_str(&self.code[start..end]),
        }
        self.cursor = end;
    }

    fn finish(mut self) -> Runs {
        if self.cursor < self.code.len() {
            let rest = self.code[self.cursor..].to_string();
            self.plain.push_str(&rest);
        }
        self.flush();
        self.runs
    }
}

/// The token class for one leaf, or `None` to leave it untinted.
///
/// The rules are stated over tree-sitter *node kinds* rather than over a
/// per-language word list, which is what makes one function serve eight
/// grammars — and what makes it keep serving a ninth nobody has added yet.
fn classify(node: Node<'_>) -> Option<Tok> {
    let kind = node.kind();

    // Anonymous leaves are the grammar's literal tokens, and their `kind` *is*
    // their text: `fn`, `let`, `if`, `{`, `->`. So "a keyword is an anonymous
    // leaf that reads as a word" replaces every hand-maintained keyword table
    // at once — the single largest thing a grammar buys here.
    if !node.is_named() {
        // ...with one exception, and it is the quote marks. Several grammars
        // spell a literal as `'` + `string_fragment` + `'`, so the delimiters
        // arrive here as anonymous leaves of a string parent. Tinting only the
        // fragment would leave `'ok'` reading as `ok` in string green with two
        // bare quotes around it — visibly worse than the hand-written scan this
        // replaced, which is how the case was found.
        if let Some(parent) = node.parent() {
            if is_str_kind(parent.kind()) {
                return Some(Tok::Str);
            }
            // The same shape once more, for a primitive type spelled as a
            // reserved word: TypeScript's `predefined_type` and PHP's
            // `primitive_type` wrap an anonymous `string`/`number` leaf, where
            // Java and C name theirs. Without this the identical `string` reads
            // as a type in one file and a keyword in the next.
            //
            // Words only, unlike the string rule above. A type *expression*
            // also holds punctuation — the `.` of Go's `time.Duration`, the
            // brackets of a slice or array type — and tinting those pulls the
            // separator into the type's colour while the package name beside it
            // stays plain, which reads as a mis-parse rather than as structure.
            if is_word(kind) && is_type_kind(parent.kind()) {
                return Some(Tok::Type);
            }
        }
        return is_word(kind).then_some(Tok::Keyword);
    }

    // `keyword_select`, `keyword_from`, … — the SQL grammar names its keywords
    // rather than leaving them anonymous.
    if kind.starts_with("keyword_") {
        return Some(Tok::Keyword);
    }

    // Strings before numbers, and both before anything else: several string
    // kinds spell an integer word inside themselves
    // (`interpreted_string_literal`), and reversing these two arms would tint
    // half a Go literal as a number.
    if kind.contains("comment") {
        return Some(Tok::Comment);
    }
    if is_str_kind(kind) {
        return Some(Tok::Str);
    }
    if is_number_kind(kind) {
        return Some(Tok::Number);
    }
    if is_type_kind(kind) {
        return Some(Tok::Type);
    }
    if is_function(node) {
        return Some(Tok::Function);
    }
    None
}

/// String and character literals, and the pieces grammars split them into
/// (`string_fragment`, `string_content`, `string_start`/`string_end`).
fn is_str_kind(kind: &str) -> bool {
    kind.contains("string") || kind.contains("char") || kind == "heredoc_body"
}

/// Whether a literal token reads as a word — the test that separates a keyword
/// from punctuation without knowing the language.
fn is_word(kind: &str) -> bool {
    kind.starts_with(|c: char| c.is_alphabetic())
        && kind.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Numeric literals, and the scalar constants that stand where a number stands.
///
/// The constants earn [`Tok::Number`] rather than a keyword hue for the reason
/// `json_runs` gives: they are values, and colouring them like structure makes
/// a body's shape unreadable at exactly the glance this colouring exists for.
fn is_number_kind(kind: &str) -> bool {
    matches!(
        kind,
        "integer"
            | "integer_literal"
            | "int_literal"
            | "float"
            | "float_literal"
            | "number"
            | "number_literal"
            | "numeric_literal"
            | "imaginary_literal"
            | "decimal_integer_literal"
            | "decimal_floating_point_literal"
            | "hex_integer_literal"
            | "octal_integer_literal"
            | "binary_integer_literal"
            | "boolean_literal"
            | "true"
            | "false"
            | "null"
            | "null_literal"
            | "nil"
            | "none"
            | "undefined"
    )
}

/// Type positions.
///
/// The `_type` suffix is the generalisation — Java spells `integral_type` and
/// `void_type`, Go and Rust `primitive_type`, TypeScript `predefined_type` —
/// and it is safe against non-type nodes because only *leaves* reach here, so a
/// `generic_type` or a `union_type` (both interior) never tests it.
fn is_type_kind(kind: &str) -> bool {
    kind == "type_identifier"
        || kind == "primitive_type"
        || kind == "predefined_type"
        || kind == "sized_type_specifier"
        || kind == "type_parameter"
        || kind.ends_with("_type")
}

/// Whether this leaf names a function — either the one being declared or the
/// one being called.
///
/// The distinction a keyword table cannot make, and the reason [`Tok::Function`]
/// is worth a palette entry: in Java or C most lines are declarations, and
/// without this the name of the thing being declared reads exactly like every
/// local variable around it.
fn is_function(node: Node<'_>) -> bool {
    let Some(parent) = node.parent() else {
        return false;
    };
    let pk = parent.kind();

    // The declared name. `function`/`method` covers `function_item` (Rust),
    // `function_declaration` (Go/JS), `method_declaration` (Java/Go),
    // `function_definition` (C/Python/PHP), `method_definition` (JS).
    if (pk.contains("function") || pk.contains("method")) && is_field(parent, "name", node) {
        return true;
    }
    // C spells the declared name through a declarator rather than a `name`
    // field: `function_definition` → `function_declarator` → `identifier`.
    if pk == "function_declarator" && is_field(parent, "declarator", node) {
        return true;
    }
    // The callee. Either directly (`foo()`), or through the field of a
    // selector when the call is `pkg.Foo()` / `obj.method()` — the shape most
    // Go, Java and TypeScript lines actually take.
    if is_call(pk) && (is_field(parent, "function", node) || is_field(parent, "name", node)) {
        return true;
    }
    if matches!(
        pk,
        "selector_expression" | "field_expression" | "member_expression" | "attribute"
    ) && (is_field(parent, "field", node)
        || is_field(parent, "property", node)
        || is_field(parent, "attribute", node))
    {
        return parent
            .parent()
            .is_some_and(|gp| is_call(gp.kind()) && is_field(gp, "function", parent));
    }
    false
}

/// Whether `kind` is a call expression in any of the eight grammars.
fn is_call(kind: &str) -> bool {
    kind == "call" || kind.contains("call_expression") || kind == "method_invocation"
}

/// Whether `parent`'s `field` is exactly `node`.
fn is_field(parent: Node<'_>, field: &str, node: Node<'_>) -> bool {
    parent
        .child_by_field_name(field)
        .is_some_and(|n| n.id() == node.id())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every grammar installs.
    ///
    /// A version skew between `tree-sitter` and a grammar crate is a silent
    /// failure otherwise: [`runs`] returns `None`, the caller renders the line
    /// plain, and the surface looks exactly like the flat text this whole
    /// change exists to stop rendering.
    #[test]
    fn every_grammar_arms() {
        for g in Grammar::ALL {
            assert!(
                runs("x", g).is_some(),
                "{g:?} did not install — a tree-sitter/grammar version skew"
            );
        }
    }

    /// Distinct cache slots, so no grammar parses with another's parser.
    #[test]
    fn every_grammar_holds_its_own_parser_slot() {
        let mut slots: Vec<usize> = Grammar::ALL.iter().map(|g| g.slot()).collect();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(slots.len(), Grammar::ALL.len(), "two grammars share a slot");
        assert!(
            slots.iter().all(|s| *s < Grammar::ALL.len()),
            "a slot is out of the cache's range: {slots:?}"
        );
    }
}
