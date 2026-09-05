//! Tree-sitter parsing: turn a source file into [`Symbol`]s and raw
//! [`ImportSpec`]s. Pure and synchronous (extraction logic stays sync, which
//! keeps it easy to test); the indexer ([`crate::store`]) is the only thing
//! that touches I/O around it.
//!
//! **Skip-with-record, never abort** (L-L1): [`parse_file`] returns `None`
//! when a grammar cannot be armed or the
//! source cannot be parsed at all, and the indexer records that as a parse
//! failure and moves on. Tree-sitter is error-tolerant, so a *syntactically
//! broken* file still yields a tree with `ERROR` nodes from which whatever
//! parsed is extracted best-effort — a broken file loses only its broken
//! regions, not the whole index batch.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

use crate::error::GraphError;
use crate::import::ImportSpec;
use crate::lang::Language;
use crate::symbol::{CallSite, Symbol, SymbolKind};

/// Compiled grammars + queries for every supported language. [`Grammars::load`]
/// compiles the 11 query pairs once per process — measured at ~100ms in a
/// release build (#4782) — and caches the result behind a process-wide
/// `OnceLock`, so every [`crate::graph::CodeGraph::open`] and
/// [`crate::storage::StorageExtractor::new`] after the first shares the same
/// `Arc` rather than paying the compile again. `Send + Sync`, so the cached
/// value is reused by the background watcher too.
///
/// `None` per language means "this build did not compile that grammar"
/// (#1268), not "loading failed" — a load failure is still a hard error,
/// because a `.scm` that does not compile is a programmer error the crate's
/// own tests catch.
pub(crate) struct Grammars {
    rust: Option<LangPack>,
    python: Option<LangPack>,
    javascript: Option<LangPack>,
    typescript: Option<LangPack>,
    tsx: Option<LangPack>,
    sql: Option<LangPack>,
    go: Option<LangPack>,
    java: Option<LangPack>,
    c: Option<LangPack>,
    cpp: Option<LangPack>,
    php: Option<LangPack>,
}

struct LangPack {
    language: tree_sitter::Language,
    symbols: Query,
    imports: Query,
    calls: Query,
}

impl LangPack {
    fn load(lang: Language) -> Result<Option<LangPack>, GraphError> {
        let Some(language) = lang.ts_language() else {
            return Ok(None);
        };
        let symbols =
            Query::new(&language, lang.symbol_query()).map_err(|e| GraphError::Query {
                lang: lang.tag(),
                kind: "symbol",
                message: e.to_string(),
            })?;
        let imports =
            Query::new(&language, lang.import_query()).map_err(|e| GraphError::Query {
                lang: lang.tag(),
                kind: "import",
                message: e.to_string(),
            })?;
        let calls = Query::new(&language, lang.call_query()).map_err(|e| GraphError::Query {
            lang: lang.tag(),
            kind: "call",
            message: e.to_string(),
        })?;
        Ok(Some(LangPack {
            language,
            symbols,
            imports,
            calls,
        }))
    }
}

impl Grammars {
    /// Compile every grammar's query pair, or hand back the already-compiled
    /// set. Fails loudly only if one of the crate's own `.scm` strings does
    /// not compile — a programmer error the crate's tests catch; a failure
    /// here leaves the cache unset, so a later call retries the compile
    /// rather than latching the error in.
    pub(crate) fn load() -> Result<Arc<Grammars>, GraphError> {
        static CACHE: OnceLock<Arc<Grammars>> = OnceLock::new();
        if let Some(cached) = CACHE.get() {
            return Ok(Arc::clone(cached));
        }
        let grammars = Arc::new(Grammars {
            rust: LangPack::load(Language::Rust)?,
            python: LangPack::load(Language::Python)?,
            javascript: LangPack::load(Language::JavaScript)?,
            go: LangPack::load(Language::Go)?,
            java: LangPack::load(Language::Java)?,
            c: LangPack::load(Language::C)?,
            cpp: LangPack::load(Language::Cpp)?,
            php: LangPack::load(Language::Php)?,
            typescript: LangPack::load(Language::TypeScript)?,
            tsx: LangPack::load(Language::Tsx)?,
            sql: LangPack::load(Language::Sql)?,
        });
        // `OnceLock::get_or_try_init` is still unstable (rust#109737); racing
        // in with a redundant compile and losing is the fallback, not a bug —
        // whichever caller's `Arc` lands first is the one everyone shares.
        Ok(Arc::clone(CACHE.get_or_init(move || grammars)))
    }

