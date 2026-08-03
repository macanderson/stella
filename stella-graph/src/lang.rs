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
            // Headers index as C. A C++ project's `.h` still yields useful
            // struct/function symbols under the C grammar, and misreading a
            // header beats skipping every declaration in the tree.
            "c" | "h" => Language::C,
            "php" => Language::Php,
            "sql" => Language::Sql,
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
    pub const ALL: [Language; 10] = [
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
    ];

    /// Whether this build carries this language's grammar.
    pub fn is_compiled_in(self) -> bool {
        self.ts_language().is_some()
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
        }
    }
}

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
        assert_eq!(Language::from_path(Path::new("README.md")), None);
        assert_eq!(Language::from_path(Path::new("noext")), None);
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
        for lang in Language::ALL {
            assert_eq!(
                lang.is_compiled_in(),
                lang.ts_language().is_some(),
                "`{}` disagrees about whether its grammar is present",
                lang.tag()
            );
        }
        for probe in [
            "a.rs", "a.py", "a.go", "a.java", "a.c", "a.php", "a.sql", "a.tsx",
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
