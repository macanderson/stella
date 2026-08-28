mod added_language;

use super::*;
use crate::symbol::SymbolKind;

fn parse(lang: Language, src: &str) -> Parsed {
    let grammars = Grammars::load().expect("grammars compile");
    parse_file(&grammars, lang, src).expect("source parses")
}

fn kinds(parsed: &Parsed, name: &str) -> Vec<SymbolKind> {
    parsed
        .symbols
        .iter()
        .filter(|s| s.name == name)
        .map(|s| s.kind)
        .collect()
}

#[test]
fn all_queries_compile() {
    // Guards the compile-time .scm strings (L-L2): a mis-edit fails here,
    // not at a host's runtime.
    Grammars::load().expect("every language query compiles");
}

#[test]
fn load_is_cached_process_wide() {
    let first = Grammars::load().expect("grammars compile");
    let second = Grammars::load().expect("grammars compile");
    assert!(
        std::sync::Arc::ptr_eq(&first, &second),
        "second load() must hand back the same Arc, not recompile (#4782)"
    );
}

#[test]
fn rust_symbols_with_method_precedence() {
    let src = "\
pub struct Widget { id: u32 }
pub enum Color { Red, Green }
pub trait Draw { fn draw(&self); }
impl Widget {
    pub fn new() -> Self { Widget { id: 0 } }
    fn helper(&self) -> u32 { self.id }
}
pub fn run() {}
";
    let parsed = parse(Language::Rust, src);
    assert_eq!(kinds(&parsed, "Widget"), vec![SymbolKind::Struct]);
    assert_eq!(kinds(&parsed, "Color"), vec![SymbolKind::Enum]);
    assert_eq!(kinds(&parsed, "Draw"), vec![SymbolKind::Trait]);
    assert_eq!(kinds(&parsed, "run"), vec![SymbolKind::Function]);
    // Impl methods are double-captured; Method wins over the general fn.
    assert_eq!(kinds(&parsed, "new"), vec![SymbolKind::Method]);
    assert_eq!(kinds(&parsed, "helper"), vec![SymbolKind::Method]);
    // A trait method *signature* is a plain function, not an impl method.
    assert_eq!(kinds(&parsed, "draw"), vec![SymbolKind::Function]);
    // Spans are 1-based and non-degenerate.
    let widget = parsed.symbols.iter().find(|s| s.name == "Widget").unwrap();
    assert_eq!(widget.start_line, 1);
}

#[test]
fn python_symbols_and_relative_import_decode() {
    let src = "\
import os
from . import helper
from .util import thing
from ..pkg import y

class Widget:
    def method_a(self):
        pass

def top():
    pass
";
    let parsed = parse(Language::Python, src);
    assert_eq!(kinds(&parsed, "Widget"), vec![SymbolKind::Class]);
    assert_eq!(kinds(&parsed, "method_a"), vec![SymbolKind::Method]);
    assert_eq!(kinds(&parsed, "top"), vec![SymbolKind::Function]);

    // `import os` → absolute.
    assert!(
        parsed
            .imports
            .iter()
            .any(|i| matches!(i, ImportSpec::PyAbsolute { specifier } if specifier == "os"))
    );
    // `from . import helper` → level 1, no module path, name `helper`.
    assert!(parsed.imports.iter().any(|i| matches!(
            i,
            ImportSpec::PyRelative { level: 1, module: None, names, .. } if names.iter().any(|n| n == "helper")
        )));
    // `from .util import thing` → level 1, module `util`.
    assert!(parsed.imports.iter().any(|i| matches!(
        i,
        ImportSpec::PyRelative { level: 1, module: Some(m), .. } if m == "util"
    )));
    // `from ..pkg import y` → level 2, module `pkg` (the multi-dot case).
    assert!(parsed.imports.iter().any(|i| matches!(
        i,
        ImportSpec::PyRelative { level: 2, module: Some(m), .. } if m == "pkg"
    )));
}

#[test]
fn typescript_symbols_and_imports() {
    let src = "\
import { a } from './util';
import React from 'react';
export function boot() {}
export const arrow = () => {};
export class App { run() {} }
export interface Shape { x: number }
export enum E { A }
";
    let parsed = parse(Language::TypeScript, src);
    assert_eq!(kinds(&parsed, "App"), vec![SymbolKind::Class]);
    assert_eq!(kinds(&parsed, "boot"), vec![SymbolKind::Function]);
    assert_eq!(kinds(&parsed, "arrow"), vec![SymbolKind::Function]);
    assert_eq!(kinds(&parsed, "run"), vec![SymbolKind::Method]);
    assert_eq!(kinds(&parsed, "Shape"), vec![SymbolKind::Interface]);
    assert_eq!(kinds(&parsed, "E"), vec![SymbolKind::Enum]);

    assert!(
        parsed
            .imports
            .iter()
            .any(|i| matches!(i, ImportSpec::TsRelative { specifier } if specifier == "./util"))
    );
    assert!(
        parsed
            .imports
            .iter()
            .any(|i| matches!(i, ImportSpec::Bare { specifier } if specifier == "react"))
    );
}