    /// `None` when this build carries no grammar for `lang`. Callers already
    /// return `Option`, so a trimmed build degrades along the path they
    /// already handle rather than a new one.
    fn pack(&self, lang: Language) -> Option<&LangPack> {
        match lang {
            Language::Rust => self.rust.as_ref(),
            Language::Python => self.python.as_ref(),
            Language::JavaScript => self.javascript.as_ref(),
            Language::TypeScript => self.typescript.as_ref(),
            Language::Tsx => self.tsx.as_ref(),
            Language::Sql => self.sql.as_ref(),
            Language::Go => self.go.as_ref(),
            Language::Java => self.java.as_ref(),
            Language::C => self.c.as_ref(),
            Language::Cpp => self.cpp.as_ref(),
            Language::Php => self.php.as_ref(),
            // Markdown and TOML are read by this crate's own line scans, not
            // by a grammar, and `parse_file` answers both before either
            // reaches here.
            Language::Markdown | Language::Toml => None,
        }
    }
}

/// Everything extracted from one file.
pub(crate) struct Parsed {
    pub symbols: Vec<Symbol>,
    pub imports: Vec<ImportSpec>,
    pub calls: Vec<CallSite>,
}

/// Parse source into a raw tree for a storage adapter's structural walk
/// ([`crate::storage`]). `None` = un-armable grammar or wholly unparseable
/// input, same contract as [`parse_file`].
pub(crate) fn parse_tree(
    grammars: &Grammars,
    lang: Language,
    source: &str,
) -> Option<tree_sitter::Tree> {
    let pack = grammars.pack(lang)?;
    let mut parser = Parser::new();
    if parser.set_language(&pack.language).is_err() {
        return None;
    }
    parser.parse(source.as_bytes(), None)
}

/// Parse SQL source into a raw tree for the SQL adapter.
pub(crate) fn parse_sql_tree(grammars: &Grammars, source: &str) -> Option<tree_sitter::Tree> {
    parse_tree(grammars, Language::Sql, source)
}

/// Parse one file's `source`. `None` = un-armable grammar or wholly
/// unparseable input → the caller records a skip and continues.
pub(crate) fn parse_file(grammars: &Grammars, lang: Language, source: &str) -> Option<Parsed> {
    // Markdown ahead of the grammar lookup, because it has none: its sections
    // and link edges come from line scans in this crate ([`crate::markdown`]),
    // which is why it is the one language present in every build. A document
    // with no headings yields no symbols and is still a successful parse — it
    // is a file the index knows about, with a file-level vector, exactly like
    // a source file that declares nothing.
    if lang == Language::Markdown {
        return Some(Parsed {
            symbols: crate::markdown::sections(source),
            imports: crate::markdown::links(source),
            calls: Vec::new(),
        });
    }
    // TOML likewise (#4492). A context record names no other file, so it has
    // no import edges — unlike markdown, whose links are exactly the relation
    // `code_graph_imports` models. A record with no table headers is still a
    // successful parse: a file the index knows about, with a file-level
    // vector, the same contract markdown's headingless document has.
    if lang == Language::Toml {
        return Some(Parsed {
            symbols: crate::record_toml::tables(source),
            imports: Vec::new(),
            calls: Vec::new(),
        });
    }
    let pack = grammars.pack(lang)?;
    let mut parser = Parser::new();
    if parser.set_language(&pack.language).is_err() {
        return None;
    }
    let tree = parser.parse(source.as_bytes(), None)?;
    let root = tree.root_node();
    let src = source.as_bytes();

    let mut symbols = extract_symbols(&pack.symbols, root, src);

    // ORM pattern detection: scan the AST for table-like definitions
    // (Diesel `table!` macros, Django/SQLAlchemy model classes) and add
    // them as Table symbols. SQL DDL is the ground truth; these are hints.
    match lang {
        Language::Rust => symbols.extend(extract_rust_orm_tables(root, src)),
        Language::Python => symbols.extend(extract_python_orm_tables(root, src)),
        _ => {}
    }

    let imports = match lang {
        Language::Rust => extract_rust_imports(&pack.imports, root, src),
        Language::Python => extract_python_imports(&pack.imports, root, src),
        Language::JavaScript | Language::TypeScript | Language::Tsx => {
            extract_ts_imports(&pack.imports, root, src)
        }
        Language::Sql => Vec::new(), // SQL has no imports
        // Unreachable: markdown and TOML both returned above, before the pack
        // lookup — markdown's link edges ride that early return (#3103), and
        // a context record has no edges to extract (#4492).
        Language::Markdown | Language::Toml => Vec::new(),
        Language::Go => extract_go_imports(&pack.imports, root, src),
        Language::Java => extract_java_imports(&pack.imports, root, src),
        // C++ shares C's reader: `#include` is the same preprocessor
        // directive in both, quoted and angled forms included (#3184).
        Language::C | Language::Cpp => extract_c_imports(&pack.imports, root, src),
        Language::Php => extract_php_imports(&pack.imports, root, src),
    };
    let calls = extract_calls(&pack.calls, root, src);
    Some(Parsed {
        symbols,
        imports,
        calls,
    })
}

