//! Decoding tests for the languages added after the first four: Go, Java,
//! C, C++, and PHP — symbols, each language's import mechanisms, call
//! sites, and the extension routing.
//!
//! A sibling file rather than a module inside [`super`]: `parse.rs` sits at
//! the gate's 1500-line ratchet, and the crate README names it as the one
//! to split before the next language arrives. C++ is that language (#3184),
//! so the split happens here.

use super::*;

fn parse(lang: Language, src: &str) -> Parsed {
    let grammars = Grammars::load().expect("grammars load");
    parse_file(&grammars, lang, src).expect("source parses")
}

fn names(parsed: &Parsed) -> Vec<&str> {
    parsed.symbols.iter().map(|s| s.name.as_str()).collect()
}

fn specifiers(parsed: &Parsed) -> Vec<String> {
    parsed
        .imports
        .iter()
        .map(|spec| match spec {
            ImportSpec::Bare { specifier }
            | ImportSpec::PathRelative { specifier }
            | ImportSpec::MarkdownLink { specifier }
            | ImportSpec::TsRelative { specifier }
            | ImportSpec::PyAbsolute { specifier }
            | ImportSpec::RustUse { specifier } => specifier.clone(),
            ImportSpec::PyRelative { text, .. } => text.clone(),
            ImportSpec::RustMod { name } => format!("mod {name}"),
        })
        .collect()
}

#[test]
fn go_symbols_and_grouped_imports() {
    let parsed = parse(
        Language::Go,
        r#"
package main

import (
    "fmt"
    "github.com/pkg/errors"
)

type Scheduler struct{ n int }

type Runner interface{ Run() error }

func New() *Scheduler { return &Scheduler{} }

func (s *Scheduler) Run() error { fmt.Println(s.n); return nil }
"#,
    );
    let names = names(&parsed);
    assert!(names.contains(&"Scheduler"), "struct type: {names:?}");
    assert!(names.contains(&"New"), "func: {names:?}");
    assert!(names.contains(&"Run"), "method: {names:?}");
    assert!(names.contains(&"Runner"), "interface type: {names:?}");
    // Every path in the grouped block, not just the first.
    let specs = specifiers(&parsed);
    assert!(specs.contains(&"fmt".to_string()), "{specs:?}");
    assert!(
        specs.contains(&"github.com/pkg/errors".to_string()),
        "{specs:?}"
    );
}

#[test]
fn java_symbols_and_imports() {
    let parsed = parse(
        Language::Java,
        r#"
package com.example.app;

import java.util.List;
import static java.util.Objects.requireNonNull;

public interface Store { void put(String k); }

public class KvStore implements Store {
    public void put(String k) {}
}

enum Mode { FAST, SAFE }
"#,
    );
    let names = names(&parsed);
    for expected in ["Store", "KvStore", "put", "Mode"] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
    }
    let specs = specifiers(&parsed);
    assert!(specs.contains(&"java.util.List".to_string()), "{specs:?}");
    // `import static` keeps its path, minus the modifier.
    assert!(
        specs.contains(&"java.util.Objects.requireNonNull".to_string()),
        "{specs:?}"
    );
}

#[test]
fn c_symbols_and_both_include_forms() {
    let parsed = parse(
        Language::C,
        r#"
#include <stdio.h>
#include "kvstore.h"

struct Entry { int key; };

typedef struct Entry Entry;

int put(const char *k) { return 0; }
"#,
    );
    let names = names(&parsed);
    assert!(names.contains(&"Entry"), "struct: {names:?}");
    assert!(names.contains(&"put"), "function: {names:?}");
    // The quoted include resolves against the tree; the angled one is a
    // system header and must stay unresolved rather than be guessed.
    let quoted = parsed
        .imports
        .iter()
        .any(|s| matches!(s, ImportSpec::PathRelative { specifier } if specifier == "kvstore.h"));
    let angled = parsed
        .imports
        .iter()
        .any(|s| matches!(s, ImportSpec::Bare { specifier } if specifier == "stdio.h"));
    assert!(
        quoted,
        "quoted include is path-relative: {:?}",
        parsed.imports
    );
    assert!(
        angled,
        "angled include stays unresolved: {:?}",
        parsed.imports
    );
}