#[test]
fn javascript_require_and_non_require_calls() {
    let src = "\
const { add } = require('./math');
function noise() { helper('not-a-require'); }
class R { go() {} }
";
    let parsed = parse(Language::JavaScript, src);
    // Only the real require() call is recorded as an import.
    let import_count = parsed
        .imports
        .iter()
        .filter(|i| matches!(i, ImportSpec::TsRelative { .. } | ImportSpec::Bare { .. }))
        .count();
    assert_eq!(
        import_count, 1,
        "helper('..') is not require and must be ignored"
    );
    assert!(
        parsed
            .imports
            .iter()
            .any(|i| matches!(i, ImportSpec::TsRelative { specifier } if specifier == "./math"))
    );
    assert_eq!(kinds(&parsed, "go"), vec![SymbolKind::Method]);
}

fn callee_names(parsed: &Parsed) -> Vec<&str> {
    parsed.calls.iter().map(|c| c.callee.as_str()).collect()
}

#[test]
fn rust_call_sites_capture_plain_path_method_and_turbofish_callees() {
    let src = "\
pub fn run() {
    helper();
    Widget::new();
    self.render();
    parse::<u32>(\"7\");
    Vec::<u8>::with_capacity(4);
    println!(\"not a call_expression\");
}
";
    let parsed = parse(Language::Rust, src);
    let names = callee_names(&parsed);
    for expected in ["helper", "new", "render", "parse", "with_capacity"] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
    }
    // Macro invocations are not call_expressions and stay out on purpose.
    assert!(!names.contains(&"println"), "{names:?}");
    // Only bare names are stored — never a qualified path fragment.
    assert!(
        parsed.calls.iter().all(|c| !c.callee.contains(':')),
        "{names:?}"
    );
    // Lines are 1-based and point at the callee's name node.
    let helper = parsed.calls.iter().find(|c| c.callee == "helper").unwrap();
    assert_eq!(helper.line, 2);
}

#[test]
fn python_and_typescript_call_sites_keep_the_final_name_only() {
    let py = parse(
        Language::Python,
        "def top():\n    helper()\n    conn.execute(\"x\")\n",
    );
    let py_names = callee_names(&py);
    assert!(py_names.contains(&"helper"), "{py_names:?}");
    assert!(py_names.contains(&"execute"), "{py_names:?}");
    assert!(!py_names.contains(&"conn"), "{py_names:?}");

    let ts = parse(
        Language::TypeScript,
        "function boot() {\n  helper();\n  app.router.dispatch();\n  import('./lazy');\n}\n",
    );
    let ts_names = callee_names(&ts);
    assert!(ts_names.contains(&"helper"), "{ts_names:?}");
    assert!(ts_names.contains(&"dispatch"), "{ts_names:?}");
    // `import(...)`'s callee is the `(import)` node, never an identifier.
    assert!(!ts_names.contains(&"import"), "{ts_names:?}");
}

#[test]
fn a_syntactically_broken_file_still_extracts_what_parsed() {
    // Tree-sitter is error-tolerant: a broken region must not lose the
    // valid symbols around it (skip-with-record, never abort).
    let src = "pub fn good() {}\npub fn (((broken\npub struct Ok;\n";
    let parsed = parse(Language::Rust, src);
    assert!(parsed.symbols.iter().any(|s| s.name == "good"));
    assert!(parsed.symbols.iter().any(|s| s.name == "Ok"));
}

#[test]
fn sql_create_table_extracts_table_and_columns() {
    let src = "\
CREATE TABLE users (
    id SERIAL PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    created_at TIMESTAMP DEFAULT NOW()
);

CREATE TABLE payments (
    id SERIAL PRIMARY KEY,
    amount NUMERIC(10,2) NOT NULL,
    user_id INTEGER REFERENCES users(id)
);

CREATE TYPE payment_status AS ENUM ('pending', 'completed', 'failed');

CREATE VIEW active_payments AS SELECT * FROM payments WHERE amount > 0;
";
    let parsed = parse(Language::Sql, src);

    // Tables
    assert_eq!(kinds(&parsed, "users"), vec![SymbolKind::Table]);
    assert_eq!(kinds(&parsed, "payments"), vec![SymbolKind::Table]);

    // Columns
    assert!(
        parsed
            .symbols
            .iter()
            .any(|s| { s.name == "email" && s.kind == SymbolKind::Column })
    );
    assert!(
        parsed
            .symbols
            .iter()
            .any(|s| { s.name == "amount" && s.kind == SymbolKind::Column })
    );

    // Custom enum type
    assert_eq!(
        kinds(&parsed, "payment_status"),
        vec![SymbolKind::SchemaEnum]
    );

    // View
    assert!(
        parsed
            .symbols
            .iter()
            .any(|s| { s.name == "active_payments" && s.kind == SymbolKind::View })
    );

    // SQL has no imports.
    assert!(parsed.imports.is_empty());
}