/// Decode call matches (#335, B1): every pattern captures the callee's name
/// node as `@callee`, so the decode is uniform across languages — take the
/// node's text, keep it only if it is a bare identifier, record the node's
/// 1-based line. Dedup by the name node's byte range in case two patterns
/// ever overlap on one node; distinct calls to the same name each keep their
/// own row (the line is part of the fact).
fn extract_calls(query: &Query, root: Node, src: &[u8]) -> Vec<CallSite> {
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut seen: HashSet<(usize, usize)> = HashSet::new();
    let mut out = Vec::new();
    let mut matches = cursor.matches(query, root, src);
    while let Some(m) = matches.next() {
        for cap in m.captures() {
            if names[cap.index as usize] != "callee" {
                continue;
            }
            let Ok(text) = cap.node.utf8_text(src) else {
                continue;
            };
            // The queries pin name nodes, but grammar wildcards (`(_)` in
            // field positions) can admit non-identifier shapes; skip those
            // rather than store a fragment no definition could ever match.
            if !is_bare_identifier(text) {
                continue;
            }
            if !seen.insert((cap.node.start_byte(), cap.node.end_byte())) {
                continue;
            }
            out.push(CallSite {
                callee: text.to_string(),
                line: cap.node.start_position().row as u32 + 1,
            });
        }
    }
    out.sort_by(|a, b| a.line.cmp(&b.line).then_with(|| a.callee.cmp(&b.callee)));
    out
}

/// A single unqualified identifier as the indexed languages spell them
/// (`$` included for JavaScript). Anything else — qualified paths, computed
/// callees, PHP `$variable` callables — is not a name the symbol index could
/// hold, so storing it would only manufacture unanswerable edges.
fn is_bare_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_alphabetic() || first == '_' || first == '$')
        && chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