/// The witness for #3184: a class method and a namespaced free function come
/// out as distinct symbols, each under its own kind. The tail of the test
/// parses the same source under `Language::C` — the stopgap this replaces —
/// and pins what that loses, so the assertions above are held to a measured
/// difference rather than an assumed one.
#[test]
fn cpp_class_methods_and_namespaced_functions_are_distinct_symbols() {
    let source = r#"
#include <string>
#include "kvstore.hpp"

namespace geometry {

struct Shape { double w; double h; };

double area(const Shape &s) { return s.w * s.h; }

class KvStore {
public:
    void put(const std::string &k);
    int size() const { return count; }
private:
    int count;
};

void KvStore::put(const std::string &k) { count++; }

}  // namespace geometry

template <typename T>
class Box {
public:
    T unwrap() { return value; }
private:
    T value;
};

using Meters = double;
"#;
    let cpp = parse(Language::Cpp, source);
    for expected in [
        "KvStore", "put", "size", "area", "Shape", "Box", "unwrap", "Meters",
    ] {
        assert!(
            names(&cpp).contains(&expected),
            "missing {expected}: {:?}",
            names(&cpp)
        );
    }
    assert!(
        cpp.symbols
            .iter()
            .any(|s| s.name == "put" && s.kind == SymbolKind::Method),
        "`put` is a method, not a free function: {:?}",
        cpp.symbols
    );
    assert!(
        cpp.symbols
            .iter()
            .any(|s| s.name == "area" && s.kind == SymbolKind::Function),
        "`area` is a free function in a namespace: {:?}",
        cpp.symbols
    );
    assert!(
        cpp.symbols
            .iter()
            .any(|s| s.name == "KvStore" && s.kind == SymbolKind::Class),
        "`KvStore` is a class: {:?}",
        cpp.symbols
    );

    // What the stopgap reaches, measured rather than asserted from outside:
    // the same source under the C grammar yields `Shape`, `area` and `put` —
    // a struct, a free function, and the out-of-line definition, all spelled
    // the way C spells them — and loses everything that needs a class body.
    let stopgap = parse(Language::C, source);
    let under_c = names(&stopgap);
    for lost in ["KvStore", "size", "Box", "unwrap", "Meters"] {
        assert!(
            !under_c.contains(&lost),
            "the C grammar cannot reach `{lost}`, so this test would prove \
             nothing if it did: {under_c:?}"
        );
    }
}

/// C++ calls: a qualified call and an explicitly instantiated template call
/// each keep their final name, which is the name their definition is indexed
/// under. Both are spellings the C grammar has no node for.
#[test]
fn cpp_qualified_and_template_call_sites_keep_their_final_name() {
    let cpp = parse(
        Language::Cpp,
        r#"
void run() {
    helper();
    geometry::area(s);
    store.put("k");
    make<Widget>();
}
"#,
    );
    let callees: Vec<&str> = cpp.calls.iter().map(|c| c.callee.as_str()).collect();
    for expected in ["helper", "area", "put", "make"] {
        assert!(
            callees.contains(&expected),
            "missing {expected}: {callees:?}"
        );
    }
}

/// `#include` is the same directive in C++, so it rides C's reader: the
/// quoted form resolves against the tree, the angled form does not.
#[test]
fn cpp_includes_decode_like_c() {
    let cpp = parse(
        Language::Cpp,
        "#include <vector>\n#include \"kvstore.hpp\"\nint main() { return 0; }\n",
    );
    assert!(
        cpp.imports.iter().any(
            |s| matches!(s, ImportSpec::PathRelative { specifier } if specifier == "kvstore.hpp")
        ),
        "{:?}",
        cpp.imports
    );
    assert!(
        cpp.imports
            .iter()
            .any(|s| matches!(s, ImportSpec::Bare { specifier } if specifier == "vector")),
        "{:?}",
        cpp.imports
    );
}

