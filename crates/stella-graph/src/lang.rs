//! The set of languages the code graph indexes, and the mapping from a file
//! extension to its tree-sitter grammar + query pair.
//!
//! Grammars are **native**, not WASM: each one is linked in at build time
//! from its own `tree-sitter-*` crate, so there is no runtime loader and no
//! asset to resolve relative to the binary's install path.

use std::path::Path;

use crate::queries;

/// A language the indexer understands. `Tsx` is split from `TypeScript`
/// because the two use different tree-sitter grammars (`LANGUAGE_TYPESCRIPT`
/// vs `LANGUAGE_TSX`) even though they share the same query strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
    Sql,
    Go,
    Java,
    C,
    Php,
    /// Prose, split on its heading hierarchy by this crate's private
    /// `markdown` module rather than by a grammar — the one language here
    /// whose parser is this crate's own, and so the one that is present in
    /// every build.
    Markdown,
}

impl Language {
    /// Classify a path by extension, or `None` if it is not an indexable
    /// source file. This is the single gate the directory walk
    /// (`crate::walk`) uses to decide what to open.
    ///
    /// `None` also covers "the extension is known but its grammar was not
    /// compiled into this build" (#1268).
    ///
    /// Filtering here rather than deeper is what keeps a feature-trimmed build
    /// honest: a `.go` file in a build without `lang-go` is treated exactly
    /// like a `.rb` file — a language this build does not index — instead of
    /// being routed to a parser that would return no symbols and read to the
    /// agent as "this file declares nothing". [`Language::compiled_in`]
    /// answers "which languages does this build know?" for anything that needs
    /// to say so out loud.
    pub fn from_path(path: &Path) -> Option<Language> {
        let ext = path.extension()?.to_str()?;
        let lang = match ext {
            "rs" => Language::Rust,
            "py" | "pyi" => Language::Python,
            "js" | "jsx" | "mjs" | "cjs" => Language::JavaScript,
            "ts" | "mts" | "cts" => Language::TypeScript,
            "tsx" => Language::Tsx,
            "go" => Language::Go,
            "java" => Language::Java,
            // C and C++ both index under the C grammar. A C++ file still
            // yields its free functions, structs, and many declarations under
            // it — tree-sitter is error-tolerant, so the class/template/
            // namespace syntax it cannot parse degrades to error nodes around
            // the symbols it can, and partial symbols beat the zero a
            // `.cpp` gets when it maps to no language at all (search then has
            // nothing to rank and falls to a lexical scan). This extends the
            // header-under-C precedent to C++ source; a dedicated
            // `tree-sitter-cpp` grammar is the sharper fix (#3184).
            "c" | "h" | "cpp" | "cc" | "cxx" | "c++" | "hpp" | "hh" | "hxx" => Language::C,
            "php" => Language::Php,
            "sql" => Language::Sql,
            "md" | "markdown" => Language::Markdown,
            _ => return None,
        };
        lang.is_compiled_in().then_some(lang)
    }

    /// Every language this build can actually index, in `tag()` order. The
    /// honest answer to "why is my Go code not in the graph?" on a trimmed
    /// build, and the thing a diagnostic surface should print.
    pub fn compiled_in() -> Vec<Language> {
        Self::ALL
            .iter()
            .copied()
            .filter(|l| l.is_compiled_in())
            .collect()
    }

    /// Every language the enum knows, compiled in or not.
    pub const ALL: [Language; 11] = [
        Language::Rust,
        Language::Python,
        Language::JavaScript,
        Language::TypeScript,
        Language::Tsx,
        Language::Sql,
        Language::Go,
        Language::Java,
        Language::C,
        Language::Php,
        Language::Markdown,
    ];

    /// Whether this build can parse this language.
    ///
    /// For every grammar-backed language that is "did this build compile the
    /// grammar in" (#1268). [`Language::Markdown`] is parsed by this crate's
    /// private `markdown` module, a line scan with no feature gate and nothing
    /// to link, so it is always available — which is why this is not simply
    /// `ts_language().is_some()`.
    pub fn is_compiled_in(self) -> bool {
        match self {
            Language::Markdown => true,
            _ => self.ts_language().is_some(),
        }
    }

    /// Whether this language is parsed by a tree-sitter grammar rather than
    /// by this crate's own reader. Only [`Language::Markdown`] answers `false`.
    ///
    /// Test-only: production code has no reason to distinguish "compiled in"
    /// from "conceptually grammar-backed" ([`Language::is_compiled_in`]
    /// already answers the question anything else would ask).
    #[cfg(test)]
    pub(crate) fn is_grammar_backed(self) -> bool {
        !matches!(self, Language::Markdown)
    }