/// Decode symbol matches. A method is captured by both the general function
/// pattern and the enclosing-type pattern; dedup by the name node's byte
/// range and let the higher-[`SymbolKind::rank`] kind win.
fn extract_symbols(query: &Query, root: Node, src: &[u8]) -> Vec<Symbol> {
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut dedup: HashMap<(usize, usize), Symbol> = HashMap::new();

    let mut matches = cursor.matches(query, root, src);
    while let Some(m) = matches.next() {
        let mut name: Option<&str> = None;
        let mut name_range: Option<(usize, usize)> = None;
        let mut kind: Option<SymbolKind> = None;
        let mut span: Option<(u32, u32)> = None;

        for cap in m.captures() {
            let cap_name = names[cap.index as usize];
            if cap_name == "name" {
                name = cap.node.utf8_text(src).ok();
                name_range = Some((cap.node.start_byte(), cap.node.end_byte()));
            } else if let Some(k) = SymbolKind::from_capture(cap_name) {
                kind = Some(k);
                span = Some((
                    cap.node.start_position().row as u32 + 1,
                    cap.node.end_position().row as u32 + 1,
                ));
            }
        }

        if let (Some(name), Some(range), Some(kind), Some((start, end))) =
            (name, name_range, kind, span)
        {
            if name.is_empty() {
                continue;
            }
            let symbol = Symbol {
                name: name.to_string(),
                kind,
                start_line: start,
                end_line: end,
            };
            match dedup.get(&range) {
                Some(existing) if existing.kind.rank() >= kind.rank() => {}
                _ => {
                    dedup.insert(range, symbol);
                }
            }
        }
    }

    let mut out: Vec<Symbol> = dedup.into_values().collect();
    out.sort_by(|a, b| {
        a.start_line
            .cmp(&b.start_line)
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

fn extract_rust_imports(query: &Query, root: Node, src: &[u8]) -> Vec<ImportSpec> {
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut out = Vec::new();
    let mut matches = cursor.matches(query, root, src);
    while let Some(m) = matches.next() {
        for cap in m.captures() {
            let Ok(text) = cap.node.utf8_text(src) else {
                continue;
            };
            match names[cap.index as usize] {
                "use" => out.push(ImportSpec::RustUse {
                    specifier: text.to_string(),
                }),
                "mod" => out.push(ImportSpec::RustMod {
                    name: text.to_string(),
                }),
                _ => {}
            }
        }
    }
    out
}

fn extract_ts_imports(query: &Query, root: Node, src: &[u8]) -> Vec<ImportSpec> {
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut out = Vec::new();
    let mut matches = cursor.matches(query, root, src);
    while let Some(m) = matches.next() {
        let mut source_text: Option<&str> = None;
        let mut callee: Option<&str> = None;
        for cap in m.captures() {
            match names[cap.index as usize] {
                "source" => source_text = cap.node.utf8_text(src).ok(),
                "callee" => callee = cap.node.utf8_text(src).ok(),
                _ => {}
            }
        }
        let Some(specifier) = source_text else {
            continue;
        };
        // A match carrying a @callee is an arbitrary `f('str')` call — keep it
        // only when the callee is `require`; every other pattern (import /
        // export-from / dynamic import) carries no @callee and is always a
        // real specifier.
        if let Some(callee) = callee
            && callee != "require"
        {
            continue;
        }
        out.push(classify_ts_specifier(specifier));
    }
    out
}

fn classify_ts_specifier(specifier: &str) -> ImportSpec {
    if specifier.starts_with("./")
        || specifier.starts_with("../")
        || specifier == "."
        || specifier == ".."
    {
        ImportSpec::TsRelative {
            specifier: specifier.to_string(),
        }
    } else {
        ImportSpec::Bare {
            specifier: specifier.to_string(),
        }
    }
}

fn extract_python_imports(query: &Query, root: Node, src: &[u8]) -> Vec<ImportSpec> {
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut out = Vec::new();
    let mut matches = cursor.matches(query, root, src);
    while let Some(m) = matches.next() {
        for cap in m.captures() {
            match names[cap.index as usize] {
                "import" => decode_py_import(cap.node, src, &mut out),
                "from_import" => decode_py_from_import(cap.node, src, &mut out),
                _ => {}
            }
        }
    }
    out
}

/// `import a`, `import a.b`, `import a as b`, `import a, b` — all absolute,
/// recorded unresolved.
fn decode_py_import(node: Node, src: &[u8], out: &mut Vec<ImportSpec>) {
    let mut cursor = node.walk();
    for child in node.children_by_field_name("name", &mut cursor) {
        if let Some(module) = py_module_name(child, src) {
            out.push(ImportSpec::PyAbsolute { specifier: module });
        }
    }
}

/// `from <module> import <names>` — the relative-import decode.
/// Counts the leading dots (`import_prefix`) to a package level and
/// carries the optional dotted module path plus imported names to
/// [`crate::import::resolve`].
fn decode_py_from_import(node: Node, src: &[u8], out: &mut Vec<ImportSpec>) {
    let module = node.child_by_field_name("module_name");
    let mut names = Vec::new();
    let mut cursor = node.walk();
    for child in node.children_by_field_name("name", &mut cursor) {
        if let Some(name) = py_module_name(child, src) {
            names.push(name);
        }
    }

    match module {
        Some(m) if m.kind() == "relative_import" => {
            let mut level = 0usize;
            let mut module_path: Option<String> = None;
            let mut c = m.walk();
            for child in m.children(&mut c) {
                match child.kind() {
                    "import_prefix" => {
                        level = child
                            .utf8_text(src)
                            .map(|t| t.chars().filter(|c| *c == '.').count())
                            .unwrap_or(1);
                    }
                    "dotted_name" => {
                        module_path = child.utf8_text(src).ok().map(|s| s.to_string());
                    }
                    _ => {}
                }
            }
            out.push(ImportSpec::PyRelative {
                level: level.max(1),
                module: module_path,
                names,
                text: m.utf8_text(src).unwrap_or(".").to_string(),
            });
        }
        Some(m) if m.kind() == "dotted_name" => {
            if let Ok(text) = m.utf8_text(src) {
                out.push(ImportSpec::PyAbsolute {
                    specifier: text.to_string(),
                });
            }
        }
        _ => {}
    }
}

/// The module name of an imported item: the aliased original for
/// `x as y`, otherwise the node's own text.
fn py_module_name(node: Node, src: &[u8]) -> Option<String> {
    let target = if node.kind() == "aliased_import" {
        node.child_by_field_name("name")?
    } else {
        node
    };
    target.utf8_text(src).ok().map(|s| s.to_string())
}

/// Detect Diesel `table!` macro invocations and extract them as Table symbols.
/// The macro looks like: `diesel::table! { users (id) { ... } }` or
/// `table! { ... }`. Every *direct* identifier child of the macro's token
/// tree is a table name — one for the single-table form, several for the
/// multi-table form Diesel also accepts. Identifiers nested deeper (the
/// column list, the primary-key group) sit inside their own token trees and
/// are therefore never mistaken for tables.
fn extract_rust_orm_tables(root: Node, src: &[u8]) -> Vec<Symbol> {
    let mut out = Vec::new();

    // Explicit worklist rather than recursion (see `walk::walk_indexable`):
    // source files are environment-controlled, and a deeply-nested tree would
    // overflow the thread stack and abort the process. Children are pushed in
    // reverse so popping preserves source order.
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "macro_invocation"
            && let Some(macro_id) = node.child_by_field_name("macro")
            && let Ok(name) = macro_id.utf8_text(src)
        {
            let name_lower = name.to_ascii_lowercase();
            if name_lower == "table" || name_lower.ends_with("::table") {
                let mut tc = node.walk();
                for child in node.children(&mut tc) {
                    if child.kind() == "token_tree" {
                        let mut inner_tc = child.walk();
                        for inner in child.children(&mut inner_tc) {
                            if inner.kind() == "identifier"
                                && let Ok(table_name) = inner.utf8_text(src)
                                && !table_name.is_empty()
                            {
                                out.push(Symbol {
                                    name: table_name.to_string(),
                                    kind: SymbolKind::Table,
                                    start_line: node.start_position().row as u32 + 1,
                                    end_line: node.end_position().row as u32 + 1,
                                });
                            }
                        }
                    }
                }
            }
        }

        for idx in (0..node.child_count()).rev() {
            if let Some(child) = node.child(idx) {
                stack.push(child);
            }
        }
    }

    out
}

/// Detect Django/SQLAlchemy model classes and extract them as Table symbols.
/// Django: `class Payment(models.Model):` — superclass contains `Model`.
/// SQLAlchemy: `class Payment(Base):` with `__tablename__ = "payments"`.
fn extract_python_orm_tables(root: Node, src: &[u8]) -> Vec<Symbol> {
    let mut out = Vec::new();

    // Explicit worklist, same reasoning as `extract_rust_orm_tables`.
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "class_definition" {
            let tablename = python_tablename_value(node, src);
            let is_model = tablename.is_some() || check_python_orm_superclass(node, src);
            if is_model
                && let Some(name_node) = node.child_by_field_name("name")
                && let Ok(name) = name_node.utf8_text(src)
            {
                let table_name = tablename.unwrap_or_else(|| python_class_to_table_name(name));
                out.push(Symbol {
                    name: table_name,
                    kind: SymbolKind::Table,
                    start_line: node.start_position().row as u32 + 1,
                    end_line: node.end_position().row as u32 + 1,
                });
            }
        }

        for idx in (0..node.child_count()).rev() {
            if let Some(child) = node.child(idx) {
                stack.push(child);
            }
        }
    }

    out
}

/// Check if a Python class inherits from a known ORM base class.
/// Matches the base expression's innermost identifier exactly (`Model`,
/// `models.Model`, `Base`, `sqlalchemy.orm.Base`, `declarative_base()`) so
/// unrelated types that merely end in the same substring — `ViewModel`,
/// `DatabaseModel` — are not mistaken for ORM bases.
fn check_python_orm_superclass(class_node: Node, src: &[u8]) -> bool {
    let Some(superclasses) = class_node.child_by_field_name("superclasses") else {
        return false;
    };
    let mut cursor = superclasses.walk();
    for arg in superclasses.children(&mut cursor) {
        if let Ok(text) = arg.utf8_text(src) {
            let ident = python_base_identifier(text.trim());
            if ident == "Model" || ident == "Base" || ident == "declarative_base" {
                return true;
            }
        }
    }
    false
}

/// The innermost identifier of a (possibly qualified, possibly called) base
/// class expression: `models.Model` -> `Model`, `declarative_base()` ->
/// `declarative_base`.
fn python_base_identifier(text: &str) -> &str {
    let text = text.strip_suffix("()").unwrap_or(text);
    text.rsplit('.').next().unwrap_or(text)
}

/// Extract the string value of `__tablename__ = "..."` from a Python class
/// body, if present.
fn python_tablename_value(class_node: Node, src: &[u8]) -> Option<String> {
    let body = class_node.child_by_field_name("body")?;
    let mut cursor = body.walk();
    for stmt in body.children(&mut cursor) {
        // Class-body statements are wrapped in `expression_statement`; the
        // `assignment` itself is an unnamed child of that wrapper.
        let assignment = if stmt.kind() == "assignment" {
            Some(stmt)
        } else if stmt.kind() == "expression_statement" {
            let mut inner = stmt.walk();
            stmt.children(&mut inner).find(|c| c.kind() == "assignment")
        } else {
            None
        };
        let Some(assignment) = assignment else {
            continue;
        };
        if let Some(left) = assignment.child_by_field_name("left")
            && let Ok(text) = left.utf8_text(src)
            && text == "__tablename__"
            && let Some(right) = assignment.child_by_field_name("right")
        {
            return python_string_literal_value(right, src);
        }
    }
    None
}

/// The inner text of a Python string literal node, quotes stripped.
fn python_string_literal_value(node: Node, src: &[u8]) -> Option<String> {
    if node.kind() != "string" {
        return None;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "string_content" {
            return child.utf8_text(src).ok().map(|s| s.to_string());
        }
    }
    None
}

/// Naive Django-style class→table conversion: CamelCase → snake_case + plural.
/// `Payment` → `payments`, `UserProfile` → `user_profiles`.
fn python_class_to_table_name(name: &str) -> String {
    let mut snake = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            snake.push('_');
        }
        snake.push(ch.to_ascii_lowercase());
    }
    // Naive pluralization
    if snake.ends_with('s') {
        format!("{snake}es")
    } else {
        format!("{snake}s")
    }
}

