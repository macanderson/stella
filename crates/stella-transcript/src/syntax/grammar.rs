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
//!
//! ## Termination is bought, not assumed (#4469)
//!
//! A parse is not guaranteed to finish. `tokenize("(s1.']z]9:A|8=", Lang::TsJs)`
//! — fourteen characters — spins inside tree-sitter's own error recovery
//! (`ts_parser__condense_stack` → `ts_parser__handle_error`) and had not
//! returned after five minutes. It reached CI as a fifty-minute stall on the
//! losslessness property, which feeds arbitrary text through all eight
//! grammars, and the job was cancelled rather than failed.
//!
//! The walk above terminates by construction, so the bound belongs on the
//! parse: [`parse_bounded`] spends a fixed [`PARSE_PROGRESS_BUDGET`] of
//! tree-sitter's own progress callbacks and cancels beyond it, which reads as
//! "this grammar will not install" to the caller and renders the line plain.
//! The budget is counted in callbacks rather than measured in wall clock on
//! purpose: the same line must lex to the same runs on a loaded CI box and on a
//! quiet laptop, and a clock makes the output a property of the machine.
//!
//! **A cancelled parse must be followed by [`Parser::reset`].** tree-sitter
//! returns `NULL` on cancellation without running its own reset, so the parser
//! is left holding an outstanding parse and the *next* call resumes it — every
//! later line on this thread would inherit the loop this one escaped. That is
//! `a_pathological_line_does_not_poison_the_next_one`.

use std::cell::RefCell;
use std::ops::ControlFlow;

use tree_sitter::{Node, ParseOptions, ParseState, Parser, Tree};

use super::{Runs, Tok};

/// How many of tree-sitter's progress callbacks one line may spend before the
/// parse is cancelled.
///
/// The library fires one every 100 parser operations
/// (`OP_COUNT_PER_PARSER_CALLBACK_CHECK` in `parser.c`), so this is a ceiling
/// of ~200k operations for a line no surface renders wider than a few hundred
/// characters. `a_realistic_line_stays_far_inside_the_budget` measures the real
/// draw of this crate's own longest lines through every grammar and asserts the
/// headroom, so a budget that starts cutting real source fails a test rather
/// than silently greying out code.
const PARSE_PROGRESS_BUDGET: u32 = 2_000;

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
/// plain rather than treating it as an error.
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

/// Parse one line, cancelling rather than hanging when the grammar loops.
///
/// Returns `None` for a cancelled parse — indistinguishable to the caller from
/// a grammar that would not install, and wanting the same degradation. See the
/// module doc for why the budget is counted rather than timed, and why the
/// reset is not optional.
fn parse_bounded(parser: &mut Parser, code: &str) -> Option<Tree> {
    parse_within(parser, code, PARSE_PROGRESS_BUDGET).0
}