    /// Stable lowercase tag stored in `code_graph_files.language` and used in
    /// error messages. Never rename without a migration.
    pub fn tag(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Python => "python",
            Language::JavaScript => "javascript",
            Language::TypeScript => "typescript",
            Language::Tsx => "tsx",
            Language::Sql => "sql",
            Language::Go => "go",
            Language::Java => "java",
            Language::C => "c",
            Language::Php => "php",
            Language::Markdown => "markdown",
        }
    }

    /// The native tree-sitter grammar for this language, or `None` when this
    /// build did not compile it in (#1268).
    ///
    /// The arms are `cfg`-gated individually and a catch-all absorbs whatever
    /// is off, so a trimmed build still type-checks without the enum losing
    /// variants — keeping the enum whole is what lets `tag()` round-trip a
    /// language tag stored by a *differently*-featured build that wrote the
    /// same `codegraph.db`.
    pub(crate) fn ts_language(self) -> Option<tree_sitter::Language> {
        Some(match self {
            #[cfg(feature = "lang-rust")]
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            #[cfg(feature = "lang-python")]
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            #[cfg(feature = "lang-javascript")]
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            #[cfg(feature = "lang-typescript")]
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            #[cfg(feature = "lang-typescript")]
            Language::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            #[cfg(feature = "lang-sql")]
            Language::Sql => tree_sitter_sequel::LANGUAGE.into(),
            #[cfg(feature = "lang-go")]
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            #[cfg(feature = "lang-java")]
            Language::Java => tree_sitter_java::LANGUAGE.into(),
            #[cfg(feature = "lang-c")]
            Language::C => tree_sitter_c::LANGUAGE.into(),
            // The HTML-embedding grammar, not `LANGUAGE_PHP_ONLY`: real
            // `.php` files routinely open and close `<?php` around markup,
            // and the PHP-only grammar cannot parse those at all.
            #[cfg(feature = "lang-php")]
            Language::Php => tree_sitter_php::LANGUAGE_PHP.into(),
            // Markdown has no grammar by design, not by trimming — see
            // [`Language::is_compiled_in`].
            #[allow(unreachable_patterns)]
            _ => return None,
        })
    }

    /// The compile-time symbol query source for this language.
    pub(crate) fn symbol_query(self) -> &'static str {
        match self {
            Language::Rust => queries::RUST_SYMBOLS,
            Language::Python => queries::PYTHON_SYMBOLS,
            Language::JavaScript => queries::JS_SYMBOLS,
            Language::TypeScript | Language::Tsx => queries::TS_SYMBOLS,
            Language::Sql => queries::SQL_SYMBOLS,
            Language::Go => queries::GO_SYMBOLS,
            Language::Java => queries::JAVA_SYMBOLS,
            Language::C => queries::C_SYMBOLS,
            Language::Php => queries::PHP_SYMBOLS,
            Language::Markdown => NO_QUERY,
        }
    }

    /// The compile-time import query source for this language.
    pub(crate) fn import_query(self) -> &'static str {
        match self {
            Language::Rust => queries::RUST_IMPORTS,
            Language::Python => queries::PYTHON_IMPORTS,
            Language::JavaScript => queries::JS_IMPORTS,
            Language::TypeScript | Language::Tsx => queries::TS_IMPORTS,
            Language::Sql => queries::SQL_IMPORTS,
            Language::Go => queries::GO_IMPORTS,
            Language::Java => queries::JAVA_IMPORTS,
            Language::C => queries::C_IMPORTS,
            Language::Php => queries::PHP_IMPORTS,
            Language::Markdown => NO_QUERY,
        }
    }

    /// The compile-time call-site query source for this language (#335, B1).
    pub(crate) fn call_query(self) -> &'static str {
        match self {
            Language::Rust => queries::RUST_CALLS,
            Language::Python => queries::PYTHON_CALLS,
            Language::JavaScript => queries::JS_CALLS,
            Language::TypeScript | Language::Tsx => queries::TS_CALLS,
            Language::Sql => queries::SQL_CALLS,
            Language::Go => queries::GO_CALLS,
            Language::Java => queries::JAVA_CALLS,
            Language::C => queries::C_CALLS,
            Language::Php => queries::PHP_CALLS,
            Language::Markdown => NO_QUERY,
        }
    }
}