#[cfg(test)]
mod tests;

/// Pull the first quoted run (double first, then single) out of a node's
/// text.
///
/// Go, C, and PHP all name their target inside quotes, and the surrounding
/// syntax differs enough (`import ( … )`, `#include "x.h"`, `require '…';`)
/// that reading the literal is steadier than matching each grammar's
/// internal node names — which vary across grammar releases.
fn first_quoted(text: &str) -> Option<&str> {
    for quote in ['"', '\''] {
        if let Some(open) = text.find(quote) {
            let rest = &text[open + 1..];
            if let Some(len) = rest.find(quote) {
                let inner = &rest[..len];
                if !inner.is_empty() {
                    return Some(inner);
                }
            }
        }
    }
    None
}

/// Go: every spec inside `import ( … )` or a single-line `import "…"`. Go
/// import paths are module-qualified and never relative, so they stay
/// unresolved rather than being guessed at against the tree.
fn extract_go_imports(query: &Query, root: Node, src: &[u8]) -> Vec<ImportSpec> {
    each_import_node(query, root, src, |text| {
        let mut out = Vec::new();
        // A grouped block holds several quoted paths; take each in turn.
        let mut rest = text;
        while let Some(open) = rest.find('"') {
            let after = &rest[open + 1..];
            let Some(len) = after.find('"') else { break };
            let specifier = &after[..len];
            if !specifier.is_empty() {
                out.push(ImportSpec::Bare {
                    specifier: specifier.to_string(),
                });
            }
            rest = &after[len + 1..];
        }
        out
    })
}