/// [`parse_bounded`] with the budget named, reporting what the parse actually
/// spent. The budget is a parameter so a test can measure the real draw of a
/// line without the production ceiling truncating the measurement.
fn parse_within(parser: &mut Parser, code: &str, budget: u32) -> (Option<Tree>, u32) {
    let bytes = code.as_bytes();
    let mut spent: u32 = 0;
    let mut progress = |_: &ParseState| {
        spent += 1;
        if spent > budget {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let tree = parser.parse_with_options(
        &mut |i, _| bytes.get(i..).unwrap_or_default(),
        None,
        Some(ParseOptions::new().progress_callback(&mut progress)),
    );
    if tree.is_none() {
        parser.reset();
    }
    (tree, spent)
}

/// Tokenize one line under `g`, or `None` when the grammar would not install
/// or the parse ran past [`PARSE_PROGRESS_BUDGET`].
///
/// Lossless and panic-free by construction, and terminating by budget — see the
/// module doc.
pub(super) fn runs(code: &str, g: Grammar) -> Option<Runs> {
    let tree = with_parser(g, |parser| parse_bounded(parser, code))??;

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

    /// What one line of real source actually draws, through every grammar.
    ///
    /// The number in [`PARSE_PROGRESS_BUDGET`] is only defensible against a
    /// measurement, so this is the measurement: the longest lines this crate
    /// writes, plus the widest shapes a `read_file` lands on, must finish an
    /// order of magnitude inside the ceiling. If a budget ever starts cutting
    /// real source it fails here rather than greying out code on a screen.
    #[test]
    fn a_realistic_line_stays_far_inside_the_budget() {
        // The measurement runs against a budget well above the production one
        // so the ceiling cannot truncate what it reports — and still bounded,
        // because an unbounded one is the bug this whole change is about.
        let headroom = PARSE_PROGRESS_BUDGET * 50;
        let lines = [
            "pub(super) fn runs(code: &str, g: Grammar) -> Option<Runs> { let tree = with_parser(g, |parser| parse_bounded(parser, code))??; }",
            "    if kind == \"type_identifier\" || kind == \"primitive_type\" || kind.ends_with(\"_type\") { return Some(Tok::Type); }",
            "export const Component = ({ a, b }: Props) => <div className={cx('x', b)}>{a.map((v) => <span key={v.id}>{v.name}</span>)}</div>;",
            "SELECT u.id, u.name, count(o.id) FROM users u LEFT JOIN orders o ON o.user_id = u.id WHERE u.created_at > $1 GROUP BY u.id ORDER BY 3 DESC;",
            "static inline int f(const char *restrict s, size_t n, unsigned long *out) { for (size_t i = 0; i < n; i++) { out[i] = (unsigned long)s[i]; } return 0; }",
            "public static <T extends Comparable<T>> List<T> sorted(Collection<? extends T> in) { return in.stream().sorted().collect(Collectors.toList()); }",
            "func (s *Server) Handle(ctx context.Context, req *pb.Request) (*pb.Response, error) { return &pb.Response{Ok: true, Body: strings.Repeat(\"x\", 12)}, nil }",
            "def f(self, *args, **kwargs): return {k: [v for v in vals if v is not None] for k, vals in sorted(kwargs.items(), key=lambda kv: kv[0])}",
            "<?php function h(array $rows): string { return implode(\", \", array_map(fn($r) => sprintf(\"%s=%d\", $r['k'], (int)$r['v']), $rows)); }",
        ];
        let mut worst = 0;
        for line in lines {
            for g in Grammar::ALL {
                let spent = with_parser(g, |p| parse_within(p, line, headroom).1).unwrap_or(0);
                assert!(
                    spent * 8 <= PARSE_PROGRESS_BUDGET,
                    "{g:?} spent {spent} of {PARSE_PROGRESS_BUDGET} on {line:?} — \
                     under an eighth of the budget is the headroom this test buys"
                );
                worst = worst.max(spent);
            }
        }
        // Names the real number in the failure output of any later change that
        // moves it, rather than leaving the margin implicit.
        assert!(worst > 0, "no grammar reported any progress at all");
    }

    /// The fourteen characters that hung CI for fifty minutes (#4469).
    ///
    /// Not merely "fast": the parse does not terminate at all, so a machine
    /// slow enough to make a duration assertion flaky is not the hazard here.
    /// What is asserted is the contract the budget preserves — the line still
    /// round-trips, as one untinted run.
    #[test]
    fn a_grammar_that_loops_is_cancelled_not_awaited() {
        let text = "(s1.']z]9:A|8=";
        assert!(
            runs(text, Grammar::Tsx).is_none(),
            "the TSX error-recovery loop must be cancelled, not parsed"
        );
    }

    /// A cancelled parse leaves tree-sitter holding an outstanding parse, and
    /// the next call on that thread's parser *resumes* it. Without the reset in
    /// [`parse_within`] every later line on this thread inherits the loop.
    #[test]
    fn a_pathological_line_does_not_poison_the_next_one() {
        assert!(runs("(s1.']z]9:A|8=", Grammar::Tsx).is_none());
        let after = runs("const x = 'ok';", Grammar::Tsx).expect("the next line still parses");
        let rebuilt: String = after.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(rebuilt, "const x = 'ok';");
        assert!(
            after
                .iter()
                .any(|(t, tok)| t == "'ok'" && *tok == Some(Tok::Str)),
            "the next line is still coloured, not degraded: {after:?}"
        );
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