/// The query source for a language that has no grammar to run one against.
///
/// Unreachable in practice — `LangPack::load` returns before it asks a
/// grammarless language for a query, and `parse_file` answers markdown before
/// it looks for a pack — but a total match needs an arm, and an empty query is
/// the honest value rather than a panic on a path no input can reach.
const NO_QUERY: &str = "";

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Asserts the FULL language set, so it only holds on a build that
    /// compiled every grammar (#1268). A trimmed build correctly maps a
    /// disabled language's extension to `None`; that is the point, not a
    /// regression, and `detection_never_names_a_language_this_build_cannot_parse`
    /// is the invariant that still holds everywhere.
    #[cfg(all(
        feature = "lang-rust",
        feature = "lang-python",
        feature = "lang-typescript",
        feature = "lang-javascript",
        feature = "lang-sql",
        feature = "lang-go",
        feature = "lang-java",
        feature = "lang-c",
        feature = "lang-php"
    ))]
    #[test]
    fn extensions_map_to_languages() {
        assert_eq!(
            Language::from_path(Path::new("a/b.rs")),
            Some(Language::Rust)
        );
        assert_eq!(
            Language::from_path(Path::new("m.py")),
            Some(Language::Python)
        );
        assert_eq!(
            Language::from_path(Path::new("m.pyi")),
            Some(Language::Python)
        );
        assert_eq!(
            Language::from_path(Path::new("x.mjs")),
            Some(Language::JavaScript)
        );
        assert_eq!(
            Language::from_path(Path::new("x.ts")),
            Some(Language::TypeScript)
        );
        assert_eq!(Language::from_path(Path::new("x.tsx")), Some(Language::Tsx));
        assert_eq!(
            Language::from_path(Path::new("migrations/001.sql")),
            Some(Language::Sql)
        );
        assert_eq!(
            Language::from_path(Path::new("README.md")),
            Some(Language::Markdown),
            "165 markdown files in this workspace were invisible to every \
             search the agent had until they became indexable (#3089)"
        );
        assert_eq!(Language::from_path(Path::new("noext")), None);
    }

    /// C++ source and headers index under the C grammar (#3184). Before this
    /// they mapped to `None` — a `.cpp` file declared nothing to the graph, so
    /// `search` had no symbols to rank over a C++ codebase (`build-pov-ray`,
    /// `sqlite`'s amalgamation) and fell to a lexical scan.
    #[cfg(feature = "lang-c")]
    #[test]
    fn cpp_source_and_headers_index_under_the_c_grammar() {
        for probe in [
            "ray.cpp", "scene.cc", "mesh.cxx", "vec.c++", "vec.hpp", "vec.hh", "vec.hxx",
        ] {
            assert_eq!(
                Language::from_path(Path::new(probe)),
                Some(Language::C),
                "`{probe}` must index (as C) so search has C++ symbols to rank"
            );
        }
    }

    /// Markdown carries no grammar and is still always indexable — the one
    /// language whose reader ships with this crate rather than with a
    /// `lang-*` feature.
    #[test]
    fn markdown_is_indexable_in_every_build() {
        assert!(Language::Markdown.is_compiled_in());
        assert!(!Language::Markdown.is_grammar_backed());
        assert_eq!(Language::Markdown.ts_language(), None);
    }

    /// The `default` feature set must reproduce the language coverage that
    /// shipped before the grammars became optional (#1268) — the whole point
    /// of gating them is that nobody taking the default build notices.
    #[cfg(all(
        feature = "lang-rust",
        feature = "lang-python",
        feature = "lang-typescript",
        feature = "lang-javascript",
        feature = "lang-sql",
        feature = "lang-go",
        feature = "lang-java",
        feature = "lang-c",
        feature = "lang-php"
    ))]
    #[test]
    fn the_default_feature_set_still_carries_every_grammar() {
        assert_eq!(
            Language::compiled_in().len(),
            Language::ALL.len(),
            "a default build must index every language it knows; missing: {:?}",
            Language::ALL
                .iter()
                .filter(|l| !l.is_compiled_in())
                .map(|l| l.tag())
                .collect::<Vec<_>>()
        );
    }

    /// `is_compiled_in` and `from_path` must agree, or a trimmed build routes
    /// a file to a parser it has no grammar for — which returns no symbols and
    /// reads to the agent as "this file declares nothing".
    #[test]
    fn detection_never_names_a_language_this_build_cannot_parse() {
        for lang in Language::ALL
            .iter()
            .copied()
            .filter(|l| l.is_grammar_backed())
        {
            assert_eq!(
                lang.is_compiled_in(),
                lang.ts_language().is_some(),
                "`{}` disagrees about whether its grammar is present",
                lang.tag()
            );
        }
        for probe in [
            "a.rs", "a.py", "a.go", "a.java", "a.c", "a.php", "a.sql", "a.tsx", "a.md",
        ] {
            if let Some(lang) = Language::from_path(Path::new(probe)) {
                assert!(
                    lang.is_compiled_in(),
                    "`{probe}` detected as `{}`, whose grammar is absent",
                    lang.tag()
                );
            }
        }
    }
}