#[test]
fn sql_schema_qualified_names_index_the_bare_object_name() {
    // `object_reference` carries qualifiers in grammar fields
    // (`database:`/`schema:`/`name:`); only the `name:` field is the
    // object's identifier. Capturing any other child would index the
    // schema (`public`) as a table and miss lookups by bare name.
    let src = "\
CREATE TABLE public.users (
    id SERIAL PRIMARY KEY
);

CREATE TYPE public.payment_status AS ENUM ('pending', 'completed');

CREATE VIEW public.active_users AS SELECT * FROM public.users;

CREATE TABLE warehouse.analytics.events (id BIGINT);
";
    let parsed = parse(Language::Sql, src);

    assert_eq!(kinds(&parsed, "users"), vec![SymbolKind::Table]);
    assert_eq!(
        kinds(&parsed, "payment_status"),
        vec![SymbolKind::SchemaEnum]
    );
    assert_eq!(kinds(&parsed, "active_users"), vec![SymbolKind::View]);
    assert_eq!(kinds(&parsed, "events"), vec![SymbolKind::Table]);

    // Neither the qualifiers nor the dotted reference may become symbols.
    assert!(
        parsed.symbols.iter().all(|s| !s.name.contains('.')
            && s.name != "public"
            && s.name != "warehouse"
            && s.name != "analytics"),
        "schema qualifiers leaked into the symbol index: {:?}",
        parsed.symbols.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

#[test]
fn diesel_table_macro_detected_as_table() {
    let src = "\
diesel::table! {
    users (id) {
        id -> Int4,
        email -> Varchar,
        created_at -> Timestamptz,
    }
}

pub struct User {
    id: i32,
    email: String,
}
";
    let parsed = parse(Language::Rust, src);
    assert!(
        parsed
            .symbols
            .iter()
            .any(|s| { s.name == "users" && s.kind == SymbolKind::Table }),
        "Diesel table! not detected: {:?}",
        parsed.symbols
    );
    assert_eq!(kinds(&parsed, "User"), vec![SymbolKind::Struct]);
}

#[test]
fn django_model_detected_as_table() {
    let src = "\
from django.db import models

class Payment(models.Model):
    amount = models.DecimalField()
    status = models.CharField(max_length=20)

class Helper:
    pass
";
    let parsed = parse(Language::Python, src);
    assert!(
        parsed
            .symbols
            .iter()
            .any(|s| s.name == "payments" && s.kind == SymbolKind::Table),
        "Django model not detected: {:?}",
        parsed.symbols
    );
    assert_eq!(kinds(&parsed, "Helper"), vec![SymbolKind::Class]);
}

#[test]
fn sqlalchemy_tablename_detected_as_table() {
    let src = "\
from sqlalchemy.orm import declarative_base
Base = declarative_base()

class Order(Base):
    __tablename__ = \"orders\"
    id = Column(Integer, primary_key=True)
";
    let parsed = parse(Language::Python, src);
    assert!(
        parsed.symbols.iter().any(|s| s.kind == SymbolKind::Table),
        "SQLAlchemy model not detected: {:?}",
        parsed.symbols
    );
}

#[test]
fn sqlalchemy_explicit_tablename_overrides_class_name_convention() {
    // `__tablename__` does not match the naive
    // CamelCase->snake_case->pluralize conversion of the class name, so
    // this only passes if the literal value is read rather than derived.
    let src = "\
from sqlalchemy.orm import declarative_base
Base = declarative_base()

class Order(Base):
    __tablename__ = \"customer_orders\"
    id = Column(Integer, primary_key=True)
";
    let parsed = parse(Language::Python, src);
    assert!(
        parsed
            .symbols
            .iter()
            .any(|s| s.name == "customer_orders" && s.kind == SymbolKind::Table),
        "explicit __tablename__ value not used: {:?}",
        parsed.symbols
    );
    assert!(
        !parsed.symbols.iter().any(|s| s.name == "orders"),
        "table name should not fall back to class-name convention: {:?}",
        parsed.symbols
    );
}

#[test]
fn python_unrelated_view_model_not_detected_as_table() {
    let src = "\
class OrderViewModel(ViewModel):
    total = 0
";
    let parsed = parse(Language::Python, src);
    assert!(
        !parsed.symbols.iter().any(|s| s.kind == SymbolKind::Table),
        "unrelated ViewModel base should not be indexed as a table: {:?}",
        parsed.symbols
    );
}

#[test]
fn rust_unrelated_table_suffixed_macro_not_detected() {
    let src = "\
render_table! {
    users (id) {
        id -> Int4,
    }
}
";
    let parsed = parse(Language::Rust, src);
    assert!(
        !parsed.symbols.iter().any(|s| s.kind == SymbolKind::Table),
        "unrelated render_table! macro should not be indexed as a table: {:?}",
        parsed.symbols
    );
}