/// Java: `import a.b.C;` — a package path. Resolving it to a file depends on
/// the conventional package-as-directory layout, which is a build-system
/// question rather than a syntactic one, so the edge is recorded unresolved.
fn extract_java_imports(query: &Query, root: Node, src: &[u8]) -> Vec<ImportSpec> {
    each_import_node(query, root, src, |text| {
        let specifier = text
            .trim()
            .trim_start_matches("import")
            .trim()
            .trim_start_matches("static")
            .trim()
            .trim_end_matches(';')
            .trim();
        if specifier.is_empty() {
            return Vec::new();
        }
        vec![ImportSpec::Bare {
            specifier: specifier.to_string(),
        }]
    })
}

/// C: `#include "x.h"` resolves against the including file's directory;
/// `#include <stdio.h>` names a system header that is not in the tree, so it
/// is recorded unresolved instead of being resolved to a coincidental match.
fn extract_c_imports(query: &Query, root: Node, src: &[u8]) -> Vec<ImportSpec> {
    each_import_node(query, root, src, |text| {
        if let Some(specifier) = first_quoted(text) {
            return vec![ImportSpec::PathRelative {
                specifier: specifier.to_string(),
            }];
        }
        let angled = text.find('<').and_then(|open| {
            let rest = &text[open + 1..];
            rest.find('>').map(|len| &rest[..len])
        });
        match angled.filter(|s| !s.is_empty()) {
            Some(specifier) => vec![ImportSpec::Bare {
                specifier: specifier.to_string(),
            }],
            None => Vec::new(),
        }
    })
}