#[test]
fn php_symbols_and_the_two_import_mechanisms() {
    let parsed = parse(
        Language::Php,
        r#"<?php
namespace App;

use App\Contracts\StoreInterface;

require 'bootstrap.php';

interface StoreInterface { public function put($k); }

trait Loggable { public function log($m) {} }

class KvStore implements StoreInterface {
    public function put($k) {}
}

function boot() {}
"#,
    );
    let names = names(&parsed);
    for expected in ["StoreInterface", "Loggable", "KvStore", "put", "boot"] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
    }
    // `require` names a file and resolves; `use` names a namespace and
    // cannot without composer's PSR-4 map.
    let required = parsed.imports.iter().any(
        |s| matches!(s, ImportSpec::PathRelative { specifier } if specifier == "bootstrap.php"),
    );
    let used = parsed.imports.iter().any(
        |s| matches!(s, ImportSpec::Bare { specifier } if specifier.contains("StoreInterface")),
    );
    assert!(required, "require resolves as a path: {:?}", parsed.imports);
    assert!(used, "use is captured unresolved: {:?}", parsed.imports);
}

/// PHP files routinely wrap markup, which the PHP-only grammar cannot
/// parse — the reason `LANGUAGE_PHP` is the one wired in.
#[test]
fn php_with_embedded_html_still_yields_symbols() {
    let parsed = parse(
        Language::Php,
        "<html><body><?php class Page { public function render() {} } ?></body></html>",
    );
    let names = names(&parsed);
    assert!(names.contains(&"Page"), "{names:?}");
    assert!(names.contains(&"render"), "{names:?}");
}

#[test]
fn go_java_c_and_php_call_sites_extract_their_callee_names() {
    let go = parse(
        Language::Go,
        "package main\nfunc run() {\n\thelper()\n\tfmt.Println(\"x\")\n}\n",
    );
    let go_names: Vec<&str> = go.calls.iter().map(|c| c.callee.as_str()).collect();
    assert!(go_names.contains(&"helper"), "{go_names:?}");
    assert!(go_names.contains(&"Println"), "{go_names:?}");

    let java = parse(
        Language::Java,
        "class A { void run() { helper(); store.put(\"k\"); } }\n",
    );
    let java_names: Vec<&str> = java.calls.iter().map(|c| c.callee.as_str()).collect();
    assert!(java_names.contains(&"helper"), "{java_names:?}");
    assert!(java_names.contains(&"put"), "{java_names:?}");

    let c = parse(
        Language::C,
        "int run(void) { helper(); ops->handler(); return 0; }\n",
    );
    let c_names: Vec<&str> = c.calls.iter().map(|call| call.callee.as_str()).collect();
    assert!(c_names.contains(&"helper"), "{c_names:?}");
    assert!(c_names.contains(&"handler"), "{c_names:?}");

    let php = parse(
        Language::Php,
        "<?php function run() { helper(); $s->put('k'); Cache::flush(); }\n",
    );
    let php_names: Vec<&str> = php.calls.iter().map(|c| c.callee.as_str()).collect();
    for expected in ["helper", "put", "flush"] {
        assert!(
            php_names.contains(&expected),
            "missing {expected}: {php_names:?}"
        );
    }

    // SQL models no call graph; its empty query yields no rows.
    let sql = parse(Language::Sql, "CREATE TABLE t (id INT);\n");
    assert!(sql.calls.is_empty());
}

#[test]
fn new_extensions_classify() {
    use std::path::Path;
    assert_eq!(Language::from_path(Path::new("m.go")), Some(Language::Go));
    assert_eq!(
        Language::from_path(Path::new("A.java")),
        Some(Language::Java)
    );
    assert_eq!(Language::from_path(Path::new("k.c")), Some(Language::C));
    assert_eq!(Language::from_path(Path::new("k.h")), Some(Language::C));
    assert_eq!(Language::from_path(Path::new("i.php")), Some(Language::Php));
}