/// PHP: `require`/`include` name a literal path and resolve; `use` names a
/// NAMESPACE, which maps to a file only through composer.json's PSR-4
/// autoload map — so those are captured and left unresolved.
fn extract_php_imports(query: &Query, root: Node, src: &[u8]) -> Vec<ImportSpec> {
    each_import_node(query, root, src, |text| {
        let trimmed = text.trim();
        if trimmed.starts_with("use") {
            let specifier = trimmed
                .trim_start_matches("use")
                .trim()
                .trim_end_matches(';')
                .trim();
            return if specifier.is_empty() {
                Vec::new()
            } else {
                vec![ImportSpec::Bare {
                    specifier: specifier.to_string(),
                }]
            };
        }
        match first_quoted(trimmed) {
            Some(specifier) => vec![ImportSpec::PathRelative {
                specifier: specifier.to_string(),
            }],
            None => Vec::new(),
        }
    })
}

/// Shared driver: run the import query and hand each matched node's text to
/// a language-specific decoder.
fn each_import_node(
    query: &Query,
    root: Node,
    src: &[u8],
    decode: impl Fn(&str) -> Vec<ImportSpec>,
) -> Vec<ImportSpec> {
    let mut cursor = QueryCursor::new();
    let mut out = Vec::new();
    let mut matches = cursor.matches(query, root, src);
    while let Some(m) = matches.next() {
        for cap in m.captures() {
            if let Ok(text) = cap.node.utf8_text(src) {
                out.extend(decode(text));
            }
        }
    }
    out
}
